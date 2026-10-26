//! T-07 round 5 (review-T-07 rounds 3 and 4, Should fix 3): a gate `Stop` hit by a mid-call
//! ask (`ToolCtx::ask`, mcp.rs) is written as one clean, scrubbed line, both as the lone core
//! stop of a call that went on and as the trailing stop of a call that stopped (D-119 (7),
//! D-123, D-127). docs/01 H-6, H-1.4.
//!
//! The gate's own stop text is already short for normal domains, so the case uses a long
//! (valid) domain whose rule name makes the raw why longer than one card line, plus an
//! element argument with control characters and a vault value. The vault is
//! `Vault::in_memory` holding `SB_VAULT_CANARY` (or a generated stand-in); no fixture holds
//! a real value. `cargo test -p harness-core --test mcp_t07_r5`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harness_core::class::{Class, Mode};
use harness_core::gate::{AskCard, AskOutcome, OnTimeout, StopReason};
use harness_core::mcp::{
    dispatch, Classified, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolRegistry,
    ToolSpec,
};
use harness_core::memory::{ts_now, LedgerEvent, LedgerMode, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::{EntryKind, Vault};
use serde_json::{json, Value};

/// The line cap `clean_line` applies (gate card `what`, 200 chars).
const LINE_MAX_CHARS: usize = 200;

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|v| v.len() >= 8)
        .unwrap_or_else(|| format!("t07r5-{}", chrono::Utc::now().timestamp_micros()))
}

/// `s` with the vault value replaced, for assertion messages.
fn redact(s: &str, v: &str) -> String {
    s.replace(v, "<canary>")
}

fn one_clean_line(s: &str) -> bool {
    !s.chars().any(|c| c.is_control())
}

/// A valid host name of 196 chars: the gate's rule name `(<domain>, Account)` makes its
/// stop why longer than one cleaned line.
fn long_domain() -> String {
    format!(
        "{}.{}.{}.{}.example",
        "a".repeat(60),
        "b".repeat(60),
        "c".repeat(15),
        "d".repeat(50)
    )
}

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "sb-mcp-t07r5-{}-{}-{}",
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

/// Classifies to `domain`, asks an Account card with no site mid-call, then returns `done`
/// or, with `stop_after`, a stopped output.
struct MidAsker {
    domain: String,
    stop_after: bool,
    mid: Arc<Mutex<Option<AskOutcome>>>,
}

#[async_trait]
impl ToolHandler for MidAsker {
    async fn classify(&self, _ctx: &ToolCtx, _args: &Value) -> Result<Classified, ToolError> {
        Ok(Classified {
            domain: Some(self.domain.clone()),
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, _args: Value) -> Result<ToolOutput, ToolError> {
        let o = ctx
            .ask(AskCard {
                class: Class::Account,
                what: "change the email".into(),
                site: None,
                values: Vec::new(),
                timeout: Duration::from_millis(1),
                on_timeout: OnTimeout::Proceed,
            })
            .await;
        *self.mid.lock().unwrap() = Some(o);
        Ok(ToolOutput {
            text: "done".into(),
            stopped: self.stop_after.then(|| StopReason {
                class: Class::Account,
                why: "stopped after the ask".into(),
                element: None,
            }),
            ..ToolOutput::default()
        })
    }
}

/// Runs one `act` call with a dirty `element` arg against an `(domain, Account) stop` rule.
async fn run_case(stop_after: bool, v: &str) -> (Vec<LedgerEvent>, Option<AskOutcome>, usize) {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "t07r5", "mid-call stop line").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "mid-call stop line".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    let domain = long_domain();
    store
        .add_rule(&domain, "Account", Mode::Stop, false)
        .unwrap();
    let asker = Arc::new(ScriptedAsker::new());
    let harness = Harness {
        store: store.clone(),
        vault: Arc::new(Vault::in_memory([(
            "test".to_owned(),
            v.to_owned(),
            EntryKind::Secret,
        )])),
        asker: asker.clone(),
        runs_root: root.0.clone(),
        run: Some(run.id.clone()),
    };
    let mid = Arc::new(Mutex::new(None));
    let spec = ToolSpec {
        name: "act".into(),
        description: "act test tool".into(),
        class: Class::Navigate,
        args: json!({"type": "object"}),
        handler: Arc::new(MidAsker {
            domain,
            stop_after,
            mid: mid.clone(),
        }),
    };
    let reg = ToolRegistry::new(vec![spec]);
    let element = format!("#31\x1b[31m\r\nnext {v}\u{7} \u{9b}2J end");
    dispatch("echo", &reg, &harness, "act", json!({ "element": element }))
        .await
        .unwrap();
    let ev = store.ledger().into_iter().map(|(_, e)| e).collect();
    let o = *mid.lock().unwrap();
    (ev, o, asker.cards().len())
}

fn assert_clean_stop(e: &LedgerEvent, v: &str, what: &str) {
    assert_eq!(e.server, "core", "{what}");
    assert_eq!(e.mode, Some(LedgerMode::Stop), "{what}");
    assert_eq!(e.class, Some(Class::Account), "{what}");
    let why = e.why.clone().unwrap_or_default();
    let shown = redact(&why, v);
    assert!(why.starts_with("gate rule ("), "{what}: {shown:?}");
    assert!(
        one_clean_line(&why),
        "{what}: why is one clean line: {shown:?}"
    );
    assert!(
        why.chars().count() <= LINE_MAX_CHARS,
        "{what}: why is cut to one card line ({} chars): {shown:?}",
        why.chars().count()
    );
    assert_eq!(e.result_summary, why, "{what}");
    let el = e.consequences["element"].as_str().unwrap_or_default();
    let el_shown = redact(el, v);
    assert!(!el.is_empty(), "{what}: the element is kept");
    assert!(one_clean_line(el), "{what}: element {el_shown:?}");
    assert!(el.starts_with("#31"), "{what}: element {el_shown:?}");
    let all = serde_json::to_string(e).unwrap();
    assert!(!all.contains(v), "{what}: the vault value is in the event");
}

#[tokio::test]
async fn a_mid_call_gate_stop_is_one_clean_scrubbed_line() {
    let v = canary();
    let raw = format!("gate rule ({}, Account) says stop", long_domain());
    assert!(
        raw.chars().count() > LINE_MAX_CHARS,
        "the case needs a raw why longer than one line"
    );

    // The call went on: the held stop is written as a lone core stop before the tool event.
    let (ev, o, shown) = run_case(false, &v).await;
    assert_eq!(o, Some(AskOutcome::TimeoutSkip));
    assert_eq!(shown, 0, "a stop rule shows no card");
    assert_eq!(
        ev.len(),
        2,
        "{:?}",
        ev.iter().map(|e| &e.tool).collect::<Vec<_>>()
    );
    assert_clean_stop(&ev[0], &v, "lone core stop");
    assert_eq!(ev[1].server, "echo");
    assert_ne!(ev[1].mode, Some(LedgerMode::Stop));

    // The call stopped: tool stop event, then the held stop as the trailing core stop.
    let (ev, o, shown) = run_case(true, &v).await;
    assert_eq!(o, Some(AskOutcome::TimeoutSkip));
    assert_eq!(shown, 0);
    assert_eq!(
        ev.len(),
        2,
        "{:?}",
        ev.iter().map(|e| &e.tool).collect::<Vec<_>>()
    );
    assert_eq!(ev[0].server, "echo");
    assert_eq!(ev[0].mode, Some(LedgerMode::Stop));
    assert_clean_stop(&ev[1], &v, "trailing core stop");
}
