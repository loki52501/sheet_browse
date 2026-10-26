//! T-07 round 2 acceptance (tester): the D-119 clauses the round-1 suites (accept_t07,
//! mcp_t07) leave open, plus the FileAsker card-publication race behind the
//! a7_file_asker_through_dispatch flake. docs/01 H-1.3, H-1.4, H-2, H-6; I-2; D-105, D-119.
//!
//! MemStore (or a rigged wrapper of it) + `Vault::in_memory` + a temp runs root. The canary
//! comes from `SB_VAULT_CANARY` (fallback `accept-t07-r2-canary-not-a-secret`); no assertion
//! message formats it. Under `SB_ACCEPT_T07_KEEP` (a directory) every runs root is kept
//! there so the tester can grep it after the process exits.
//!
//! `SB_VAULT_CANARY=<unique> cargo test -p harness-core --test accept_t07_r2 -- --nocapture`

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use harness_core::ask::{self, FileAsker};
use harness_core::class::{Class, Mode};
use harness_core::gate::{AskCard, AskOutcome, OnTimeout, StopReason, ASK_TIMEOUT};
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
use serde_json::{json, Value};

const SERVER: &str = "echo";
const STRANGER: &str = "stranger@example.com";
/// A personal value with a newline: whitespace collapse or truncation before the scrub
/// would reshape it past the scrubber (D-119 (7)).
const ADDR: &str = "1 Main St\nSpringfield";
/// Cap on every wait that is not the D-119 (3) cap itself.
const CAP: Duration = Duration::from_secs(20);

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "accept-t07-r2-canary-not-a-secret".to_owned())
}

fn vault() -> Arc<Vault> {
    Arc::new(Vault::in_memory([
        ("test".to_owned(), canary(), EntryKind::Secret),
        ("addr".to_owned(), ADDR.to_owned(), EntryKind::Personal),
    ]))
}

/// No control character, no escape, no line break.
fn one_clean_line(s: &str) -> bool {
    !s.chars().any(|c| c.is_control())
}

struct Tmp(PathBuf, bool);
impl Tmp {
    fn new() -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let keep = std::env::var_os("SB_ACCEPT_T07_KEEP").map(PathBuf::from);
        let parent = keep.clone().unwrap_or_else(std::env::temp_dir);
        let p = parent.join(format!(
            "sb-accept-t07r2-{}-{}-{}",
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

// ---- a rigged store: switchable gate and run-lookup failures ----

#[derive(Default)]
struct Rigged {
    inner: MemStore,
    /// `gate_rule` fails for this (canonical) domain.
    fail_gate_domain: Mutex<Option<String>>,
    /// The error text of that failure.
    gate_err: Mutex<String>,
    /// `run_get` succeeds this many times, then fails (None = never fails).
    run_get_ok: Mutex<Option<usize>>,
    run_gets: AtomicUsize,
}

#[async_trait]
impl MemoryStore for Rigged {
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
        self.inner.append(e).await
    }
    async fn tool_health(&self, domain: &str) -> MemResult<Vec<ToolHealth>> {
        self.inner.tool_health(domain).await
    }
    async fn tool_health_set(&self, h: ToolHealth) -> MemResult<()> {
        self.inner.tool_health_set(h).await
    }
    async fn gate_rule(&self, domain: &str, class: &str) -> MemResult<Option<GateRule>> {
        let fail = self.fail_gate_domain.lock().unwrap().clone();
        if fail.as_deref() == Some(domain) {
            let msg = self.gate_err.lock().unwrap().clone();
            return Err(MemoryError::Backend(msg.into()));
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
        let n = self.run_gets.fetch_add(1, Ordering::SeqCst);
        if let Some(ok) = *self.run_get_ok.lock().unwrap() {
            if n >= ok {
                return Err(MemoryError::Backend("injected run lookup failure".into()));
            }
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

// ---- askers ----

/// Never answers and ignores its timer.
struct Hang;
#[async_trait]
impl Asker for Hang {
    async fn ask(&self, _card: &AskCard) -> AskOutcome {
        std::future::pending().await
    }
}

/// Claims every card timed out and proceeded, whatever the card says; records cards.
#[derive(Default)]
struct Loose(Mutex<Vec<AskCard>>);
#[async_trait]
impl Asker for Loose {
    async fn ask(&self, card: &AskCard) -> AskOutcome {
        self.0.lock().unwrap().push(card.clone());
        AskOutcome::TimeoutProceed
    }
}

// ---- a configurable tool ----

#[derive(Default, Clone)]
struct Beh {
    classify: Classified,
    classify_panics: bool,
    mid_ask: Option<AskCard>,
    stop: Option<StopReason>,
}

#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    mid: Mutex<Option<AskOutcome>>,
}

struct T {
    b: Beh,
    p: Arc<Probe>,
}

#[async_trait]
impl ToolHandler for T {
    async fn classify(&self, _ctx: &ToolCtx, _args: &Value) -> Result<Classified, ToolError> {
        if self.b.classify_panics {
            panic!("classifier fell over");
        }
        Ok(self.b.classify.clone())
    }
    async fn call(&self, ctx: &ToolCtx, _args: Value) -> Result<ToolOutput, ToolError> {
        self.p.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(card) = &self.b.mid_ask {
            let o = ctx.ask(card.clone()).await;
            *self.p.mid.lock().unwrap() = Some(o);
        }
        Ok(ToolOutput {
            text: "partial result".into(),
            summary: None,
            consequences: Value::Null,
            files: Vec::new(),
            stopped: self.b.stop.clone(),
        })
    }
}

fn tool(name: &str, class: Class, b: Beh) -> (ToolSpec, Arc<Probe>) {
    let p = Arc::new(Probe::default());
    (
        ToolSpec {
            name: name.into(),
            description: format!("{name} test tool"),
            class,
            args: json!({"type": "object"}),
            handler: Arc::new(T { b, p: p.clone() }),
        },
        p,
    )
}

fn mid(class: Class, site: &str, what: &str, on_timeout: OnTimeout) -> Beh {
    Beh {
        mid_ask: Some(AskCard {
            class,
            what: what.into(),
            site: Some(site.into()),
            values: Vec::new(),
            timeout: Duration::from_millis(1),
            on_timeout,
        }),
        ..Beh::default()
    }
}

fn send_to(to: &str) -> Beh {
    Beh {
        classify: Classified {
            recipients: vec![to.into()],
            domain: Some("mail.example".into()),
            ..Classified::default()
        },
        ..Beh::default()
    }
}

// ---- environment ----

struct Env {
    _root: Tmp,
    run: RunFolder,
    store: Arc<Rigged>,
    harness: Harness,
}

/// `strict`: the runs row's flag; `None` = no runs row at all.
async fn env(asker: Arc<dyn Asker>, strict: Option<bool>) -> Env {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "t07r2", "acceptance t07 r2").unwrap();
    let store = Arc::new(Rigged::default());
    if let Some(strict) = strict {
        store
            .run_start(Run {
                id: run.id.clone(),
                started: ts_now(),
                ended: None,
                prompt: "acceptance t07 r2".into(),
                status: RunStatus::Running,
                folder: format!("runs/{}", run.id),
                parent_run: None,
                strict,
            })
            .await
            .unwrap();
    }
    store.inner.add_contact("friend@example.com");
    let harness = Harness {
        store: store.clone(),
        vault: vault(),
        asker,
        runs_root: root.0.clone(),
        run: Some(run.id.clone()),
    };
    Env {
        _root: root,
        run,
        store,
        harness,
    }
}

fn events(e: &Env) -> Vec<LedgerEvent> {
    let mut t = e.store.inner.ledger();
    t.sort_by_key(|(id, _)| *id);
    t.into_iter().map(|(_, ev)| ev).collect()
}

fn assert_mirror(e: &Env) {
    let mut table = e.store.inner.ledger();
    table.sort_by_key(|(id, _)| *id);
    let mut lines = read_jsonl(&e.run.ledger_jsonl()).unwrap();
    lines.sort_by_key(|(id, _)| *id);
    assert!(table == lines, "ledger.jsonl must match the table");
}

async fn call(e: &Env, reg: &ToolRegistry, name: &str, args: Value) -> ToolOutput {
    tokio::time::timeout(CAP, dispatch(SERVER, reg, &e.harness, name, args))
        .await
        .expect("dispatch within the cap")
        .unwrap()
}

// =====================================================================================
// D-119 (1): mid-call cards through Gate::check
// =====================================================================================

/// (1) Allow: the handler's card is still shown, tightened by Gate::on_timeout.
#[tokio::test]
async fn d119_1_allow_rule_still_shows_the_card_tightened_by_on_timeout() {
    // Pay allowed on x.example: the card is shown and Gate::on_timeout(Pay) makes it skip.
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone(), Some(false)).await;
    e.store
        .inner
        .add_rule("x.example", "Pay", Mode::Allow, false)
        .unwrap();
    let (spec, p) = tool(
        "act",
        Class::Navigate,
        mid(Class::Pay, "x.example", "confirm order", OnTimeout::Proceed),
    );
    let reg = ToolRegistry::new(vec![spec]);
    call(&e, &reg, "act", json!({})).await;
    let cards = asker.cards();
    assert_eq!(
        cards.len(),
        1,
        "an allow rule still shows the handler's card"
    );
    assert_eq!(cards[0].on_timeout, OnTimeout::Skip, "Pay skips on timeout");
    assert_eq!(cards[0].timeout, ASK_TIMEOUT);
    assert_eq!(*p.mid.lock().unwrap(), Some(AskOutcome::TimeoutSkip));
    let ev = events(&e);
    assert_eq!(ev[0].server, "core");
    assert_eq!(ev[0].mode, Some(LedgerMode::TimeoutSkip));
    assert_mirror(&e);

    // Account allowed on x.example: shown, and Account may proceed.
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone(), Some(false)).await;
    e.store
        .inner
        .add_rule("x.example", "Account", Mode::Allow, false)
        .unwrap();
    let (spec, p) = tool(
        "act",
        Class::Navigate,
        mid(
            Class::Account,
            "x.example",
            "change email",
            OnTimeout::Proceed,
        ),
    );
    let reg = ToolRegistry::new(vec![spec]);
    call(&e, &reg, "act", json!({})).await;
    assert_eq!(asker.cards().len(), 1);
    assert_eq!(asker.cards()[0].on_timeout, OnTimeout::Proceed);
    assert_eq!(*p.mid.lock().unwrap(), Some(AskOutcome::TimeoutProceed));

    // The same allow rule on a strict run: Gate::on_timeout(Account, strict) skips.
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone(), Some(true)).await;
    e.store
        .inner
        .add_rule("x.example", "Account", Mode::Allow, false)
        .unwrap();
    let (spec, p) = tool(
        "act",
        Class::Navigate,
        mid(
            Class::Account,
            "x.example",
            "change email",
            OnTimeout::Proceed,
        ),
    );
    let reg = ToolRegistry::new(vec![spec]);
    call(&e, &reg, "act", json!({})).await;
    assert_eq!(asker.cards().len(), 1);
    assert_eq!(asker.cards()[0].on_timeout, OnTimeout::Skip);
    assert_eq!(*p.mid.lock().unwrap(), Some(AskOutcome::TimeoutSkip));

    // The handler's own Skip is never loosened by an allow rule.
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone(), Some(false)).await;
    e.store
        .inner
        .add_rule("x.example", "Account", Mode::Allow, false)
        .unwrap();
    let (spec, p) = tool(
        "act",
        Class::Navigate,
        mid(Class::Account, "x.example", "change email", OnTimeout::Skip),
    );
    let reg = ToolRegistry::new(vec![spec]);
    call(&e, &reg, "act", json!({})).await;
    assert_eq!(asker.cards()[0].on_timeout, OnTimeout::Skip);
    assert_eq!(*p.mid.lock().unwrap(), Some(AskOutcome::TimeoutSkip));
}

/// (1) A gate error: the card is shown with on_timeout Skip.
#[tokio::test]
async fn d119_1_gate_error_shows_the_card_with_skip() {
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone(), Some(false)).await;
    *e.store.fail_gate_domain.lock().unwrap() = Some("err.example".into());
    *e.store.gate_err.lock().unwrap() = "injected gate failure".into();
    let (spec, p) = tool(
        "act",
        Class::Navigate,
        mid(
            Class::Account,
            "err.example",
            "change email",
            OnTimeout::Proceed,
        ),
    );
    let reg = ToolRegistry::new(vec![spec]);
    call(&e, &reg, "act", json!({})).await;
    assert_eq!(
        p.calls.load(Ordering::SeqCst),
        1,
        "the call itself was allowed"
    );
    let cards = asker.cards();
    assert_eq!(cards.len(), 1, "a gate error still shows the card");
    assert_eq!(cards[0].on_timeout, OnTimeout::Skip);
    assert_eq!(*p.mid.lock().unwrap(), Some(AskOutcome::TimeoutSkip));
    let ev = events(&e);
    assert_eq!(ev[0].server, "core");
    assert_eq!(ev[0].class, Some(Class::Account));
    assert_eq!(ev[0].mode, Some(LedgerMode::TimeoutSkip));
    assert_mirror(&e);
}

// =====================================================================================
// D-119 (2): no runs row or a failed lookup reads as strict in ToolCtx::ask
// =====================================================================================

#[tokio::test]
async fn d119_2_tool_ctx_ask_without_a_runs_row_reads_as_strict() {
    // Control: a non-strict runs row lets the Account card proceed.
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone(), Some(false)).await;
    let (spec, p) = tool(
        "act",
        Class::Navigate,
        mid(
            Class::Account,
            "y.example",
            "change email",
            OnTimeout::Proceed,
        ),
    );
    let reg = ToolRegistry::new(vec![spec]);
    call(&e, &reg, "act", json!({})).await;
    assert_eq!(*p.mid.lock().unwrap(), Some(AskOutcome::TimeoutProceed));

    // No runs row: strict, so it skips.
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone(), None).await;
    let (spec, p) = tool(
        "act",
        Class::Navigate,
        mid(
            Class::Account,
            "y.example",
            "change email",
            OnTimeout::Proceed,
        ),
    );
    let reg = ToolRegistry::new(vec![spec]);
    call(&e, &reg, "act", json!({})).await;
    assert_eq!(p.calls.load(Ordering::SeqCst), 1);
    assert_eq!(asker.cards()[0].on_timeout, OnTimeout::Skip);
    assert_eq!(*p.mid.lock().unwrap(), Some(AskOutcome::TimeoutSkip));
    assert_eq!(events(&e)[0].mode, Some(LedgerMode::TimeoutSkip));
}

#[tokio::test]
async fn d119_2_tool_ctx_ask_failed_lookup_reads_as_strict() {
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone(), Some(false)).await;
    // dispatch's own lookup succeeds (a non-strict row); every later one fails.
    *e.store.run_get_ok.lock().unwrap() = Some(1);
    let (spec, p) = tool(
        "act",
        Class::Navigate,
        mid(
            Class::Account,
            "y.example",
            "change email",
            OnTimeout::Proceed,
        ),
    );
    let reg = ToolRegistry::new(vec![spec]);
    call(&e, &reg, "act", json!({})).await;
    assert_eq!(p.calls.load(Ordering::SeqCst), 1);
    assert!(
        e.store.run_gets.load(Ordering::SeqCst) >= 2,
        "ask looked the run up"
    );
    assert_eq!(asker.cards()[0].on_timeout, OnTimeout::Skip);
    assert_eq!(*p.mid.lock().unwrap(), Some(AskOutcome::TimeoutSkip));
}

/// dispatch with a failed run lookup: a Send(new) that would proceed on timeout never runs.
/// (dispatch refuses the call outright, which is stricter than "reads as strict"; the Read
/// half only records what happens, for the report.)
#[tokio::test]
async fn d119_2_dispatch_failed_lookup_never_proceeds() {
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone(), Some(false)).await;
    *e.store.run_get_ok.lock().unwrap() = Some(0);
    let (spec, p) = tool("echo.send", Class::Send, send_to(STRANGER));
    let reg = ToolRegistry::new(vec![spec]);
    let r = dispatch(
        SERVER,
        &reg,
        &e.harness,
        "echo.send",
        json!({"to": STRANGER}),
    )
    .await;
    assert!(r.is_err() || r.as_ref().unwrap().text.starts_with("skipped"));
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    let (spec, p) = tool("echo.read", Class::Read, Beh::default());
    let reg = ToolRegistry::new(vec![spec]);
    let r = dispatch(SERVER, &reg, &e.harness, "echo.read", json!({})).await;
    eprintln!(
        "d119_2 dispatch failed lookup, Read call: ran={} err={}",
        p.calls.load(Ordering::SeqCst),
        r.is_err()
    );
}

// =====================================================================================
// D-119 (3): every Asker call capped at card.timeout + 5 s; a loosened timeout is a skip
// =====================================================================================

/// The real cap through dispatch and through ToolCtx::ask (60 s card + 5 s margin, run
/// concurrently: about 65 s of wall time).
#[tokio::test(flavor = "multi_thread")]
async fn d119_3_hung_asker_is_capped_at_timeout_plus_5s() {
    let e1 = env(Arc::new(Hang), Some(false)).await;
    let (spec, p1) = tool("echo.send", Class::Send, send_to(STRANGER));
    let reg1 = ToolRegistry::new(vec![spec]);
    let e2 = env(Arc::new(Hang), Some(false)).await;
    let (spec, p2) = tool(
        "act",
        Class::Navigate,
        mid(
            Class::Account,
            "y.example",
            "change email",
            OnTimeout::Proceed,
        ),
    );
    let reg2 = ToolRegistry::new(vec![spec]);
    let limit = ASK_TIMEOUT + Duration::from_secs(15);
    let started = Instant::now();
    let a = async {
        let o = tokio::time::timeout(
            limit,
            dispatch(
                SERVER,
                &reg1,
                &e1.harness,
                "echo.send",
                json!({"to": STRANGER}),
            ),
        )
        .await
        .expect("dispatch is capped")
        .unwrap();
        (o, started.elapsed())
    };
    let b = async {
        let o = tokio::time::timeout(
            limit,
            dispatch(SERVER, &reg2, &e2.harness, "act", json!({})),
        )
        .await
        .expect("dispatch is capped")
        .unwrap();
        (o, started.elapsed())
    };
    let ((o1, t1), (_o2, t2)) = tokio::join!(a, b);
    let lo = ASK_TIMEOUT + Duration::from_secs(5);
    let hi = lo + Duration::from_secs(5);
    eprintln!("d119_3 cap: dispatch {t1:?}, mid-call {t2:?} (cap {lo:?})");
    assert!(t1 >= lo && t1 < hi, "dispatch cap {t1:?}");
    assert!(t2 >= lo && t2 < hi, "mid-call cap {t2:?}");
    assert!(o1.text.starts_with("skipped: "), "{}", o1.text);
    assert_eq!(
        p1.calls.load(Ordering::SeqCst),
        0,
        "a cap hit never proceeds"
    );
    let ev1 = events(&e1);
    assert_eq!(ev1.len(), 1);
    assert_eq!(ev1[0].mode, Some(LedgerMode::TimeoutSkip));
    assert_eq!(*p2.mid.lock().unwrap(), Some(AskOutcome::TimeoutSkip));
    assert_eq!(events(&e2)[0].mode, Some(LedgerMode::TimeoutSkip));
}

#[tokio::test]
async fn d119_3_timeout_proceed_on_a_skip_card_counts_as_skip() {
    // Through dispatch: Pay asks with on_timeout Skip; the asker claims TimeoutProceed.
    let loose = Arc::new(Loose::default());
    let e = env(loose.clone(), Some(false)).await;
    let mut b = send_to("friend@example.com");
    b.classify.class = Some(Class::Pay);
    let (spec, p) = tool("echo.send", Class::Send, b);
    let reg = ToolRegistry::new(vec![spec]);
    let out = call(&e, &reg, "echo.send", json!({"to": "friend@example.com"})).await;
    assert_eq!(loose.0.lock().unwrap()[0].on_timeout, OnTimeout::Skip);
    assert!(out.text.starts_with("skipped: "), "{}", out.text);
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    let ev = events(&e);
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].mode, Some(LedgerMode::TimeoutSkip));

    // Through ToolCtx::ask: a Pay card the handler marked Proceed is tightened to Skip.
    let loose = Arc::new(Loose::default());
    let e = env(loose.clone(), Some(false)).await;
    let (spec, p) = tool(
        "act",
        Class::Navigate,
        mid(Class::Pay, "y.example", "confirm order", OnTimeout::Proceed),
    );
    let reg = ToolRegistry::new(vec![spec]);
    call(&e, &reg, "act", json!({})).await;
    assert_eq!(loose.0.lock().unwrap()[0].on_timeout, OnTimeout::Skip);
    assert_eq!(*p.mid.lock().unwrap(), Some(AskOutcome::TimeoutSkip));
    assert_eq!(events(&e)[0].mode, Some(LedgerMode::TimeoutSkip));
}

// =====================================================================================
// D-119 (4): FileAsker fails closed and closes asks no-clobber
// =====================================================================================

#[cfg(windows)]
fn deny_writes(dir: &Path) -> bool {
    std::process::Command::new("icacls")
        .arg(dir)
        .args(["/deny", "*S-1-1-0:(W)"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
#[cfg(windows)]
fn allow_writes(dir: &Path) {
    let _ = std::process::Command::new("icacls")
        .arg(dir)
        .args(["/remove:d", "*S-1-1-0"])
        .output();
}
#[cfg(unix)]
fn deny_writes(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).is_ok()
}
#[cfg(unix)]
fn allow_writes(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
}

/// `asks/` exists but the card cannot be written in it: TimeoutSkip at once.
#[tokio::test]
async fn d119_4_file_asker_skips_when_the_card_cannot_be_written() {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
    let asks = ask::asks_dir(&run.dir);
    std::fs::create_dir_all(&asks).unwrap();
    assert!(deny_writes(&asks), "precondition: could not deny writes");
    let probe = std::fs::write(asks.join("probe"), "x");
    if probe.is_ok() {
        allow_writes(&asks);
        panic!("precondition: writes into asks/ still succeed (elevated?)");
    }
    let fa = FileAsker::new(&root.0, run.id.clone(), vault());
    let k = AskCard {
        class: Class::Send,
        what: "send to new@example.com".into(),
        site: Some("mail.example".into()),
        values: Vec::new(),
        timeout: Duration::from_secs(10),
        on_timeout: OnTimeout::Proceed,
    };
    let started = Instant::now();
    let o = tokio::time::timeout(CAP, fa.ask(&k)).await;
    let took = started.elapsed();
    allow_writes(&asks);
    let o = o.expect("returns");
    assert_eq!(
        o,
        AskOutcome::TimeoutSkip,
        "an unwritten card never proceeds"
    );
    assert!(
        took < Duration::from_secs(5),
        "returns at once, not at the timer ({took:?})"
    );
    assert!(!ask::card_path(&run.dir, 1).exists());
}

/// The user's answer racing the asker's own timeout: whoever claims `<n>.answer` first
/// wins, and `ask::answer` succeeds exactly when the asker reports that answer.
#[test]
fn d119_4_answer_racing_the_timeout_is_never_lost() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let timeout = Duration::from_millis(40);
    let (mut won, mut lost) = (0, 0);
    for i in 0..120u32 {
        let root = Tmp::new();
        let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
        let fa = FileAsker::new(&root.0, run.id.clone(), vault());
        let k = AskCard {
            class: Class::Send,
            what: "send to new@example.com".into(),
            site: None,
            values: Vec::new(),
            timeout,
            on_timeout: OnTimeout::Proceed,
        };
        let card = ask::card_path(&run.dir, 1);
        let (root_p, run_id) = (root.0.clone(), run.id.clone());
        // Aim the "no" at a point that sweeps across the deadline (-10 ms .. +10 ms).
        let offset = Duration::from_micros(u64::from(i % 21) * 1000);
        let answerer = std::thread::spawn(move || {
            let t0 = Instant::now();
            while !card.is_file() {
                assert!(t0.elapsed() < CAP, "card never appeared");
                std::thread::yield_now();
            }
            let target = t0 + timeout + offset - Duration::from_millis(10);
            while Instant::now() < target {
                std::thread::yield_now();
            }
            ask::answer(&root_p, &run_id, 1, false)
        });
        let o = rt.block_on(async { tokio::time::timeout(CAP, fa.ask(&k)).await.unwrap() });
        let r = answerer.join().unwrap();
        let file = std::fs::read_to_string(ask::answer_path(&run.dir, 1)).unwrap();
        match r {
            Ok(()) => {
                won += 1;
                assert_eq!(
                    o,
                    AskOutcome::AnsweredSkip,
                    "iteration {i}: accepted no was lost"
                );
                assert_eq!(file.trim(), "no");
            }
            Err(err) => {
                lost += 1;
                assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{i}: {err}");
                assert_eq!(o, AskOutcome::TimeoutProceed, "iteration {i}");
                assert_eq!(file.trim(), "timeout");
            }
        }
    }
    eprintln!("d119_4 race: answer won {won}, timeout won {lost}");
}

/// The a7 flake: can a reader see `asks/<n>.json` empty or half-written? A reader thread
/// polls the card path while FileAsker::ask publishes it. A small card (many iterations)
/// and a large card (serialisation happens after `create_new`, so the window is wide).
#[test]
fn file_asker_card_is_never_seen_empty_or_partial() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    #[derive(Default, Debug)]
    struct Seen {
        empty: u32,
        partial: u32,
        other_err: u32,
        full: u32,
    }
    let mut total = Seen::default();
    let mut iterations = 0;
    for (runs, nvalues) in [(150usize, 1usize), (10, 20_000)] {
        let mut hit_runs = 0;
        for _ in 0..runs {
            iterations += 1;
            let root = Tmp::new();
            let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
            let fa = FileAsker::new(&root.0, run.id.clone(), vault());
            let values: Vec<(String, String)> = (0..nvalues)
                .map(|i| (format!("k{i}"), "v".repeat(64)))
                .collect();
            let k = AskCard {
                class: Class::Send,
                what: "send to new@example.com".into(),
                site: Some("mail.example".into()),
                values,
                timeout: Duration::from_millis(1),
                on_timeout: OnTimeout::Skip,
            };
            let path = ask::card_path(&run.dir, 1);
            let done = Arc::new(AtomicBool::new(false));
            let d = done.clone();
            let reader = std::thread::spawn(move || {
                let mut s = Seen::default();
                loop {
                    let finished = d.load(Ordering::SeqCst);
                    match std::fs::read(&path) {
                        Ok(b) if b.is_empty() => s.empty += 1,
                        Ok(b) => {
                            if serde_json::from_slice::<Value>(&b).is_ok() {
                                s.full += 1;
                                break;
                            }
                            s.partial += 1;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => s.other_err += 1,
                    }
                    if finished {
                        break;
                    }
                }
                s
            });
            let o = rt.block_on(async { tokio::time::timeout(CAP, fa.ask(&k)).await.unwrap() });
            assert_eq!(o, AskOutcome::TimeoutSkip);
            done.store(true, Ordering::SeqCst);
            let s = reader.join().unwrap();
            total.empty += s.empty;
            total.partial += s.partial;
            total.other_err += s.other_err;
            if s.empty + s.partial > 0 {
                hit_runs += 1;
            }
            total.full += s.full;
        }
        eprintln!(
            "card race, {nvalues} value(s): {hit_runs} of {runs} asks had an empty or partial read"
        );
    }
    eprintln!("card race over {iterations} asks: {total:?}");
    assert_eq!(
        total.empty + total.partial,
        0,
        "a reader saw the card empty or half-written: {total:?}"
    );
}

// =====================================================================================
// D-119 (5): a classifier panic
// =====================================================================================

#[tokio::test]
async fn d119_5_classifier_panic_is_an_error_and_a_failed_tool_event() {
    let e = env(Arc::new(ScriptedAsker::new()), Some(false)).await;
    let b = Beh {
        classify_panics: true,
        ..Beh::default()
    };
    let (spec, p) = tool("echo.cls", Class::Fill, b);
    let reg = ToolRegistry::new(vec![spec]);
    let err = dispatch(SERVER, &reg, &e.harness, "echo.cls", json!({}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("panic"), "{err}");
    assert!(!err.to_string().contains("fell over"), "payload not shown");
    assert_eq!(p.calls.load(Ordering::SeqCst), 0, "call never runs");
    let ev = events(&e);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0].server, SERVER);
    assert_eq!(ev[0].tool, "echo.cls");
    assert!(!ev[0].ok);
    assert_mirror(&e);

    let input = format!(
        "{}\n",
        json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": {"name": "echo.cls", "arguments": {}}})
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
    assert_eq!(reply["id"], json!(9));
    assert_eq!(reply["result"]["isError"], json!(true));
    let ev = events(&e);
    assert_eq!(ev.len(), 2);
    assert!(!ev[1].ok);
    assert_mirror(&e);
}

// =====================================================================================
// D-119 (6): only payload strings are scrubbed
// =====================================================================================

#[tokio::test]
async fn d119_6_envelope_words_equal_to_a_vault_value_survive() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let mut e = env(Arc::new(ScriptedAsker::new()), Some(false)).await;
    // "tools" is in the capability key, the list_changed method and our string id.
    e.harness.vault = Arc::new(Vault::in_memory([(
        "w".to_owned(),
        "tools".to_owned(),
        EntryKind::Secret,
    )]));
    let (spec, _p) = tool("echo.tools", Class::Read, Beh::default());
    let reg = ToolRegistry::new(vec![spec]);
    let (client, server_end) = tokio::io::duplex(1 << 16);
    let (sr, sw) = tokio::io::split(server_end);
    let task = tokio::spawn(serve_io(
        SERVER,
        reg.clone(),
        e.harness.clone(),
        tokio::io::BufReader::new(sr),
        sw,
    ));
    let (cr, mut cw) = tokio::io::split(client);
    let mut lines = tokio::io::BufReader::new(cr).lines();
    let send = |v: Value| {
        let mut s = v.to_string();
        s.push('\n');
        s.into_bytes()
    };
    macro_rules! next {
        () => {{
            let l = tokio::time::timeout(CAP, lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            serde_json::from_str::<Value>(&l).unwrap()
        }};
    }
    cw.write_all(&send(
        json!({"jsonrpc": "2.0", "id": "tools", "method": "initialize", "params": {}}),
    ))
    .await
    .unwrap();
    let init = next!();
    assert_eq!(init["id"], json!("tools"), "string id untouched");
    assert_eq!(init["jsonrpc"], json!("2.0"));
    assert_eq!(
        init["result"]["capabilities"]["tools"]["listChanged"],
        json!(true)
    );
    cw.write_all(&send(
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    ))
    .await
    .unwrap();
    cw.write_all(&send(
        json!({"jsonrpc": "2.0", "id": "tools", "method": "tools/list"}),
    ))
    .await
    .unwrap();
    let list = next!();
    assert_eq!(list["id"], json!("tools"));
    assert_eq!(
        list["result"]["tools"][0]["name"],
        json!("echo.@vault(w)"),
        "payload scrubbed"
    );
    assert_eq!(
        list["result"]["tools"][0]["description"],
        json!("echo.@vault(w) test tool")
    );
    let (mut extra, _) = tool("other", Class::Read, Beh::default());
    extra.description = "x".into();
    reg.add(extra);
    let note = next!();
    assert_eq!(
        note,
        json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
        "method untouched"
    );
    cw.write_all(&send(
        json!({"jsonrpc": "2.0", "id": "tools", "method": "tools/call",
            "params": {"name": "other", "arguments": {}}}),
    ))
    .await
    .unwrap();
    let r = next!();
    assert_eq!(r["id"], json!("tools"));
    assert_eq!(r["result"]["isError"], json!(false));
    cw.shutdown().await.unwrap();
    tokio::time::timeout(CAP, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

// =====================================================================================
// D-119 (7): diagnostic and stop lines: scrubbed, cleaned, scrubbed
// =====================================================================================

/// Stop lines the wrapper builds itself: a mid-call challenge and a gate stop rule (element
/// arg). Each why (ledger) and the returned stop text carry the multi-line vault value as
/// @vault(addr), on one clean line.
#[tokio::test]
async fn d119_7_stop_lines_are_one_clean_scrubbed_line() {
    let c = canary();
    let dirty = format!("turnstile\x1b[31m on {ADDR}\r\nnext {c}");
    // Mid-call challenge.
    let e = env(Arc::new(ScriptedAsker::new()), Some(false)).await;
    let (spec, _p) = tool(
        "act",
        Class::Navigate,
        mid(Class::Challenge, "x.example", &dirty, OnTimeout::Skip),
    );
    let reg = ToolRegistry::new(vec![spec]);
    call(&e, &reg, "act", json!({})).await;
    let ev = events(&e);
    let why = ev[0].why.clone().unwrap_or_default();
    assert_eq!(ev[0].mode, Some(LedgerMode::Stop));
    assert!(
        one_clean_line(&why),
        "mid-call stop why is one clean line: {why:?}"
    );
    assert!(
        why.contains("@vault(addr)") && why.contains("@vault(test)"),
        "{why:?}"
    );
    assert!(
        !why.contains("Main") && !why.contains("Springfield"),
        "{why:?}"
    );

    // Gate stop rule; the element arg carries the dirty text.
    let e = env(Arc::new(ScriptedAsker::new()), Some(false)).await;
    e.store
        .inner
        .add_rule("x.example", "Submit", Mode::Stop, false)
        .unwrap();
    let b = Beh {
        classify: Classified {
            domain: Some("x.example".into()),
            ..Classified::default()
        },
        ..Beh::default()
    };
    let (spec, _p) = tool("act", Class::Submit, b);
    let reg = ToolRegistry::new(vec![spec]);
    let out = call(&e, &reg, "act", json!({"element": dirty.clone()})).await;
    let ev = events(&e);
    let why = ev[0].why.clone().unwrap_or_default();
    assert!(one_clean_line(&why), "{why:?}");
    assert_eq!(
        out.text.lines().count(),
        1,
        "gate stop text is one line: {:?}",
        out.text
    );
    assert!(one_clean_line(&out.text), "{:?}", out.text);
    assert!(!out.text.contains("Main") && !out.text.contains(&c));
}

/// D-119 (7) for a handler's stopped output (ToolOutput::stopped): the why and element land
/// in the ledger (tool event and core stop), the returned output and the model's stop line.
#[tokio::test]
async fn d119_7_handler_stop_line_is_one_clean_scrubbed_line() {
    let c = canary();
    let dirty = format!("turnstile\x1b[31m on {ADDR}\r\nnext {c}");
    let e = env(Arc::new(ScriptedAsker::new()), Some(false)).await;
    let b = Beh {
        stop: Some(StopReason {
            class: Class::Challenge,
            why: dirty.clone(),
            element: Some(format!("#31\n{ADDR}")),
        }),
        ..Beh::default()
    };
    let (spec, _p) = tool("act", Class::Navigate, b);
    let reg = ToolRegistry::new(vec![spec]);
    let out = call(&e, &reg, "act", json!({})).await;
    let rendered = serde_json::to_string(&out).unwrap();
    assert!(!rendered.contains(&c) && !rendered.contains("Springfield"));
    let ev = events(&e);
    assert_eq!(ev.len(), 2, "{ev:?}");
    // Every place the handler's stop line lands; all must be one clean line.
    let mut bad: Vec<String> = Vec::new();
    for x in &ev {
        let why = x.why.clone().unwrap_or_default();
        assert!(!why.contains("Main") && !why.contains(&c), "{why:?}");
        if !one_clean_line(&why) {
            bad.push(format!("ledger why ({}): {why:?}", x.server));
        }
    }
    let stop = out.stopped.clone().unwrap();
    if !one_clean_line(&stop.why) {
        bad.push(format!("returned stopped.why: {:?}", stop.why));
    }
    // The model's text over the transport: the stop line follows the partial result.
    let input = format!(
        "{}\n",
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "act", "arguments": {}}})
    );
    let mut wire = Vec::new();
    tokio::time::timeout(
        CAP,
        serve_io(SERVER, reg, e.harness.clone(), input.as_bytes(), &mut wire),
    )
    .await
    .unwrap()
    .unwrap();
    let reply: Value = serde_json::from_str(String::from_utf8(wire).unwrap().trim()).unwrap();
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(!text.contains(&c) && !text.contains("Springfield"));
    let after = text.strip_prefix("partial result\n").unwrap_or(&text);
    if !after.starts_with("stopped: challenge") || !one_clean_line(after) {
        bad.push(format!("model text stop line: {after:?}"));
    }
    assert!(
        bad.is_empty(),
        "handler stop lines are not one clean line:\n{}",
        bad.join("\n")
    );
}

/// Wrapper diagnostics on stderr (a gate error during a mid-call ask whose message carries
/// the canary and the multi-line value): scrubbed and one line. Runs as a child process.
const DIAG_CHILD: &str = "SB_ACCEPT_T07R2_DIAG_CHILD";

#[tokio::test]
#[ignore = "child of d119_7_wrapper_diag_lines_are_scrubbed_and_clean"]
async fn d119_7_diag_child() {
    if std::env::var_os(DIAG_CHILD).is_none() {
        return;
    }
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker, Some(false)).await;
    *e.store.fail_gate_domain.lock().unwrap() = Some("err.example".into());
    *e.store.gate_err.lock().unwrap() = format!("db said {}\x1b[2J and {ADDR}", canary());
    let (spec, p) = tool(
        "act",
        Class::Navigate,
        mid(
            Class::Account,
            "err.example",
            "change email",
            OnTimeout::Proceed,
        ),
    );
    let reg = ToolRegistry::new(vec![spec]);
    call(&e, &reg, "act", json!({})).await;
    assert_eq!(*p.mid.lock().unwrap(), Some(AskOutcome::TimeoutSkip));
}

#[test]
fn d119_7_wrapper_diag_lines_are_scrubbed_and_clean() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "d119_7_diag_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(DIAG_CHILD, "1")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "child failed ({:?})", out.status);
    assert!(stdout.contains("1 passed"), "the child test ran");
    let c = canary();
    assert!(
        !err.contains(&c) && !stdout.contains(&c),
        "stderr must be scrubbed"
    );
    let line = err
        .lines()
        .find(|l| l.contains("gate (mid-call ask)"))
        .expect("the diag line is printed");
    assert!(
        line.contains("@vault(test)") && line.contains("@vault(addr)"),
        "{line:?}"
    );
    assert!(one_clean_line(line), "{line:?}");
    assert!(
        !err.contains("Springfield"),
        "value reshaped past the scrubber"
    );
}
