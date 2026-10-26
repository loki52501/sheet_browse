//! T-07: the H-6 wrapper (dispatch, ToolCtx::ask, serve_io over MCP stdio framing) and the
//! D-105 FileAsker. docs/01 H-1.3, H-1.4, H-2, H-3, H-6; I-2; D-41, D-102, D-104, D-105.
//!
//! The vault is `Vault::in_memory` with one planted canary (`SB_VAULT_CANARY`, else
//! `canary-mcp-not-a-secret`) and is NOT installed globally: the wrapper must scrub with
//! `harness.vault` on its own. No assertion message formats the canary.
//! `cargo test -p harness-core --test mcp_t07`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harness_core::ask::{self, FileAsker};
use harness_core::class::Class;
use harness_core::class::Mode;
use harness_core::gate::{AskCard, AskOutcome, OnTimeout, StopReason};
use harness_core::ledger::read_jsonl;
use harness_core::mcp::{
    dispatch, serve_io, Classified, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput,
    ToolRegistry, ToolSpec,
};
use harness_core::memory::{
    ts_now, Baseline, Contact, Fact, FactId, Filter, GateRule, Hit, LedgerEvent, LedgerId,
    LedgerMode, MemoryError, MemoryStore, Node, NodeId, Result as MemResult, Review, ReviewId, Run,
    RunStatus, Schedule, Source, ToolHealth, Ts,
};
use harness_core::run::{RunFolder, RunId};
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::{EntryKind, Vault};
use harness_core::Asker;
use notify::Watcher;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const SERVER: &str = "echo";
/// Cap on every wait in these tests (a hung wrapper fails instead of hanging CI).
const CAP: Duration = Duration::from_secs(20);

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "canary-mcp-not-a-secret".to_owned())
}

/// A unique folder under the OS temp dir, removed on drop.
struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "sb-mcp-t07-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            chrono::Utc::now().timestamp_micros()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What a test handler does.
#[derive(Default)]
struct Behaviour {
    classify: Classified,
    /// Resolve `args.secret` at the sink and leak it everywhere in the output (the wrapper
    /// must scrub it all).
    leak: bool,
    /// Fail with the resolved secret in the message.
    fail_with_secret: bool,
    /// Return a challenge stop part-way.
    stop: Option<StopReason>,
    /// Ask mid-call with this card.
    mid_ask: Option<AskCard>,
    /// Panic inside `call` (after the "side effect" counted in `calls`).
    panic: bool,
    /// Wait for this before returning (concurrency tests).
    hold: Option<Arc<tokio::sync::Notify>>,
}

#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    classifies: AtomicUsize,
    /// The sink saw exactly the canary.
    sink_saw_value: Mutex<Option<bool>>,
    class_in_call: Mutex<Option<Class>>,
    mid_outcome: Mutex<Option<AskOutcome>>,
}

struct Handler {
    b: Behaviour,
    probe: Arc<Probe>,
}

#[async_trait]
impl ToolHandler for Handler {
    async fn classify(&self, _ctx: &ToolCtx, _args: &Value) -> Result<Classified, ToolError> {
        self.probe.classifies.fetch_add(1, Ordering::SeqCst);
        Ok(self.b.classify.clone())
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        self.probe.calls.fetch_add(1, Ordering::SeqCst);
        *self.probe.class_in_call.lock().unwrap() = Some(ctx.class);
        if let Some(card) = &self.b.mid_ask {
            let o = ctx.ask(card.clone()).await;
            *self.probe.mid_outcome.lock().unwrap() = Some(o);
        }
        if self.b.panic {
            panic!("handler fell over");
        }
        if let Some(hold) = &self.b.hold {
            hold.notified().await;
        }
        let template = args
            .get("secret")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if self.b.leak || self.b.fail_with_secret {
            let want = canary();
            // A misbehaving handler: copies the resolved value out of the sink.
            let leaked = ctx.resolve_into(&template, |r| {
                *self.probe.sink_saw_value.lock().unwrap() = Some(r.expose() == want);
                r.expose().to_string()
            })?;
            if self.b.fail_with_secret {
                return Err(ToolError::Failed(format!("boom: {leaked}")));
            }
            let file = ctx.run_dir.join("files").join("out.txt");
            std::fs::write(&file, "deliverable without secrets").unwrap();
            return Ok(ToolOutput {
                text: format!("sent {leaked}\nsecond line {leaked}"),
                summary: Some(format!("sent {leaked}")),
                consequences: json!({"sent": leaked, "n": 1}),
                files: vec![PathBuf::from(format!("files/{leaked}.txt")), file],
                stopped: None,
            });
        }
        Ok(ToolOutput {
            text: "sent\nmore".into(),
            summary: None,
            consequences: json!(["message sent"]),
            files: Vec::new(),
            stopped: self.b.stop.clone(),
        })
    }
}

struct Env {
    root: Tmp,
    run: RunFolder,
    store: Arc<MemStore>,
    asker: Arc<ScriptedAsker>,
    harness: Harness,
}

async fn env(asker: ScriptedAsker) -> Env {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "t07", "mcp test").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "mcp test".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    store.add_contact("friend@example.com");
    let asker = Arc::new(asker);
    let vault = Arc::new(Vault::in_memory([(
        "test".to_owned(),
        canary(),
        EntryKind::Secret,
    )]));
    let harness = Harness {
        store: store.clone(),
        vault,
        asker: asker.clone(),
        runs_root: root.0.clone(),
        run: Some(run.id.clone()),
    };
    Env {
        root,
        run,
        store,
        asker,
        harness,
    }
}

fn tool(name: &str, class: Class, b: Behaviour) -> (ToolSpec, Arc<Probe>) {
    let probe = Arc::new(Probe::default());
    (
        ToolSpec {
            name: name.into(),
            description: format!("{name} test tool"),
            class,
            args: json!({"type": "object", "properties": {"to": {"type": "string"}}}),
            handler: Arc::new(Handler {
                b,
                probe: probe.clone(),
            }),
        },
        probe,
    )
}

fn send_to(to: &str) -> Behaviour {
    Behaviour {
        classify: Classified {
            recipients: vec![to.into()],
            domain: Some("mail.example".into()),
            ..Classified::default()
        },
        ..Behaviour::default()
    }
}

fn events(e: &Env) -> Vec<LedgerEvent> {
    e.store.ledger().into_iter().map(|(_, ev)| ev).collect()
}

/// The jsonl mirror equals the store, row for row (keyed by id).
fn assert_mirror(e: &Env) {
    let mut table = e.store.ledger();
    table.sort_by_key(|(id, _)| *id);
    let mut lines = read_jsonl(&e.run.ledger_jsonl()).unwrap();
    lines.sort_by_key(|(id, _)| *id);
    assert_eq!(table, lines, "ledger.jsonl must match the table");
}

/// No file under `dir` contains `needle` (bytes).
fn assert_clean_tree(dir: &Path, needle: &str) {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, &mut files);
    assert!(!files.is_empty());
    for f in files {
        let bytes = std::fs::read(&f).unwrap();
        let hit = bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
        assert!(!hit, "a vault value leaked into {}", f.display());
        assert!(
            !f.to_string_lossy().contains(needle),
            "a vault value leaked into a file name"
        );
    }
}

#[tokio::test]
async fn allow_writes_one_tool_event_and_the_mirror_matches() {
    let e = env(ScriptedAsker::new()).await;
    let (spec, probe) = tool("echo.send", Class::Send, send_to("Friend@Example.com"));
    let reg = ToolRegistry::new(vec![spec]);
    let out = dispatch(
        SERVER,
        &reg,
        &e.harness,
        "echo.send",
        json!({"to": "friend@example.com"}),
    )
    .await
    .unwrap();
    assert_eq!(out.text, "sent\nmore");
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert_eq!(*probe.class_in_call.lock().unwrap(), Some(Class::Send));
    assert!(e.asker.cards().is_empty());

    let ev = events(&e);
    assert_eq!(ev.len(), 1, "{ev:?}");
    let t = &ev[0];
    assert_eq!(t.server, SERVER);
    assert_eq!(t.tool, "echo.send");
    assert_eq!(t.class, Some(Class::Send));
    assert_eq!(t.mode, Some(LedgerMode::Allow));
    assert!(t.ok);
    assert!(t.duration_ms >= 0);
    assert_eq!(t.run_id.as_ref(), Some(&e.run.id));
    assert_eq!(t.result_summary, "sent");
    assert_eq!(t.consequences, json!(["message sent"]));
    assert_eq!(t.args, json!({"to": "friend@example.com"}));
    assert_mirror(&e);
}

#[tokio::test]
async fn unknown_tool_and_missing_run_are_errors() {
    let mut e = env(ScriptedAsker::new()).await;
    let (spec, probe) = tool("echo.send", Class::Read, Behaviour::default());
    let reg = ToolRegistry::new(vec![spec]);
    let err = dispatch(SERVER, &reg, &e.harness, "nope", json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Args(_)), "{err}");
    e.harness.run = None;
    let err = dispatch(SERVER, &reg, &e.harness, "echo.send", json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ToolError::Failed(m) if m == "no run"),
        "{err}"
    );
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    assert!(events(&e).is_empty());
}

#[tokio::test]
async fn classify_raising_send_to_pay_changes_the_gate() {
    let e = env(ScriptedAsker::new()).await;
    let mut b = send_to("friend@example.com");
    b.classify.class = Some(Class::Pay);
    b.classify.what = Some("pay 10 EUR on shop.example".into());
    let (spec, probe) = tool("echo.send", Class::Send, b);
    let reg = ToolRegistry::new(vec![spec]);
    let out = dispatch(
        SERVER,
        &reg,
        &e.harness,
        "echo.send",
        json!({"to": "friend@example.com"}),
    )
    .await
    .unwrap();
    // Pay asks and times out to skip: the handler never runs.
    assert!(out.text.starts_with("skipped: "), "{}", out.text);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    let cards = e.asker.cards();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].class, Class::Pay);
    assert_eq!(cards[0].on_timeout, OnTimeout::Skip);

    let ev = events(&e);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0].server, "core");
    assert_eq!(ev[0].tool, "echo.send");
    assert_eq!(ev[0].class, Some(Class::Pay));
    assert_eq!(ev[0].mode, Some(LedgerMode::TimeoutSkip));
    assert_eq!(ev[0].why.as_deref(), Some("pay 10 EUR on shop.example"));
    assert_mirror(&e);
}

#[tokio::test]
async fn classify_lowering_the_class_is_ignored() {
    let e = env(ScriptedAsker::new()).await;
    let b = Behaviour {
        classify: Classified {
            class: Some(Class::Read),
            ..Classified::default()
        },
        ..Behaviour::default()
    };
    let (spec, probe) = tool("shop.pay", Class::Pay, b);
    let reg = ToolRegistry::new(vec![spec]);
    let out = dispatch(SERVER, &reg, &e.harness, "shop.pay", json!({}))
        .await
        .unwrap();
    assert!(out.text.starts_with("skipped: "));
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    assert_eq!(e.asker.cards()[0].class, Class::Pay);
    assert_eq!(events(&e)[0].class, Some(Class::Pay));
}

#[tokio::test]
async fn ask_timeout_proceed_runs_the_tool_and_logs_both_events() {
    let e = env(ScriptedAsker::new()).await;
    let (spec, probe) = tool("echo.send", Class::Send, send_to("stranger@example.com"));
    let reg = ToolRegistry::new(vec![spec]);
    let out = dispatch(
        SERVER,
        &reg,
        &e.harness,
        "echo.send",
        json!({"to": "stranger@example.com", "body": "hi"}),
    )
    .await
    .unwrap();
    assert_eq!(out.text, "sent\nmore");
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    let cards = e.asker.cards();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].on_timeout, OnTimeout::Proceed);

    let ev = events(&e);
    assert_eq!(ev.len(), 2, "{ev:?}");
    let (core, t) = (&ev[0], &ev[1]);
    assert_eq!(core.server, "core");
    assert_eq!(core.mode, Some(LedgerMode::TimeoutProceed));
    assert_eq!(core.class, Some(Class::Send));
    assert_eq!(core.why.as_deref(), Some(cards[0].what.as_str()));
    assert_eq!(t.server, SERVER);
    assert_eq!(t.mode, Some(LedgerMode::TimeoutProceed));
    assert!(t.ok);
    assert_mirror(&e);
}

#[tokio::test]
async fn ask_answered_skip_does_not_call_the_handler() {
    let e = env(ScriptedAsker::with([AskOutcome::AnsweredSkip])).await;
    let (spec, probe) = tool("echo.send", Class::Send, send_to("stranger@example.com"));
    let reg = ToolRegistry::new(vec![spec]);
    let out = dispatch(SERVER, &reg, &e.harness, "echo.send", json!({"to": "x"}))
        .await
        .unwrap();
    assert!(out.text.starts_with("skipped: you said no"), "{}", out.text);
    assert!(out.stopped.is_none());
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    let ev = events(&e);
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].mode, Some(LedgerMode::AnsweredSkip));
    assert_eq!(ev[0].server, "core");
}

#[tokio::test]
async fn ask_answered_proceed_marks_the_tool_event() {
    let e = env(ScriptedAsker::with([AskOutcome::AnsweredProceed])).await;
    let (spec, probe) = tool("shop.pay", Class::Pay, Behaviour::default());
    let reg = ToolRegistry::new(vec![spec]);
    dispatch(SERVER, &reg, &e.harness, "shop.pay", json!({}))
        .await
        .unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    let modes: Vec<_> = events(&e)
        .iter()
        .map(|x| (x.server.clone(), x.mode))
        .collect();
    assert_eq!(
        modes,
        vec![
            ("core".to_string(), Some(LedgerMode::AnsweredProceed)),
            (SERVER.to_string(), Some(LedgerMode::AnsweredProceed)),
        ]
    );
}

#[tokio::test]
async fn challenge_stops_before_the_handler() {
    let e = env(ScriptedAsker::new()).await;
    let b = Behaviour {
        classify: Classified {
            class: Some(Class::Challenge),
            ..Classified::default()
        },
        ..Behaviour::default()
    };
    let (spec, probe) = tool("act", Class::Fill, b);
    let reg = ToolRegistry::new(vec![spec]);
    let out = dispatch(SERVER, &reg, &e.harness, "act", json!({"element": "#31"}))
        .await
        .unwrap();
    let stop = out.stopped.clone().expect("stopped");
    assert_eq!(stop.class, Class::Challenge);
    assert_eq!(stop.element.as_deref(), Some("#31"));
    assert!(
        out.text.starts_with("stopped: challenge at #31"),
        "{}",
        out.text
    );
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    assert!(e.asker.cards().is_empty());
    let ev = events(&e);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0].server, "core");
    assert_eq!(ev[0].mode, Some(LedgerMode::Stop));
    assert_eq!(ev[0].class, Some(Class::Challenge));
    assert_eq!(ev[0].why.as_deref(), Some(stop.why.as_str()));
    assert_eq!(ev[0].consequences, json!({"element": "#31"}));
    assert_mirror(&e);
}

#[tokio::test]
async fn handler_stop_writes_a_stop_tool_event_and_a_core_stop() {
    let e = env(ScriptedAsker::new()).await;
    let reason = StopReason {
        class: Class::Challenge,
        why: "turnstile on the register form".into(),
        element: Some("#44".into()),
    };
    let b = Behaviour {
        stop: Some(reason.clone()),
        ..Behaviour::default()
    };
    let (spec, probe) = tool("act", Class::Submit, b);
    let reg = ToolRegistry::new(vec![spec]);
    let out = dispatch(SERVER, &reg, &e.harness, "act", json!({}))
        .await
        .unwrap();
    assert_eq!(out.stopped.as_ref(), Some(&reason));
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    let ev = events(&e);
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[0].server, SERVER);
    assert_eq!(ev[0].mode, Some(LedgerMode::Stop));
    assert_eq!(ev[0].why.as_deref(), Some(reason.why.as_str()));
    assert_eq!(ev[1].server, "core");
    assert_eq!(ev[1].mode, Some(LedgerMode::Stop));
    assert_eq!(ev[1].consequences, json!({"element": "#44"}));
    assert_mirror(&e);
}

#[tokio::test]
async fn vault_value_reaches_the_sink_and_nothing_else() {
    let e = env(ScriptedAsker::new()).await;
    let c = canary();
    let b = Behaviour {
        leak: true,
        ..Behaviour::default()
    };
    let (spec, probe) = tool("echo.leak", Class::Fill, b);
    let reg = ToolRegistry::new(vec![spec]);
    let out = dispatch(
        SERVER,
        &reg,
        &e.harness,
        "echo.leak",
        json!({"secret": "@vault(test)", "also": c.clone()}),
    )
    .await
    .unwrap();
    assert_eq!(*probe.sink_saw_value.lock().unwrap(), Some(true));
    let rendered = serde_json::to_string(&out).unwrap();
    assert!(!rendered.contains(&c), "returned output must be scrubbed");
    assert!(out.text.contains("@vault(test)"));
    for ev in events(&e) {
        let row = serde_json::to_string(&ev).unwrap();
        assert!(!row.contains(&c), "ledger row must be scrubbed");
    }
    assert_eq!(events(&e)[0].args["also"], json!("@vault(test)"));
    assert_mirror(&e);
    assert_clean_tree(&e.root.0, &c);
}

#[tokio::test]
async fn handler_error_carrying_the_value_is_scrubbed() {
    let e = env(ScriptedAsker::new()).await;
    let c = canary();
    let b = Behaviour {
        fail_with_secret: true,
        ..Behaviour::default()
    };
    let (spec, _probe) = tool("echo.fail", Class::Read, b);
    let reg = ToolRegistry::new(vec![spec]);
    let err = dispatch(
        SERVER,
        &reg,
        &e.harness,
        "echo.fail",
        json!({"secret": "@vault(test)"}),
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(!msg.contains(&c), "error must be scrubbed");
    assert!(msg.contains("@vault(test)"));
    let ev = events(&e);
    assert_eq!(ev.len(), 1);
    assert!(!ev[0].ok);
    assert!(ev[0].result_summary.starts_with("error: "));
    assert!(!serde_json::to_string(&ev[0]).unwrap().contains(&c));
    assert_clean_tree(&e.root.0, &c);
}

#[tokio::test]
async fn mid_call_ask_is_logged_and_cannot_loosen_its_timeout() {
    let e = env(ScriptedAsker::new()).await;
    let b = Behaviour {
        mid_ask: Some(AskCard {
            class: Class::Pay,
            what: format!("download\r\n\x1b[31m{}", canary()),
            site: Some("shop.example".into()),
            values: vec![("card".into(), canary())],
            timeout: Duration::from_millis(1),
            on_timeout: OnTimeout::Proceed,
        }),
        ..Behaviour::default()
    };
    let (spec, probe) = tool("act", Class::Navigate, b);
    let reg = ToolRegistry::new(vec![spec]);
    dispatch(SERVER, &reg, &e.harness, "act", json!({}))
        .await
        .unwrap();
    assert_eq!(
        *probe.mid_outcome.lock().unwrap(),
        Some(AskOutcome::TimeoutSkip)
    );
    let card = &e.asker.cards()[0];
    assert_eq!(card.on_timeout, OnTimeout::Skip);
    assert_eq!(card.what, "download @vault(test)");
    assert_eq!(card.values, vec![("card".into(), "@vault(test)".into())]);
    let ev = events(&e);
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[0].server, "core");
    assert_eq!(ev[0].tool, "act");
    assert_eq!(ev[0].mode, Some(LedgerMode::TimeoutSkip));
    assert_eq!(ev[0].why.as_deref(), Some("download @vault(test)"));
    assert_eq!(ev[1].server, SERVER);
    assert_mirror(&e);
}

// ---- serve_io over a duplex pipe (D-104) ----

async fn next_msg<R: tokio::io::AsyncBufRead + Unpin>(lines: &mut tokio::io::Lines<R>) -> Value {
    let line = tokio::time::timeout(CAP, lines.next_line())
        .await
        .expect("a reply within the cap")
        .unwrap()
        .expect("a line");
    serde_json::from_str(&line).expect("one JSON value per line")
}

#[tokio::test]
async fn serve_io_round_trip_and_list_changed() {
    let e = env(ScriptedAsker::new()).await;
    let c = canary();
    let (spec, _probe) = tool("echo.send", Class::Send, send_to("friend@example.com"));
    let (leak, _) = tool(
        "echo.leak",
        Class::Fill,
        Behaviour {
            leak: true,
            ..Behaviour::default()
        },
    );
    let reg = ToolRegistry::new(vec![spec, leak]);

    let (client, server_end) = tokio::io::duplex(1 << 16);
    let (sr, sw) = tokio::io::split(server_end);
    let task = tokio::spawn(serve_io(
        SERVER,
        reg.clone(),
        e.harness.clone(),
        BufReader::new(sr),
        sw,
    ));
    let (cr, mut cw) = tokio::io::split(client);
    let mut lines = BufReader::new(cr).lines();
    let send = |v: Value| {
        let mut s = serde_json::to_string(&v).unwrap();
        s.push('\n');
        s
    };

    cw.write_all(send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}})).as_bytes())
        .await
        .unwrap();
    let init = next_msg(&mut lines).await;
    assert_eq!(init["id"], json!(1));
    assert_eq!(init["result"]["protocolVersion"], json!("2025-06-18"));
    assert_eq!(
        init["result"]["capabilities"]["tools"]["listChanged"],
        json!(true)
    );
    assert_eq!(init["result"]["serverInfo"]["name"], json!(SERVER));

    cw.write_all(send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).as_bytes())
        .await
        .unwrap();
    cw.write_all(send(json!({"jsonrpc": "2.0", "id": "p", "method": "ping"})).as_bytes())
        .await
        .unwrap();
    let pong = next_msg(&mut lines).await;
    assert_eq!(pong, json!({"jsonrpc": "2.0", "id": "p", "result": {}}));

    cw.write_all(send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).as_bytes())
        .await
        .unwrap();
    let list = next_msg(&mut lines).await;
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], json!("echo.send"));
    assert_eq!(tools[0]["description"], json!("echo.send test tool"));
    assert_eq!(tools[0]["inputSchema"]["type"], json!("object"));

    cw.write_all(
        send(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "echo.send", "arguments": {"to": "friend@example.com"}}}))
        .as_bytes(),
    )
    .await
    .unwrap();
    let call = next_msg(&mut lines).await;
    assert_eq!(call["id"], json!(3));
    assert_eq!(call["result"]["isError"], json!(false));
    assert_eq!(call["result"]["content"][0]["type"], json!("text"));
    assert_eq!(call["result"]["content"][0]["text"], json!("sent\nmore"));

    cw.write_all(
        send(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {"name": "echo.leak", "arguments": {"secret": "@vault(test)"}}}))
        .as_bytes(),
    )
    .await
    .unwrap();
    let leak = next_msg(&mut lines).await;
    assert!(!leak.to_string().contains(&c), "stdout must be scrubbed");
    assert_eq!(leak["result"]["isError"], json!(false));

    // Site tool joins: the server announces it.
    let (site, _) = tool("luma.register", Class::Submit, Behaviour::default());
    reg.add(site);
    let note = next_msg(&mut lines).await;
    assert_eq!(
        note,
        json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
    );

    cw.write_all(send(json!({"jsonrpc": "2.0", "id": 5, "method": "nope"})).as_bytes())
        .await
        .unwrap();
    assert_eq!(next_msg(&mut lines).await["error"]["code"], json!(-32601));
    cw.write_all(b"{not json\n").await.unwrap();
    let bad = next_msg(&mut lines).await;
    assert_eq!(bad["error"]["code"], json!(-32700));
    assert_eq!(bad["id"], Value::Null);
    cw.write_all(
        send(json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call",
        "params": {"name": "missing"}}))
        .as_bytes(),
    )
    .await
    .unwrap();
    assert_eq!(next_msg(&mut lines).await["error"]["code"], json!(-32602));

    // stdin closes: serve_io returns.
    cw.shutdown().await.unwrap();
    tokio::time::timeout(CAP, task)
        .await
        .expect("serve_io ends at EOF")
        .unwrap()
        .unwrap();
    assert_eq!(events(&e).len(), 2);
    assert_mirror(&e);
    assert_clean_tree(&e.root.0, &c);
}

#[tokio::test]
async fn serve_io_reports_tool_errors_as_is_error() {
    let e = env(ScriptedAsker::new()).await;
    let c = canary();
    let (spec, _) = tool(
        "echo.fail",
        Class::Read,
        Behaviour {
            fail_with_secret: true,
            ..Behaviour::default()
        },
    );
    let reg = ToolRegistry::new(vec![spec]);
    let input = format!(
        "{}\n",
        json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": {"name": "echo.fail", "arguments": {"secret": "@vault(test)"}}})
    );
    let mut out = Vec::new();
    tokio::time::timeout(
        CAP,
        serve_io(SERVER, reg, e.harness.clone(), input.as_bytes(), &mut out),
    )
    .await
    .unwrap()
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(!text.contains(&c));
    let reply: Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(reply["id"], json!(9));
    assert_eq!(reply["result"]["isError"], json!(true));
    assert!(reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("@vault(test)"));
}

// ---- FileAsker (D-105) ----

fn card(timeout: Duration, on_timeout: OnTimeout, what: &str) -> AskCard {
    AskCard {
        class: Class::Send,
        what: what.into(),
        site: Some("mail.example".into()),
        values: vec![("to".into(), "new@example.com".into())],
        timeout,
        on_timeout,
    }
}

/// Waits (notify watch on the parent, one check after arming, capped) until `path` exists.
async fn wait_for(path: &Path) {
    let dir = path.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut w = notify::recommended_watcher(move |_r: notify::Result<notify::Event>| {
        let _ = tx.send(());
    })
    .unwrap();
    w.watch(&dir, notify::RecursiveMode::NonRecursive).unwrap();
    let wait = async {
        while !path.exists() {
            rx.recv().await;
        }
    };
    tokio::time::timeout(CAP, wait)
        .await
        .expect("file appears within the cap");
}

async fn file_ask(yes: Option<bool>, timeout: Duration, on_timeout: OnTimeout) -> AskOutcome {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask test").unwrap();
    let vault = Arc::new(Vault::in_memory([(
        "test".to_owned(),
        canary(),
        EntryKind::Secret,
    )]));
    let asker = FileAsker::new(&root.0, run.id.clone(), vault);
    let c = card(timeout, on_timeout, &format!("send {} to new", canary()));
    let asking = asker.ask(&c);
    let answering = async {
        let json = run.dir.join("asks").join("1.json");
        wait_for(&json).await;
        let doc = std::fs::read_to_string(&json).unwrap();
        assert!(!doc.contains(&canary()), "the card file must be scrubbed");
        let v: Value = serde_json::from_str(&doc).unwrap();
        assert_eq!(v["what"], json!("send @vault(test) to new"));
        assert_eq!(v["n"], json!(1));
        if let Some(yes) = yes {
            ask::answer(&root.0, &run.id, 1, yes).unwrap();
        }
    };
    let (outcome, ()) = tokio::time::timeout(CAP, async { tokio::join!(asking, answering) })
        .await
        .expect("ask ends within the cap");
    if let Some(yes) = yes {
        let ans = std::fs::read_to_string(run.dir.join("asks").join("1.answer")).unwrap();
        assert_eq!(ans.trim(), if yes { "yes" } else { "no" });
    }
    outcome
}

#[tokio::test]
async fn file_asker_answered_yes() {
    let o = file_ask(Some(true), CAP, OnTimeout::Skip).await;
    assert_eq!(o, AskOutcome::AnsweredProceed);
}

#[tokio::test]
async fn file_asker_answered_no() {
    let o = file_ask(Some(false), CAP, OnTimeout::Proceed).await;
    assert_eq!(o, AskOutcome::AnsweredSkip);
}

#[tokio::test]
async fn file_asker_times_out_per_card() {
    let short = Duration::from_millis(300);
    assert_eq!(
        file_ask(None, short, OnTimeout::Skip).await,
        AskOutcome::TimeoutSkip
    );
    assert_eq!(
        file_ask(None, short, OnTimeout::Proceed).await,
        AskOutcome::TimeoutProceed
    );
}

#[tokio::test]
async fn file_asker_numbers_asks_and_answer_checks_the_card() {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask test").unwrap();
    let asker = FileAsker::new(&root.0, run.id.clone(), Arc::new(Vault::in_memory([])));
    let short = Duration::from_millis(50);
    asker.ask(&card(short, OnTimeout::Skip, "one")).await;
    asker.ask(&card(short, OnTimeout::Skip, "two")).await;
    let second: Value =
        serde_json::from_str(&std::fs::read_to_string(run.dir.join("asks/2.json")).unwrap())
            .unwrap();
    assert_eq!(second["what"], json!("two"));
    assert!(second["answer_with"]
        .as_str()
        .unwrap()
        .ends_with(" 2 yes|no"));
    let err = ask::answer(&root.0, &run.id, 7, true).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    let err = ask::answer(&root.0, &RunId("../x".into()), 1, true).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

// ---- round 2 (review-T-07): gate rules on mid-call asks, fail-closed paths, panics,
// concurrency, transport robustness, FileAsker closing (D-119) ----

fn mid_ask(class: Class, site: &str, what: &str) -> AskCard {
    AskCard {
        class,
        what: what.into(),
        site: Some(site.into()),
        values: Vec::new(),
        timeout: Duration::from_millis(1),
        on_timeout: OnTimeout::Proceed,
    }
}

fn asking(card: AskCard) -> Behaviour {
    Behaviour {
        mid_ask: Some(card),
        ..Behaviour::default()
    }
}

#[tokio::test]
async fn mid_call_challenge_is_logged_as_a_stop_and_never_shown() {
    let e = env(ScriptedAsker::new()).await;
    let b = asking(mid_ask(Class::Challenge, "x.example", "turnstile on login"));
    let (spec, probe) = tool("act", Class::Navigate, b);
    let reg = ToolRegistry::new(vec![spec]);
    dispatch(SERVER, &reg, &e.harness, "act", json!({}))
        .await
        .unwrap();
    assert_eq!(
        *probe.mid_outcome.lock().unwrap(),
        Some(AskOutcome::TimeoutSkip)
    );
    assert!(
        e.asker.cards().is_empty(),
        "a challenge is never asked (D-5)"
    );
    let ev = events(&e);
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[0].server, "core");
    assert_eq!(ev[0].tool, "act");
    assert_eq!(ev[0].class, Some(Class::Challenge));
    assert_eq!(ev[0].mode, Some(LedgerMode::Stop));
    assert_eq!(
        ev[0].why.as_deref(),
        Some("challenge during act: turnstile on login")
    );
    assert_eq!(ev[1].server, SERVER);
    assert_mirror(&e);
}

#[tokio::test]
async fn mid_call_ask_meets_a_stop_rule() {
    let e = env(ScriptedAsker::new()).await;
    e.store
        .add_rule("x.example", "Account", Mode::Stop, false)
        .unwrap();
    let b = asking(mid_ask(Class::Account, "x.example", "change the email"));
    let (spec, probe) = tool("act", Class::Navigate, b);
    let reg = ToolRegistry::new(vec![spec]);
    dispatch(SERVER, &reg, &e.harness, "act", json!({"element": "#9"}))
        .await
        .unwrap();
    assert_eq!(
        *probe.mid_outcome.lock().unwrap(),
        Some(AskOutcome::TimeoutSkip)
    );
    assert!(e.asker.cards().is_empty(), "a stop rule shows no card");
    let ev = events(&e);
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[0].server, "core");
    assert_eq!(ev[0].class, Some(Class::Account));
    assert_eq!(ev[0].mode, Some(LedgerMode::Stop));
    assert!(
        ev[0].why.as_deref().unwrap_or("").contains("x.example"),
        "{:?}",
        ev[0].why
    );
    assert_eq!(ev[0].consequences, json!({"element": "#9"}));
    assert_mirror(&e);
}

#[tokio::test]
async fn mid_call_ask_meets_strict_and_hint_rules() {
    // Control: Account on a site with only the seed ('*', Account, ask) may proceed.
    let e = env(ScriptedAsker::new()).await;
    let b = asking(mid_ask(Class::Account, "y.example", "change the email"));
    let (spec, probe) = tool("act", Class::Navigate, b);
    let reg = ToolRegistry::new(vec![spec]);
    dispatch(SERVER, &reg, &e.harness, "act", json!({}))
        .await
        .unwrap();
    assert_eq!(e.asker.cards()[0].on_timeout, OnTimeout::Proceed);
    assert_eq!(
        *probe.mid_outcome.lock().unwrap(),
        Some(AskOutcome::TimeoutProceed)
    );

    // A strict rule on the card's site: skips on timeout (H-1.3 "or a rule").
    let e = env(ScriptedAsker::new()).await;
    e.store
        .add_rule("x.example", "Account", Mode::Ask, true)
        .unwrap();
    let b = asking(mid_ask(Class::Account, "x.example", "change the email"));
    let (spec, probe) = tool("act", Class::Navigate, b);
    let reg = ToolRegistry::new(vec![spec]);
    dispatch(SERVER, &reg, &e.harness, "act", json!({}))
        .await
        .unwrap();
    assert_eq!(e.asker.cards()[0].on_timeout, OnTimeout::Skip);
    assert_eq!(
        *probe.mid_outcome.lock().unwrap(),
        Some(AskOutcome::TimeoutSkip)
    );
    assert_eq!(events(&e)[0].mode, Some(LedgerMode::TimeoutSkip));

    // A hint rule hit through the call's classifier hints: skips on timeout (D-110 (4)).
    let e = env(ScriptedAsker::new()).await;
    e.store
        .add_rule("*", "Account:wire", Mode::Ask, false)
        .unwrap();
    let mut b = asking(mid_ask(Class::Account, "y.example", "change the email"));
    b.classify.hints = vec!["wire".into()];
    let (spec, probe) = tool("act", Class::Navigate, b);
    let reg = ToolRegistry::new(vec![spec]);
    dispatch(SERVER, &reg, &e.harness, "act", json!({}))
        .await
        .unwrap();
    assert_eq!(e.asker.cards()[0].on_timeout, OnTimeout::Skip);
    assert_eq!(
        *probe.mid_outcome.lock().unwrap(),
        Some(AskOutcome::TimeoutSkip)
    );
}

/// A harness over a fresh MemStore whose run row is `strict` (or absent when `None`).
async fn with_run_row(e: &Env, strict: Option<bool>) -> (Harness, Arc<MemStore>) {
    let store = Arc::new(MemStore::new());
    if let Some(strict) = strict {
        let mut run = e.store.run_get(&e.run.id).await.unwrap().unwrap();
        run.strict = strict;
        store.run_start(run).await.unwrap();
    }
    let mut h = e.harness.clone();
    h.store = store.clone();
    (h, store)
}

#[tokio::test]
async fn strict_run_send_to_a_stranger_times_out_to_skip() {
    let e = env(ScriptedAsker::new()).await;
    let (h, store) = with_run_row(&e, Some(true)).await;
    let (spec, probe) = tool("echo.send", Class::Send, send_to("stranger@example.com"));
    let reg = ToolRegistry::new(vec![spec]);
    let out = dispatch(
        SERVER,
        &reg,
        &h,
        "echo.send",
        json!({"to": "stranger@example.com"}),
    )
    .await
    .unwrap();
    assert!(out.text.starts_with("skipped: no answer"), "{}", out.text);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    assert_eq!(e.asker.cards()[0].on_timeout, OnTimeout::Skip);
    let ev: Vec<_> = store.ledger().into_iter().map(|(_, x)| x).collect();
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0].mode, Some(LedgerMode::TimeoutSkip));
}

#[tokio::test]
async fn a_run_without_a_runs_row_reads_as_strict() {
    let e = env(ScriptedAsker::new()).await;
    let (h, _store) = with_run_row(&e, None).await;
    let (spec, probe) = tool("echo.send", Class::Send, send_to("stranger@example.com"));
    let reg = ToolRegistry::new(vec![spec]);
    let out = dispatch(
        SERVER,
        &reg,
        &h,
        "echo.send",
        json!({"to": "stranger@example.com"}),
    )
    .await
    .unwrap();
    assert!(out.text.starts_with("skipped: "), "{}", out.text);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    assert_eq!(e.asker.cards()[0].on_timeout, OnTimeout::Skip);
}

// -- a store with switchable failures --

#[derive(Default)]
struct Flaky {
    inner: MemStore,
    fail_append: AtomicBool,
    fail_run_get: AtomicBool,
    fail_gate: AtomicBool,
}

fn injected(what: &str) -> MemoryError {
    MemoryError::Backend(format!("injected {what} failure").into())
}

#[async_trait]
impl MemoryStore for Flaky {
    async fn remember(&self, f: Fact) -> MemResult<FactId> {
        self.inner.remember(f).await
    }
    async fn forget(&self, id: FactId) -> MemResult<()> {
        self.inner.forget(id).await
    }
    async fn link(&self, src: NodeId, rel: &str, dst: NodeId, source: Source) -> MemResult<()> {
        self.inner.link(src, rel, dst, source).await
    }
    async fn node(&self, kind: &str, name: &str) -> MemResult<NodeId> {
        self.inner.node(kind, name).await
    }
    async fn neighbors(&self, n: NodeId, rel: Option<&str>, depth: u8) -> MemResult<Vec<Node>> {
        self.inner.neighbors(n, rel, depth).await
    }
    async fn recall(&self, q: &str, k: usize, filter: Option<Filter>) -> MemResult<Vec<Hit>> {
        self.inner.recall(q, k, filter).await
    }
    async fn search(&self, q: &str, k: usize) -> MemResult<Vec<Hit>> {
        self.inner.search(q, k).await
    }
    async fn contact(&self, person: &str) -> MemResult<Option<Contact>> {
        self.inner.contact(person).await
    }
    async fn touch_contact(&self, person: &str, channel: &str, note: &str) -> MemResult<()> {
        self.inner.touch_contact(person, channel, note).await
    }
    async fn due_reviews(&self, now: Ts, k: usize) -> MemResult<Vec<Review>> {
        self.inner.due_reviews(now, k).await
    }
    async fn grade_review(&self, id: ReviewId, grade: u8) -> MemResult<()> {
        self.inner.grade_review(id, grade).await
    }
    async fn baseline_get(&self, watch: &str) -> MemResult<Option<Baseline>> {
        self.inner.baseline_get(watch).await
    }
    async fn baseline_set(&self, watch: &str, b: Baseline) -> MemResult<()> {
        self.inner.baseline_set(watch, b).await
    }
    async fn append(&self, e: LedgerEvent) -> MemResult<LedgerId> {
        if self.fail_append.load(Ordering::SeqCst) {
            return Err(injected("append"));
        }
        self.inner.append(e).await
    }
    async fn tool_health(&self, domain: &str) -> MemResult<Vec<ToolHealth>> {
        self.inner.tool_health(domain).await
    }
    async fn tool_health_set(&self, h: ToolHealth) -> MemResult<()> {
        self.inner.tool_health_set(h).await
    }
    async fn gate_rule(&self, domain: &str, class: &str) -> MemResult<Option<GateRule>> {
        if self.fail_gate.load(Ordering::SeqCst) {
            return Err(injected("gate rule"));
        }
        self.inner.gate_rule(domain, class).await
    }
    async fn run_start(&self, run: Run) -> MemResult<()> {
        self.inner.run_start(run).await
    }
    async fn run_end(&self, id: &RunId, status: RunStatus, ended: Ts) -> MemResult<()> {
        self.inner.run_end(id, status, ended).await
    }
    async fn run_get(&self, id: &RunId) -> MemResult<Option<Run>> {
        if self.fail_run_get.load(Ordering::SeqCst) {
            return Err(injected("run lookup"));
        }
        self.inner.run_get(id).await
    }
    async fn schedule_add(&self, s: Schedule) -> MemResult<i32> {
        self.inner.schedule_add(s).await
    }
    async fn schedule_list(&self) -> MemResult<Vec<Schedule>> {
        self.inner.schedule_list().await
    }
    async fn schedule_rm(&self, id: i32) -> MemResult<()> {
        self.inner.schedule_rm(id).await
    }
    async fn schedule_fired(&self, id: i32, at: Ts) -> MemResult<()> {
        self.inner.schedule_fired(id, at).await
    }
    async fn gate_rule_set(
        &self,
        domain: &str,
        class: &str,
        rule: GateRule,
        note: &str,
    ) -> MemResult<()> {
        self.inner.gate_rule_set(domain, class, rule, note).await
    }
    async fn review_add(&self, item: &str, answer: &str) -> MemResult<ReviewId> {
        self.inner.review_add(item, answer).await
    }
    async fn ledger_events(&self, run: &RunId) -> MemResult<Vec<(LedgerId, LedgerEvent)>> {
        self.inner.ledger_events(run).await
    }
}

/// An env plus a harness over a `Flaky` store with the same run row and contact.
async fn flaky_env(asker: ScriptedAsker) -> (Env, Arc<Flaky>, Harness) {
    let e = env(asker).await;
    let flaky = Arc::new(Flaky::default());
    let run = e.store.run_get(&e.run.id).await.unwrap().unwrap();
    flaky.run_start(run).await.unwrap();
    flaky.inner.add_contact("friend@example.com");
    let mut h = e.harness.clone();
    h.store = flaky.clone();
    (e, flaky, h)
}

#[tokio::test]
async fn ledger_failure_on_the_ask_path_runs_nothing() {
    let (_e, flaky, h) = flaky_env(ScriptedAsker::with([AskOutcome::AnsweredProceed])).await;
    flaky.fail_append.store(true, Ordering::SeqCst);
    let (spec, probe) = tool("echo.send", Class::Send, send_to("stranger@example.com"));
    let reg = ToolRegistry::new(vec![spec]);
    let err = dispatch(
        SERVER,
        &reg,
        &h,
        "echo.send",
        json!({"to": "stranger@example.com"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::Failed(_)), "{err}");
    assert_eq!(
        probe.calls.load(Ordering::SeqCst),
        0,
        "a yes nobody logged runs nothing"
    );
}

#[tokio::test]
async fn gate_or_run_lookup_errors_run_nothing() {
    for which in ["gate", "run"] {
        let (_e, flaky, h) = flaky_env(ScriptedAsker::new()).await;
        match which {
            "gate" => flaky.fail_gate.store(true, Ordering::SeqCst),
            _ => flaky.fail_run_get.store(true, Ordering::SeqCst),
        }
        let (spec, probe) = tool("echo.send", Class::Send, send_to("stranger@example.com"));
        let reg = ToolRegistry::new(vec![spec]);
        let err = dispatch(
            SERVER,
            &reg,
            &h,
            "echo.send",
            json!({"to": "stranger@example.com"}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::Failed(_)), "{which}: {err}");
        assert_eq!(probe.calls.load(Ordering::SeqCst), 0, "{which}");
        assert!(flaky.inner.ledger().is_empty(), "{which}");
    }
}

#[tokio::test]
async fn mid_call_ledger_failure_turns_proceed_into_skip() {
    let (_e, flaky, h) = flaky_env(ScriptedAsker::with([AskOutcome::AnsweredProceed])).await;
    flaky.fail_append.store(true, Ordering::SeqCst);
    let b = asking(mid_ask(Class::Account, "y.example", "change the email"));
    let (spec, probe) = tool("act", Class::Navigate, b);
    let reg = ToolRegistry::new(vec![spec]);
    dispatch(SERVER, &reg, &h, "act", json!({})).await.unwrap();
    assert_eq!(
        *probe.mid_outcome.lock().unwrap(),
        Some(AskOutcome::AnsweredSkip),
        "a yes the ledger cannot show is a skip"
    );
}

#[tokio::test]
async fn panicking_handler_is_an_error_and_a_failed_tool_event() {
    let e = env(ScriptedAsker::new()).await;
    let b = Behaviour {
        panic: true,
        ..Behaviour::default()
    };
    let (spec, probe) = tool("echo.panic", Class::Fill, b);
    let reg = ToolRegistry::new(vec![spec]);
    let err = dispatch(SERVER, &reg, &e.harness, "echo.panic", json!({}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("panic"), "{err}");
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    let ev = events(&e);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0].server, SERVER);
    assert_eq!(ev[0].mode, Some(LedgerMode::Allow));
    assert!(!ev[0].ok);
    assert!(ev[0].result_summary.starts_with("error: "));
    assert_mirror(&e);

    // Over the transport: an isError result for that id, and a second failed event.
    let input = format!(
        "{}\n",
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {"name": "echo.panic", "arguments": {}}})
    );
    let mut out = Vec::new();
    tokio::time::timeout(
        CAP,
        serve_io(SERVER, reg, e.harness.clone(), input.as_bytes(), &mut out),
    )
    .await
    .unwrap()
    .unwrap();
    let reply: Value = serde_json::from_str(String::from_utf8(out).unwrap().trim()).unwrap();
    assert_eq!(reply["id"], json!(5));
    assert_eq!(reply["result"]["isError"], json!(true));
    let ev = events(&e);
    assert_eq!(ev.len(), 2);
    assert!(!ev[1].ok);
}

#[tokio::test]
async fn concurrent_calls_answer_out_of_order() {
    let e = env(ScriptedAsker::new()).await;
    let hold = Arc::new(tokio::sync::Notify::new());
    let (slow, slow_probe) = tool(
        "echo.slow",
        Class::Read,
        Behaviour {
            hold: Some(hold.clone()),
            ..Behaviour::default()
        },
    );
    let (fast, _) = tool("echo.fast", Class::Read, Behaviour::default());
    let reg = ToolRegistry::new(vec![slow, fast]);
    let (client, server_end) = tokio::io::duplex(1 << 16);
    let (sr, sw) = tokio::io::split(server_end);
    let task = tokio::spawn(serve_io(
        SERVER,
        reg,
        e.harness.clone(),
        BufReader::new(sr),
        sw,
    ));
    let (cr, mut cw) = tokio::io::split(client);
    let mut lines = BufReader::new(cr).lines();
    for (id, name) in [(1, "echo.slow"), (2, "echo.fast")] {
        let mut s = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": {}}})
        .to_string();
        s.push('\n');
        cw.write_all(s.as_bytes()).await.unwrap();
    }
    let first = next_msg(&mut lines).await;
    assert_eq!(
        first["id"],
        json!(2),
        "the fast call is not queued behind the slow one"
    );
    assert_eq!(
        slow_probe.calls.load(Ordering::SeqCst),
        1,
        "both run at once"
    );
    hold.notify_one();
    let second = next_msg(&mut lines).await;
    assert_eq!(second["id"], json!(1));
    assert_eq!(second["result"]["isError"], json!(false));
    cw.shutdown().await.unwrap();
    tokio::time::timeout(CAP, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(events(&e).len(), 2);
    assert_mirror(&e);
}

#[tokio::test]
async fn a_bad_line_is_a_parse_error_and_the_server_keeps_reading() {
    let e = env(ScriptedAsker::new()).await;
    let reg = ToolRegistry::new(Vec::new());
    let mut input: Vec<u8> = vec![0xff, 0xfe, b'{', b'}', b'\n'];
    // The last line has no newline: it is answered at EOF.
    let ping = json!({"jsonrpc": "2.0", "id": "p", "method": "ping"}).to_string();
    input.extend_from_slice(ping.as_bytes());
    let mut out = Vec::new();
    tokio::time::timeout(
        CAP,
        serve_io(SERVER, reg, e.harness.clone(), &input[..], &mut out),
    )
    .await
    .unwrap()
    .expect("a bad line does not end the server");
    let text = String::from_utf8(out).unwrap();
    let replies: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replies.len(), 2, "{text}");
    assert_eq!(replies[0]["error"]["code"], json!(-32700));
    assert_eq!(replies[0]["id"], Value::Null);
    assert_eq!(
        replies[1],
        json!({"jsonrpc": "2.0", "id": "p", "result": {}})
    );
}

#[tokio::test]
async fn a_short_vault_value_does_not_rewrite_the_envelope() {
    let mut e = env(ScriptedAsker::new()).await;
    e.harness.vault = Arc::new(Vault::in_memory([(
        "pin".to_owned(),
        "2025".to_owned(),
        EntryKind::Secret,
    )]));
    let reg = ToolRegistry::new(Vec::new());
    let input = format!(
        "{}\n{}\n",
        json!({"jsonrpc": "2.0", "id": 2025, "method": "initialize", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2025, "method": "tools/call", "params": {"name": "x2025"}})
    );
    let mut out = Vec::new();
    tokio::time::timeout(
        CAP,
        serve_io(SERVER, reg, e.harness.clone(), input.as_bytes(), &mut out),
    )
    .await
    .unwrap()
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    let replies: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replies[0]["id"], json!(2025));
    assert_eq!(replies[0]["result"]["protocolVersion"], json!("2025-06-18"));
    assert_eq!(replies[1]["id"], json!(2025));
    assert_eq!(
        replies[1]["error"]["code"],
        json!(-32602),
        "D-71 unknown tool"
    );
    assert_eq!(
        replies[1]["error"]["message"],
        json!("unknown tool: x@vault(pin)")
    );
}

// -- FileAsker closing and failing closed --

#[tokio::test]
async fn file_asker_fails_closed_when_the_card_cannot_be_written() {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask test").unwrap();
    // `asks` is a file: the folder cannot be created, so nobody can see a card.
    std::fs::write(run.dir.join("asks"), "not a folder").unwrap();
    let asker = FileAsker::new(&root.0, run.id.clone(), Arc::new(Vault::in_memory([])));
    let o = tokio::time::timeout(CAP, asker.ask(&card(CAP, OnTimeout::Proceed, "send")))
        .await
        .expect("returns at once, not at the card's timer");
    assert_eq!(o, AskOutcome::TimeoutSkip, "an unseen card never proceeds");

    // Through dispatch: Send(new) would proceed on timeout, but the handler does not run.
    let mut e = env(ScriptedAsker::new()).await;
    std::fs::write(e.run.dir.join("asks"), "not a folder").unwrap();
    e.harness.asker = Arc::new(FileAsker::new(
        &e.root.0,
        e.run.id.clone(),
        e.harness.vault.clone(),
    ));
    let (spec, probe) = tool("echo.send", Class::Send, send_to("stranger@example.com"));
    let reg = ToolRegistry::new(vec![spec]);
    let out = tokio::time::timeout(
        CAP,
        dispatch(
            SERVER,
            &reg,
            &e.harness,
            "echo.send",
            json!({"to": "stranger@example.com"}),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(out.text.starts_with("skipped: "), "{}", out.text);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    let ev = events(&e);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0].mode, Some(LedgerMode::TimeoutSkip));
}

#[tokio::test]
async fn file_asker_refuses_an_answer_after_the_ask_closed() {
    // Timed out: the asker claimed the answer file, so a late yes is refused.
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask test").unwrap();
    let asker = FileAsker::new(&root.0, run.id.clone(), Arc::new(Vault::in_memory([])));
    let o = asker
        .ask(&card(
            Duration::from_millis(100),
            OnTimeout::Proceed,
            "late",
        ))
        .await;
    assert_eq!(o, AskOutcome::TimeoutProceed);
    let err = ask::answer(&root.0, &run.id, 1, true).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(err.to_string().contains("timed out"), "{err}");

    // Answered: a second answer is refused and the first stands.
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask test").unwrap();
    let asker = FileAsker::new(&root.0, run.id.clone(), Arc::new(Vault::in_memory([])));
    let twice = card(CAP, OnTimeout::Skip, "twice");
    let asking = asker.ask(&twice);
    let answering = async {
        wait_for(&ask::card_path(&run.dir, 1)).await;
        ask::answer(&root.0, &run.id, 1, true).unwrap();
        let err = ask::answer(&root.0, &run.id, 1, false).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(err.to_string().contains("already answered"), "{err}");
    };
    let (o, ()) = tokio::time::timeout(CAP, async { tokio::join!(asking, answering) })
        .await
        .unwrap();
    assert_eq!(o, AskOutcome::AnsweredProceed);
    let ans = std::fs::read_to_string(ask::answer_path(&run.dir, 1)).unwrap();
    assert_eq!(ans.trim(), "yes");
}

/// Set in the child process `file_asker_stderr_has_no_canary` starts.
const STDERR_CHILD: &str = "SB_MCP_T07_STDERR_CHILD";

/// Prints FileAsker notices (a card with the canary in every field, then the fail-closed
/// path) to the real stderr. Runs only as the child of `file_asker_stderr_has_no_canary`.
#[tokio::test]
#[ignore = "child of file_asker_stderr_has_no_canary"]
async fn file_asker_stderr_child() {
    if std::env::var_os(STDERR_CHILD).is_none() {
        return;
    }
    let c = canary();
    let vault = Arc::new(Vault::in_memory([(
        "test".to_owned(),
        c.clone(),
        EntryKind::Secret,
    )]));
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask test").unwrap();
    let asker = FileAsker::new(&root.0, run.id.clone(), vault);
    let k = AskCard {
        class: Class::Send,
        what: format!("send {c}\nto new"),
        site: Some(format!("{c}.example")),
        values: vec![("body".into(), c.clone())],
        timeout: Duration::from_millis(50),
        on_timeout: OnTimeout::Skip,
    };
    assert_eq!(asker.ask(&k).await, AskOutcome::TimeoutSkip);
    std::fs::remove_dir_all(run.dir.join("asks")).unwrap();
    std::fs::write(run.dir.join("asks"), "not a folder").unwrap();
    assert_eq!(asker.ask(&k).await, AskOutcome::TimeoutSkip);
}

#[test]
fn file_asker_stderr_has_no_canary() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "file_asker_stderr_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(STDERR_CHILD, "1")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "child failed ({:?})", out.status);
    assert!(stdout.contains("1 passed"), "the child test ran");
    let c = canary();
    assert!(!err.contains(&c), "FileAsker stderr must be scrubbed");
    assert!(!stdout.contains(&c), "child stdout must be scrubbed");
    assert!(
        err.contains("ask 1 (Send): send @vault(test) to new"),
        "the notice line is printed"
    );
    assert!(
        err.contains("nobody saw"),
        "the fail-closed line is printed"
    );
}
