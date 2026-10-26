//! T-07 rounds 3 and 4 acceptance (tester): the D-123 and D-127 clauses, checked from the
//! outside through the public I-2 surface (FileAsker, ask::answer, dispatch, serve_io,
//! ToolRegistry, ledger::write_report). docs/01 H-1.3, H-1.4, H-6; I-2; D-119, D-121 (4),
//! D-123, D-127; CLAUDE.md rules 2, 4, 6.
//!
//! MemStore + `Vault::in_memory` + a temp runs root. The canary comes from `SB_VAULT_CANARY`
//! (fallback `accept-t07-r3-canary-not-a-secret`); no assertion message formats it. Under
//! `SB_ACCEPT_T07_KEEP` (a directory) every runs root is kept there so the tester can grep it
//! after the process exits.
//!
//! The no-hard-link tests are `#[ignore]`d because they need a folder on a filesystem with no
//! hard links (FAT, exFAT): run them with `SB_ACCEPT_NOLINK_DIR=<dir> -- --ignored nolink`.
//!
//! `SB_VAULT_CANARY=<unique> cargo test -p harness-core --test accept_t07_r3 -- --nocapture`

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use harness_core::ask::{self, FileAsker};
use harness_core::class::{Class, Mode};
use harness_core::gate::{AskCard, AskOutcome, OnTimeout, StopReason};
use harness_core::ledger::{read_jsonl, write_report};
use harness_core::mcp::{
    dispatch, serve_io, Classified, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput,
    ToolRegistry, ToolSpec,
};
use harness_core::memory::{ts_now, LedgerEvent, LedgerMode, MemoryStore, Run, RunStatus};
use harness_core::run::{RunFolder, RunId};
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::{EntryKind, Vault};
use harness_core::Asker;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const SERVER: &str = "echo";
const STRANGER: &str = "stranger@example.com";
/// Cap on every wait.
const CAP: Duration = Duration::from_secs(30);
/// D-127 (3): MAX_LINE_BYTES is pub(crate); the decision says 8 MiB, newline not counted.
const MAX_LINE: usize = 8 * 1024 * 1024;

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "accept-t07-r3-canary-not-a-secret".to_owned())
}

fn vault() -> Arc<Vault> {
    Arc::new(Vault::in_memory([(
        "test".to_owned(),
        canary(),
        EntryKind::Secret,
    )]))
}

struct Tmp(PathBuf, bool);
impl Tmp {
    fn new() -> Tmp {
        let keep = std::env::var_os("SB_ACCEPT_T07_KEEP").map(PathBuf::from);
        let parent = keep.clone().unwrap_or_else(std::env::temp_dir);
        Tmp::under(&parent, keep.is_some())
    }
    fn under(parent: &Path, keep: bool) -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = parent.join(format!(
            "sb-accept-t07r3-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            chrono::Utc::now().timestamp_micros()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p, keep)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        if !self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn card(class: Class, site: Option<&str>, timeout: Duration, on_timeout: OnTimeout) -> AskCard {
    AskCard {
        class,
        what: "send to new@example.com".into(),
        site: site.map(str::to_owned),
        values: Vec::new(),
        timeout,
        on_timeout,
    }
}

/// Every entry of `asks/`, sorted.
fn asks_entries(run_dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(ask::asks_dir(run_dir))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Waits (polling, capped) until `path` is a file. Test-side only.
fn wait_file_blocking(path: &Path) {
    let t0 = Instant::now();
    while !path.is_file() {
        assert!(
            t0.elapsed() < CAP,
            "file never appeared: {}",
            path.display()
        );
        std::thread::yield_now();
    }
}

async fn wait_file(path: PathBuf) {
    tokio::task::spawn_blocking(move || wait_file_blocking(&path))
        .await
        .unwrap();
}

#[derive(Default, Debug, Clone, Copy)]
struct Seen {
    empty: u32,
    partial: u32,
    full: u32,
    other_err: u32,
}

/// Polls every `asks/*.json` until `done`, classifying each read.
fn poll_cards(run_dir: PathBuf, done: Arc<AtomicBool>) -> std::thread::JoinHandle<Seen> {
    std::thread::spawn(move || {
        let mut s = Seen::default();
        let asks = ask::asks_dir(&run_dir);
        loop {
            let finished = done.load(Ordering::SeqCst);
            if let Ok(rd) = std::fs::read_dir(&asks) {
                for e in rd.filter_map(Result::ok) {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') || !name.ends_with(".json") {
                        continue;
                    }
                    match std::fs::read(e.path()) {
                        Ok(b) if b.is_empty() => s.empty += 1,
                        Ok(b) => match serde_json::from_slice::<Value>(&b) {
                            Ok(v) if v.is_object() => s.full += 1,
                            _ => s.partial += 1,
                        },
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => s.other_err += 1,
                    }
                }
            }
            if finished {
                break;
            }
        }
        s
    })
}

// ---- a configurable tool ----

#[derive(Default, Clone)]
struct Beh {
    classify: Classified,
    mid_asks: Vec<AskCard>,
    /// Drop each mid-call ask as soon as its card `asks/1.json` exists (FileAsker only).
    drop_mid_on_card: bool,
    stop: Option<StopReason>,
    fail: bool,
    panic: bool,
}

#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    /// One per mid-call ask; None = the ask future was dropped.
    mids: Mutex<Vec<Option<AskOutcome>>>,
    /// For a dropped mid-call ask: was `<n>.answer` there right after the drop returned?
    closed_on_drop: Mutex<Vec<bool>>,
}

struct H {
    b: Beh,
    p: Arc<Probe>,
}

#[async_trait]
impl ToolHandler for H {
    async fn classify(&self, _ctx: &ToolCtx, _args: &Value) -> Result<Classified, ToolError> {
        Ok(self.b.classify.clone())
    }
    async fn call(&self, ctx: &ToolCtx, _args: Value) -> Result<ToolOutput, ToolError> {
        self.p.calls.fetch_add(1, Ordering::SeqCst);
        for c in &self.b.mid_asks {
            let o = if self.b.drop_mid_on_card {
                let cardp = ask::card_path(&ctx.run_dir, 1);
                let o = tokio::select! {
                    o = ctx.ask(c.clone()) => Some(o),
                    _ = wait_file(cardp) => None,
                };
                if o.is_none() {
                    let closed = ask::answer_path(&ctx.run_dir, 1).is_file();
                    self.p.closed_on_drop.lock().unwrap().push(closed);
                }
                o
            } else {
                Some(ctx.ask(c.clone()).await)
            };
            self.p.mids.lock().unwrap().push(o);
        }
        if self.b.panic {
            panic!("handler fell over");
        }
        if self.b.fail {
            return Err(ToolError::Failed("handler failed".into()));
        }
        Ok(ToolOutput {
            text: "partial result".into(),
            stopped: self.b.stop.clone(),
            ..ToolOutput::default()
        })
    }
}

fn tool(name: &str, class: Class, desc: &str, b: Beh) -> (ToolSpec, Arc<Probe>) {
    let p = Arc::new(Probe::default());
    (
        ToolSpec {
            name: name.into(),
            description: desc.into(),
            class,
            args: json!({"type": "object"}),
            handler: Arc::new(H { b, p: p.clone() }),
        },
        p,
    )
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

fn site_less(class: Class, what: &str) -> AskCard {
    AskCard {
        class,
        what: what.into(),
        site: None,
        values: Vec::new(),
        timeout: Duration::from_millis(1),
        on_timeout: OnTimeout::Proceed,
    }
}

fn stop(why: &str, element: Option<&str>) -> Option<StopReason> {
    Some(StopReason {
        class: Class::Challenge,
        why: why.into(),
        element: element.map(str::to_owned),
    })
}

// ---- environment ----

struct Env {
    _root: Tmp,
    run: RunFolder,
    store: Arc<MemStore>,
    harness: Harness,
}

async fn env_with(asker: Arc<dyn Asker>, root: Tmp) -> Env {
    let run = RunFolder::create(&root.0, "t07r3", "acceptance t07 r3").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "acceptance t07 r3".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    store.add_contact("friend@example.com");
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

async fn env(asker: Arc<dyn Asker>) -> Env {
    env_with(asker, Tmp::new()).await
}

/// The same env with a FileAsker bound to its run as the harness asker.
async fn file_asker_env() -> Env {
    let root = Tmp::new();
    let runs_root = root.0.clone();
    let e = env_with(Arc::new(ScriptedAsker::new()), root).await;
    let fa = FileAsker::new(&runs_root, e.run.id.clone(), vault());
    Env {
        harness: Harness {
            asker: Arc::new(fa),
            ..e.harness.clone()
        },
        ..e
    }
}

/// Ledger rows in id order; asserts the jsonl mirror matches the table row for row.
fn events(e: &Env) -> Vec<LedgerEvent> {
    let mut rows = e.store.ledger();
    rows.sort_by_key(|(id, _)| *id);
    let mut lines = read_jsonl(&e.run.ledger_jsonl()).unwrap_or_default();
    lines.sort_by_key(|(id, _)| *id);
    assert!(rows == lines, "ledger.jsonl must match the table");
    rows.into_iter().map(|(_, ev)| ev).collect()
}

fn core_stops(ev: &[LedgerEvent]) -> usize {
    ev.iter()
        .filter(|x| x.server == "core" && x.mode == Some(LedgerMode::Stop))
        .count()
}

/// Writes report.md from the ledger and returns how many stops its Not done line lists.
fn report_stop_listings(e: &Env, ev: &[LedgerEvent]) -> (usize, String) {
    write_report(&e.run, "acceptance t07 r3", ev).unwrap();
    let r = std::fs::read_to_string(e.run.report_md()).unwrap();
    let nd = r
        .lines()
        .find(|l| l.starts_with("Not done"))
        .unwrap_or_default()
        .to_string();
    (nd.matches("stopped at ").count(), nd)
}

async fn call(e: &Env, reg: &ToolRegistry, name: &str, args: Value) -> ToolOutput {
    tokio::time::timeout(CAP, dispatch(SERVER, reg, &e.harness, name, args))
        .await
        .expect("dispatch within the cap")
        .expect("dispatch ok")
}

// =====================================================================================
// D-123 (1): cards are published whole, no-clobber
// =====================================================================================

/// Two FileAskers on one run (two server processes: each counts its own `n` from 1) ask 25
/// cards each, all at once, while a reader polls every card file. Every card is whole the
/// first time it is read, no two asks share an `n`, every ask is closed, and no temp file
/// is left behind.
#[test]
fn d123_1_two_askers_on_one_run_publish_whole_cards_with_unique_numbers() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let mut total = Seen::default();
    for round in 0..4 {
        let root = Tmp::new();
        let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
        let a = Arc::new(FileAsker::new(&root.0, run.id.clone(), vault()));
        let b = Arc::new(FileAsker::new(&root.0, run.id.clone(), vault()));
        let done = Arc::new(AtomicBool::new(false));
        let reader = poll_cards(run.dir.clone(), done.clone());
        let outcomes = rt.block_on(async {
            let mut hs = Vec::new();
            for i in 0..50 {
                let fa = if i % 2 == 0 { a.clone() } else { b.clone() };
                hs.push(tokio::spawn(async move {
                    let k = AskCard {
                        values: (0..(i * 40))
                            .map(|j| (format!("k{j}"), "v".repeat(32)))
                            .collect(),
                        ..card(
                            Class::Send,
                            Some("mail.example"),
                            Duration::from_millis(300),
                            OnTimeout::Skip,
                        )
                    };
                    tokio::time::timeout(CAP, fa.ask(&k)).await.unwrap()
                }));
            }
            let mut v = Vec::new();
            for h in hs {
                v.push(h.await.unwrap());
            }
            v
        });
        done.store(true, Ordering::SeqCst);
        let s = reader.join().unwrap();
        total.empty += s.empty;
        total.partial += s.partial;
        total.full += s.full;
        total.other_err += s.other_err;
        assert!(outcomes.iter().all(|o| *o == AskOutcome::TimeoutSkip));
        let entries = asks_entries(&run.dir);
        let temps: Vec<_> = entries.iter().filter(|n| n.starts_with('.')).collect();
        assert!(
            temps.is_empty(),
            "round {round}: temp files left: {temps:?}"
        );
        let mut ns = Vec::new();
        for name in entries.iter().filter(|n| n.ends_with(".json")) {
            let n: u64 = name.trim_end_matches(".json").parse().unwrap();
            let v: Value =
                serde_json::from_slice(&std::fs::read(ask::card_path(&run.dir, n)).unwrap())
                    .unwrap();
            assert_eq!(v["n"], json!(n), "card {n} names its own number");
            assert_eq!(
                std::fs::read_to_string(ask::answer_path(&run.dir, n))
                    .unwrap()
                    .trim(),
                "timeout",
                "ask {n} closed by its timer"
            );
            ns.push(n);
        }
        ns.sort_unstable();
        assert_eq!(ns, (1..=50).collect::<Vec<u64>>(), "round {round}");
    }
    eprintln!("d123_1 two askers, 4 x 50 asks: {total:?}");
    assert_eq!(total.empty + total.partial, 0, "{total:?}");
}

/// A card that cannot be written leaves nothing in `asks/`: no card and no temp file.
#[cfg(windows)]
#[tokio::test]
async fn d123_1_a_failed_write_leaves_no_card_and_no_temp_file() {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
    let asks = ask::asks_dir(&run.dir);
    std::fs::create_dir_all(&asks).unwrap();
    assert!(deny_writes(&asks), "precondition: could not deny writes");
    if std::fs::write(asks.join("probe"), "x").is_ok() {
        allow_writes(&asks);
        panic!("precondition: writes into asks/ still succeed");
    }
    let fa = FileAsker::new(&root.0, run.id.clone(), vault());
    let k = card(
        Class::Send,
        Some("mail.example"),
        Duration::from_secs(10),
        OnTimeout::Proceed,
    );
    let o = tokio::time::timeout(CAP, fa.ask(&k)).await;
    allow_writes(&asks);
    assert_eq!(o.expect("returns"), AskOutcome::TimeoutSkip);
    let left = asks_entries(&run.dir);
    assert!(left.is_empty(), "asks/ holds {left:?}");
}

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

// ---- no-hard-link fallback (needs SB_ACCEPT_NOLINK_DIR on FAT/exFAT) ----

fn nolink_root() -> Tmp {
    let dir = std::env::var_os("SB_ACCEPT_NOLINK_DIR")
        .map(PathBuf::from)
        .expect("set SB_ACCEPT_NOLINK_DIR to a folder on a filesystem without hard links");
    let t = Tmp::under(&dir, false);
    let a = t.0.join("probe-a");
    std::fs::write(&a, "x").unwrap();
    let linked = std::fs::hard_link(&a, t.0.join("probe-b"));
    let _ = std::fs::remove_file(t.0.join("probe-b"));
    std::fs::remove_file(&a).unwrap();
    assert!(
        linked.is_err(),
        "precondition: hard links work under SB_ACCEPT_NOLINK_DIR"
    );
    t
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Waits (polling, capped) until `asks/<n>.json` is listed with a non-zero size: what a user
/// sees before typing `harness ask` (the notice is printed after the card is published).
/// Uses the directory listing only, so it never opens the card: on Windows an open handle,
/// even a metadata one, is itself a reader of the placeholder.
fn wait_card_listed(run_dir: &Path, n: u64) {
    let asks = ask::asks_dir(run_dir);
    let name = format!("{n}.json");
    let t0 = Instant::now();
    loop {
        let listed = std::fs::read_dir(&asks).ok().is_some_and(|rd| {
            rd.filter_map(Result::ok).any(|e| {
                e.file_name().to_string_lossy() == name
                    && e.metadata().map(|m| m.len() > 0).unwrap_or(false)
            })
        });
        if listed {
            return;
        }
        assert!(t0.elapsed() < CAP, "card {n} never listed whole");
        std::thread::yield_now();
    }
}

/// D-123 (1) fallback: where hard links fail, a polling reader sees no card, an empty
/// placeholder or the whole card, never a part; a card whose publish failed leaves nothing
/// anyone could answer and no temp file.
#[test]
#[ignore = "needs SB_ACCEPT_NOLINK_DIR on a filesystem without hard links (exFAT/FAT)"]
fn d123_1_nolink_reader_sees_empty_or_whole_never_partial() {
    let rt = rt();
    // Reader race, small and large cards.
    let mut total = Seen::default();
    let (mut published, mut failed) = (0, 0);
    for (runs, nvalues) in [(60usize, 1usize), (6, 20_000)] {
        for _ in 0..runs {
            let root = nolink_root();
            let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
            let fa = FileAsker::new(&root.0, run.id.clone(), vault());
            let k = AskCard {
                values: (0..nvalues)
                    .map(|i| (format!("k{i}"), "v".repeat(64)))
                    .collect(),
                ..card(
                    Class::Send,
                    Some("mail.example"),
                    Duration::from_millis(1),
                    OnTimeout::Skip,
                )
            };
            let done = Arc::new(AtomicBool::new(false));
            let reader = poll_cards(run.dir.clone(), done.clone());
            let cardp = ask::card_path(&run.dir, 1);
            let o = rt.block_on(async { tokio::time::timeout(CAP, fa.ask(&k)).await.unwrap() });
            done.store(true, Ordering::SeqCst);
            let s = reader.join().unwrap();
            assert_eq!(o, AskOutcome::TimeoutSkip);
            total.empty += s.empty;
            total.partial += s.partial;
            total.full += s.full;
            total.other_err += s.other_err;
            let temps: Vec<_> = asks_entries(&run.dir)
                .into_iter()
                .filter(|n| n.starts_with('.'))
                .collect();
            assert!(temps.is_empty(), "temp files left: {temps:?}");
            let whole = std::fs::read(&cardp)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .is_some_and(|v| v.is_object());
            if whole {
                published += 1;
                assert_eq!(
                    std::fs::read_to_string(ask::answer_path(&run.dir, 1))
                        .unwrap()
                        .trim(),
                    "timeout"
                );
            } else {
                // The publish failed (the asker said so on stderr and skipped): no card is
                // left that anyone could answer.
                failed += 1;
                let r = ask::answer(&root.0, &run.id, 1, true);
                assert!(r.is_err(), "an unpublished card took an answer");
                assert!(!ask::answer_path(&run.dir, 1).exists());
            }
        }
    }
    eprintln!(
        "nolink card race: {total:?}; published {published}, publish failed under a reader {failed}"
    );
    assert_eq!(total.partial, 0, "a reader saw a partial card: {total:?}");
}

/// D-123 (1) fallback: two askers on one run (two processes), 20 asks at once, nobody else
/// reading: every ask publishes its card under its own `n`, and no temp file is left.
#[test]
#[ignore = "red-first T-07 round 5: needs SB_ACCEPT_NOLINK_DIR (exFAT/FAT); on exFAT claim_without_links' rename over the placeholder fails with Access denied while any handle is open on it, and claim()'s own target.exists() probe (std::fs::metadata opens a handle) from the other asker is one, so asks are skipped with 'cannot write the card' (13, 16 and 19 of 20 published over three runs); one run also left a temp file .4.json.<pid>.<seq>.tmp behind"]
fn d123_1_nolink_two_askers_each_publish_their_card() {
    let rt = rt();
    let root = nolink_root();
    let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
    let a = Arc::new(FileAsker::new(&root.0, run.id.clone(), vault()));
    let b = Arc::new(FileAsker::new(&root.0, run.id.clone(), vault()));
    rt.block_on(async {
        let mut hs = Vec::new();
        for i in 0..20 {
            let fa = if i % 2 == 0 { a.clone() } else { b.clone() };
            hs.push(tokio::spawn(async move {
                let k = card(
                    Class::Send,
                    None,
                    Duration::from_millis(200),
                    OnTimeout::Skip,
                );
                tokio::time::timeout(CAP, fa.ask(&k)).await.unwrap()
            }));
        }
        for h in hs {
            assert_eq!(h.await.unwrap(), AskOutcome::TimeoutSkip);
        }
    });
    let entries = asks_entries(&run.dir);
    assert!(
        !entries.iter().any(|n| n.starts_with('.')),
        "temp files left: {entries:?}"
    );
    let cards = entries.iter().filter(|n| n.ends_with(".json")).count();
    eprintln!("nolink two askers: {cards} of 20 cards published");
    assert_eq!(cards, 20, "{entries:?}");
}

/// D-123 (1) fallback, one asker: a yes typed after the card is shown reaches the asker; a
/// timed-out ask refuses a late answer.
#[test]
#[ignore = "needs SB_ACCEPT_NOLINK_DIR on a filesystem without hard links (exFAT/FAT)"]
fn d123_1_nolink_yes_and_timeout() {
    let rt = rt();
    let root = nolink_root();
    let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
    let fa = FileAsker::new(&root.0, run.id.clone(), vault());
    let (rp, id, rd) = (root.0.clone(), run.id.clone(), run.dir.clone());
    let answerer = std::thread::spawn(move || {
        wait_card_listed(&rd, 1);
        ask::answer(&rp, &id, 1, true)
    });
    let k = card(Class::Send, None, Duration::from_secs(10), OnTimeout::Skip);
    let o = rt.block_on(async { tokio::time::timeout(CAP, fa.ask(&k)).await.unwrap() });
    answerer.join().unwrap().unwrap();
    assert_eq!(o, AskOutcome::AnsweredProceed);

    // A timed-out ask refuses a late answer.
    let k = card(
        Class::Send,
        None,
        Duration::from_millis(100),
        OnTimeout::Proceed,
    );
    let o = rt.block_on(async { tokio::time::timeout(CAP, fa.ask(&k)).await.unwrap() });
    assert_eq!(o, AskOutcome::TimeoutProceed);
    let err = ask::answer(&root.0, &run.id, 2, false).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
    assert_eq!(
        std::fs::read_to_string(ask::answer_path(&run.dir, 2))
            .unwrap()
            .trim(),
        "timeout"
    );
}

/// D-119 (4) on the no-hard-link path: the user's yes racing the asker's own timeout. An
/// answer `ask::answer` accepted must be the outcome the asker reports.
#[test]
#[ignore = "red-first T-07 round 5: needs SB_ACCEPT_NOLINK_DIR (exFAT/FAT); on the no-hard-link path ask::answer's empty placeholder <n>.answer is visible before its rename, so the asker's close() gets AlreadyExists, reads the empty file and reports TimeoutSkip while answer() then returns Ok: an accepted yes is lost"]
fn d119_4_nolink_answer_racing_the_timeout_is_never_lost() {
    let rt = rt();
    let timeout = Duration::from_millis(40);
    let (mut won, mut refused, mut lost) = (0, 0, Vec::new());
    let (mut other_err, mut reopened) = (Vec::new(), Vec::new());
    for i in 0..120u32 {
        let root = nolink_root();
        let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
        let fa = FileAsker::new(&root.0, run.id.clone(), vault());
        let k = card(Class::Send, None, timeout, OnTimeout::Skip);
        let (rp, id, rd) = (root.0.clone(), run.id.clone(), run.dir.clone());
        let offset = Duration::from_micros(u64::from(i % 21) * 1000);
        let answerer = std::thread::spawn(move || {
            wait_card_listed(&rd, 1);
            let t0 = Instant::now();
            let target = t0 + timeout + offset - Duration::from_millis(10);
            while Instant::now() < target {
                std::thread::yield_now();
            }
            ask::answer(&rp, &id, 1, true)
        });
        let o = rt.block_on(async { tokio::time::timeout(CAP, fa.ask(&k)).await.unwrap() });
        match answerer.join().unwrap() {
            Ok(()) => {
                won += 1;
                if o != AskOutcome::AnsweredProceed {
                    lost.push((i, o));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                refused += 1;
                assert_eq!(o, AskOutcome::TimeoutSkip, "iteration {i}");
            }
            Err(e) => other_err.push((i, e.kind(), o)),
        }
        // Once the asker returned, the ask must stay closed: a later answer is refused.
        if ask::answer(&root.0, &run.id, 1, false).is_ok() {
            reopened.push((i, o));
        }
    }
    eprintln!(
        "nolink answer race: accepted {won}, refused (AlreadyExists) {refused}, \
         accepted yes lost {}: {lost:?}; other errors {}: {other_err:?}; \
         ask open again after the asker returned {}: {reopened:?}",
        lost.len(),
        other_err.len(),
        reopened.len()
    );
    assert!(
        lost.is_empty() && reopened.is_empty() && other_err.is_empty(),
        "lost {}, reopened {}, other errors {}",
        lost.len(),
        reopened.len(),
        other_err.len()
    );
}

// =====================================================================================
// D-123 (2): ask::answer refuses an incomplete card and writes nothing
// =====================================================================================

#[test]
fn d123_2_answer_refuses_an_incomplete_card_and_writes_nothing() {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
    let asks = ask::asks_dir(&run.dir);
    std::fs::create_dir_all(&asks).unwrap();

    // No card at all.
    let err = ask::answer(&root.0, &run.id, 1, true).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");

    let bad: [&[u8]; 9] = [
        b"",
        b"{",
        b"{\"n\": 1, \"what\": \"send to",
        b"[]",
        b"\"card\"",
        b"null",
        b"42",
        b"{\"n\": 1} {\"n\": 2}",
        b"\xff\xfe{}",
    ];
    for (i, content) in bad.iter().enumerate() {
        std::fs::write(ask::card_path(&run.dir, 1), content).unwrap();
        for yes in [true, false] {
            let err = ask::answer(&root.0, &run.id, 1, yes).unwrap_err();
            assert_eq!(
                err.kind(),
                std::io::ErrorKind::InvalidData,
                "case {i}: {err}"
            );
        }
        assert_eq!(
            asks_entries(&run.dir),
            vec!["1.json".to_string()],
            "case {i}: nothing written"
        );
    }

    // A complete card is answered once.
    std::fs::write(
        ask::card_path(&run.dir, 1),
        serde_json::to_vec_pretty(&json!({"n": 1, "what": "send"})).unwrap(),
    )
    .unwrap();
    ask::answer(&root.0, &run.id, 1, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(ask::answer_path(&run.dir, 1))
            .unwrap()
            .trim(),
        "no"
    );
    let err = ask::answer(&root.0, &run.id, 1, true).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
}

// =====================================================================================
// D-123 (3): the asker closes <n>.answer (as timeout) on every exit path
// =====================================================================================

/// The card's own timer: `timeout` is claimed and a late answer is refused.
#[tokio::test(flavor = "multi_thread")]
async fn d123_3_timer_closes_the_ask_and_a_late_answer_is_refused() {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
    let fa = FileAsker::new(&root.0, run.id.clone(), vault());
    let k = card(
        Class::Send,
        None,
        Duration::from_millis(200),
        OnTimeout::Proceed,
    );
    let o = tokio::time::timeout(CAP, fa.ask(&k)).await.unwrap();
    assert_eq!(o, AskOutcome::TimeoutProceed);
    let err = ask::answer(&root.0, &run.id, 1, false).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(err.to_string().contains("timed out"), "{err}");
    assert_eq!(
        std::fs::read_to_string(ask::answer_path(&run.dir, 1))
            .unwrap()
            .trim(),
        "timeout"
    );
}

/// A dropped ask future (what the wrapper's cap or a cancelled call does) whose card is
/// already on disk: `<n>.answer` is claimed before the drop returns, so a `harness ask` right
/// after it is refused. Tallies: closed inline; still open right after the drop and an
/// answer then accepted (nobody waits for it); still open but closed a moment later.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "red-first T-07 round 5: a card is on disk before its OpenAsk reaches the future (the guard rides the oneshot only after write_card returns, which includes claim()'s remove_file of the temp file), so a future dropped in that window does not close the ask inline; it stays open until the blocking thread's send fails, and a harness ask in that window is accepted with nobody waiting"]
async fn d123_3_a_dropped_ask_is_closed_before_the_drop_returns() {
    let (mut inline, mut taken, mut later) = (0, Vec::new(), Vec::new());
    for i in 0..60u64 {
        let root = Tmp::new();
        let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
        let fa = FileAsker::new(&root.0, run.id.clone(), vault());
        let k = card(
            Class::Send,
            None,
            Duration::from_secs(60),
            OnTimeout::Proceed,
        );
        let cp = ask::card_path(&run.dir, 1);
        // Drop the ask once its card exists, plus 0..19 ms.
        let extra = Duration::from_millis(i % 20);
        let dropped = tokio::select! {
            _ = fa.ask(&k) => false,
            _ = async { wait_file(cp).await; tokio::time::sleep(extra).await } => true,
        };
        assert!(dropped);
        // No wait: the drop itself should have closed the ask.
        let ap = ask::answer_path(&run.dir, 1);
        if ap.is_file() {
            inline += 1;
            assert_eq!(std::fs::read_to_string(&ap).unwrap().trim(), "timeout");
            let err = ask::answer(&root.0, &run.id, 1, true).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{i}: {err}");
            continue;
        }
        match ask::answer(&root.0, &run.id, 1, true) {
            Ok(()) => taken.push((i, extra)),
            Err(_) => later.push((i, extra)),
        }
    }
    eprintln!(
        "d123_3 dropped ask: closed inline {inline}; open after the drop and an answer accepted {}: {taken:?}; open after the drop, closed a moment later {}: {later:?}",
        taken.len(),
        later.len()
    );
    assert!(
        taken.is_empty() && later.is_empty(),
        "{} of 60 dropped asks were still open when the drop returned",
        taken.len() + later.len()
    );
}

/// A card that lands after the ask future was dropped (the drop came while the card was
/// being written) is closed too: after the runtime has finished its blocking work, every
/// card on disk has `<n>.answer` = timeout and refuses an answer.
#[test]
fn d123_3_a_card_landing_after_the_drop_is_closed() {
    let (mut landed_after, mut before, mut none) = (0, 0, 0);
    for i in 0..60u64 {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let root = Tmp::new();
        let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
        let fa = FileAsker::new(&root.0, run.id.clone(), vault());
        let k = AskCard {
            values: (0..20_000)
                .map(|j| (format!("k{j}"), "v".repeat(64)))
                .collect(),
            ..card(
                Class::Send,
                None,
                Duration::from_secs(60),
                OnTimeout::Proceed,
            )
        };
        let cp = ask::card_path(&run.dir, 1);
        let delay = Duration::from_micros((i % 20) * 400);
        let r = rt.block_on(async { tokio::time::timeout(delay, fa.ask(&k)).await });
        assert!(r.is_err(), "the ask was dropped");
        let existed_at_drop = cp.is_file();
        // Dropping the runtime waits for the card-writing thread.
        drop(rt);
        if !cp.is_file() {
            none += 1;
            assert!(
                asks_entries(&run.dir).iter().all(|n| !n.starts_with('.')),
                "temp file left"
            );
            continue;
        }
        if existed_at_drop {
            before += 1;
        } else {
            landed_after += 1;
        }
        let ap = ask::answer_path(&run.dir, 1);
        assert!(ap.is_file(), "iteration {i}: published card left open");
        assert_eq!(std::fs::read_to_string(&ap).unwrap().trim(), "timeout");
        let err = ask::answer(&root.0, &run.id, 1, true).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{i}: {err}");
    }
    eprintln!(
        "d123_3 late card: landed after the drop {landed_after}, before {before}, never written {none}"
    );
    assert!(
        landed_after > 0,
        "the late-card path was never hit; widen the card or the delays"
    );
}

/// The gate's ask inside dispatch, with FileAsker as the harness asker: dropping dispatch
/// (a cancelled call) closes the ask, the handler never runs, a late answer is refused.
#[tokio::test(flavor = "multi_thread")]
async fn d123_3_a_dropped_dispatch_closes_its_gate_ask() {
    let e = file_asker_env().await;
    let (spec, p) = tool(
        "echo.send",
        Class::Send,
        "send",
        Beh {
            classify: Classified {
                recipients: vec![STRANGER.into()],
                domain: Some("mail.example".into()),
                ..Classified::default()
            },
            ..Beh::default()
        },
    );
    let reg = ToolRegistry::new(vec![spec]);
    let cp = ask::card_path(&e.run.dir, 1);
    tokio::select! {
        _ = dispatch(SERVER, &reg, &e.harness, "echo.send", json!({"to": STRANGER})) => {
            panic!("dispatch returned before the card was answered")
        }
        _ = wait_file(cp) => {}
    }
    let ap = ask::answer_path(&e.run.dir, 1);
    assert!(
        ap.is_file(),
        "gate ask still open after dispatch was dropped"
    );
    assert_eq!(std::fs::read_to_string(&ap).unwrap().trim(), "timeout");
    let err = ask::answer(&e.harness.runs_root, &e.run.id, 1, true).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
    assert_eq!(p.calls.load(Ordering::SeqCst), 0, "the handler never ran");
}

/// A mid-call ask dropped by its handler (the handler gave up waiting) is closed before the
/// drop returns.
#[tokio::test(flavor = "multi_thread")]
async fn d123_3_a_dropped_mid_call_ask_is_closed() {
    let e = file_asker_env().await;
    let (spec, p) = tool(
        "act",
        Class::Navigate,
        "act",
        Beh {
            mid_asks: vec![AskCard {
                site: Some("y.example".into()),
                ..site_less(Class::Account, "change the email")
            }],
            drop_mid_on_card: true,
            ..Beh::default()
        },
    );
    let reg = ToolRegistry::new(vec![spec]);
    call(&e, &reg, "act", json!({})).await;
    assert_eq!(*p.mids.lock().unwrap(), vec![None], "the ask was dropped");
    assert_eq!(*p.closed_on_drop.lock().unwrap(), vec![true]);
    let err = ask::answer(&e.harness.runs_root, &e.run.id, 1, true).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
}

const BARE_CHILD: &str = "SB_ACCEPT_T07R3_BARE_CHILD";

/// Child of `d123_3_close_falls_back_to_a_bare_create_new`: in a fresh process (so the temp
/// name sequence starts at 0) every temp name the answer claim can use is taken by a
/// folder, so the temp-file claim fails for an I/O reason and the bare create_new closes it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "child of d123_3_close_falls_back_to_a_bare_create_new"]
async fn d123_3_bare_child() {
    let Some(root) = std::env::var_os(BARE_CHILD).map(PathBuf::from) else {
        return;
    };
    let run = RunId(std::fs::read_to_string(root.join("run-id")).unwrap());
    let run_dir = root.join(&run.0);
    let asks = ask::asks_dir(&run_dir);
    std::fs::create_dir_all(&asks).unwrap();
    let pid = std::process::id();
    for seq in 0..256 {
        std::fs::create_dir(asks.join(format!(".1.answer.{pid}.{seq}.tmp"))).unwrap();
    }
    let fa = FileAsker::new(&root, run, vault());
    let k = card(
        Class::Send,
        None,
        Duration::from_millis(300),
        OnTimeout::Proceed,
    );
    let o = tokio::time::timeout(CAP, fa.ask(&k)).await.unwrap();
    let answer = std::fs::read(ask::answer_path(&run_dir, 1)).unwrap();
    assert!(answer.is_empty(), "the bare create_new path was not taken");
    assert_eq!(
        o,
        AskOutcome::TimeoutProceed,
        "a closed ask times out per its card"
    );
    println!("BARE_CHILD_OK");
}

#[test]
fn d123_3_close_falls_back_to_a_bare_create_new() {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
    std::fs::write(root.0.join("run-id"), &run.id.0).unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "d123_3_bare_child", "--ignored", "--nocapture"])
        .env(BARE_CHILD, &root.0)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("BARE_CHILD_OK"),
        "child failed: {stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The card is whole, and a late answer from another process is refused.
    let v: Value =
        serde_json::from_slice(&std::fs::read(ask::card_path(&run.dir, 1)).unwrap()).unwrap();
    assert_eq!(v["n"], json!(1));
    let err = ask::answer(&root.0, &run.id, 1, true).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
    assert!(err.to_string().contains("timed out"), "{err}");
}

/// Neither the temp-file claim nor the bare create_new can close the ask (writes into
/// `asks/` are denied after the card appeared): the outcome is TimeoutSkip even though the
/// card says Proceed.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn d123_3_an_unclosable_ask_is_timeout_skip_never_proceed() {
    let root = Tmp::new();
    let run = RunFolder::create(&root.0, "ask", "ask").unwrap();
    let asks = ask::asks_dir(&run.dir);
    let fa = FileAsker::new(&root.0, run.id.clone(), vault());
    let k = card(
        Class::Send,
        None,
        Duration::from_secs(3),
        OnTimeout::Proceed,
    );
    let cp = ask::card_path(&run.dir, 1);
    let a2 = asks.clone();
    let denier = std::thread::spawn(move || {
        wait_file_blocking(&cp);
        let ok = deny_writes(&a2);
        let probe = std::fs::write(a2.join("probe"), "x").is_err();
        (ok && probe, Instant::now())
    });
    let started = Instant::now();
    let o = tokio::time::timeout(CAP, fa.ask(&k)).await.unwrap();
    let (denied, at) = denier.join().unwrap();
    allow_writes(&asks);
    assert!(denied, "precondition: writes into asks/ not denied");
    assert!(
        at.duration_since(started) < Duration::from_secs(3),
        "precondition: the deny landed after the timer"
    );
    assert!(!ask::answer_path(&run.dir, 1).exists());
    assert_eq!(o, AskOutcome::TimeoutSkip, "an open ask never proceeds");
}

// =====================================================================================
// D-123 (4): a site-less mid-call card is checked against the call's domain
// =====================================================================================

#[tokio::test]
async fn d123_4_site_less_card_uses_the_call_domain() {
    // (a) stop rule on the call's domain: no card, one core stop, TimeoutSkip.
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone()).await;
    e.store
        .add_rule("stop.example", "Account", Mode::Stop, false)
        .unwrap();
    let b = Beh {
        classify: Classified {
            domain: Some("stop.example".into()),
            ..Classified::default()
        },
        mid_asks: vec![site_less(Class::Account, "change the email")],
        ..Beh::default()
    };
    let (spec, p) = tool("act", Class::Navigate, "act", b.clone());
    let reg = ToolRegistry::new(vec![spec]);
    call(&e, &reg, "act", json!({"element": "#4"})).await;
    assert_eq!(*p.mids.lock().unwrap(), vec![Some(AskOutcome::TimeoutSkip)]);
    assert!(asker.cards().is_empty(), "a stop rule shows no card");
    let ev = events(&e);
    assert_eq!(ev.len(), 2, "{ev:#?}");
    assert_eq!(
        (ev[0].server.as_str(), ev[0].mode, ev[0].class),
        ("core", Some(LedgerMode::Stop), Some(Class::Account))
    );
    assert!(
        ev[0].why.as_deref().unwrap_or("").contains("stop.example"),
        "{:?}",
        ev[0].why
    );
    assert_eq!(ev[1].server, SERVER);
    let (n, nd) = report_stop_listings(&e, &ev);
    assert_eq!(n, 1, "{nd}");

    // (b) the same tool, but the card names its own site: the call's rule does not apply.
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone()).await;
    e.store
        .add_rule("stop.example", "Account", Mode::Stop, false)
        .unwrap();
    let mut b2 = b.clone();
    b2.mid_asks[0].site = Some("other.example".into());
    let (spec, p) = tool("act", Class::Navigate, "act", b2);
    call(&e, &ToolRegistry::new(vec![spec]), "act", json!({})).await;
    assert_eq!(
        *p.mids.lock().unwrap(),
        vec![Some(AskOutcome::TimeoutProceed)]
    );
    assert_eq!(asker.cards().len(), 1);
    assert_eq!(asker.cards()[0].site.as_deref(), Some("other.example"));

    // (c) strict rule on the call's domain: shown, on_timeout Skip.
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone()).await;
    e.store
        .add_rule("stop.example", "Account", Mode::Ask, true)
        .unwrap();
    let (spec, p) = tool("act", Class::Navigate, "act", b.clone());
    call(&e, &ToolRegistry::new(vec![spec]), "act", json!({})).await;
    assert_eq!(asker.cards().len(), 1);
    assert_eq!(asker.cards()[0].on_timeout, OnTimeout::Skip);
    assert_eq!(*p.mids.lock().unwrap(), vec![Some(AskOutcome::TimeoutSkip)]);

    // (d) a call domain carrying a vault value is scrubbed before the gate and the ledger.
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone()).await;
    let mut b4 = b.clone();
    b4.classify.domain = Some(format!("{}.example", canary()));
    let (spec, _p) = tool("act", Class::Navigate, "act", b4);
    call(&e, &ToolRegistry::new(vec![spec]), "act", json!({})).await;
    let c = canary();
    let ev = events(&e);
    let dump = serde_json::to_string(&ev).unwrap();
    assert!(
        !dump.contains(&c),
        "the ledger carries the call domain's value"
    );
    let cards = format!("{:?}", asker.cards());
    assert!(
        !cards.contains(&c),
        "a card carries the call domain's value"
    );
}

// =====================================================================================
// D-127 (1): re-registration never lowers a class
// =====================================================================================

#[tokio::test]
async fn d127_1_re_registration_never_lowers_a_class() {
    let asker = Arc::new(ScriptedAsker::new());
    let e = env(asker.clone()).await;
    let (pay, pay_p) = tool("shop.buy", Class::Pay, "pay v1", Beh::default());
    let reg = ToolRegistry::new(vec![pay]);

    // Re-added as Read: still Pay, with the new handler and description.
    let (read1, read1_p) = tool("shop.buy", Class::Read, "read v2", Beh::default());
    reg.add(read1);
    let got = reg.get("shop.buy").unwrap();
    assert_eq!(got.class, Class::Pay);
    assert_eq!(got.description, "read v2");
    assert_eq!(reg.list().len(), 1);
    assert_eq!(reg.list()[0].class, Class::Pay);
    let out = call(&e, &reg, "shop.buy", json!({})).await;
    assert!(out.text.starts_with("skipped: "), "{}", out.text);
    assert_eq!(read1_p.calls.load(Ordering::SeqCst), 0);
    assert_eq!(pay_p.calls.load(Ordering::SeqCst), 0);
    assert_eq!(asker.cards().last().unwrap().class, Class::Pay);

    // Removed, then added back as Read (through a clone): still Pay.
    let clone = reg.clone();
    clone.remove("shop.buy");
    assert!(reg.get("shop.buy").is_none());
    let (read2, read2_p) = tool("shop.buy", Class::Read, "read v3", Beh::default());
    reg.add(read2);
    assert_eq!(clone.get("shop.buy").unwrap().class, Class::Pay);
    let out = call(&e, &clone, "shop.buy", json!({})).await;
    assert!(out.text.starts_with("skipped: "), "{}", out.text);
    assert_eq!(read2_p.calls.load(Ordering::SeqCst), 0);

    // Raising works and the raised class becomes the floor.
    let (r, _) = tool("fs.read", Class::Read, "r", Beh::default());
    reg.add(r);
    let (s, _) = tool("fs.read", Class::Submit, "s", Beh::default());
    reg.add(s);
    assert_eq!(reg.get("fs.read").unwrap().class, Class::Submit);
    let (r, _) = tool("fs.read", Class::Read, "r again", Beh::default());
    reg.add(r);
    assert_eq!(reg.get("fs.read").unwrap().class, Class::Submit);

    // A name listed twice in new: both at the higher class.
    let (a, _) = tool("dup", Class::Read, "a", Beh::default());
    let (b, _) = tool("dup", Class::Pay, "b", Beh::default());
    let reg2 = ToolRegistry::new(vec![a, b]);
    assert!(reg2.list().iter().all(|s| s.class == Class::Pay));
    assert_eq!(reg2.get("dup").unwrap().class, Class::Pay);

    // Ledger: every gated call was asked at Pay.
    let ev = events(&e);
    assert!(ev
        .iter()
        .all(|x| x.class == Some(Class::Pay) && x.mode == Some(LedgerMode::TimeoutSkip)));
    assert_eq!(ev.len(), 2, "{ev:#?}");
}

// =====================================================================================
// D-127 (2): one core stop per stopped call; one listing in report.md
// =====================================================================================

struct Case {
    name: &'static str,
    b: Beh,
    /// (server, mode) of the expected events, in order (mode None reads as Allow).
    shape: Vec<(&'static str, LedgerMode)>,
    /// Text the `why` of the one core stop contains.
    core_why: &'static str,
    /// `consequences.element` of the core stop.
    element: Option<&'static str>,
}

#[tokio::test]
async fn d127_2_one_core_stop_per_stopped_call_and_one_listing() {
    let ch = || challenge("hCaptcha on the form");
    let gate_stop = || site_less(Class::Account, "change the email");
    let stop_domain = Classified {
        domain: Some("stop.example".into()),
        ..Classified::default()
    };
    let cases = vec![
        Case {
            name: "challenge mid-call, handler stopped",
            b: Beh {
                mid_asks: vec![ch()],
                stop: stop("hCaptcha on lu.ma", Some("#9")),
                ..Beh::default()
            },
            shape: vec![(SERVER, LedgerMode::Stop), ("core", LedgerMode::Stop)],
            core_why: "challenge during act: hCaptcha on the form",
            element: Some("#9"),
        },
        Case {
            name: "gate stop mid-call, handler stopped",
            b: Beh {
                classify: stop_domain.clone(),
                mid_asks: vec![gate_stop()],
                stop: stop("blocked", None),
                ..Beh::default()
            },
            shape: vec![(SERVER, LedgerMode::Stop), ("core", LedgerMode::Stop)],
            core_why: "stop.example",
            element: None,
        },
        Case {
            name: "two mid-call stops, handler stopped",
            b: Beh {
                classify: stop_domain.clone(),
                mid_asks: vec![ch(), gate_stop()],
                stop: stop("blocked twice", Some("#2")),
                ..Beh::default()
            },
            shape: vec![(SERVER, LedgerMode::Stop), ("core", LedgerMode::Stop)],
            core_why: "challenge during act: hCaptcha on the form",
            element: Some("#2"),
        },
        Case {
            name: "challenge mid-call, handler went on",
            b: Beh {
                mid_asks: vec![ch()],
                ..Beh::default()
            },
            shape: vec![("core", LedgerMode::Stop), (SERVER, LedgerMode::Allow)],
            core_why: "challenge during act: hCaptcha on the form",
            element: None,
        },
        Case {
            name: "challenge mid-call, handler failed",
            b: Beh {
                mid_asks: vec![ch()],
                fail: true,
                ..Beh::default()
            },
            shape: vec![("core", LedgerMode::Stop), (SERVER, LedgerMode::Allow)],
            core_why: "challenge during act: hCaptcha on the form",
            element: None,
        },
        Case {
            name: "challenge mid-call, handler panicked",
            b: Beh {
                mid_asks: vec![ch()],
                panic: true,
                ..Beh::default()
            },
            shape: vec![("core", LedgerMode::Stop), (SERVER, LedgerMode::Allow)],
            core_why: "challenge during act: hCaptcha on the form",
            element: None,
        },
        Case {
            name: "handler stopped only",
            b: Beh {
                stop: stop("Turnstile on lu.ma", Some("#31")),
                ..Beh::default()
            },
            shape: vec![(SERVER, LedgerMode::Stop), ("core", LedgerMode::Stop)],
            core_why: "Turnstile on lu.ma",
            element: Some("#31"),
        },
    ];
    for c in cases {
        let e = env(Arc::new(ScriptedAsker::new())).await;
        e.store
            .add_rule("stop.example", "Account", Mode::Stop, false)
            .unwrap();
        let (spec, _p) = tool("act", Class::Navigate, "act", c.b.clone());
        let reg = ToolRegistry::new(vec![spec]);
        let r = tokio::time::timeout(CAP, dispatch(SERVER, &reg, &e.harness, "act", json!({})))
            .await
            .unwrap();
        assert_eq!(r.is_err(), c.b.fail || c.b.panic, "{}", c.name);
        let ev = events(&e);
        let shape: Vec<_> = ev
            .iter()
            .map(|x| (x.server.as_str(), x.mode.unwrap_or(LedgerMode::Allow)))
            .collect();
        assert_eq!(shape, c.shape, "{}: {ev:#?}", c.name);
        assert_eq!(core_stops(&ev), 1, "{}", c.name);
        let core = ev.iter().find(|x| x.server == "core").unwrap();
        assert!(
            core.why.as_deref().unwrap_or("").contains(c.core_why),
            "{}: {:?}",
            c.name,
            core.why
        );
        assert_eq!(
            core.consequences["element"].as_str(),
            c.element,
            "{}",
            c.name
        );
        let (n, nd) = report_stop_listings(&e, &ev);
        assert_eq!(n, 1, "{}: {nd}", c.name);
    }

    // A gate stop before the call: one core stop, the handler never runs, one listing.
    let e = env(Arc::new(ScriptedAsker::new())).await;
    e.store
        .add_rule("stop.example", "Submit", Mode::Stop, false)
        .unwrap();
    let (spec, p) = tool(
        "act",
        Class::Submit,
        "act",
        Beh {
            classify: stop_domain.clone(),
            ..Beh::default()
        },
    );
    let out = call(&e, &ToolRegistry::new(vec![spec]), "act", json!({})).await;
    assert!(out.stopped.is_some());
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    let ev = events(&e);
    assert_eq!(ev.len(), 1, "{ev:#?}");
    assert_eq!(core_stops(&ev), 1);
    assert_eq!(report_stop_listings(&e, &ev).0, 1);
}

/// A resumed run stopped again on the same tool and args lists two stops; two calls stopped
/// at once (different args) list two stops, each with one core stop.
#[tokio::test(flavor = "multi_thread")]
async fn d127_2_resumed_and_concurrent_stopped_calls_list_once_each() {
    let b = Beh {
        mid_asks: vec![challenge("Turnstile appeared")],
        stop: stop("Turnstile on lu.ma", Some("#31")),
        ..Beh::default()
    };
    // Resume: the same call stopped twice.
    let e = env(Arc::new(ScriptedAsker::new())).await;
    let (spec, _) = tool("act", Class::Navigate, "act", b.clone());
    let reg = ToolRegistry::new(vec![spec]);
    for _ in 0..2 {
        call(&e, &reg, "act", json!({"element": "#31"})).await;
    }
    let ev = events(&e);
    assert_eq!(ev.len(), 4, "{ev:#?}");
    assert_eq!(core_stops(&ev), 2);
    let (n, nd) = report_stop_listings(&e, &ev);
    assert_eq!(n, 2, "{nd}");

    // Concurrent: two calls at once.
    let e = env(Arc::new(ScriptedAsker::new())).await;
    let (spec, _) = tool("act", Class::Navigate, "act", b.clone());
    let reg = ToolRegistry::new(vec![spec]);
    let (a, c) = tokio::join!(
        dispatch(SERVER, &reg, &e.harness, "act", json!({"element": "#1"})),
        dispatch(SERVER, &reg, &e.harness, "act", json!({"element": "#2"})),
    );
    assert!(a.unwrap().stopped.is_some() && c.unwrap().stopped.is_some());
    let ev = events(&e);
    assert_eq!(ev.len(), 4, "{ev:#?}");
    assert_eq!(core_stops(&ev), 2);
    let (n, nd) = report_stop_listings(&e, &ev);
    assert_eq!(n, 2, "{nd}");
}

// =====================================================================================
// D-127 (3): an over-long JSON-RPC line gets -32600 with id null; the next line is served
// =====================================================================================

/// Runs serve_io over a duplex pipe, writing `chunks` in 256 KiB pieces with a yield between
/// them (so a long line arrives in many reads), then closing input. Returns every reply.
async fn serve_chunks(reg: ToolRegistry, harness: Harness, chunks: Vec<Vec<u8>>) -> Vec<Value> {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (sr, sw) = tokio::io::split(server);
    let (cr, mut cw) = tokio::io::split(client);
    let srv = tokio::spawn(async move {
        serve_io(SERVER, reg, harness, BufReader::new(sr), sw)
            .await
            .unwrap()
    });
    let writer = tokio::spawn(async move {
        for c in chunks {
            for part in c.chunks(256 * 1024) {
                cw.write_all(part).await.unwrap();
                tokio::task::yield_now().await;
            }
        }
        cw.shutdown().await.unwrap();
    });
    let mut lines = BufReader::new(cr).lines();
    let mut out = Vec::new();
    while let Some(l) = tokio::time::timeout(CAP, lines.next_line())
        .await
        .expect("reply within the cap")
        .unwrap()
    {
        out.push(serde_json::from_str(&l).unwrap());
    }
    writer.await.unwrap();
    tokio::time::timeout(CAP, srv).await.unwrap().unwrap();
    out
}

/// A request line of exactly `len` bytes (no newline): a tools/call with a padded argument
/// that also carries the vault value.
fn call_line(id: u64, len: usize) -> Vec<u8> {
    let head = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{{\"name\":\"probe\",\"arguments\":{{\"secret\":\"{}\",\"pad\":\"",
        canary()
    );
    let tail = "\"}}}";
    let mut v = head.into_bytes();
    v.resize(len - tail.len(), b'a');
    v.extend_from_slice(tail.as_bytes());
    assert_eq!(v.len(), len);
    v
}

fn ping(id: &str) -> Vec<u8> {
    format!("{{\"jsonrpc\":\"2.0\",\"id\":\"{id}\",\"method\":\"ping\"}}\n").into_bytes()
}

#[tokio::test(flavor = "multi_thread")]
async fn d127_3_over_long_line_is_32600_id_null_and_the_next_line_is_served() {
    let e = env(Arc::new(ScriptedAsker::new())).await;
    let (spec, p) = tool("probe", Class::Read, "probe", Beh::default());
    let reg = ToolRegistry::new(vec![spec]);

    // (a) A valid tools/call one byte over the cap, then a ping.
    let mut long = call_line(7, MAX_LINE + 1);
    long.push(b'\n');
    let out = serve_chunks(reg.clone(), e.harness.clone(), vec![long, ping("after")]).await;
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(out[0]["error"]["code"], json!(-32600));
    assert_eq!(out[0]["id"], Value::Null);
    assert_eq!(
        out[1],
        json!({"jsonrpc": "2.0", "id": "after", "result": {}})
    );
    assert_eq!(p.calls.load(Ordering::SeqCst), 0, "the long call never ran");
    assert!(e.store.ledger().is_empty(), "nothing was logged for it");
    let dump = serde_json::to_string(&out).unwrap();
    assert!(!dump.contains(&canary()));

    // (b) Exactly at the cap: served (the tool runs); two over-long lines in a row, then a
    // ping: two -32600, then the ping.
    let mut at_cap = call_line(8, MAX_LINE);
    at_cap.push(b'\n');
    let mut over1 = call_line(9, MAX_LINE + 10);
    over1.push(b'\n');
    let mut over2 = vec![b'x'; 3 * MAX_LINE];
    over2.push(b'\n');
    let out = serve_chunks(
        reg.clone(),
        e.harness.clone(),
        vec![at_cap, over1, over2, ping("p2")],
    )
    .await;
    assert_eq!(out.len(), 4, "{out:?}");
    let at_cap_reply = out
        .iter()
        .find(|m| m["id"] == json!(8))
        .expect("id 8 answered");
    assert_eq!(
        at_cap_reply["result"]["isError"],
        json!(false),
        "{at_cap_reply}"
    );
    let nulls: Vec<_> = out
        .iter()
        .filter(|m| m["id"].is_null() && m["error"]["code"] == json!(-32600))
        .collect();
    assert_eq!(nulls.len(), 2, "{out:?}");
    assert!(out
        .iter()
        .any(|m| m["id"] == json!("p2") && m["result"] == json!({})));
    assert!(
        !out.iter().any(|m| m["id"] == json!(9)),
        "id 9 was never parsed"
    );
    assert_eq!(
        p.calls.load(Ordering::SeqCst),
        1,
        "only the at-cap call ran"
    );

    // (c) An over-long last line with no newline before EOF is answered too.
    let out = serve_chunks(
        reg.clone(),
        e.harness.clone(),
        vec![ping("first"), vec![b'{'; MAX_LINE + 1]],
    )
    .await;
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(out[0]["id"], json!("first"));
    assert_eq!(out[1]["error"]["code"], json!(-32600));
    assert_eq!(out[1]["id"], Value::Null);
}
