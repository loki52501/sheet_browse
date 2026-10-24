//! Scheduler (H-5, D-106, D-107, D-116). Owned by area core (T-08; watch and reflect are T-16).
//! The row types are contract and live in `memory` (D-48).
//!
//! * Cron expressions are 5-field standard cron, parsed with `croner`, evaluated in UTC (D-107).
//! * [`Scheduler::run_loop`] (the `harnessd` loop) sleeps until the next due instant across
//!   enabled schedules ([`Clock::sleep_until`]) or until a change is signalled on its `wake`
//!   [`Notify`] (`harness schedule add|rm` calls [`notify_changed`]; harnessd watches
//!   [`changed_dir`]). Never a fixed poll interval (CLAUDE.md rule 6).
//! * A firing ([`Runner::fire`]) records `schedules.last_run`, creates a run folder and a
//!   `runs` row (status running, `parent_run` = the schedule's run, H-4), launches it through
//!   a [`Launcher`] (D-106), then writes `report.md` from the ledger and sets ended/status.
//! * Due rule (D-116): the next occurrence strictly after the latest of `last_run`, the first
//!   time this loop saw the schedule, and the loop's last attempt. Missed occurrences coalesce
//!   into one firing; a schedule whose previous firing still runs skips.

use crate::ledger::{self, Ledger, LedgerError, CORE_SERVER};
use crate::memory::{ts_now, LedgerEvent, MemoryError, MemoryStore, Run, RunStatus, Ts};
pub use crate::memory::{Schedule, ScheduleKind};
use crate::run::{RunFolder, RunId};
use crate::vault::scrub;
use async_trait::async_trait;
use croner::parser::{CronParser, Seconds, Year};
use croner::Cron;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch, Notify};
use tokio::task::JoinSet;

/// Folder under the runs root whose writes mean "schedules changed" (D-116).
pub const CHANGED_DIR: &str = ".sched";
const CHANGED_FILE: &str = "changed";
/// `tool` of the core ledger event a firing writes first.
pub const FIRE_TOOL: &str = "sched.fire";
/// `tool` of the core ledger event written when the launcher fails.
pub const LAUNCH_TOOL: &str = "launch";

/// Retry cap after a failed `schedule_list` (the database is away).
const DB_RETRY_MAX: Duration = Duration::from_secs(60);
const DB_RETRY_MIN: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum SchedError {
    #[error("cron {cron:?}: {why}")]
    BadCron { cron: String, why: String },
    /// watch and reflect schedules are stored, but only T-16 (wave 3) runs them.
    #[error("schedule kind {0} is not in Phase 1 (T-16)")]
    NotInPhase1(&'static str),
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("run folder: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = SchedError> = std::result::Result<T, E>;

/// Parses a 5-field cron expression (minute hour day-of-month month day-of-week), UTC (D-107).
pub fn parse_cron(expr: &str) -> Result<Cron> {
    let bad = |why: String| SchedError::BadCron {
        cron: scrub(expr),
        why,
    };
    let n = expr.split_whitespace().count();
    if n != 5 {
        return Err(bad(format!(
            "need 5 fields (minute hour day-of-month month day-of-week), got {n}"
        )));
    }
    CronParser::builder()
        .seconds(Seconds::Disallowed)
        .year(Year::Disallowed)
        .build()
        .parse(expr)
        .map_err(|e| bad(e.to_string()))
}

/// The first occurrence strictly after `after`, or `None` when the pattern never matches again.
pub fn next_after(cron: &Cron, after: Ts) -> Option<Ts> {
    cron.find_next_occurrence(&after, false).ok()
}

/// `<runs_root>/.sched/`: harnessd watches it (non-recursive) to re-read schedules.
pub fn changed_dir(runs_root: &Path) -> PathBuf {
    runs_root.join(CHANGED_DIR)
}

/// Signals a running harnessd that schedules changed: rewrites `<runs_root>/.sched/changed`.
pub fn notify_changed(runs_root: &Path) -> std::io::Result<()> {
    let dir = changed_dir(runs_root);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(CHANGED_FILE), ts_now().to_rfc3339())
}

// ---------------------------------------------------------------- clock

/// Wall-clock time for the loop. [`SystemClock`] in harnessd; [`ManualClock`] in tests.
#[async_trait]
pub trait Clock: Send + Sync {
    fn now(&self) -> Ts;
    /// Returns once `now() >= t`.
    async fn sleep_until(&self, t: Ts);
}

/// The real clock: sleeps on a computed deadline (D-107), re-checking the wall clock on wake.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

#[async_trait]
impl Clock for SystemClock {
    fn now(&self) -> Ts {
        ts_now()
    }

    async fn sleep_until(&self, t: Ts) {
        loop {
            let now = chrono::Utc::now();
            if now >= t {
                return;
            }
            let d = (t - now).to_std().unwrap_or_default();
            match tokio::time::Instant::now().checked_add(d) {
                // A wait on the next due instant, not a fixed interval (rule 6). The loop
                // re-checks the wall clock because the monotonic clock can drift from it.
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        }
    }
}

/// A clock tests move by hand. [`ManualClock::next_deadline`] waits until something sleeps
/// on the clock, so a test can advance exactly to the deadline the loop computed.
#[derive(Debug)]
pub struct ManualClock {
    now: watch::Sender<Ts>,
    waiting: watch::Sender<Option<Ts>>,
}

impl ManualClock {
    pub fn new(start: Ts) -> ManualClock {
        ManualClock {
            now: watch::Sender::new(start),
            waiting: watch::Sender::new(None),
        }
    }

    pub fn set(&self, t: Ts) {
        self.now.send_replace(t);
    }

    /// The deadline of a sleeper that is still in the future (waits for one to appear).
    pub async fn next_deadline(&self) -> Ts {
        self.deadline_where(|t| t > *self.now.borrow()).await
    }

    /// Waits until a task sleeps on this clock with a deadline for which `f` holds.
    pub async fn deadline_where(&self, f: impl Fn(Ts) -> bool) -> Ts {
        let mut rx = self.waiting.subscribe();
        loop {
            if let Some(t) = *rx.borrow_and_update() {
                if f(t) {
                    return t;
                }
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// Clears [`ManualClock`]'s published deadline when its sleeper ends or is dropped.
struct Sleeping<'a> {
    waiting: &'a watch::Sender<Option<Ts>>,
    t: Ts,
}

impl Drop for Sleeping<'_> {
    fn drop(&mut self) {
        self.waiting.send_if_modified(|w| {
            let mine = *w == Some(self.t);
            if mine {
                *w = None;
            }
            mine
        });
    }
}

#[async_trait]
impl Clock for ManualClock {
    fn now(&self) -> Ts {
        *self.now.borrow()
    }

    async fn sleep_until(&self, t: Ts) {
        let mut rx = self.now.subscribe();
        self.waiting.send_replace(Some(t));
        let _sleeping = Sleeping {
            waiting: &self.waiting,
            t,
        };
        while *rx.borrow_and_update() < t {
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

// ---------------------------------------------------------------- launcher

/// What a [`Launcher`] starts: a fresh Claude Code session for this run (H-5).
#[derive(Debug, Clone, Copy)]
pub struct LaunchRequest<'a> {
    pub run: &'a RunFolder,
    /// Root holding `run.dir`; the session's tool servers get it as `SB_RUNS_ROOT`.
    pub runs_root: &'a Path,
    /// Already scrubbed (vault values appear only as `@vault(name)`).
    pub prompt: &'a str,
}

/// How the session ended. `code: None` means it was killed by a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchExit {
    pub code: Option<i32>,
}

impl LaunchExit {
    pub const OK: LaunchExit = LaunchExit { code: Some(0) };

    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// The launcher could not start or wait for the session. The message is scrubbed by the caller.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LaunchError(pub String);

/// Starts one run's session and waits for it (D-106). The default, `harness::ClaudeLauncher`,
/// runs `claude -p`; tests use a launcher that does not call a model (rule 1).
#[async_trait]
pub trait Launcher: Send + Sync {
    async fn launch(&self, req: LaunchRequest<'_>) -> Result<LaunchExit, LaunchError>;
}

// ---------------------------------------------------------------- one run

/// A run to start: the prompt and where it came from.
#[derive(Debug, Clone, Copy)]
pub struct RunSpec<'a> {
    pub prompt: &'a str,
    pub parent_run: Option<&'a RunId>,
    /// H-1.3: every ask in the run times out to skip.
    pub strict: bool,
    /// The schedule that fired, if any (adds the `sched.fire` ledger event).
    pub schedule: Option<&'a Schedule>,
}

/// A run that started and ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub run: RunId,
    pub folder: PathBuf,
    pub status: RunStatus,
    pub parent_run: Option<RunId>,
    pub schedule: Option<i32>,
    /// Why the launcher failed (scrubbed), when it did.
    pub launch_problem: Option<String>,
}

/// Starts runs: folder, `runs` row, launcher, report, end (H-4, D-106).
#[derive(Clone)]
pub struct Runner {
    pub store: Arc<dyn MemoryStore>,
    pub launcher: Arc<dyn Launcher>,
    pub clock: Arc<dyn Clock>,
    pub runs_root: PathBuf,
}

fn core_event(
    run: &RunId,
    tool: &str,
    args: serde_json::Value,
    ok: bool,
    why: Option<String>,
) -> LedgerEvent {
    LedgerEvent {
        run_id: Some(run.clone()),
        ts: ts_now(),
        server: CORE_SERVER.into(),
        tool: tool.into(),
        class: None,
        mode: None,
        args,
        result_summary: why.clone().unwrap_or_default(),
        consequences: json!({}),
        duration_ms: 0,
        ok,
        why,
    }
}

/// `report.md`'s `Result:` line, rewritten to `failed (<why>)`.
fn mark_failed(report: &str, why: &str) -> String {
    let mut done = false;
    let mut out: Vec<String> = Vec::new();
    for line in report.lines() {
        if !done && line.starts_with("Result: ") {
            out.push(format!("Result: failed ({why})"));
            done = true;
        } else {
            out.push(line.to_owned());
        }
    }
    if !done {
        out.insert(out.len().min(1), format!("Result: failed ({why})"));
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

impl Runner {
    /// Fires a schedule (H-5): `schedule_fired` first (so a failing firing never repeats at
    /// once), then [`Runner::execute`] with `parent_run` = the schedule's run.
    /// watch and reflect are [`SchedError::NotInPhase1`] (T-16).
    pub async fn fire(&self, s: &Schedule) -> Result<Finished> {
        match s.kind {
            ScheduleKind::Cron => {}
            ScheduleKind::Watch => return Err(SchedError::NotInPhase1("watch")),
            ScheduleKind::Reflect => return Err(SchedError::NotInPhase1("reflect")),
        }
        self.store.schedule_fired(s.id, self.clock.now()).await?;
        self.execute(RunSpec {
            prompt: &s.prompt,
            parent_run: s.run_id.as_ref(),
            strict: false,
            schedule: Some(s),
        })
        .await
    }

    /// One run end to end (H-4, D-106): `runs/<ts>-<slug>/` with prompt.md, a `runs` row with
    /// status running, the launcher, then `report.md` from the ledger and `runs.ended/status`.
    /// Status is the ledger's ([`ledger::run_status`]), or `failed` when the launcher failed
    /// and the ledger does not say stopped. Errors before the row exists return early; after
    /// it, the report and the end are always attempted and the first error is returned.
    pub async fn execute(&self, spec: RunSpec<'_>) -> Result<Finished> {
        let prompt = scrub(spec.prompt);
        let started = self.clock.now();
        let folder = RunFolder::create_at(&self.runs_root, &prompt, &prompt, started)?;
        self.store
            .run_start(Run {
                id: folder.id.clone(),
                started,
                ended: None,
                prompt: prompt.clone(),
                status: RunStatus::Running,
                folder: folder.dir.to_string_lossy().replace('\\', "/"),
                parent_run: spec.parent_run.cloned(),
                strict: spec.strict,
            })
            .await?;
        let ledger = Ledger::new(self.store.clone(), folder.clone());
        let mut first_err: Option<SchedError> = None;

        if let Some(s) = spec.schedule {
            let args = json!({
                "schedule": s.id,
                "cron": s.cron,
                "parent_run": s.run_id.as_ref().map(|r| r.0.clone()),
            });
            let why = format!("schedule {} fired", s.id);
            if let Err(e) = ledger
                .write(core_event(&folder.id, FIRE_TOOL, args, true, Some(why)))
                .await
            {
                first_err.get_or_insert(e.into());
            }
        }

        let launched = self
            .launcher
            .launch(LaunchRequest {
                run: &folder,
                runs_root: &self.runs_root,
                prompt: &prompt,
            })
            .await;
        let launch_problem = match launched {
            Ok(exit) if exit.success() => None,
            Ok(exit) => Some(match exit.code {
                Some(c) => format!("session exited with code {c}"),
                None => "session was killed".to_owned(),
            }),
            Err(e) => Some(scrub(&format!("launcher: {e}"))),
        };
        if let Some(why) = &launch_problem {
            let ev = core_event(&folder.id, LAUNCH_TOOL, json!({}), false, Some(why.clone()));
            if let Err(e) = ledger.write(ev).await {
                first_err.get_or_insert(e.into());
            }
        }

        let events: Vec<LedgerEvent> = match self.store.ledger_events(&folder.id).await {
            Ok(rows) => rows.into_iter().map(|(_, e)| e).collect(),
            Err(e) => {
                first_err.get_or_insert(e.into());
                ledger::read_jsonl(&folder.ledger_jsonl())
                    .map(|rows| rows.into_iter().map(|(_, e)| e).collect())
                    .unwrap_or_default()
            }
        };
        let mut status = ledger::run_status(&events);
        if launch_problem.is_some() && status != RunStatus::Stopped {
            status = RunStatus::Failed;
        }
        match ledger::write_report(&folder, &prompt, &events) {
            Ok(text) => {
                if let (RunStatus::Failed, Some(why)) = (status, &launch_problem) {
                    if let Err(e) = std::fs::write(folder.report_md(), mark_failed(&text, why)) {
                        first_err.get_or_insert(e.into());
                    }
                }
            }
            Err(e) => {
                first_err.get_or_insert(e.into());
            }
        }
        let ended = self.clock.now().max(started);
        if let Err(e) = self.store.run_end(&folder.id, status, ended).await {
            first_err.get_or_insert(e.into());
        }
        if let Some(e) = first_err {
            return Err(e);
        }
        Ok(Finished {
            run: folder.id.clone(),
            folder: folder.dir.clone(),
            status,
            parent_run: spec.parent_run.cloned(),
            schedule: spec.schedule.map(|s| s.id),
            launch_problem,
        })
    }
}

// ---------------------------------------------------------------- the loop

/// One firing's outcome, sent on [`Scheduler::fired`] when it ends.
#[derive(Debug)]
pub struct Fired {
    pub schedule: i32,
    pub result: Result<Finished>,
}

/// The `harnessd` loop (H-5 Phase 1).
pub struct Scheduler {
    pub runner: Runner,
    /// Notified when schedules change; harnessd's watcher on [`changed_dir`] calls `notify_one`.
    pub wake: Arc<Notify>,
    /// Every finished firing, for logs and tests (tests wait on this, never on a sleep).
    pub fired: Option<mpsc::UnboundedSender<Fired>>,
}

/// Loop state that outlives one pass.
#[derive(Default)]
struct Book {
    first_seen: HashMap<i32, Ts>,
    attempted: HashMap<i32, Ts>,
    warned: HashSet<(i32, String)>,
    running: HashMap<tokio::task::Id, i32>,
}

impl Book {
    fn warn_once(&mut self, id: i32, what: String) {
        if self.warned.insert((id, what.clone())) {
            eprintln!("harnessd: schedule {id}: {}", scrub(&what));
        }
    }

    fn is_running(&self, id: i32) -> bool {
        self.running.values().any(|&r| r == id)
    }
}

impl Scheduler {
    /// Due schedules start as tasks; between passes the loop waits for the earliest next due
    /// instant, a change on `wake`, a firing ending, or `shutdown`. On shutdown it waits for
    /// running firings to finish (their report and status get written), then returns.
    /// `schedule_list` failures are logged and retried with a capped backoff.
    pub async fn run_loop(self, shutdown: impl Future<Output = ()> + Send) -> Result<()> {
        tokio::pin!(shutdown);
        let mut book = Book::default();
        let mut tasks: JoinSet<Result<Finished>> = JoinSet::new();
        let mut retry = DB_RETRY_MIN;
        loop {
            let changed = self.wake.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();

            let next = match self.runner.store.schedule_list().await {
                Ok(list) => {
                    retry = DB_RETRY_MIN;
                    self.pass(&list, &mut book, &mut tasks)
                }
                Err(e) => {
                    eprintln!(
                        "harnessd: reading schedules failed, retrying in {}s: {}",
                        retry.as_secs_f32(),
                        scrub(&e.to_string())
                    );
                    tokio::select! {
                        _ = &mut shutdown => break,
                        _ = &mut changed => {}
                        // No event says "the database is back"; a capped backoff is the wait.
                        _ = tokio::time::sleep(retry) => {}
                    }
                    retry = (retry * 2).min(DB_RETRY_MAX);
                    continue;
                }
            };

            let clock = self.runner.clock.clone();
            let due = async move {
                match next {
                    Some(t) => clock.sleep_until(t).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = &mut shutdown => break,
                _ = &mut changed => {}
                _ = due => {}
                Some(joined) = tasks.join_next_with_id(), if !tasks.is_empty() => {
                    self.finished(joined, &mut book);
                }
            }
        }
        while let Some(joined) = tasks.join_next_with_id().await {
            self.finished(joined, &mut book);
        }
        Ok(())
    }

    /// Starts every due schedule; returns the earliest next due instant of the others.
    fn pass(
        &self,
        list: &[Schedule],
        book: &mut Book,
        tasks: &mut JoinSet<Result<Finished>>,
    ) -> Option<Ts> {
        let now = self.runner.clock.now();
        let live: HashSet<i32> = list.iter().map(|s| s.id).collect();
        book.first_seen.retain(|id, _| live.contains(id));
        book.attempted.retain(|id, _| live.contains(id));
        let mut next: Option<Ts> = None;
        for s in list {
            if !s.enabled || book.is_running(s.id) {
                continue;
            }
            match s.kind {
                ScheduleKind::Cron => {}
                ScheduleKind::Watch | ScheduleKind::Reflect => {
                    let kind = if s.kind == ScheduleKind::Watch {
                        "watch"
                    } else {
                        "reflect"
                    };
                    book.warn_once(
                        s.id,
                        format!("kind {kind} is not in Phase 1 (T-16); skipped"),
                    );
                    continue;
                }
            }
            let cron = match parse_cron(&s.cron) {
                Ok(c) => c,
                Err(e) => {
                    book.warn_once(s.id, format!("{e}; skipped"));
                    continue;
                }
            };
            let first = *book.first_seen.entry(s.id).or_insert(now);
            let reference = [s.last_run, book.attempted.get(&s.id).copied(), Some(first)]
                .into_iter()
                .flatten()
                .max()
                .unwrap_or(now);
            let Some(at) = next_after(&cron, reference) else {
                book.warn_once(s.id, format!("cron {:?} never matches again", s.cron));
                continue;
            };
            if at <= now {
                book.attempted.insert(s.id, now);
                let runner = self.runner.clone();
                let row = s.clone();
                let handle = tasks.spawn(async move { runner.fire(&row).await });
                book.running.insert(handle.id(), s.id);
            } else {
                next = Some(next.map_or(at, |n| n.min(at)));
            }
        }
        next
    }

    fn finished(
        &self,
        joined: std::result::Result<(tokio::task::Id, Result<Finished>), tokio::task::JoinError>,
        book: &mut Book,
    ) {
        let (task, result) = match joined {
            Ok((task, r)) => (task, r),
            Err(e) => {
                let task = e.id();
                let why = if e.is_panic() {
                    "firing panicked"
                } else {
                    "firing was cancelled"
                };
                (task, Err(SchedError::Io(std::io::Error::other(why))))
            }
        };
        let Some(schedule) = book.running.remove(&task) else {
            return;
        };
        match &result {
            Ok(f) => eprintln!(
                "harnessd: schedule {schedule} -> run {} {:?}{}",
                f.run,
                f.status,
                f.launch_problem
                    .as_deref()
                    .map(|p| format!(" ({p})"))
                    .unwrap_or_default()
            ),
            Err(e) => eprintln!(
                "harnessd: schedule {schedule} firing failed: {}",
                scrub(&e.to_string())
            ),
        }
        if let Some(tx) = &self.fired {
            let _ = tx.send(Fired { schedule, result });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MemStore;
    use chrono::{TimeZone, Utc};
    use std::sync::Mutex;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Tmp {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            Tmp(std::env::temp_dir().join(format!(
                "sb-sched-test-{}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed),
                Utc::now().timestamp_micros()
            )))
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Records each request; exits with `code` (no model, rule 1).
    struct FakeLauncher {
        code: Option<i32>,
        fail: bool,
        seen: Mutex<Vec<(RunId, String)>>,
    }
    impl FakeLauncher {
        fn ok() -> Arc<FakeLauncher> {
            Arc::new(FakeLauncher {
                code: Some(0),
                fail: false,
                seen: Mutex::new(vec![]),
            })
        }
    }
    #[async_trait]
    impl Launcher for FakeLauncher {
        async fn launch(&self, req: LaunchRequest<'_>) -> Result<LaunchExit, LaunchError> {
            assert!(req.run.dir.is_dir());
            self.seen
                .lock()
                .unwrap()
                .push((req.run.id.clone(), req.prompt.to_owned()));
            if self.fail {
                return Err(LaunchError("no such program: claude".into()));
            }
            Ok(LaunchExit { code: self.code })
        }
    }

    fn t(h: u32, m: u32, s: u32) -> Ts {
        Utc.with_ymd_and_hms(2026, 10, 23, h, m, s).unwrap()
    }

    fn cron_row(cron: &str, prompt: &str, parent: Option<&RunId>) -> Schedule {
        Schedule {
            id: 0,
            cron: cron.into(),
            prompt: prompt.into(),
            kind: ScheduleKind::Cron,
            watch_target: None,
            enabled: true,
            last_run: None,
            run_id: parent.cloned(),
        }
    }

    async fn parent_run(store: &MemStore, root: &Path) -> RunId {
        let f = RunFolder::create_at(root, "schedule-add", "schedule add", t(11, 0, 0)).unwrap();
        store
            .run_start(Run {
                id: f.id.clone(),
                started: t(11, 0, 0),
                ended: Some(t(11, 0, 0)),
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

    fn runner(
        store: Arc<MemStore>,
        launcher: Arc<FakeLauncher>,
        clock: Arc<ManualClock>,
        root: &Path,
    ) -> Runner {
        Runner {
            store,
            launcher,
            clock,
            runs_root: root.to_path_buf(),
        }
    }

    const CAP: Duration = Duration::from_secs(20);

    #[test]
    fn cron_is_five_fields_utc() {
        let c = parse_cron("*/1 * * * *").unwrap();
        assert_eq!(next_after(&c, t(12, 0, 30)), Some(t(12, 1, 0)));
        assert_eq!(next_after(&c, t(12, 1, 0)), Some(t(12, 2, 0)));
        let nine = parse_cron("0 9 * * *").unwrap();
        assert_eq!(
            next_after(&nine, t(10, 0, 0)),
            Some(t(9, 0, 0) + chrono::Duration::days(1))
        );
        for bad in [
            "* * * * * *",
            "* * * *",
            "",
            "@hourly",
            "61 * * * *",
            "0 0 * * * 2027",
        ] {
            assert!(
                matches!(parse_cron(bad), Err(SchedError::BadCron { .. })),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn mark_failed_rewrites_result_line() {
        let r = "# say hi\nResult: done\nRead: 0\n";
        assert_eq!(
            mark_failed(r, "x"),
            "# say hi\nResult: failed (x)\nRead: 0\n"
        );
    }

    #[tokio::test]
    async fn manual_clock_wakes_at_deadline() {
        let clock = Arc::new(ManualClock::new(t(12, 0, 0)));
        let c2 = clock.clone();
        let sleeper = tokio::spawn(async move { c2.sleep_until(t(12, 1, 0)).await });
        let d = tokio::time::timeout(CAP, clock.next_deadline())
            .await
            .unwrap();
        assert_eq!(d, t(12, 1, 0));
        clock.set(t(12, 1, 0));
        tokio::time::timeout(CAP, sleeper).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn fire_creates_child_run_with_report() {
        let tmp = Tmp::new();
        let store = Arc::new(MemStore::new());
        let parent = parent_run(&store, &tmp.0).await;
        let id = store
            .schedule_add(cron_row("*/1 * * * *", "say hi", Some(&parent)))
            .await
            .unwrap();
        let s = store
            .schedule_list()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == id)
            .unwrap();
        let launcher = FakeLauncher::ok();
        let clock = Arc::new(ManualClock::new(t(12, 1, 0)));
        let r = runner(store.clone(), launcher.clone(), clock, &tmp.0);
        let f = r.fire(&s).await.unwrap();
        assert_eq!(f.status, RunStatus::Done);
        assert_eq!(f.parent_run.as_ref(), Some(&parent));
        let row = store.run_get(&f.run).await.unwrap().unwrap();
        assert_eq!(row.parent_run.as_ref(), Some(&parent));
        assert_eq!(row.status, RunStatus::Done);
        assert_eq!(row.ended, Some(t(12, 1, 0)));
        assert_eq!(f.run.0, "2026-10-23T12-01-00-say-hi");
        let report = std::fs::read_to_string(f.folder.join("report.md")).unwrap();
        assert!(report.starts_with("# say hi\nResult: done\n"), "{report}");
        let ev = store.ledger_events(&f.run).await.unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].1.tool, FIRE_TOOL);
        assert_eq!(ev[0].1.server, CORE_SERVER);
        let last = store.schedule_list().await.unwrap()[0].last_run;
        assert_eq!(last, Some(t(12, 1, 0)));
        assert_eq!(
            launcher.seen.lock().unwrap().as_slice(),
            &[(f.run.clone(), "say hi".to_owned())]
        );
    }

    #[tokio::test]
    async fn launcher_failure_marks_run_failed() {
        let tmp = Tmp::new();
        let store = Arc::new(MemStore::new());
        let launcher = Arc::new(FakeLauncher {
            code: None,
            fail: true,
            seen: Mutex::new(vec![]),
        });
        let r = runner(
            store.clone(),
            launcher,
            Arc::new(ManualClock::new(t(12, 0, 0))),
            &tmp.0,
        );
        let f = r
            .execute(RunSpec {
                prompt: "say hi",
                parent_run: None,
                strict: false,
                schedule: None,
            })
            .await
            .unwrap();
        assert_eq!(f.status, RunStatus::Failed);
        assert_eq!(
            store.run_get(&f.run).await.unwrap().unwrap().status,
            RunStatus::Failed
        );
        let report = std::fs::read_to_string(f.folder.join("report.md")).unwrap();
        assert!(
            report.contains("Result: failed (launcher: no such program: claude)"),
            "{report}"
        );

        let launcher = Arc::new(FakeLauncher {
            code: Some(3),
            fail: false,
            seen: Mutex::new(vec![]),
        });
        let r = runner(
            store.clone(),
            launcher,
            Arc::new(ManualClock::new(t(12, 0, 0))),
            &tmp.0,
        );
        let f = r
            .execute(RunSpec {
                prompt: "again",
                parent_run: None,
                strict: true,
                schedule: None,
            })
            .await
            .unwrap();
        assert_eq!(f.status, RunStatus::Failed);
        assert_eq!(
            f.launch_problem.as_deref(),
            Some("session exited with code 3")
        );
        assert!(store.run_get(&f.run).await.unwrap().unwrap().strict);
    }

    #[tokio::test]
    async fn watch_and_reflect_are_not_phase1() {
        let tmp = Tmp::new();
        let store = Arc::new(MemStore::new());
        let r = runner(
            store.clone(),
            FakeLauncher::ok(),
            Arc::new(ManualClock::new(t(12, 0, 0))),
            &tmp.0,
        );
        let mut s = cron_row("*/1 * * * *", "watch it", None);
        s.kind = ScheduleKind::Watch;
        assert!(matches!(
            r.fire(&s).await,
            Err(SchedError::NotInPhase1("watch"))
        ));
        s.kind = ScheduleKind::Reflect;
        assert!(matches!(
            r.fire(&s).await,
            Err(SchedError::NotInPhase1("reflect"))
        ));
    }

    /// The loop: waits for a change, computes the next due minute, fires once per advance,
    /// skips watch and disabled rows, and coalesces missed occurrences into one firing.
    #[tokio::test]
    async fn loop_fires_on_deadline_and_wakes_on_change() {
        let tmp = Tmp::new();
        let store = Arc::new(MemStore::new());
        let parent = parent_run(&store, &tmp.0).await;
        let clock = Arc::new(ManualClock::new(t(12, 0, 30)));
        let launcher = FakeLauncher::ok();
        let wake = Arc::new(Notify::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sched = Scheduler {
            runner: runner(store.clone(), launcher.clone(), clock.clone(), &tmp.0),
            wake: wake.clone(),
            fired: Some(tx),
        };
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(sched.run_loop(async move {
            let _ = stop_rx.await;
        }));

        // Rows the loop must not fire.
        let mut w = cron_row("*/1 * * * *", "watch", None);
        w.kind = ScheduleKind::Watch;
        store.schedule_add(w).await.unwrap();
        let mut off = cron_row("*/1 * * * *", "off", None);
        off.enabled = false;
        store.schedule_add(off).await.unwrap();
        let id = store
            .schedule_add(cron_row("*/1 * * * *", "say hi", Some(&parent)))
            .await
            .unwrap();
        wake.notify_one();

        let due = tokio::time::timeout(CAP, clock.next_deadline())
            .await
            .unwrap();
        assert_eq!(due, t(12, 1, 0));
        clock.set(due);
        let fired = tokio::time::timeout(CAP, rx.recv()).await.unwrap().unwrap();
        assert_eq!(fired.schedule, id);
        let f = fired.result.unwrap();
        assert_eq!(f.parent_run.as_ref(), Some(&parent));
        assert!(f.folder.join("report.md").is_file());

        // Next deadline is the following minute; jump 10 minutes: one firing, not ten.
        let due = tokio::time::timeout(CAP, clock.next_deadline())
            .await
            .unwrap();
        assert_eq!(due, t(12, 2, 0));
        clock.set(t(12, 11, 0));
        let fired = tokio::time::timeout(CAP, rx.recv()).await.unwrap().unwrap();
        assert_eq!(fired.result.unwrap().run.0, "2026-10-23T12-11-00-say-hi");
        let due = tokio::time::timeout(CAP, clock.next_deadline())
            .await
            .unwrap();
        assert_eq!(due, t(12, 12, 0));

        stop_tx.send(()).unwrap();
        tokio::time::timeout(CAP, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(rx.try_recv().is_err());
        assert_eq!(launcher.seen.lock().unwrap().len(), 2);
        let children: Vec<_> = launcher
            .seen
            .lock()
            .unwrap()
            .iter()
            .map(|(_, p)| p.clone())
            .collect();
        assert_eq!(children, ["say hi", "say hi"]);
    }

    #[test]
    fn notify_changed_writes_marker() {
        let tmp = Tmp::new();
        notify_changed(&tmp.0).unwrap();
        assert!(changed_dir(&tmp.0).join(CHANGED_FILE).is_file());
    }
}
