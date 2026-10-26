//! T-16 package C: the wrapper touches `memory.contacts` after a Send (docs/01 H-1.1, H-6;
//! docs/02 M-2 `touch_contact`, M-3 contacts, 02-acc-3; D-141).
//!
//! "Send to a recipient never contacted: ask"; "touch_contact makes a Send to that person
//! allow on the next gate check". So: Send(new) answered yes runs and makes the recipient a
//! contact, and the same recipient (any ASCII case) is then allowed without a card. A Send
//! that was skipped, failed, stopped (by the handler or mid-call), or ran on a timeout
//! proceed touches nothing, and neither does a non-Send call with recipients.
//!
//! In-process: `MemStore`, `ScriptedAsker`, `Vault::in_memory` (no model, rule 1; no sleeps,
//! rule 6). `cargo test -p harness-core --test accept_t16_contacts`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use harness_core::class::Class;
use harness_core::gate::{AskCard, AskOutcome, OnTimeout, StopReason, ASK_TIMEOUT};
use harness_core::mcp::{
    dispatch, Classified, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolRegistry,
    ToolSpec,
};
use harness_core::memory::{ts_now, LedgerMode, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::{EntryKind, Vault};
use serde_json::{json, Value};

const SERVER: &str = "echo";
const TOOL: &str = "echo.send";
const DOMAIN: &str = "mail.example";

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "canary-t16-contacts-not-a-secret".to_owned())
}

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "sb-t16-contacts-{}-{}-{}",
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

#[derive(Clone, Copy, PartialEq)]
enum Ends {
    Ok,
    Fails,
    Stops,
    /// A mid-call Challenge card (held as the call's stop), then an ordinary ok output.
    MidCallStop,
}

struct Send {
    to: Vec<String>,
    ends: Ends,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ToolHandler for Send {
    async fn classify(&self, _ctx: &ToolCtx, _args: &Value) -> Result<Classified, ToolError> {
        Ok(Classified {
            recipients: self.to.clone(),
            domain: Some(DOMAIN.into()),
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, _args: Value) -> Result<ToolOutput, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.ends {
            Ends::Ok => Ok(ToolOutput {
                text: "sent".into(),
                ..ToolOutput::default()
            }),
            Ends::Fails => Err(ToolError::Failed("smtp said no".into())),
            Ends::Stops => Ok(ToolOutput {
                text: "partly".into(),
                stopped: Some(StopReason {
                    class: Class::Challenge,
                    why: "verify you are human".into(),
                    element: Some("#captcha".into()),
                }),
                ..ToolOutput::default()
            }),
            Ends::MidCallStop => {
                let o = ctx
                    .ask(AskCard {
                        class: Class::Challenge,
                        what: "captcha before send".into(),
                        site: Some(DOMAIN.into()),
                        values: Vec::new(),
                        timeout: ASK_TIMEOUT,
                        on_timeout: OnTimeout::Skip,
                    })
                    .await;
                assert_eq!(o, AskOutcome::TimeoutSkip);
                Ok(ToolOutput {
                    text: "sent anyway".into(),
                    ..ToolOutput::default()
                })
            }
        }
    }
}

struct Env {
    _root: Tmp,
    store: Arc<MemStore>,
    asker: Arc<ScriptedAsker>,
    harness: Harness,
}

async fn env(asker: ScriptedAsker) -> Env {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "t16c", "contacts").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "contacts".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    let asker = Arc::new(asker);
    let harness = Harness {
        store: store.clone(),
        vault: Arc::new(Vault::in_memory([(
            "test".to_owned(),
            canary(),
            EntryKind::Secret,
        )])),
        asker: asker.clone(),
        runs_root: root.0.clone(),
        run: Some(run.id.clone()),
    };
    Env {
        _root: root,
        store,
        asker,
        harness,
    }
}

fn registry(class: Class, to: &[&str], ends: Ends) -> (ToolRegistry, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let spec = ToolSpec {
        name: TOOL.into(),
        description: "send a test message".into(),
        class,
        args: json!({"type": "object"}),
        handler: Arc::new(Send {
            to: to.iter().map(|s| s.to_string()).collect(),
            ends,
            calls: calls.clone(),
        }),
    };
    (ToolRegistry::new(vec![spec]), calls)
}

async fn send(e: &Env, reg: &ToolRegistry) -> Result<ToolOutput, ToolError> {
    dispatch(SERVER, reg, &e.harness, TOOL, json!({"body": "hi"})).await
}

/// The mode of the last `echo` tool event.
fn last_tool_mode(e: &Env) -> Option<LedgerMode> {
    e.store
        .ledger()
        .into_iter()
        .rev()
        .find(|(_, ev)| ev.server == SERVER)
        .and_then(|(_, ev)| ev.mode)
}

#[tokio::test]
async fn send_new_answered_yes_then_the_same_recipient_is_allowed_without_a_card() {
    let e = env(ScriptedAsker::with([AskOutcome::AnsweredProceed])).await;
    let (reg, calls) = registry(Class::Send, &["Stranger@Example.com"], Ends::Ok);
    assert!(e
        .store
        .contact("stranger@example.com")
        .await
        .unwrap()
        .is_none());

    let out = send(&e, &reg).await.unwrap();
    assert_eq!(out.text, "sent");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(e.asker.cards().len(), 1, "Send(new) asks first");
    assert_eq!(last_tool_mode(&e), Some(LedgerMode::AnsweredProceed));

    let c = e
        .store
        .contact("stranger@example.com")
        .await
        .unwrap()
        .expect("an answered-yes Send makes the recipient a contact");
    assert_eq!(c.person, "stranger@example.com");
    assert_eq!(c.count, 1);
    assert_eq!(c.channel.as_deref(), Some(DOMAIN));
    assert!(c.last.is_some());

    // Same person, other ASCII case: allowed, no second card, and counted again.
    let (reg2, calls2) = registry(Class::Send, &["STRANGER@example.COM"], Ends::Ok);
    send(&e, &reg2).await.unwrap();
    assert_eq!(calls2.load(Ordering::SeqCst), 1);
    assert_eq!(
        e.asker.cards().len(),
        1,
        "a known contact is not asked about"
    );
    assert_eq!(last_tool_mode(&e), Some(LedgerMode::Allow));
    let c = e
        .store
        .contact("stranger@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c.count, 2);
}

#[tokio::test]
async fn every_distinct_recipient_is_touched_once() {
    let e = env(ScriptedAsker::with([AskOutcome::AnsweredProceed])).await;
    let (reg, _) = registry(
        Class::Send,
        &["a@example.com", " A@Example.com ", "   ", "b@example.com"],
        Ends::Ok,
    );
    send(&e, &reg).await.unwrap();
    for p in ["a@example.com", "b@example.com"] {
        let c = e.store.contact(p).await.unwrap().expect("touched");
        assert_eq!(c.count, 1, "{p} touched once per call");
    }
    assert!(e.store.contact("").await.unwrap().is_none());
}

#[tokio::test]
async fn answered_no_touches_nothing_and_the_next_send_asks_again() {
    let e = env(ScriptedAsker::with([
        AskOutcome::AnsweredSkip,
        AskOutcome::AnsweredSkip,
    ]))
    .await;
    let (reg, calls) = registry(Class::Send, &["stranger@example.com"], Ends::Ok);
    let out = send(&e, &reg).await.unwrap();
    assert!(out.text.starts_with("skipped"), "{}", out.text);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(e
        .store
        .contact("stranger@example.com")
        .await
        .unwrap()
        .is_none());
    send(&e, &reg).await.unwrap();
    assert_eq!(e.asker.cards().len(), 2, "still a new recipient");
}

#[tokio::test]
async fn timeout_proceed_runs_but_does_not_make_a_contact() {
    // No scripted answer: the Send(new) card times out to proceed (H-1.3).
    let e = env(ScriptedAsker::new()).await;
    let (reg, calls) = registry(Class::Send, &["stranger@example.com"], Ends::Ok);
    send(&e, &reg).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(last_tool_mode(&e), Some(LedgerMode::TimeoutProceed));
    assert!(
        e.store
            .contact("stranger@example.com")
            .await
            .unwrap()
            .is_none(),
        "nobody approved this recipient"
    );
}

#[tokio::test]
async fn failed_send_touches_nothing() {
    let e = env(ScriptedAsker::with([AskOutcome::AnsweredProceed])).await;
    let (reg, calls) = registry(Class::Send, &["stranger@example.com"], Ends::Fails);
    assert!(send(&e, &reg).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(e
        .store
        .contact("stranger@example.com")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn stopped_send_touches_nothing() {
    let e = env(ScriptedAsker::with([AskOutcome::AnsweredProceed])).await;
    let (reg, _) = registry(Class::Send, &["stranger@example.com"], Ends::Stops);
    let out = send(&e, &reg).await.unwrap();
    assert!(out.stopped.is_some());
    assert!(e
        .store
        .contact("stranger@example.com")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn mid_call_stop_touches_nothing_and_a_known_contact_is_not_bumped() {
    let e = env(ScriptedAsker::with([AskOutcome::AnsweredProceed])).await;
    let (reg, _) = registry(Class::Send, &["stranger@example.com"], Ends::MidCallStop);
    let out = send(&e, &reg).await.unwrap();
    assert!(out.stopped.is_none());
    assert!(e
        .store
        .contact("stranger@example.com")
        .await
        .unwrap()
        .is_none());

    e.store.add_contact("friend@example.com");
    let (reg, _) = registry(Class::Send, &["friend@example.com"], Ends::Stops);
    send(&e, &reg).await.unwrap();
    let c = e
        .store
        .contact("friend@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        c.count, 1,
        "a stopped Send does not count as contacting them"
    );
}

#[tokio::test]
async fn an_allowed_send_to_a_known_contact_bumps_count() {
    let e = env(ScriptedAsker::new()).await;
    e.store.add_contact("friend@example.com");
    let (reg, _) = registry(Class::Send, &["friend@example.com"], Ends::Ok);
    send(&e, &reg).await.unwrap();
    assert!(e.asker.cards().is_empty());
    let c = e
        .store
        .contact("friend@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c.count, 2);
    assert_eq!(c.channel.as_deref(), Some(DOMAIN), "channel updated");
}

#[tokio::test]
async fn non_send_call_with_recipients_touches_nothing() {
    let e = env(ScriptedAsker::new()).await;
    let (reg, calls) = registry(Class::Submit, &["stranger@example.com"], Ends::Ok);
    send(&e, &reg).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(e
        .store
        .contact("stranger@example.com")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn a_vault_value_recipient_is_stored_only_as_its_reference() {
    let e = env(ScriptedAsker::with([AskOutcome::AnsweredProceed])).await;
    let secret_to = canary();
    let (reg, _) = registry(Class::Send, &[secret_to.as_str()], Ends::Ok);
    send(&e, &reg).await.unwrap();
    assert!(
        e.store.contact(&secret_to).await.unwrap().is_none(),
        "the raw value is never a contacts key"
    );
    assert!(e.store.contact("@vault(test)").await.unwrap().is_some());
}
