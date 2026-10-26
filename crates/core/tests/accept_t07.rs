//! T-07 acceptance (tester): docs/01 "Acceptance for this doc" driven through the H-6 wrapper
//! (`harness_core::mcp::dispatch` / `serve_io`) with a fake `echo.send(to, body)` registered
//! in the test, plus the D-105 FileAsker. H-1.3, H-1.4, H-2, H-3, H-6; I-2; D-104, D-105.
//!
//! MemStore + ScriptedAsker (or FileAsker) + `Vault::in_memory` + a temp runs root. The vault
//! canary comes from `SB_VAULT_CANARY` (fallback `accept-t07-canary-not-a-secret`). No
//! assertion message ever formats the canary. The Postgres + real stdio-process half of the
//! acceptance is T-09's and is not here.
//!
//! `SB_VAULT_CANARY=<unique> cargo test -p harness-core --test accept_t07`

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harness_core::ask::{self, FileAsker};
use harness_core::class::Class;
use harness_core::gate::{AskCard, AskOutcome, OnTimeout, ASK_TIMEOUT};
use harness_core::ledger::read_jsonl;
use harness_core::mcp::{
    dispatch, serve_io, Classified, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput,
    ToolRegistry, ToolSpec,
};
use harness_core::memory::{ts_now, LedgerEvent, LedgerMode, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::{EntryKind, Vault};
use harness_core::Asker;
use notify::Watcher;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const SERVER: &str = "echo";
const FRIEND: &str = "friend@example.com";
const STRANGER: &str = "stranger@example.com";
/// Cap on every wait (a hung wrapper fails instead of hanging).
const CAP: Duration = Duration::from_secs(20);

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "accept-t07-canary-not-a-secret".to_owned())
}

fn has(hay: &str, needle: &str) -> bool {
    hay.contains(needle)
}

/// A unique runs root. Under `SB_ACCEPT_T07_KEEP` (a directory) it is created there and
/// kept, so the tester can grep every runs root after the process exits; otherwise it is
/// under the OS temp dir and removed on drop.
struct Tmp(PathBuf, bool);
impl Tmp {
    fn new() -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let keep = std::env::var_os("SB_ACCEPT_T07_KEEP").map(PathBuf::from);
        let parent = keep.clone().unwrap_or_else(std::env::temp_dir);
        let p = parent.join(format!(
            "sb-accept-t07-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            chrono::Utc::now().timestamp_micros()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p, keep.is_some())
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        if !self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

// ---- the fake echo.send(to, body) ----

#[derive(Default)]
struct Probe {
    classifies: AtomicUsize,
    calls: AtomicUsize,
    /// What went out on the "wire" (the sink): resolved bodies.
    wire: Mutex<Vec<String>>,
    class_in_call: Mutex<Option<Class>>,
}

/// `echo.send(to, body)`: classify reports `to` as the recipient (and optionally raises the
/// class); `call` resolves `body` at the sink, puts it on the wire, then (badly) echoes the
/// resolved body back in its text, summary and consequences. The wrapper must scrub that.
struct EchoSend {
    raise: Option<Class>,
    probe: Arc<Probe>,
}

#[async_trait]
impl ToolHandler for EchoSend {
    async fn classify(&self, _ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        self.probe.classifies.fetch_add(1, Ordering::SeqCst);
        let to = args
            .get("to")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Args("to is required".into()))?;
        Ok(Classified {
            class: self.raise,
            domain: Some("mail.example".into()),
            recipients: vec![to.to_string()],
            hints: Vec::new(),
            what: Some(format!("send a message to {to}")),
            values: vec![("to".into(), to.to_string())],
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        self.probe.calls.fetch_add(1, Ordering::SeqCst);
        *self.probe.class_in_call.lock().unwrap() = Some(ctx.class);
        let to = args.get("to").and_then(Value::as_str).unwrap_or("");
        let body = args.get("body").and_then(Value::as_str).unwrap_or("");
        let echoed = ctx.resolve_into(body, |r| {
            self.probe.wire.lock().unwrap().push(r.expose().to_string());
            r.expose().to_string()
        })?;
        Ok(ToolOutput {
            text: format!("sent to {to}: {echoed}"),
            summary: Some(format!("sent {echoed}")),
            consequences: json!({"to": to, "body": echoed}),
            files: Vec::new(),
            stopped: None,
        })
    }
}

fn echo_send(raise: Option<Class>) -> (ToolSpec, Arc<Probe>) {
    let probe = Arc::new(Probe::default());
    (
        ToolSpec {
            name: "echo.send".into(),
            description: "echo.send(to, body): fake send for acceptance".into(),
            class: Class::Send,
            args: json!({
                "type": "object",
                "properties": {"to": {"type": "string"}, "body": {"type": "string"}},
                "required": ["to", "body"]
            }),
            handler: Arc::new(EchoSend {
                raise,
                probe: probe.clone(),
            }),
        },
        probe,
    )
}

// ---- environment ----

struct Env {
    root: Tmp,
    run: RunFolder,
    store: Arc<MemStore>,
    harness: Harness,
}

async fn env_with(asker: Arc<dyn Asker>) -> Env {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "accept-t07", "acceptance t07").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "acceptance t07".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    store.add_contact(FRIEND);
    let vault = Arc::new(Vault::in_memory([(
        "test".to_owned(),
        canary(),
        EntryKind::Secret,
    )]));
    let harness = Harness {
        store: store.clone(),
        vault,
        asker,
        runs_root: root.0.clone(),
        run: Some(run.id.clone()),
    };
    Env {
        root,
        run,
        store,
        harness,
    }
}

async fn env(asker: Arc<ScriptedAsker>) -> Env {
    env_with(asker).await
}

fn events(e: &Env) -> Vec<LedgerEvent> {
    let mut t = e.store.ledger();
    t.sort_by_key(|(id, _)| *id);
    t.into_iter().map(|(_, ev)| ev).collect()
}

/// H-3: the jsonl mirror equals the table row for row.
fn assert_mirror(e: &Env) {
    let mut table = e.store.ledger();
    table.sort_by_key(|(id, _)| *id);
    let mut lines = read_jsonl(&e.run.ledger_jsonl()).unwrap();
    lines.sort_by_key(|(id, _)| *id);
    assert_eq!(table.len(), lines.len(), "mirror row count");
    assert!(
        table == lines,
        "ledger.jsonl must match the table row for row"
    );
}

/// Every file under `dir` (contents and names) is free of `needle`. Returns the file count.
fn assert_clean_tree(dir: &Path, needle: &str) -> usize {
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
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let hit = bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
        assert!(!hit, "a vault value leaked into {}", f.display());
        assert!(
            !has(&f.to_string_lossy(), needle),
            "a vault value leaked into a file name"
        );
    }
    files.len()
}

// ---- A1: known contact -> runs, ledger Send/allow ----

#[tokio::test]
async fn a1_send_to_contact_runs_and_ledger_is_send_allow() {
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone()).await;
    let (spec, probe) = echo_send(None);
    let reg = ToolRegistry::new(vec![spec]);
    let out = dispatch(
        SERVER,
        &reg,
        &e.harness,
        "echo.send",
        json!({"to": FRIEND, "body": "hello"}),
    )
    .await
    .unwrap();
    assert_eq!(out.text, format!("sent to {FRIEND}: hello"));
    assert!(out.stopped.is_none());
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1, "handler ran once");
    assert_eq!(*probe.class_in_call.lock().unwrap(), Some(Class::Send));
    assert_eq!(*probe.wire.lock().unwrap(), vec!["hello".to_string()]);
    assert!(asker.cards().is_empty(), "a known contact is not asked");

    let ev = events(&e);
    assert_eq!(ev.len(), 1, "{ev:?}");
    let t = &ev[0];
    assert_eq!(t.server, SERVER);
    assert_eq!(t.tool, "echo.send");
    assert_eq!(t.class, Some(Class::Send));
    assert_eq!(t.mode, Some(LedgerMode::Allow));
    assert!(t.ok);
    assert_eq!(t.run_id.as_ref(), Some(&e.run.id), "run id attached");
    assert!(t.duration_ms >= 0);
    assert_eq!(t.args, json!({"to": FRIEND, "body": "hello"}));
    assert_eq!(t.result_summary, "sent hello");

    // The mirror line carries the same class/mode words.
    let jsonl = std::fs::read_to_string(e.run.ledger_jsonl()).unwrap();
    assert_eq!(jsonl.lines().count(), 1);
    let line: Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
    assert_eq!(line["mode"], json!("allow"), "{line}");
    assert_mirror(&e);
}

// ---- A2: unknown recipient -> ask card, timeout -> proceed, ledger timeout_proceed ----

#[tokio::test]
async fn a2_send_to_stranger_asks_times_out_and_proceeds() {
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone()).await;
    let (spec, probe) = echo_send(None);
    let reg = ToolRegistry::new(vec![spec]);
    let out = dispatch(
        SERVER,
        &reg,
        &e.harness,
        "echo.send",
        json!({"to": STRANGER, "body": "hi"}),
    )
    .await
    .unwrap();
    assert_eq!(out.text, format!("sent to {STRANGER}: hi"));
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1, "proceeded");

    let cards = asker.cards();
    assert_eq!(cards.len(), 1, "one ask card");
    let card = &cards[0];
    assert_eq!(card.class, Class::Send);
    assert_eq!(card.on_timeout, OnTimeout::Proceed, "Send(new) proceeds");
    assert_eq!(card.timeout, ASK_TIMEOUT);
    assert_eq!(
        card.timeout,
        Duration::from_secs(60),
        "H-1.3 60-second timer"
    );
    assert!(has(&card.what, STRANGER), "card says what will happen");
    assert_eq!(
        card.site.as_deref(),
        Some("mail.example"),
        "card names the site"
    );

    let ev = events(&e);
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[0].server, "core");
    assert_eq!(ev[0].tool, "echo.send");
    assert_eq!(ev[0].class, Some(Class::Send));
    assert_eq!(ev[0].mode, Some(LedgerMode::TimeoutProceed));
    assert_eq!(ev[1].server, SERVER);
    assert_eq!(ev[1].class, Some(Class::Send));
    assert_eq!(ev[1].mode, Some(LedgerMode::TimeoutProceed));
    assert!(ev[1].ok);

    let jsonl = std::fs::read_to_string(e.run.ledger_jsonl()).unwrap();
    assert!(
        jsonl.lines().all(|l| has(l, "\"timeout_proceed\"")),
        "ledger.jsonl shows timeout_proceed"
    );
    assert_mirror(&e);
}

// ---- A3: classify raises Send to Pay -> timeout -> skip, handler not called ----

#[tokio::test]
async fn a3_class_forced_to_pay_times_out_and_skips() {
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone()).await;
    let (spec, probe) = echo_send(Some(Class::Pay));
    let reg = ToolRegistry::new(vec![spec]);
    let out = dispatch(
        SERVER,
        &reg,
        &e.harness,
        "echo.send",
        json!({"to": FRIEND, "body": "pay me"}),
    )
    .await
    .unwrap();
    assert!(out.text.starts_with("skipped: "), "{}", out.text);
    assert!(has(&out.text, "timed out"), "{}", out.text);
    assert_eq!(probe.classifies.load(Ordering::SeqCst), 1);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0, "handler not called");
    assert!(probe.wire.lock().unwrap().is_empty(), "nothing went out");

    let cards = asker.cards();
    assert_eq!(cards.len(), 1, "Pay asks even for a known contact");
    assert_eq!(cards[0].class, Class::Pay);
    assert_eq!(cards[0].on_timeout, OnTimeout::Skip);

    let ev = events(&e);
    assert_eq!(ev.len(), 1, "only the core outcome event: {ev:?}");
    assert_eq!(ev[0].server, "core");
    assert_eq!(ev[0].class, Some(Class::Pay));
    assert_eq!(ev[0].mode, Some(LedgerMode::TimeoutSkip));
    assert!(
        ev.iter().all(|x| x.server != SERVER),
        "no tool event for a skipped call"
    );
    assert_mirror(&e);
}

// ---- A4: Challenge -> stopped, handler not called, core stop event ----

#[tokio::test]
async fn a4_challenge_stops_without_calling_the_handler() {
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone()).await;
    let (spec, probe) = echo_send(Some(Class::Challenge));
    let reg = ToolRegistry::new(vec![spec]);
    let out = dispatch(
        SERVER,
        &reg,
        &e.harness,
        "echo.send",
        json!({"to": FRIEND, "body": "x", "element": "#7"}),
    )
    .await
    .unwrap();
    let stop = out.stopped.clone().expect("stopped is set");
    assert_eq!(stop.class, Class::Challenge);
    assert!(
        out.text.starts_with("stopped: challenge"),
        "H-1.4 stop line: {}",
        out.text
    );
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0, "handler not called");
    assert!(probe.wire.lock().unwrap().is_empty());
    assert!(asker.cards().is_empty(), "a challenge is never asked (D-5)");

    let ev = events(&e);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0].server, "core");
    assert_eq!(ev[0].tool, "echo.send");
    assert_eq!(ev[0].class, Some(Class::Challenge));
    assert_eq!(ev[0].mode, Some(LedgerMode::Stop));
    assert!(ev[0].why.is_some());
    assert_mirror(&e);
}

// ---- A5: @vault(test) resolves inside the sink and nowhere else ----

#[tokio::test]
async fn a5_vault_canary_only_in_the_sink() {
    let c = canary();
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone()).await;
    let (spec, probe) = echo_send(None);
    let reg = ToolRegistry::new(vec![spec]);

    // Known contact, body is the vault literal: the sink gets the value.
    let out = dispatch(
        SERVER,
        &reg,
        &e.harness,
        "echo.send",
        json!({"to": FRIEND, "body": "code @vault(test) end"}),
    )
    .await
    .unwrap();
    {
        let wire = probe.wire.lock().unwrap();
        assert_eq!(wire.len(), 1);
        assert!(
            wire[0] == format!("code {c} end"),
            "the sink saw the resolved value"
        );
    }
    let rendered = serde_json::to_string(&out).unwrap();
    assert!(
        !has(&rendered, &c),
        "returned output must not carry the value"
    );
    assert!(
        has(&out.text, "@vault(test)"),
        "value shown as @vault(test)"
    );

    // Unknown recipient spelled as the raw value: the ask card and args are scrubbed too.
    let out2 = dispatch(
        SERVER,
        &reg,
        &e.harness,
        "echo.send",
        json!({"to": c.clone(), "body": c.clone()}),
    )
    .await
    .unwrap();
    assert!(!has(&serde_json::to_string(&out2).unwrap(), &c));
    for card in asker.cards() {
        let s = format!("{} {:?} {:?}", card.what, card.site, card.values);
        assert!(
            !has(&s, &c),
            "ask card must show @vault(name), not the value"
        );
    }
    assert_eq!(asker.cards().len(), 1);
    assert!(has(&asker.cards()[0].what, "@vault(test)"));

    let ev = events(&e);
    assert!(ev.len() >= 3, "{}", ev.len());
    for row in &ev {
        let s = serde_json::to_string(row).unwrap();
        assert!(!has(&s, &c), "ledger row must not carry the value");
    }
    let jsonl = std::fs::read_to_string(e.run.ledger_jsonl()).unwrap();
    assert!(!has(&jsonl, &c), "ledger.jsonl must not carry the value");
    assert_mirror(&e);
    let n = assert_clean_tree(&e.root.0, &c);
    assert!(n >= 2, "runs root has files to check ({n})");
}

// ---- A6: serve_io over a duplex pipe ----

async fn next_msg<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
    seen: &mut String,
) -> Value {
    let line = tokio::time::timeout(CAP, lines.next_line())
        .await
        .expect("a message within the cap")
        .unwrap()
        .expect("a line");
    seen.push_str(&line);
    seen.push('\n');
    serde_json::from_str(&line).expect("one JSON-RPC message per line")
}

fn frame(v: Value) -> Vec<u8> {
    let mut s = serde_json::to_string(&v).unwrap();
    s.push('\n');
    s.into_bytes()
}

#[tokio::test]
async fn a6_serve_io_initialize_list_call_list_changed() {
    let c = canary();
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone()).await;
    let (spec, probe) = echo_send(None);
    let reg = ToolRegistry::new(vec![spec]);

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
    let mut stdout = String::new();

    cw.write_all(&frame(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "accept-t07", "version": "0"}}}),
    ))
    .await
    .unwrap();
    let init = next_msg(&mut lines, &mut stdout).await;
    assert_eq!(init["jsonrpc"], json!("2.0"));
    assert_eq!(init["id"], json!(1));
    assert_eq!(init["result"]["protocolVersion"], json!("2025-06-18"));
    assert_eq!(
        init["result"]["capabilities"]["tools"]["listChanged"],
        json!(true)
    );
    assert_eq!(init["result"]["serverInfo"]["name"], json!(SERVER));
    cw.write_all(&frame(
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    ))
    .await
    .unwrap();

    cw.write_all(&frame(
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ))
    .await
    .unwrap();
    let list = next_msg(&mut lines, &mut stdout).await;
    assert_eq!(list["id"], json!(2));
    let tools = list["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], json!("echo.send"));
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["to", "body"]));

    // Known contact, vault literal in the body.
    cw.write_all(&frame(
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "echo.send",
                   "arguments": {"to": FRIEND, "body": "@vault(test)"}}}),
    ))
    .await
    .unwrap();
    let call = next_msg(&mut lines, &mut stdout).await;
    assert_eq!(call["id"], json!(3));
    assert_eq!(call["result"]["isError"], json!(false));
    assert_eq!(call["result"]["content"][0]["type"], json!("text"));
    assert_eq!(
        call["result"]["content"][0]["text"],
        json!(format!("sent to {FRIEND}: @vault(test)"))
    );
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert!(probe.wire.lock().unwrap()[0] == c, "sink saw the value");

    // A site tool joins: the server announces it, and tools/list shows it.
    let (mut extra, _p) = echo_send(None);
    extra.name = "luma.register".into();
    reg.add(extra);
    let note = next_msg(&mut lines, &mut stdout).await;
    assert_eq!(
        note,
        json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
    );
    cw.write_all(&frame(
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list"}),
    ))
    .await
    .unwrap();
    let list2 = next_msg(&mut lines, &mut stdout).await;
    let names: Vec<&str> = list2["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"echo.send") && names.contains(&"luma.register"));
    reg.remove("luma.register");
    let note2 = next_msg(&mut lines, &mut stdout).await;
    assert_eq!(note2["method"], json!("notifications/tools/list_changed"));

    // stdin closes: serve_io answers what is in flight and returns.
    cw.shutdown().await.unwrap();
    tokio::time::timeout(CAP, task)
        .await
        .expect("serve_io returns at EOF")
        .unwrap()
        .unwrap();

    assert!(!has(&stdout, &c), "server stdout must not carry the value");
    let ev = events(&e);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0].mode, Some(LedgerMode::Allow));
    assert_mirror(&e);
    assert_clean_tree(&e.root.0, &c);
}

// ---- A7: FileAsker yes / no / timeout ----

/// Waits (notify watch on the parent + a check after arming, capped) until `path` exists.
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

fn card(timeout: Duration, on_timeout: OnTimeout) -> AskCard {
    AskCard {
        class: Class::Send,
        what: format!("send {} to new@example.com", canary()),
        site: Some("mail.example".into()),
        values: vec![("body".into(), canary())],
        timeout,
        on_timeout,
    }
}

async fn file_ask(answer: Option<bool>, timeout: Duration, on_timeout: OnTimeout) -> AskOutcome {
    let c = canary();
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
    let vault = Arc::new(Vault::in_memory([(
        "test".to_owned(),
        c.clone(),
        EntryKind::Secret,
    )]));
    let asker = FileAsker::new(&root.0, run.id.clone(), vault);
    let k = card(timeout, on_timeout);
    let asking = asker.ask(&k);
    let answering = async {
        let json_path = ask::card_path(&run.dir, 1);
        wait_for(&json_path).await;
        let doc = std::fs::read_to_string(&json_path).unwrap();
        assert!(!has(&doc, &c), "card file must be scrubbed");
        let v: Value = serde_json::from_str(&doc).unwrap();
        assert_eq!(v["what"], json!("send @vault(test) to new@example.com"));
        if let Some(yes) = answer {
            ask::answer(&root.0, &run.id, 1, yes).unwrap();
        }
    };
    let (o, ()) = tokio::time::timeout(CAP, async { tokio::join!(asking, answering) })
        .await
        .expect("ask ends within the cap");
    assert_clean_tree(&root.0, &c);
    o
}

#[tokio::test]
async fn a7_file_asker_yes() {
    // on_timeout Skip: only a real "yes" can proceed.
    assert_eq!(
        file_ask(Some(true), CAP, OnTimeout::Skip).await,
        AskOutcome::AnsweredProceed
    );
}

#[tokio::test]
async fn a7_file_asker_no() {
    // on_timeout Proceed: only a real "no" can skip.
    assert_eq!(
        file_ask(Some(false), CAP, OnTimeout::Proceed).await,
        AskOutcome::AnsweredSkip
    );
}

#[tokio::test]
async fn a7_file_asker_timeout() {
    let short = Duration::from_millis(300);
    assert_eq!(
        file_ask(None, short, OnTimeout::Proceed).await,
        AskOutcome::TimeoutProceed
    );
    assert_eq!(
        file_ask(None, short, OnTimeout::Skip).await,
        AskOutcome::TimeoutSkip
    );
}

/// FileAsker wired as the harness asker: an unknown recipient writes a card under the run
/// folder; answering yes runs the tool (answered_proceed), answering no skips it.
#[tokio::test]
async fn a7_file_asker_through_dispatch() {
    for yes in [true, false] {
        let c = canary();
        // Build the env with a placeholder, then swap in a FileAsker for its run.
        let mut e = env(Arc::new(ScriptedAsker::new())).await;
        let fa = FileAsker::new(&e.root.0, e.run.id.clone(), e.harness.vault.clone());
        e.harness.asker = Arc::new(fa);
        let (spec, probe) = echo_send(None);
        let reg = ToolRegistry::new(vec![spec]);
        let calling = dispatch(
            SERVER,
            &reg,
            &e.harness,
            "echo.send",
            json!({"to": STRANGER, "body": "@vault(test)"}),
        );
        let answering = async {
            let p = ask::card_path(&e.run.dir, 1);
            wait_for(&p).await;
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            assert_eq!(
                v["class"],
                serde_json::to_value(Class::Send).unwrap(),
                "{v}"
            );
            ask::answer(&e.root.0, &e.run.id, 1, yes).unwrap();
        };
        let (out, ()) = tokio::time::timeout(CAP, async { tokio::join!(calling, answering) })
            .await
            .expect("dispatch ends within the cap");
        let out = out.unwrap();
        let ev = events(&e);
        if yes {
            assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
            assert_eq!(ev.len(), 2, "{ev:?}");
            assert_eq!(ev[0].mode, Some(LedgerMode::AnsweredProceed));
            assert_eq!(ev[1].mode, Some(LedgerMode::AnsweredProceed));
        } else {
            assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
            assert!(out.text.starts_with("skipped: you said no"), "{}", out.text);
            assert_eq!(ev.len(), 1, "{ev:?}");
            assert_eq!(ev[0].mode, Some(LedgerMode::AnsweredSkip));
        }
        assert_mirror(&e);
        assert_clean_tree(&e.root.0, &c);
    }
}
