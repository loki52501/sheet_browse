//! T-07 round 3 (review-T-07 round 2): the ask card is published whole, a mid-call card
//! with no site meets the call's domain rules, and the asker claims `<n>.answer` on every
//! path where it stops waiting. docs/01 H-6, H-1.3, H-1.4; I-2; D-119, D-123.
//!
//! The vault is `Vault::in_memory` with no entries or one test canary; no fixture holds a
//! real value. `cargo test -p harness-core --test mcp_t07_r3`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harness_core::ask::{self, FileAsker};
use harness_core::class::{Class, Mode};
use harness_core::gate::{AskCard, AskOutcome, OnTimeout};
use harness_core::mcp::{
    dispatch, Classified, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolRegistry,
    ToolSpec,
};
use harness_core::memory::{ts_now, LedgerMode, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::Vault;
use harness_core::Asker;
use notify::Watcher;
use serde_json::{json, Value};

/// Cap on every wait (a hung asker fails the test instead of hanging it).
const CAP: Duration = Duration::from_secs(20);

/// A unique folder under the OS temp dir, removed on drop.
struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "sb-mcp-t07r3-{}-{}-{}",
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

fn vault() -> Arc<Vault> {
    Arc::new(Vault::in_memory([]))
}

fn card(timeout: Duration, values: usize) -> AskCard {
    AskCard {
        class: Class::Send,
        what: "send to new@example.com".into(),
        site: Some("mail.example".into()),
        values: (0..values)
            .map(|i| (format!("k{i}"), "v".repeat(64)))
            .collect(),
        timeout,
        on_timeout: OnTimeout::Skip,
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

/// What a reader thread saw while polling a card path.
#[derive(Default, Debug)]
struct Seen {
    /// Reads that were not a complete JSON object carrying the expected `n`.
    bad: u32,
    full: u32,
}

/// Polls `path` until `done` is set (then reads once more), classifying every read.
fn poll_card(path: PathBuf, n: u64, done: Arc<AtomicBool>) -> std::thread::JoinHandle<Seen> {
    std::thread::spawn(move || {
        let mut s = Seen::default();
        loop {
            let finished = done.load(Ordering::SeqCst);
            match std::fs::read(&path) {
                Ok(b) => match serde_json::from_slice::<Value>(&b) {
                    Ok(v) if v.is_object() && v["n"] == json!(n) => s.full += 1,
                    _ => s.bad += 1,
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                // Windows: a link being made can briefly deny a read; not a partial card.
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
                Err(_) => s.bad += 1,
            }
            if finished {
                break;
            }
            // Give the core back between reads: a reader that never yields competes with
            // the asker's blocking-pool and watcher threads on a loaded machine.
            std::thread::yield_now();
        }
        s
    })
}

/// Item 1: a reader polling `<n>.json` while FileAsker publishes it only ever sees no
/// file or the whole card. Small cards (many asks) and large cards (a wide write window).
#[test]
fn a_polling_reader_only_ever_sees_a_complete_card() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut total = Seen::default();
    for (asks, values) in [(200usize, 1usize), (15, 20_000)] {
        for _ in 0..asks {
            let root = Tmp::new();
            let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
            let fa = FileAsker::new(&root.0, run.id.clone(), vault());
            let k = card(Duration::from_millis(1), values);
            let done = Arc::new(AtomicBool::new(false));
            let reader = poll_card(ask::card_path(&run.dir, 1), 1, done.clone());
            let o = rt.block_on(async { tokio::time::timeout(CAP, fa.ask(&k)).await.unwrap() });
            done.store(true, Ordering::SeqCst);
            let s = reader.join().unwrap();
            assert_eq!(o, AskOutcome::TimeoutSkip);
            total.bad += s.bad;
            total.full += s.full;
            let left: Vec<String> = std::fs::read_dir(ask::asks_dir(&run.dir))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".tmp"))
                .collect();
            assert!(left.is_empty(), "temp files left behind: {left:?}");
        }
    }
    assert_eq!(total.bad, 0, "a reader saw an incomplete card: {total:?}");
    assert!(total.full > 0, "the reader saw the card at least once");
}

/// Item 1: several askers on one run (separate `next` counters, as in separate processes)
/// publishing at once never share an `n`, and every card on disk is complete.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_askers_on_one_run_never_share_an_n() {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
    let mut tasks = Vec::new();
    for i in 0..12 {
        let fa = FileAsker::new(&root.0, run.id.clone(), vault());
        let mut k = card(Duration::from_millis(1), 50);
        k.what = format!("ask {i}");
        tasks.push(tokio::spawn(async move { fa.ask(&k).await }));
    }
    for t in tasks {
        let o = tokio::time::timeout(CAP, t).await.unwrap().unwrap();
        assert_eq!(o, AskOutcome::TimeoutSkip);
    }
    let mut whats = Vec::new();
    for n in 1..=12u64 {
        let v: Value =
            serde_json::from_slice(&std::fs::read(ask::card_path(&run.dir, n)).unwrap()).unwrap();
        assert_eq!(v["n"], json!(n));
        whats.push(v["what"].as_str().unwrap().to_owned());
        let a = std::fs::read_to_string(ask::answer_path(&run.dir, n)).unwrap();
        assert_eq!(a.trim(), "timeout");
    }
    whats.sort();
    whats.dedup();
    assert_eq!(whats.len(), 12, "each ask got its own n");
    assert!(!ask::card_path(&run.dir, 13).exists());
}

/// Item 1: `answer` accepts no answer for a card file that is not a complete card.
#[test]
fn answer_refuses_an_empty_or_partial_card() {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
    std::fs::create_dir_all(ask::asks_dir(&run.dir)).unwrap();
    for (n, text) in [(1u64, ""), (2, "{\n  \"n\": 2,\n  \"run\": "), (3, "[]")] {
        std::fs::write(ask::card_path(&run.dir, n), text).unwrap();
        let err = ask::answer(&root.0, &run.id, n, true).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "card {n}");
        assert!(!ask::answer_path(&run.dir, n).exists(), "card {n}");
    }
    // A complete JSON object is a card (`harness ask` tests write `{}`).
    std::fs::write(ask::card_path(&run.dir, 4), "{}").unwrap();
    ask::answer(&root.0, &run.id, 4, false).unwrap();
    let a = std::fs::read_to_string(ask::answer_path(&run.dir, 4)).unwrap();
    assert_eq!(a.trim(), "no");
}

/// Item 3: an ask whose future is dropped while it waits (the wrapper's cap) claims
/// `<n>.answer` as `timeout`, so a late answer is refused instead of reported as taken.
#[tokio::test]
async fn a_dropped_ask_closes_its_answer() {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
    let fa = FileAsker::new(&root.0, run.id.clone(), vault());
    let k = card(Duration::from_secs(600), 1);
    let card_file = ask::card_path(&run.dir, 1);
    let dropped = tokio::select! {
        o = fa.ask(&k) => Some(o),
        () = wait_for(&card_file) => None,
    };
    assert_eq!(
        dropped, None,
        "the ask was still waiting when it was dropped"
    );
    let answer_file = ask::answer_path(&run.dir, 1);
    // The guard closes the ask as it is dropped: inline when the asker holds it, or on the
    // card-writing thread when the drop came first.
    wait_for(&answer_file).await;
    assert_eq!(
        std::fs::read_to_string(&answer_file).unwrap().trim(),
        "timeout"
    );
    let err = ask::answer(&root.0, &run.id, 1, true).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read_to_string(&answer_file).unwrap().trim(),
        "timeout"
    );
}

/// Item 3: an ask dropped at any point, including while the card is still being written,
/// never leaves a card on disk without a claimed answer once its file work has finished.
#[test]
fn an_ask_dropped_mid_publish_never_leaves_an_open_card() {
    let (mut with_card, mut without) = (0, 0);
    for i in 0..120u64 {
        let root = Tmp::new();
        let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
        let fa = FileAsker::new(&root.0, run.id.clone(), vault());
        let k = card(Duration::from_secs(600), if i % 2 == 0 { 1 } else { 5_000 });
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Sweep the drop point across the publish (0 .. ~6 ms).
        let cut = Duration::from_micros((i % 40) * 150);
        let r = rt.block_on(async { tokio::time::timeout(cut, fa.ask(&k)).await });
        assert!(r.is_err(), "a 600 s ask cannot end within {cut:?}");
        // Waits for the blocking file task (the card write) to finish: the condition.
        rt.shutdown_timeout(CAP);
        let card_file = ask::card_path(&run.dir, 1);
        if card_file.exists() {
            with_card += 1;
            let v: Value = serde_json::from_slice(&std::fs::read(&card_file).unwrap()).unwrap();
            assert_eq!(v["n"], json!(1));
            let a = std::fs::read_to_string(ask::answer_path(&run.dir, 1));
            assert_eq!(
                a.as_deref().map(str::trim).ok(),
                Some("timeout"),
                "iteration {i}: a card was left open"
            );
        } else {
            without += 1;
            assert!(!ask::answer_path(&run.dir, 1).exists());
        }
    }
    eprintln!("dropped asks: {with_card} with a card, {without} before the card");
    assert!(with_card > 0, "the sweep reached a published card");
}

// ---- item 2: a mid-call card with no site meets the call's domain rules ----

#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    mid: Mutex<Option<AskOutcome>>,
}

struct MidAsker {
    domain: &'static str,
    card: AskCard,
    probe: Arc<Probe>,
}

#[async_trait]
impl ToolHandler for MidAsker {
    async fn classify(&self, _ctx: &ToolCtx, _args: &Value) -> Result<Classified, ToolError> {
        Ok(Classified {
            domain: Some(self.domain.into()),
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, _args: Value) -> Result<ToolOutput, ToolError> {
        self.probe.calls.fetch_add(1, Ordering::SeqCst);
        let o = ctx.ask(self.card.clone()).await;
        *self.probe.mid.lock().unwrap() = Some(o);
        Ok(ToolOutput {
            text: "done".into(),
            ..ToolOutput::default()
        })
    }
}

struct Env {
    _root: Tmp,
    store: Arc<MemStore>,
    asker: Arc<ScriptedAsker>,
    harness: Harness,
}

async fn env() -> Env {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "t07r3", "mid-call test").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "mid-call test".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    let asker = Arc::new(ScriptedAsker::new());
    let harness = Harness {
        store: store.clone(),
        vault: vault(),
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

/// An Account card with no site, from a Navigate call classified to `domain`.
async fn mid_call(e: &Env, domain: &'static str) -> (Option<AskOutcome>, usize) {
    let probe = Arc::new(Probe::default());
    let spec = ToolSpec {
        name: "act".into(),
        description: "act test tool".into(),
        class: Class::Navigate,
        args: json!({"type": "object"}),
        handler: Arc::new(MidAsker {
            domain,
            card: AskCard {
                class: Class::Account,
                what: "change the email".into(),
                site: None,
                values: Vec::new(),
                timeout: Duration::from_millis(1),
                on_timeout: OnTimeout::Proceed,
            },
            probe: probe.clone(),
        }),
    };
    let reg = ToolRegistry::new(vec![spec]);
    dispatch("echo", &reg, &e.harness, "act", json!({}))
        .await
        .unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    let o = *probe.mid.lock().unwrap();
    (o, e.asker.cards().len())
}

#[tokio::test]
async fn site_less_mid_call_card_meets_the_call_domain_stop_rule() {
    let e = env().await;
    e.store
        .add_rule("x.example", "Account", Mode::Stop, false)
        .unwrap();
    let (o, shown) = mid_call(&e, "x.example").await;
    assert_eq!(o, Some(AskOutcome::TimeoutSkip));
    assert_eq!(shown, 0, "a stop rule shows no card");
    let ev: Vec<_> = e.store.ledger().into_iter().map(|(_, ev)| ev).collect();
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[0].server, "core");
    assert_eq!(ev[0].class, Some(Class::Account));
    assert_eq!(ev[0].mode, Some(LedgerMode::Stop));
    assert!(
        ev[0].why.as_deref().unwrap_or("").contains("x.example"),
        "{:?}",
        ev[0].why
    );

    // Control: the same rule does not touch a call to another site.
    let e = env().await;
    e.store
        .add_rule("x.example", "Account", Mode::Stop, false)
        .unwrap();
    let (o, shown) = mid_call(&e, "y.example").await;
    assert_eq!(o, Some(AskOutcome::TimeoutProceed));
    assert_eq!(shown, 1);
}

#[tokio::test]
async fn site_less_mid_call_card_meets_the_call_domain_strict_rule() {
    let e = env().await;
    e.store
        .add_rule("x.example", "Account", Mode::Ask, true)
        .unwrap();
    let (o, shown) = mid_call(&e, "x.example").await;
    assert_eq!(shown, 1);
    assert_eq!(e.asker.cards()[0].on_timeout, OnTimeout::Skip);
    assert_eq!(o, Some(AskOutcome::TimeoutSkip));
}
