//! T-16 package A acceptance (docs/09 step 4): watch schedules and the run preamble.
//! docs/01 H-5 (watch, scheduler), docs/02 M-1(5), M-3 baselines and acceptance bullet 5
//! ("02-acc-5": an unchanged watch target sends nothing, a changed one exactly one
//! notification), ladder rung 7 (a weekday cron fires a fresh run with its deliverable in
//! files/) and rung 8 (watch), B-563 (run preamble); D-140 defaults 1-5 and 10.
//!
//! No model is called (rule 1): an in-process launcher plays the session and writes
//! `files/watch.txt`. Time is a `ManualClock`; every wait is on a condition with a cap
//! (rule 6). A vault canary (`SB_VAULT_CANARY`, else a generated one) is installed as the
//! global scrubber; no assertion message formats it (rule 2).
//!
//! `SB_VAULT_CANARY=sbcanary-x-<hex> cargo test -p harness-core --test accept_t16_watch`

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use harness_core::ledger::{Ledger, CORE_SERVER};
use harness_core::memory::{
    ts_now, LedgerEvent, LedgerMode, MemoryStore, Run, RunStatus, Schedule, ScheduleKind, Ts,
};
use harness_core::run::{RunFolder, RunId};
use harness_core::sched::{
    run_preamble, Fired, LaunchError, LaunchExit, LaunchRequest, Launcher, ManualClock, Runner,
    Scheduler, FIRE_TOOL,
};
use harness_core::testing::MemStore;
use harness_core::vault::{EntryKind, Vault};
use harness_core::watch::{
    normalise, state_hash, CHECK_TOOL, NOTIFICATIONS_LOG, NOTIFICATION_FILE, NOTIFY_TOOL,
    WATCH_FILE,
};
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::time::timeout;

const CAP: Duration = Duration::from_secs(20);

fn canary() -> &'static str {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| {
        std::env::var("SB_VAULT_CANARY")
            .ok()
            .filter(|c| c.chars().count() >= 8)
            .unwrap_or_else(|| {
                format!(
                    "sbcanary-t16-{}-{}",
                    std::process::id(),
                    Utc::now().timestamp_micros()
                )
            })
    })
}

fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        Vault::in_memory([("site".to_owned(), canary().to_owned(), EntryKind::Secret)])
            .install_global()
    });
}

struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str) -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "sb-t16-{name}-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            Utc::now().timestamp_micros()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn runs(&self) -> PathBuf {
        self.0.join("runs")
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Plays the session: writes `files/watch.txt` (when `state` is set) and any `extra`
/// deliverables, records what it was asked, and exits with `code`. Never calls a model.
struct Session {
    state: Mutex<Option<String>>,
    extra: Mutex<Vec<(String, String)>>,
    code: Mutex<Option<i32>>,
    /// When set, the session's tool hits a gate stop (written to this store's ledger).
    stop_in: Mutex<Option<Arc<dyn MemoryStore>>>,
    seen: Mutex<Vec<Seen>>,
}

/// One launch as the session saw it.
#[derive(Clone)]
struct Seen {
    run: RunId,
    prompt: String,
    kind: Option<ScheduleKind>,
    preamble: String,
}

impl Session {
    fn new() -> Arc<Session> {
        Arc::new(Session {
            state: Mutex::new(None),
            extra: Mutex::new(vec![]),
            code: Mutex::new(Some(0)),
            stop_in: Mutex::new(None),
            seen: Mutex::new(vec![]),
        })
    }
    fn set_state(&self, s: Option<&str>) {
        *self.state.lock().unwrap() = s.map(str::to_owned);
    }
}

#[async_trait]
impl Launcher for Session {
    async fn launch(&self, req: LaunchRequest<'_>) -> Result<LaunchExit, LaunchError> {
        let files = req.run.files();
        if let Some(s) = self.state.lock().unwrap().clone() {
            std::fs::write(files.join(WATCH_FILE), s).unwrap();
        }
        let stop_in = self.stop_in.lock().unwrap().clone();
        if let Some(store) = stop_in {
            let stop = LedgerEvent {
                run_id: Some(req.run.id.clone()),
                ts: ts_now(),
                server: "web".into(),
                tool: "page.open".into(),
                class: None,
                mode: Some(LedgerMode::Stop),
                args: serde_json::json!({ "url": "https://jobs.example/" }),
                result_summary: "challenge".into(),
                consequences: serde_json::json!({}),
                duration_ms: 1,
                ok: false,
                why: Some("challenge: verify you are human".into()),
            };
            Ledger::new(store, req.run.clone())
                .write(stop)
                .await
                .unwrap();
        }
        for (name, body) in self.extra.lock().unwrap().iter() {
            std::fs::write(files.join(name), body).unwrap();
        }
        self.seen.lock().unwrap().push(Seen {
            run: req.run.id.clone(),
            prompt: req.prompt.to_owned(),
            kind: req.schedule.map(|s| s.kind),
            preamble: run_preamble(&req),
        });
        Ok(LaunchExit {
            code: *self.code.lock().unwrap(),
        })
    }
}

/// 2026-10-23 is a Friday.
fn t(d: u32, h: u32, m: u32) -> Ts {
    Utc.with_ymd_and_hms(2026, 10, d, h, m, 0).unwrap()
}

async fn parent_run(store: &MemStore, root: &Path) -> RunId {
    let f = RunFolder::create_at(root, "schedule-add", "schedule add", t(23, 11, 0)).unwrap();
    store
        .run_start(Run {
            id: f.id.clone(),
            started: t(23, 11, 0),
            ended: Some(t(23, 11, 0)),
            prompt: "schedule add".into(),
            status: RunStatus::Done,
            folder: f.dir.to_string_lossy().into_owned(),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    f.id
}

fn row(kind: ScheduleKind, cron: &str, prompt: &str, parent: Option<&RunId>) -> Schedule {
    Schedule {
        id: 0,
        cron: cron.into(),
        prompt: prompt.into(),
        kind,
        watch_target: (kind == ScheduleKind::Watch).then(|| "jobs".to_owned()),
        enabled: true,
        last_run: None,
        run_id: parent.cloned(),
    }
}

async fn add(store: &MemStore, mut s: Schedule) -> Schedule {
    s.id = store.schedule_add(s.clone()).await.unwrap();
    s
}

struct Loop {
    clock: Arc<ManualClock>,
    fired: mpsc::UnboundedReceiver<Fired>,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Loop {
    fn start(
        store: Arc<MemStore>,
        session: Arc<Session>,
        clock: Arc<ManualClock>,
        root: &Path,
    ) -> Loop {
        let (tx, fired) = mpsc::unbounded_channel();
        let wake = Arc::new(Notify::new());
        let s = Scheduler {
            runner: Runner {
                store,
                launcher: session,
                clock: clock.clone(),
                runs_root: root.to_path_buf(),
            },
            wake,
            fired: Some(tx),
        };
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            s.run_loop(async move {
                let _ = stopped.await;
            })
            .await
            .unwrap();
        });
        Loop {
            clock,
            fired,
            stop: Some(stop),
            task,
        }
    }

    /// Waits for the loop to sleep on its next deadline, checks it, moves the clock there and
    /// returns the firing that results.
    async fn fire_at(&mut self, want: Ts) -> Fired {
        let due = timeout(CAP, self.clock.deadline_where(|d| d > self.clock_now()))
            .await
            .expect("the loop sleeps on a deadline");
        assert_eq!(due, want);
        self.clock.set(due);
        timeout(CAP, self.fired.recv())
            .await
            .expect("a firing ended within the cap")
            .expect("fired channel open")
    }

    fn clock_now(&self) -> Ts {
        use harness_core::sched::Clock;
        self.clock.now()
    }

    async fn finish(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        timeout(CAP, self.task).await.unwrap().unwrap();
    }
}

async fn events(store: &MemStore, run: &RunId) -> Vec<LedgerEvent> {
    store
        .ledger_events(run)
        .await
        .unwrap()
        .into_iter()
        .map(|(_, e)| e)
        .collect()
}

fn core_tools(ev: &[LedgerEvent]) -> Vec<(String, bool)> {
    ev.iter()
        .filter(|e| e.server == CORE_SERVER)
        .map(|e| (e.tool.clone(), e.ok))
        .collect()
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn watch_line(folder: &Path) -> String {
    read(&folder.join("report.md"))
        .lines()
        .find(|l| l.starts_with("Watch: "))
        .unwrap_or_default()
        .to_owned()
}

// ------------------------------------------------------------------ rung 8, 02-acc-5

/// harnessd fires a watch schedule hourly. First observation sets the baseline silently;
/// the same state (other whitespace) is "no change" and sends nothing; a changed state
/// sends exactly one notification (run file + one line in <runs_root>/notifications.md +
/// a watch.notify event) and stores the new baseline; the next unchanged firing sends
/// nothing again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rung8_watch_notifies_once_per_change() {
    install();
    let tmp = Tmp::new("rung8");
    let runs = tmp.runs();
    let store = Arc::new(MemStore::new());
    let parent = parent_run(&store, &runs).await;
    let w = add(
        &store,
        row(
            ScheduleKind::Watch,
            "0 * * * *",
            "check the jobs board",
            Some(&parent),
        ),
    )
    .await;
    let session = Session::new();
    let clock = Arc::new(ManualClock::new(t(23, 12, 30)));
    let mut lp = Loop::start(store.clone(), session.clone(), clock, &runs);
    let log = runs.join(NOTIFICATIONS_LOG);

    // 1. First observation: baseline set, nothing sent.
    session.set_state(Some("Jobs:\n  Rust engineer\n  Go engineer\n"));
    let f = lp.fire_at(t(23, 13, 0)).await;
    assert_eq!(f.schedule, w.id);
    let f1 = f.result.unwrap();
    assert_eq!(f1.parent_run.as_ref(), Some(&parent));
    assert_eq!(f1.status, RunStatus::Done);
    assert!(!f1.folder.join("files").join(NOTIFICATION_FILE).exists());
    assert!(!log.exists(), "first observation notified");
    assert_eq!(
        watch_line(&f1.folder),
        "Watch: jobs: first observation, baseline set, nothing sent"
    );
    let ev = events(&store, &f1.run).await;
    assert_eq!(
        core_tools(&ev),
        [(FIRE_TOOL.to_owned(), true), (CHECK_TOOL.to_owned(), true)]
    );
    let b1 = store
        .baseline_get("jobs")
        .await
        .unwrap()
        .expect("baseline set");
    assert_eq!(
        b1.hash,
        state_hash(&normalise("Jobs: Rust engineer Go engineer"))
    );
    assert_eq!(b1.updated, t(23, 13, 0));

    // 2. Same state, other whitespace: no change, nothing sent.
    session.set_state(Some("Jobs: Rust engineer\r\n\tGo engineer"));
    let f2 = lp.fire_at(t(23, 14, 0)).await.result.unwrap();
    assert_eq!(
        watch_line(&f2.folder),
        "Watch: jobs: no change, nothing sent"
    );
    assert!(!f2.folder.join("files").join(NOTIFICATION_FILE).exists());
    assert!(!log.exists(), "unchanged state notified");
    assert_eq!(store.baseline_get("jobs").await.unwrap(), Some(b1.clone()));
    let ev = events(&store, &f2.run).await;
    assert!(ev.iter().all(|e| e.tool != NOTIFY_TOOL));

    // 3. Changed: exactly one notification, new baseline.
    session.set_state(Some("Jobs: Rust engineer, Go engineer, Zig engineer"));
    let f3 = lp.fire_at(t(23, 15, 0)).await.result.unwrap();
    let rel = format!("runs/{}/files/{NOTIFICATION_FILE}", f3.run);
    assert_eq!(
        watch_line(&f3.folder),
        format!("Watch: jobs: changed, notified ({rel})")
    );
    let note = read(&f3.folder.join("files").join(NOTIFICATION_FILE));
    assert!(note.starts_with("# Watch: jobs changed\n"), "{note}");
    assert!(note.contains("Zig engineer"), "{note}");
    let lines: Vec<String> = read(&log).lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("watch jobs changed"), "{lines:?}");
    assert!(lines[0].ends_with(&rel), "{lines:?}");
    let report = read(&f3.folder.join("report.md"));
    assert!(
        report.contains(&rel),
        "Files: lists the notification\n{report}"
    );
    let ev = events(&store, &f3.run).await;
    assert_eq!(
        core_tools(&ev),
        [
            (FIRE_TOOL.to_owned(), true),
            (CHECK_TOOL.to_owned(), true),
            (NOTIFY_TOOL.to_owned(), true)
        ]
    );
    let check = ev.iter().find(|e| e.tool == CHECK_TOOL).unwrap();
    assert_eq!(check.args["baseline"], b1.hash.as_str());
    let b3 = store.baseline_get("jobs").await.unwrap().unwrap();
    assert_ne!(b3.hash, b1.hash);
    assert_eq!(check.args["hash"], b3.hash.as_str());
    assert_eq!(b3.updated, t(23, 15, 0));
    assert_eq!(
        f3.status,
        RunStatus::Done,
        "watch events do not change the status"
    );

    // 4. Unchanged again: still one line.
    let f4 = lp.fire_at(t(23, 16, 0)).await.result.unwrap();
    assert_eq!(
        watch_line(&f4.folder),
        "Watch: jobs: no change, nothing sent"
    );
    assert_eq!(read(&log).lines().count(), 1);
    lp.finish().await;

    // Every firing was a fresh run with the watch schedule in its launch request.
    let seen = session.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 4);
    let mut ids: Vec<&RunId> = seen.iter().map(|s| &s.run).collect();
    ids.dedup();
    assert_eq!(ids.len(), 4);
    assert!(seen
        .iter()
        .all(|s| s.prompt == "check the jobs board" && s.kind == Some(ScheduleKind::Watch)));
    assert!(seen.iter().all(|s| s.preamble.contains(WATCH_FILE)));
    // prompt.md stays the task; the preamble is only in the launch.
    assert_eq!(read(&f1.folder.join("prompt.md")), "check the jobs board");
}

/// D-140 (3): no watch.txt (or only whitespace) is an ok:false watch.check, nothing is
/// sent and the baseline is untouched; a stopped run or a failed session is not checked
/// either. The next good state is still the first observation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_without_state_sends_nothing_and_keeps_baseline() {
    install();
    let tmp = Tmp::new("missing");
    let runs = tmp.runs();
    let store = Arc::new(MemStore::new());
    let w = add(
        &store,
        row(ScheduleKind::Watch, "0 * * * *", "watch it", None),
    )
    .await;
    let session = Session::new();
    let clock = Arc::new(ManualClock::new(t(23, 13, 0)));
    let r = Runner {
        store: store.clone(),
        launcher: session.clone(),
        clock: clock.clone(),
        runs_root: runs.clone(),
    };

    let f = r.fire(&w).await.unwrap();
    assert_eq!(
        watch_line(&f.folder),
        format!("Watch: jobs: no state in files/{WATCH_FILE}, baseline untouched, nothing sent")
    );
    let ev = events(&store, &f.run).await;
    let check = ev
        .iter()
        .find(|e| e.tool == CHECK_TOOL)
        .expect("watch.check");
    assert!(!check.ok);
    assert!(ev.iter().all(|e| e.tool != NOTIFY_TOOL));
    assert_eq!(store.baseline_get("jobs").await.unwrap(), None);
    assert_eq!(f.status, RunStatus::Done);

    session.set_state(Some(" \n\t "));
    let f = r.fire(&w).await.unwrap();
    assert!(watch_line(&f.folder).contains("no state in files/watch.txt"));
    assert_eq!(store.baseline_get("jobs").await.unwrap(), None);

    // A stopped run (a challenge, rule 3) wrote a state: not trusted, not checked.
    session.set_state(Some("half a page behind a challenge"));
    *session.stop_in.lock().unwrap() = Some(store.clone());
    let f = r.fire(&w).await.unwrap();
    assert_eq!(f.status, RunStatus::Stopped);
    assert_eq!(
        watch_line(&f.folder),
        "Watch: jobs: not checked (the run stopped), baseline untouched, nothing sent"
    );
    assert_eq!(store.baseline_get("jobs").await.unwrap(), None);
    *session.stop_in.lock().unwrap() = None;

    // A failed session wrote a state: not trusted, not checked.
    session.set_state(Some("half a page"));
    *session.code.lock().unwrap() = Some(3);
    let f = r.fire(&w).await.unwrap();
    assert_eq!(f.status, RunStatus::Failed);
    assert_eq!(
        watch_line(&f.folder),
        "Watch: jobs: not checked (the session failed), baseline untouched, nothing sent"
    );
    assert_eq!(store.baseline_get("jobs").await.unwrap(), None);
    let report = read(&f.folder.join("report.md"));
    assert!(
        report.contains("Result: failed (session exited with code 3)"),
        "{report}"
    );

    *session.code.lock().unwrap() = Some(0);
    session.set_state(Some("the page"));
    let f = r.fire(&w).await.unwrap();
    assert_eq!(
        watch_line(&f.folder),
        "Watch: jobs: first observation, baseline set, nothing sent"
    );
    assert!(!runs.join(NOTIFICATIONS_LOG).exists());
}

/// Rule 2: a vault value in watch.txt reaches neither notification file, the ledger
/// (rows and jsonl), the report nor the baseline; they hold `@vault(site)` instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_canary_never_leaves_the_session_files() {
    install();
    let c = canary();
    let tmp = Tmp::new("canary");
    let runs = tmp.runs();
    let store = Arc::new(MemStore::new());
    let w = add(
        &store,
        row(ScheduleKind::Watch, "0 * * * *", "watch it", None),
    )
    .await;
    let session = Session::new();
    let r = Runner {
        store: store.clone(),
        launcher: session.clone(),
        clock: Arc::new(ManualClock::new(t(23, 13, 0))),
        runs_root: runs.clone(),
    };
    session.set_state(Some(&format!("token {c} open")));
    let f1 = r.fire(&w).await.unwrap();
    session.set_state(Some(&format!("token {c} closed")));
    let f2 = r.fire(&w).await.unwrap();
    assert!(watch_line(&f2.folder).contains("changed, notified"));

    let note = read(&f2.folder.join("files").join(NOTIFICATION_FILE));
    assert!(note.contains("token @vault(site) closed"));
    let log = read(&runs.join(NOTIFICATIONS_LOG));
    assert_eq!(log.lines().count(), 1);
    let mut texts = vec![("notification.md", note), ("notifications.md", log)];
    for f in [&f1, &f2] {
        texts.push(("report.md", read(&f.folder.join("report.md"))));
        texts.push(("ledger.jsonl", read(&f.folder.join("ledger.jsonl"))));
        for e in events(&store, &f.run).await {
            texts.push(("ledger row", serde_json::to_string(&e).unwrap()));
        }
    }
    let b = store.baseline_get("jobs").await.unwrap().unwrap();
    let snap = serde_json::to_string(&b).unwrap();
    assert!(snap.contains("@vault(site)"));
    texts.push(("baseline", snap));
    for (what, text) in &texts {
        // The message names the place, never the value.
        assert!(!text.contains(c), "{what} holds the vault value");
    }
}

// ------------------------------------------------------------------ rung 7

/// Rung 7 shape: `0 8 * * 1-5` set on a Friday afternoon next fires on Monday 08:00 UTC,
/// as a fresh run whose parent is the schedule's run; the deliverable the session saved in
/// files/ is listed in report.md; the next deadline is Tuesday 08:00.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rung7_weekday_cron_fires_with_deliverable() {
    install();
    let tmp = Tmp::new("rung7");
    let runs = tmp.runs();
    let store = Arc::new(MemStore::new());
    let parent = parent_run(&store, &runs).await;
    let s = add(
        &store,
        row(
            ScheduleKind::Cron,
            "0 8 * * 1-5",
            "morning brief",
            Some(&parent),
        ),
    )
    .await;
    let session = Session::new();
    session
        .extra
        .lock()
        .unwrap()
        .push(("brief.md".into(), "# Brief\n3 new mails\n".into()));
    let clock = Arc::new(ManualClock::new(t(23, 15, 0)));
    let mut lp = Loop::start(store.clone(), session.clone(), clock.clone(), &runs);
    let f = lp.fire_at(t(26, 8, 0)).await;
    assert_eq!(f.schedule, s.id);
    let f = f.result.unwrap();
    assert_eq!(f.run.0, "2026-10-26T08-00-00-morning-brief");
    assert_eq!(f.parent_run.as_ref(), Some(&parent));
    assert_eq!(f.status, RunStatus::Done);
    let report = read(&f.folder.join("report.md"));
    assert!(
        report.starts_with("# morning brief\nResult: done\n"),
        "{report}"
    );
    assert!(
        report.contains(&format!("runs/{}/files/brief.md", f.run)),
        "{report}"
    );
    assert!(!report.contains("Watch:"), "{report}");
    let row = store.run_get(&f.run).await.unwrap().unwrap();
    assert_eq!(row.parent_run.as_ref(), Some(&parent));
    let ev = events(&store, &f.run).await;
    assert_eq!(core_tools(&ev), [(FIRE_TOOL.to_owned(), true)]);
    let next = timeout(CAP, clock.deadline_where(|d| d > t(26, 8, 0)))
        .await
        .unwrap();
    assert_eq!(next, t(27, 8, 0));
    lp.finish().await;
    let seen = session.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].kind, Some(ScheduleKind::Cron));
}

/// Reflect rows fire too (D-140); until harness handles them they launch like cron.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reflect_row_fires_a_fresh_run() {
    install();
    let tmp = Tmp::new("reflect");
    let runs = tmp.runs();
    let store = Arc::new(MemStore::new());
    let s = add(
        &store,
        row(ScheduleKind::Reflect, "0 3 * * *", "reflect", None),
    )
    .await;
    let session = Session::new();
    let clock = Arc::new(ManualClock::new(t(23, 15, 0)));
    let mut lp = Loop::start(store.clone(), session.clone(), clock, &runs);
    let f = lp.fire_at(t(24, 3, 0)).await;
    assert_eq!(f.schedule, s.id);
    let f = f.result.unwrap();
    assert!(f.folder.join("report.md").is_file());
    lp.finish().await;
    assert_eq!(
        session.seen.lock().unwrap()[0].kind,
        Some(ScheduleKind::Reflect)
    );
}

// ------------------------------------------------------------------ B-563 preamble

/// The run preamble names the run id and the absolute files/ path and says to cite only
/// what tools returned in this run; for a watch run it says to record the state with
/// harness-mem `watch.observe`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_preamble_names_run_files_and_watch_file() {
    install();
    let tmp = Tmp::new("preamble");
    let runs = tmp.runs();
    let run = RunFolder::create_at(&runs, "x", "x", t(23, 13, 0)).unwrap();
    let files = std::path::absolute(run.files()).unwrap();
    let files = files.to_string_lossy().replace('\\', "/");
    let mut req = LaunchRequest {
        run: &run,
        runs_root: &runs,
        prompt: "say hi",
        schedule: None,
    };
    let p = run_preamble(&req);
    assert!(p.contains(&run.id.0), "{p}");
    assert!(p.contains(&files), "{p}");
    assert!(
        p.contains("Cite only what tools returned in this run"),
        "{p}"
    );
    assert!(!p.contains(WATCH_FILE), "{p}");
    assert!(!p.contains("say hi"), "the preamble is not the task: {p}");

    let w = row(ScheduleKind::Watch, "0 * * * *", "watch it", None);
    req.schedule = Some(&w);
    let p = run_preamble(&req);
    assert!(p.contains(&format!("files/{WATCH_FILE}")), "{p}");
    assert!(p.contains("\"jobs\""), "{p}");
    // Review T-16 r1 should-fix 2: the state goes through the gated mem tool, never a
    // built-in file write (refused in -p and outside the gate).
    assert!(p.contains("harness-mem `watch.observe`"), "{p}");
    assert!(p.contains("do not write that file yourself"), "{p}");
    assert!(!p.contains(&format!("to {files}/{WATCH_FILE}")), "{p}");
    // A user may send nothing of this, but keep rule 7's style anyway.
    assert!(!p.contains('\u{2014}') && !p.contains("--"), "{p}");

    let c = row(ScheduleKind::Cron, "0 * * * *", "x", None);
    req.schedule = Some(&c);
    assert!(!run_preamble(&req).contains(WATCH_FILE));
}
