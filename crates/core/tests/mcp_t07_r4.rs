//! T-07 round 4 (review-T-07 Notes, carried since round 1): the registry never lowers a
//! tool's class, a stopped call writes one core stop, and serve_io caps an incoming
//! JSON-RPC line. docs/01 H-6, H-1.4; I-2; D-119, D-121 (4), D-127; CLAUDE.md rule 4.
//!
//! The vault is `Vault::in_memory` with one test canary (SB_VAULT_CANARY when set); no
//! fixture holds a real value. `cargo test -p harness-core --test mcp_t07_r4`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use harness_core::class::{Class, Mode};
use harness_core::gate::{AskCard, AskOutcome, OnTimeout, StopReason};
use harness_core::ledger::{read_jsonl, report};
use harness_core::mcp::{
    dispatch, serve_io, Classified, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput,
    ToolRegistry, ToolSpec,
};
use harness_core::memory::{ts_now, LedgerEvent, LedgerMode, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::{EntryKind, Vault};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const SERVER: &str = "sheetbrowse";
/// Cap on every wait (a hung server fails the test instead of hanging it).
const CAP: Duration = Duration::from_secs(60);
/// mcp.rs MAX_LINE_BYTES (pub(crate)); D-127 documents 8 MiB.
const MAX_LINE: usize = 8 * 1024 * 1024;

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "canary-mcp-r4-not-a-secret".to_owned())
}

/// A unique folder under the OS temp dir, removed on drop.
struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "sb-mcp-t07r4-{}-{}-{}",
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

struct Env {
    _root: Tmp,
    run: RunFolder,
    store: Arc<MemStore>,
    asker: Arc<ScriptedAsker>,
    harness: Harness,
}

async fn env(asker: ScriptedAsker) -> Env {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "t07r4", "mcp r4 test").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "mcp r4 test".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
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
        _root: root,
        run,
        store,
        asker,
        harness,
    }
}

/// Ledger rows in id order; the jsonl mirror must match the table row for row.
fn events(e: &Env) -> Vec<LedgerEvent> {
    let mut rows = e.store.ledger();
    rows.sort_by_key(|(id, _)| *id);
    let mut lines = read_jsonl(&e.run.ledger_jsonl()).unwrap();
    lines.sort_by_key(|(id, _)| *id);
    assert_eq!(rows, lines, "ledger.jsonl must match the table");
    rows.into_iter().map(|(_, ev)| ev).collect()
}

fn core_stops(ev: &[LedgerEvent]) -> Vec<&LedgerEvent> {
    ev.iter()
        .filter(|x| x.server == "core" && x.mode == Some(LedgerMode::Stop))
        .collect()
}

/// The report's `Not done` line.
fn not_done(ev: &[LedgerEvent]) -> String {
    let r = report("r4", ev);
    r.lines()
        .find(|l| l.starts_with("Not done"))
        .unwrap_or_default()
        .to_string()
}

#[derive(Default, Clone)]
struct Beh {
    classify: Classified,
    mid_asks: Vec<AskCard>,
    stop: Option<StopReason>,
    fail: Option<String>,
    panic: bool,
    label: &'static str,
}

struct H(Beh);

#[async_trait]
impl ToolHandler for H {
    async fn classify(&self, _ctx: &ToolCtx, _args: &Value) -> Result<Classified, ToolError> {
        Ok(self.0.classify.clone())
    }
    async fn call(&self, ctx: &ToolCtx, _args: Value) -> Result<ToolOutput, ToolError> {
        for card in &self.0.mid_asks {
            assert_eq!(ctx.ask(card.clone()).await, AskOutcome::TimeoutSkip);
        }
        if self.0.panic {
            panic!("handler blew up");
        }
        if let Some(msg) = &self.0.fail {
            return Err(ToolError::Failed(msg.clone()));
        }
        Ok(ToolOutput {
            text: format!("partial {}", self.0.label),
            stopped: self.0.stop.clone(),
            ..ToolOutput::default()
        })
    }
}

fn spec(name: &str, class: Class, b: Beh) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: format!("{name} {}", b.label),
        class,
        args: json!({"type": "object"}),
        handler: Arc::new(H(b)),
    }
}

fn challenge(what: &str) -> AskCard {
    AskCard {
        class: Class::Challenge,
        what: what.into(),
        site: Some("x.example".into()),
        values: Vec::new(),
        timeout: Duration::from_millis(1),
        on_timeout: OnTimeout::Skip,
    }
}

fn handler_stop(why: &str, element: Option<&str>) -> Option<StopReason> {
    Some(StopReason {
        class: Class::Challenge,
        why: why.into(),
        element: element.map(str::to_owned),
    })
}

// =====================================================================================
// Item 1: re-registering a name never lowers its class (rule 4, D-127)
// =====================================================================================

#[test]
fn re_adding_a_tool_keeps_the_higher_class_and_takes_the_new_handler() {
    let r = ToolRegistry::new(vec![spec("luma.register", Class::Pay, Beh::default())]);
    r.add(spec(
        "luma.register",
        Class::Read,
        Beh {
            label: "repaired",
            ..Beh::default()
        },
    ));
    let got = r.get("luma.register").unwrap();
    assert_eq!(got.class, Class::Pay, "a re-registration cannot lower");
    assert_eq!(
        got.description, "luma.register repaired",
        "the new spec is used"
    );
    assert_eq!(r.list().len(), 1);
    assert_eq!(r.list()[0].class, Class::Pay);
    // Raising still works.
    r.add(spec("luma.register", Class::Challenge, Beh::default()));
    assert_eq!(r.get("luma.register").unwrap().class, Class::Challenge);
    r.add(spec("luma.register", Class::Submit, Beh::default()));
    assert_eq!(r.get("luma.register").unwrap().class, Class::Challenge);
}

#[test]
fn the_floor_outlives_remove() {
    let r = ToolRegistry::default();
    r.add(spec("shop.buy", Class::Pay, Beh::default()));
    r.remove("shop.buy");
    assert!(r.get("shop.buy").is_none());
    r.add(spec("shop.buy", Class::Navigate, Beh::default()));
    assert_eq!(
        r.get("shop.buy").unwrap().class,
        Class::Pay,
        "remove then add is not a way to lower a class"
    );
    // Clones share the floors.
    let c = r.clone();
    c.remove("shop.buy");
    r.add(spec("shop.buy", Class::Read, Beh::default()));
    assert_eq!(c.get("shop.buy").unwrap().class, Class::Pay);
    // Other names are untouched.
    r.add(spec("fetch", Class::Read, Beh::default()));
    assert_eq!(r.get("fetch").unwrap().class, Class::Read);
}

#[test]
fn a_name_listed_twice_in_new_gets_the_higher_class() {
    let r = ToolRegistry::new(vec![
        spec("act", Class::Read, Beh::default()),
        spec("act", Class::Delete, Beh::default()),
    ]);
    assert!(r.list().iter().all(|s| s.class == Class::Delete));
    assert_eq!(r.get("act").unwrap().class, Class::Delete);
    r.add(spec("act", Class::Fill, Beh::default()));
    assert_eq!(r.get("act").unwrap().class, Class::Delete);
}

/// The gate sees the floor, not the lowered spec: a Pay tool re-registered as Read still
/// asks as Pay, and the tool event carries Pay.
#[tokio::test]
async fn dispatch_gates_a_re_registered_tool_at_its_floor() {
    let e = env(ScriptedAsker::with([AskOutcome::AnsweredProceed])).await;
    let reg = ToolRegistry::new(vec![spec("shop.buy", Class::Pay, Beh::default())]);
    reg.add(spec("shop.buy", Class::Read, Beh::default()));
    dispatch(SERVER, &reg, &e.harness, "shop.buy", json!({}))
        .await
        .unwrap();
    let cards = e.asker.cards();
    assert_eq!(cards.len(), 1, "a Read tool would not ask");
    assert_eq!(cards[0].class, Class::Pay);
    let ev = events(&e);
    let tool_ev = ev.iter().find(|x| x.server == SERVER).unwrap();
    assert_eq!(tool_ev.class, Some(Class::Pay));
    assert_eq!(tool_ev.mode, Some(LedgerMode::AnsweredProceed));
}

// =====================================================================================
// Item 2: one core stop per stopped call (D-121 (4), D-127)
// =====================================================================================

/// A mid-call Challenge, then the handler returns `stopped`: the tool stop event, then one
/// trailing core stop carrying the challenge (the call's first stop) with the handler's
/// element. The report lists one stop with the challenge's why.
#[tokio::test]
async fn mid_call_challenge_then_handler_stop_writes_one_core_stop() {
    let e = env(ScriptedAsker::new()).await;
    let b = Beh {
        mid_asks: vec![challenge("hCaptcha on the register form")],
        stop: handler_stop("hCaptcha on lu.ma", Some("#9")),
        ..Beh::default()
    };
    let reg = ToolRegistry::new(vec![spec("act.click", Class::Submit, b)]);
    let args = json!({"element": "#9"});
    let out = dispatch(SERVER, &reg, &e.harness, "act.click", args.clone())
        .await
        .unwrap();
    assert!(out.stopped.is_some());
    assert!(
        e.asker.cards().is_empty(),
        "a challenge is never shown (D-5)"
    );

    let ev = events(&e);
    assert_eq!(ev.len(), 2, "{ev:#?}");
    assert_eq!(ev[0].server, SERVER);
    assert_eq!(ev[0].mode, Some(LedgerMode::Stop));
    assert_eq!(ev[0].why.as_deref(), Some("hCaptcha on lu.ma"));
    assert_eq!(ev[1].server, "core");
    assert_eq!(ev[1].mode, Some(LedgerMode::Stop));
    assert_eq!(ev[1].tool, "act.click");
    assert_eq!(ev[1].args, args);
    assert_eq!(ev[1].class, Some(Class::Challenge));
    assert_eq!(
        ev[1].why.as_deref(),
        Some("challenge during act.click: hCaptcha on the register form")
    );
    assert_eq!(ev[1].consequences, json!({"element": "#9"}));
    assert_eq!(core_stops(&ev).len(), 1);

    let nd = not_done(&ev);
    assert_eq!(nd.matches("stopped at").count(), 1, "{nd}");
    assert!(nd.contains("challenge during act.click"), "{nd}");
}

/// A mid-call gate Stop (a stop rule on the card's site), then the handler returns
/// `stopped`: one core stop, the gate's.
#[tokio::test]
async fn mid_call_gate_stop_then_handler_stop_writes_one_core_stop() {
    let e = env(ScriptedAsker::new()).await;
    e.store
        .add_rule("x.example", "Account", Mode::Stop, false)
        .unwrap();
    let b = Beh {
        mid_asks: vec![AskCard {
            class: Class::Account,
            ..challenge("change the email")
        }],
        stop: handler_stop("account change refused", None),
        ..Beh::default()
    };
    let reg = ToolRegistry::new(vec![spec("act", Class::Navigate, b)]);
    dispatch(SERVER, &reg, &e.harness, "act", json!({"element": "#4"}))
        .await
        .unwrap();
    let ev = events(&e);
    assert_eq!(ev.len(), 2, "{ev:#?}");
    assert_eq!(ev[0].server, SERVER);
    let stops = core_stops(&ev);
    assert_eq!(stops.len(), 1, "{ev:#?}");
    assert_eq!(stops[0].class, Some(Class::Account));
    assert!(
        stops[0].why.as_deref().unwrap_or("").contains("x.example"),
        "{:?}",
        stops[0].why
    );
    assert_eq!(stops[0].consequences, json!({"element": "#4"}));
    assert_eq!(not_done(&ev).matches("stopped at").count(), 1);
}

/// Several mid-call stops in one call still make one core stop (the first).
#[tokio::test]
async fn two_mid_call_stops_in_one_call_log_the_first_once() {
    let e = env(ScriptedAsker::new()).await;
    let b = Beh {
        mid_asks: vec![challenge("first wall"), challenge("second wall")],
        stop: handler_stop("walls", None),
        ..Beh::default()
    };
    let reg = ToolRegistry::new(vec![spec("act", Class::Navigate, b)]);
    dispatch(SERVER, &reg, &e.harness, "act", json!({}))
        .await
        .unwrap();
    let ev = events(&e);
    let stops = core_stops(&ev);
    assert_eq!(stops.len(), 1, "{ev:#?}");
    assert_eq!(
        stops[0].why.as_deref(),
        Some("challenge during act: first wall")
    );
    assert_eq!(not_done(&ev).matches("stopped at").count(), 1);
}

/// A mid-call stop on a call that did not return `stopped` (went on, failed, panicked) is
/// still logged, once, right before the tool event.
#[tokio::test]
async fn a_mid_call_stop_without_a_handler_stop_is_logged_before_the_tool_event() {
    for (label, b) in [
        ("ok", Beh::default()),
        (
            "fail",
            Beh {
                fail: Some("gave up".into()),
                ..Beh::default()
            },
        ),
        (
            "panic",
            Beh {
                panic: true,
                ..Beh::default()
            },
        ),
    ] {
        let e = env(ScriptedAsker::new()).await;
        let b = Beh {
            mid_asks: vec![challenge("turnstile")],
            label,
            ..b
        };
        let reg = ToolRegistry::new(vec![spec("act", Class::Navigate, b)]);
        let _ = dispatch(SERVER, &reg, &e.harness, "act", json!({})).await;
        let ev = events(&e);
        assert_eq!(ev.len(), 2, "{label}: {ev:#?}");
        assert_eq!(ev[0].server, "core", "{label}");
        assert_eq!(ev[0].mode, Some(LedgerMode::Stop), "{label}");
        assert_eq!(
            ev[0].why.as_deref(),
            Some("challenge during act: turnstile"),
            "{label}"
        );
        assert_eq!(ev[1].server, SERVER, "{label}");
        assert_eq!(ev[1].ok, label == "ok", "{label}");
        assert_eq!(not_done(&ev).matches("stopped at").count(), 1, "{label}");
    }
}

/// H-1.4 resume: the same tool and args stopped again lists a second stop, whether the
/// second call is stopped mid-call or by the gate before it runs (D-121 (4)).
#[tokio::test]
async fn a_resumed_call_stopped_again_is_listed_again() {
    let e = env(ScriptedAsker::new()).await;
    let args = json!({"element": "#9"});
    let mid = Beh {
        mid_asks: vec![challenge("hCaptcha")],
        stop: handler_stop("hCaptcha on lu.ma", Some("#9")),
        ..Beh::default()
    };
    let reg = ToolRegistry::new(vec![spec("act.click", Class::Submit, mid)]);
    for _ in 0..2 {
        dispatch(SERVER, &reg, &e.harness, "act.click", args.clone())
            .await
            .unwrap();
    }
    let ev = events(&e);
    assert_eq!(ev.len(), 4, "{ev:#?}");
    assert_eq!(core_stops(&ev).len(), 2);
    assert_eq!(not_done(&ev).matches("stopped at").count(), 2);

    // Third call: the classifier sees the challenge first, so the gate stops it.
    reg.add(spec(
        "act.click",
        Class::Submit,
        Beh {
            classify: Classified {
                class: Some(Class::Challenge),
                ..Classified::default()
            },
            ..Beh::default()
        },
    ));
    dispatch(SERVER, &reg, &e.harness, "act.click", args.clone())
        .await
        .unwrap();
    let ev = events(&e);
    assert_eq!(ev.len(), 5, "{ev:#?}");
    assert_eq!(core_stops(&ev).len(), 3);
    let nd = not_done(&ev);
    assert_eq!(nd.matches("stopped at").count(), 3, "{nd}");
}

/// The held stop is scrubbed like every stop line (rule 2).
#[tokio::test]
async fn the_trailing_stop_carries_no_vault_value() {
    let c = canary();
    let e = env(ScriptedAsker::new()).await;
    let b = Beh {
        mid_asks: vec![challenge(&format!("wall near {c}\r\nnext"))],
        stop: handler_stop(&format!("stopped near {c}"), Some(&format!("#1 {c}"))),
        ..Beh::default()
    };
    let reg = ToolRegistry::new(vec![spec("act", Class::Navigate, b)]);
    dispatch(SERVER, &reg, &e.harness, "act", json!({}))
        .await
        .unwrap();
    let ev = events(&e);
    let stops = core_stops(&ev);
    assert_eq!(stops.len(), 1);
    let rendered = serde_json::to_string(&ev).unwrap();
    assert!(!rendered.contains(&c), "canary in the ledger");
    let why = stops[0].why.clone().unwrap_or_default();
    assert!(
        why.contains("@vault(test)") && !why.contains('\n'),
        "{why:?}"
    );
    assert_eq!(
        stops[0].consequences,
        json!({"element": "#1 @vault(test)"}),
        "the element comes from the handler's stop, scrubbed"
    );
}

// =====================================================================================
// Item 3: serve_io caps an incoming line (D-127)
// =====================================================================================

/// Drives serve_io over a duplex pipe: writes `input` from a task, returns the reply lines.
async fn serve_bytes(input: Vec<u8>) -> Vec<Value> {
    let e = env(ScriptedAsker::new()).await;
    let reg = ToolRegistry::new(vec![spec("act", Class::Read, Beh::default())]);
    let (client, server_end) = tokio::io::duplex(1 << 16);
    let (sr, sw) = tokio::io::split(server_end);
    let server = tokio::spawn(serve_io(
        SERVER,
        reg,
        e.harness.clone(),
        BufReader::new(sr),
        sw,
    ));
    let (cr, mut cw) = tokio::io::split(client);
    let writer = tokio::spawn(async move {
        cw.write_all(&input).await.unwrap();
        cw.shutdown().await.unwrap();
    });
    let mut replies = Vec::new();
    let mut lines = BufReader::new(cr).lines();
    let read = async {
        while let Some(l) = lines.next_line().await.unwrap() {
            replies.push(serde_json::from_str::<Value>(&l).unwrap());
        }
    };
    tokio::time::timeout(CAP, read).await.expect("replies end");
    tokio::time::timeout(CAP, writer).await.unwrap().unwrap();
    tokio::time::timeout(CAP, server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    replies
}

fn ping(id: u64) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": "ping"}).to_string()
}

#[tokio::test]
async fn an_over_long_line_answers_32600_and_the_next_line_is_served() {
    let mut input = vec![b'a'; MAX_LINE + 1];
    input.push(b'\n');
    input.extend_from_slice(format!("{}\n", ping(2)).as_bytes());
    let replies = serve_bytes(input).await;
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0]["error"]["code"], json!(-32600));
    assert_eq!(replies[0]["id"], Value::Null);
    assert!(replies[0]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("8 MiB"));
    assert_eq!(replies[1]["id"], json!(2));
    assert_eq!(replies[1]["result"], json!({}));
}

/// A request that is a valid ping padded to exactly the cap is served (the newline does
/// not count); one byte more is refused.
#[tokio::test]
async fn a_line_at_the_cap_is_served_and_one_byte_more_is_not() {
    let mut at_cap = ping(7).into_bytes();
    at_cap.resize(MAX_LINE, b' ');
    let mut input = at_cap.clone();
    input.push(b'\n');
    let mut over = ping(8).into_bytes();
    over.resize(MAX_LINE + 1, b' ');
    input.extend_from_slice(&over);
    input.push(b'\n');
    input.extend_from_slice(format!("{}\n", ping(9)).as_bytes());
    let replies = serve_bytes(input).await;
    assert_eq!(replies.len(), 3, "{replies:?}");
    assert_eq!(replies[0]["id"], json!(7));
    assert_eq!(replies[0]["result"], json!({}));
    assert_eq!(replies[1]["id"], Value::Null);
    assert_eq!(replies[1]["error"]["code"], json!(-32600));
    assert_eq!(replies[2]["id"], json!(9));
}

/// An over-long line cut off by EOF (no newline) still answers -32600, then serve_io ends.
#[tokio::test]
async fn an_over_long_last_line_without_newline_is_answered() {
    let mut input = format!("{}\n", ping(1)).into_bytes();
    input.extend(std::iter::repeat_n(b'{', MAX_LINE + 10));
    let replies = serve_bytes(input).await;
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0]["id"], json!(1));
    assert_eq!(replies[1]["error"]["code"], json!(-32600));
}
