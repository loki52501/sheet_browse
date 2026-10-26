//! Tester acceptance for T-08 (docs/01 H-4, H-5, H-7, step-1 acceptance item 4; D-116, B-177).
//!
//! * `acc4_*`: "`harness schedule add "*/1 * * * *" "say hi"` fires, creates a run folder with
//!   `report.md`, and `runs` has a row with `parent_run` set", driven through the real
//!   `harness` and `harnessd` binaries against Postgres, with a fake `claude` (rule 1) compiled
//!   from bench/fixtures/fake_claude.rs. It waits for a real minute boundary (up to ~60 s).
//! * `d116_<n>_*`: one or more tests per numbered clause of D-116.
//! * `vault_set_*`, `ask_*`: H-7 `harness vault set` (no echo, canary never printed) and
//!   `harness ask` on a closed ask (B-177 / D-79).
//!
//! Postgres: a throwaway `sb_t_<random>` database per test on SB_TEST_DATABASE_URL (default
//! the compose Postgres on 127.0.0.1:5433), dropped at the end (D-100). The vault canary is
//! `SB_VAULT_CANARY` (a fresh value is generated when it is unset).
//!
//! Tests named `child_*` do nothing unless `SB_T08_CHILD` names them: their parent test runs
//! them in a separate process so it can grep that process's whole stdout and stderr, and so a
//! process-wide vault (`Vault::install_global`) never leaks into the other tests.

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use clap::Parser;
use harness::cli::{execute, Cli, Ctx};
use harness::daemon::Daemon;
use harness::{ClaudeLauncher, Env};
use harness_core::ledger::Ledger;
use harness_core::memory::*;
use harness_core::run::{RunFolder, RunId};
use harness_core::sched::{
    self, Clock, Fired, LaunchError, LaunchExit, LaunchRequest, Launcher, ManualClock, RunSpec,
    Runner, SchedError, Scheduler, SystemClock, FIRE_TOOL, LAUNCH_TOOL,
};
use harness_core::vault::{EntryKind, KeySource, Vault};
use sqlx::{Connection, Row};
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tokio::time::timeout;
use zeroize::Zeroizing;

const CAP: Duration = Duration::from_secs(30);
/// A real `*/1` firing: up to 60 s to the next minute boundary, plus start-up and the run.
const CAP_MINUTE: Duration = Duration::from_secs(100);
const DEFAULT_ADMIN_URL: &str = "postgres://sb@127.0.0.1:5433/postgres";
const CHILD_VAR: &str = "SB_T08_CHILD";

// ------------------------------------------------------------------ throwaway database

fn admin_url() -> String {
    std::env::var("SB_TEST_DATABASE_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_ADMIN_URL.to_string())
}

fn with_db(url: &str, db: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let after_scheme = base.find("://").map_or(0, |i| i + 3);
    let base = match base[after_scheme..].find('/') {
        Some(i) => &base[..after_scheme + i],
        None => base,
    };
    match query {
        Some(q) => format!("{base}/{db}?{q}"),
        None => format!("{base}/{db}"),
    }
}

fn random_name() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    h.write_u32(N.fetch_add(1, Ordering::Relaxed));
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    );
    format!("sb_t_{:016x}", h.finish())
}

struct TestDb {
    name: String,
    url: String,
}

impl TestDb {
    async fn store() -> (TestDb, Arc<dyn MemoryStore>) {
        let admin = admin_url();
        let name = random_name();
        let mut conn = sqlx::PgConnection::connect(&admin)
            .await
            .unwrap_or_else(|e| panic!("cannot reach the test Postgres ({e})"));
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database {name}")))
            .execute(&mut conn)
            .await
            .expect("create test database");
        let _ = conn.close().await;
        let db = TestDb {
            url: with_db(&admin, &name),
            name,
        };
        let store = core_pg::PgStore::connect(&db.url)
            .await
            .unwrap_or_else(|e| panic!("connect + migrate {}: {e}", db.name));
        (db, Arc::new(store))
    }

    async fn conn(&self) -> sqlx::PgConnection {
        sqlx::PgConnection::connect(&self.url).await.unwrap()
    }

    /// Every row of the tables a run can touch, as text, for the canary grep.
    async fn dump(&self) -> String {
        let mut conn = self.conn().await;
        let mut all = String::new();
        for t in [
            "ledger.events",
            "runs",
            "schedules",
            "gate.rules",
            "tools.health",
        ] {
            let sql = format!("select t::text from {t} t");
            let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
                .fetch_all(&mut conn)
                .await
                .unwrap_or_else(|e| panic!("dump {t}: {e}"));
            for r in rows {
                all.push_str(&r.get::<String, _>(0));
                all.push('\n');
            }
        }
        all
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let name = self.name.clone();
        let admin = admin_url();
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime for test database cleanup");
            rt.block_on(async move {
                if let Ok(mut conn) = sqlx::PgConnection::connect(&admin).await {
                    let sql = format!("drop database if exists {name} with (force)");
                    let _ = sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                        .execute(&mut conn)
                        .await;
                    let _ = conn.close().await;
                }
            });
        })
        .join();
    }
}

// ------------------------------------------------------------------ shared fixtures

fn canary() -> String {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| {
        std::env::var("SB_VAULT_CANARY")
            .ok()
            .filter(|c| c.len() >= 8)
            .unwrap_or_else(|| {
                format!(
                    "sbcanary-t08acc-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                )
            })
    })
    .clone()
}

/// Every file under `dir` whose bytes contain `needle`.
fn files_containing(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut hits = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(b) = std::fs::read(&p) {
                if String::from_utf8_lossy(&b).contains(needle) {
                    hits.push(p);
                }
            }
        }
    }
    hits
}

fn t(h: u32, m: u32, s: u32) -> Ts {
    Utc.with_ymd_and_hms(2026, 10, 25, h, m, s).unwrap()
}

fn cron_row(cron: &str, prompt: &str, last_run: Option<Ts>) -> Schedule {
    Schedule {
        id: 0,
        cron: cron.into(),
        prompt: prompt.into(),
        kind: ScheduleKind::Cron,
        watch_target: None,
        enabled: true,
        last_run,
        run_id: Some(RunId("2026-10-25T11-00-00-schedule-add".into())),
    }
}

/// bench/fixtures/fake_claude.rs compiled with plain rustc (once per test process).
fn fake_claude() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/fixtures/fake_claude.rs");
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("accept_t08_fake_claude")
            .join(std::process::id().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join(format!("fake-claude{}", std::env::consts::EXE_SUFFIX));
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
        let st = std::process::Command::new(rustc)
            .args(["--edition", "2021", "-o"])
            .arg(&out)
            .arg(&src)
            .status()
            .expect("run rustc for the fake claude");
        assert!(st.success(), "compiling {}", src.display());
        out
    })
    .clone()
}

/// Wraps a store: counts `schedule_list` calls and can fail `schedule_fired` for chosen ids.
struct ObservedStore {
    inner: Arc<dyn MemoryStore>,
    lists: watch::Sender<usize>,
    fail_fired: Mutex<HashSet<i32>>,
}

impl ObservedStore {
    fn new(inner: Arc<dyn MemoryStore>) -> Arc<ObservedStore> {
        Arc::new(ObservedStore {
            inner,
            lists: watch::Sender::new(0),
            fail_fired: Mutex::new(HashSet::new()),
        })
    }

    async fn wait_lists(&self, n: usize) {
        let mut rx = self.lists.subscribe();
        timeout(CAP, rx.wait_for(|c| *c >= n))
            .await
            .unwrap_or_else(|_| panic!("schedule_list never reached {n} calls"))
            .unwrap();
    }

    fn lists(&self) -> usize {
        *self.lists.borrow()
    }
}

#[async_trait]
impl MemoryStore for ObservedStore {
    async fn remember(&self, f: Fact) -> Result<FactId> {
        self.inner.remember(f).await
    }
    async fn forget(&self, id: FactId) -> Result<()> {
        self.inner.forget(id).await
    }
    async fn link(&self, src: NodeId, rel: &str, dst: NodeId, source: Source) -> Result<()> {
        self.inner.link(src, rel, dst, source).await
    }
    async fn node(&self, kind: &str, name: &str) -> Result<NodeId> {
        self.inner.node(kind, name).await
    }
    async fn neighbors(&self, n: NodeId, rel: Option<&str>, depth: u8) -> Result<Vec<Node>> {
        self.inner.neighbors(n, rel, depth).await
    }
    async fn recall(&self, q: &str, k: usize, filter: Option<Filter>) -> Result<Vec<Hit>> {
        self.inner.recall(q, k, filter).await
    }
    async fn search(&self, q: &str, k: usize) -> Result<Vec<Hit>> {
        self.inner.search(q, k).await
    }
    async fn contact(&self, person: &str) -> Result<Option<Contact>> {
        self.inner.contact(person).await
    }
    async fn touch_contact(&self, person: &str, channel: &str, note: &str) -> Result<()> {
        self.inner.touch_contact(person, channel, note).await
    }
    async fn due_reviews(&self, now: Ts, k: usize) -> Result<Vec<Review>> {
        self.inner.due_reviews(now, k).await
    }
    async fn grade_review(&self, id: ReviewId, grade: u8) -> Result<()> {
        self.inner.grade_review(id, grade).await
    }
    async fn baseline_get(&self, w: &str) -> Result<Option<Baseline>> {
        self.inner.baseline_get(w).await
    }
    async fn baseline_set(&self, w: &str, b: Baseline) -> Result<()> {
        self.inner.baseline_set(w, b).await
    }
    async fn append(&self, e: LedgerEvent) -> Result<LedgerId> {
        self.inner.append(e).await
    }
    async fn tool_health(&self, domain: &str) -> Result<Vec<ToolHealth>> {
        self.inner.tool_health(domain).await
    }
    async fn tool_health_set(&self, h: ToolHealth) -> Result<()> {
        self.inner.tool_health_set(h).await
    }
    async fn gate_rule(&self, domain: &str, class: &str) -> Result<Option<GateRule>> {
        self.inner.gate_rule(domain, class).await
    }
    async fn run_start(&self, run: Run) -> Result<()> {
        self.inner.run_start(run).await
    }
    async fn run_end(&self, id: &RunId, status: RunStatus, ended: Ts) -> Result<()> {
        self.inner.run_end(id, status, ended).await
    }
    async fn run_get(&self, id: &RunId) -> Result<Option<Run>> {
        self.inner.run_get(id).await
    }
    async fn schedule_add(&self, s: Schedule) -> Result<i32> {
        self.inner.schedule_add(s).await
    }
    async fn schedule_list(&self) -> Result<Vec<Schedule>> {
        let r = self.inner.schedule_list().await;
        self.lists.send_modify(|c| *c += 1);
        r
    }
    async fn schedule_rm(&self, id: i32) -> Result<()> {
        self.inner.schedule_rm(id).await
    }
    async fn schedule_fired(&self, id: i32, at: Ts) -> Result<()> {
        if self.fail_fired.lock().unwrap().contains(&id) {
            return Err(MemoryError::Invalid(format!(
                "test: schedule_fired({id}) refused"
            )));
        }
        self.inner.schedule_fired(id, at).await
    }
    async fn gate_rule_set(&self, d: &str, c: &str, r: GateRule, note: &str) -> Result<()> {
        self.inner.gate_rule_set(d, c, r, note).await
    }
    async fn review_add(&self, item: &str, answer: &str) -> Result<ReviewId> {
        self.inner.review_add(item, answer).await
    }
    async fn ledger_events(&self, run: &RunId) -> Result<Vec<(LedgerId, LedgerEvent)>> {
        self.inner.ledger_events(run).await
    }
}

#[derive(Clone, Copy, Debug)]
enum Behave {
    Ok,
    Exit(Option<i32>),
    Err,
    /// The first launch for this prompt waits for `release`.
    BlockFirst,
    /// Writes a stop event (mode stop) to the run's ledger, then exits 1.
    StopThenExit1,
}

#[derive(Clone, Debug)]
struct Seen {
    run: RunId,
    prompt: String,
    /// `schedules.last_run` of the schedule with this prompt, read at launch time.
    last_run_at_launch: Option<Option<Ts>>,
}

/// A launcher that never calls a model (rule 1).
struct TestLauncher {
    store: Arc<dyn MemoryStore>,
    behave: fn(&str) -> Behave,
    seen: Mutex<Vec<Seen>>,
    launches: watch::Sender<usize>,
    release: Notify,
    running: Mutex<HashMap<String, usize>>,
    max_running: Mutex<HashMap<String, usize>>,
}

impl TestLauncher {
    fn new(store: Arc<dyn MemoryStore>, behave: fn(&str) -> Behave) -> Arc<TestLauncher> {
        Arc::new(TestLauncher {
            store,
            behave,
            seen: Mutex::new(Vec::new()),
            launches: watch::Sender::new(0),
            release: Notify::new(),
            running: Mutex::new(HashMap::new()),
            max_running: Mutex::new(HashMap::new()),
        })
    }

    fn count(&self, prompt: &str) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.prompt == prompt)
            .count()
    }

    fn max_running(&self, prompt: &str) -> usize {
        *self.max_running.lock().unwrap().get(prompt).unwrap_or(&0)
    }

    async fn wait_launches(&self, n: usize) {
        let mut rx = self.launches.subscribe();
        timeout(CAP, rx.wait_for(|c| *c >= n))
            .await
            .unwrap_or_else(|_| panic!("never reached {n} launches"))
            .unwrap();
    }
}

#[async_trait]
impl Launcher for TestLauncher {
    async fn launch(&self, req: LaunchRequest<'_>) -> Result<LaunchExit, LaunchError> {
        let last = self
            .store
            .schedule_list()
            .await
            .ok()
            .and_then(|l| l.into_iter().find(|s| s.prompt == req.prompt))
            .map(|s| s.last_run);
        let nth = {
            let mut seen = self.seen.lock().unwrap();
            seen.push(Seen {
                run: req.run.id.clone(),
                prompt: req.prompt.to_owned(),
                last_run_at_launch: last,
            });
            seen.iter().filter(|s| s.prompt == req.prompt).count()
        };
        {
            let mut r = self.running.lock().unwrap();
            let n = r.entry(req.prompt.to_owned()).or_insert(0);
            *n += 1;
            let mut m = self.max_running.lock().unwrap();
            let mx = m.entry(req.prompt.to_owned()).or_insert(0);
            *mx = (*mx).max(*n);
        }
        self.launches.send_modify(|c| *c += 1);
        let out = match (self.behave)(req.prompt) {
            Behave::Ok => Ok(LaunchExit::OK),
            Behave::Exit(code) => Ok(LaunchExit { code }),
            Behave::Err => Err(LaunchError("no such program: claude".into())),
            Behave::BlockFirst => {
                if nth == 1 {
                    self.release.notified().await;
                }
                Ok(LaunchExit::OK)
            }
            Behave::StopThenExit1 => {
                let ledger = Ledger::new(self.store.clone(), req.run.clone());
                ledger
                    .write(LedgerEvent {
                        run_id: Some(req.run.id.clone()),
                        ts: ts_now(),
                        server: "core".into(),
                        tool: "stop".into(),
                        class: None,
                        mode: Some(LedgerMode::Stop),
                        args: serde_json::json!({}),
                        result_summary: "challenge: verify you are human".into(),
                        consequences: serde_json::json!({}),
                        duration_ms: 0,
                        ok: false,
                        why: Some("challenge".into()),
                    })
                    .await
                    .expect("write stop event");
                Ok(LaunchExit { code: Some(1) })
            }
        };
        *self
            .running
            .lock()
            .unwrap()
            .get_mut(req.prompt)
            .expect("running entry") -= 1;
        out
    }
}

/// The scheduler loop under a ManualClock, its `fired` channel, and a stop switch.
struct LoopUnderTest {
    clock: Arc<ManualClock>,
    fired: mpsc::UnboundedReceiver<Fired>,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::result::Result<(), String>>,
}

impl LoopUnderTest {
    /// harnessd's Daemon: the loop plus the `.sched/` watcher.
    fn daemon(
        store: Arc<dyn MemoryStore>,
        launcher: Arc<dyn Launcher>,
        clock: Arc<ManualClock>,
        runs_root: &Path,
    ) -> LoopUnderTest {
        let (tx, fired) = mpsc::unbounded_channel();
        let d = Daemon::new(
            store,
            launcher,
            clock.clone(),
            runs_root.to_path_buf(),
            Some(tx),
        )
        .expect("Daemon::new");
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            d.run(async move {
                let _ = stopped.await;
            })
            .await
            .map_err(|e| e.to_string())
        });
        LoopUnderTest {
            clock,
            fired,
            stop: Some(stop),
            task,
        }
    }

    /// The bare core::sched loop (no watcher), for a runs root Daemon::new cannot create.
    fn scheduler(
        store: Arc<dyn MemoryStore>,
        launcher: Arc<dyn Launcher>,
        clock: Arc<ManualClock>,
        runs_root: &Path,
    ) -> LoopUnderTest {
        let (tx, fired) = mpsc::unbounded_channel();
        let s = Scheduler {
            runner: Runner {
                store,
                launcher,
                clock: clock.clone(),
                runs_root: runs_root.to_path_buf(),
            },
            wake: Arc::new(Notify::new()),
            fired: Some(tx),
        };
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            s.run_loop(async move {
                let _ = stopped.await;
            })
            .await
            .map_err(|e| e.to_string())
        });
        LoopUnderTest {
            clock,
            fired,
            stop: Some(stop),
            task,
        }
    }

    async fn deadline(&self, f: impl Fn(Ts) -> bool) -> Ts {
        timeout(CAP, self.clock.deadline_where(f))
            .await
            .expect("the loop sleeps on a deadline")
    }

    async fn next_fired(&mut self) -> Fired {
        timeout(CAP, self.fired.recv())
            .await
            .expect("a firing ended within the cap")
            .expect("fired channel open")
    }

    /// Stops the loop (it drains running firings) and returns firings not yet received.
    async fn finish(mut self) -> Vec<Fired> {
        let _ = self.stop.take().unwrap().send(());
        timeout(CAP, self.task)
            .await
            .expect("the loop stops")
            .expect("loop task joined")
            .expect("loop returned Ok");
        let mut rest = Vec::new();
        while let Ok(f) = self.fired.try_recv() {
            rest.push(f);
        }
        rest
    }
}

/// A child process's stderr, line by line, also kept whole.
struct Lines {
    rx: mpsc::UnboundedReceiver<String>,
    all: Arc<Mutex<String>>,
}

impl Lines {
    fn spawn<R: tokio::io::AsyncRead + Unpin + Send + 'static>(r: R) -> Lines {
        let (tx, rx) = mpsc::unbounded_channel();
        let all = Arc::new(Mutex::new(String::new()));
        let keep = all.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                keep.lock().unwrap().push_str(&l);
                keep.lock().unwrap().push('\n');
                let _ = tx.send(l);
            }
        });
        Lines { rx, all }
    }

    async fn until(&mut self, cap: Duration, what: &str, f: impl Fn(&str) -> bool) -> String {
        let fut = async {
            while let Some(l) = self.rx.recv().await {
                if f(&l) {
                    return Some(l);
                }
            }
            None
        };
        match timeout(cap, fut).await {
            Ok(Some(l)) => l,
            _ => panic!(
                "no stderr line {what} within {cap:?}; stderr so far:\n{}",
                self.all.lock().unwrap()
            ),
        }
    }

    fn text(&self) -> String {
        self.all.lock().unwrap().clone()
    }
}

/// The environment the binaries read (H-7), with relative runs/tools roots under `dir`.
fn bin_env(cmd: &mut tokio::process::Command, dir: &Path, db_url: &str, calls: &Path) {
    cmd.current_dir(dir)
        .env("DATABASE_URL", db_url)
        .env("SB_RUNS_ROOT", "runs")
        .env("SB_TOOLS_ROOT", "tools")
        .env("SB_VAULT_PATH", dir.join("vault.toml.age"))
        .env("SB_CLAUDE", fake_claude())
        .env("FAKE_CLAUDE_DIR", calls)
        .env("SB_VAULT_CANARY", canary())
        .env_remove("SB_RUN_ID")
        .env_remove("FAKE_CLAUDE_EXIT")
        .env_remove("FAKE_CLAUDE_LEAK")
        .kill_on_drop(true);
}

async fn run_child(name: &str, envs: &[(&str, &std::ffi::OsStr)]) -> std::process::Output {
    let exe = std::env::current_exe().unwrap();
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD_VAR, name)
        .env("SB_VAULT_CANARY", canary())
        .stdin(Stdio::null())
        .kill_on_drop(true);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = timeout(CAP_MINUTE, cmd.output())
        .await
        .expect("child test finished")
        .expect("child test ran");
    let so = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && so.contains("test result: ok. 1 passed;"),
        "child {name} failed: {:?}\nstdout:\n{so}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn is_child(name: &str) -> bool {
    std::env::var(CHILD_VAR).as_deref() == Ok(name)
}

// ------------------------------------------------------------------ acceptance item 4

/// docs/01 acceptance 4 with the real binaries: harnessd (SystemClock, fake claude through
/// SB_CLAUDE) is started first with no schedules, then `harness schedule add "*/1 * * * *"
/// "say hi"` runs as its own process. The firing happens at the next real minute.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acc4_schedule_add_fires_with_real_harness_and_harnessd() {
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let calls = dir.join("claude-calls");
    let runs = dir.join("runs");

    let mut d = tokio::process::Command::new(env!("CARGO_BIN_EXE_harnessd"));
    bin_env(&mut d, &dir, &db.url, &calls);
    d.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut harnessd = d.spawn().expect("start harnessd");
    let mut err = Lines::spawn(harnessd.stderr.take().unwrap());
    let out = Lines::spawn(harnessd.stdout.take().unwrap());
    err.until(CAP, "`harnessd: scheduling`", |l| {
        l.starts_with("harnessd: scheduling")
    })
    .await;

    let mut add = tokio::process::Command::new(env!("CARGO_BIN_EXE_harness"));
    bin_env(&mut add, &dir, &db.url, &calls);
    add.args(["schedule", "add", "*/1 * * * *", "say hi"]);
    let add = timeout(CAP, add.output()).await.unwrap().unwrap();
    let add_out = String::from_utf8_lossy(&add.stdout).into_owned();
    let add_err = String::from_utf8_lossy(&add.stderr).into_owned();
    assert!(add.status.success(), "schedule add: {add_err}");
    assert!(add_out.starts_with("schedule "), "{add_out}");

    let s = store
        .schedule_list()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.prompt == "say hi")
        .expect("schedule row");
    assert_eq!(s.cron, "*/1 * * * *");
    let parent = s.run_id.clone().expect("schedule has its run (parent)");

    // harnessd logs "harnessd: schedule <id> -> run <run> <status>" when a firing ends.
    let tag = format!("harnessd: schedule {} -> run ", s.id);
    let line = err
        .until(CAP_MINUTE, "for the firing", |l| l.starts_with(&tag))
        .await;
    let rest = line.strip_prefix(&tag).unwrap();
    let run = RunId(rest.split_whitespace().next().unwrap().to_owned());
    assert!(rest.ends_with(" Done"), "{line}");

    // runs has a row with parent_run set.
    let row = store.run_get(&run).await.unwrap().expect("runs row");
    assert_eq!(row.parent_run.as_ref(), Some(&parent), "parent_run");
    assert_eq!(row.status, RunStatus::Done);
    assert_eq!(row.prompt, "say hi");
    assert!(row.ended.is_some());
    let mut conn = db.conn().await;
    let n: i64 = sqlx::query_scalar("select count(*) from runs where parent_run = $1")
        .bind(&parent.0)
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert!(n >= 1, "runs rows with parent_run = {parent}: {n}");

    // A run folder with report.md (H-4).
    let folder = runs.join(&run.0);
    let report = std::fs::read_to_string(folder.join("report.md")).expect("report.md");
    assert!(report.starts_with("# say hi\nResult: done\n"), "{report}");
    assert_eq!(
        std::fs::read_to_string(folder.join("prompt.md")).unwrap(),
        "say hi"
    );
    let jsonl = std::fs::read_to_string(folder.join("ledger.jsonl")).unwrap();
    assert!(jsonl.contains(FIRE_TOOL), "{jsonl}");

    // D-116 (7) through the real binary: `claude -p`, prompt on stdin, SB_RUN_ID, absolute
    // SB_RUNS_ROOT (harnessd got the relative "runs"), stdout in files/answer.md.
    let argv = std::fs::read_to_string(calls.join(format!("{}.argv", run.0))).unwrap();
    assert_eq!(argv, "-p", "claude argv");
    let stdin = std::fs::read_to_string(calls.join(format!("{}.stdin", run.0))).unwrap();
    assert_eq!(stdin, "say hi");
    let envs = std::fs::read_to_string(calls.join(format!("{}.env", run.0))).unwrap();
    assert!(envs.contains(&format!("SB_RUN_ID={}\n", run.0)), "{envs}");
    let root = envs
        .lines()
        .find_map(|l| l.strip_prefix("SB_RUNS_ROOT="))
        .unwrap();
    assert!(Path::new(root).is_absolute(), "SB_RUNS_ROOT {root:?}");
    assert_eq!(
        std::fs::canonicalize(root).unwrap(),
        std::fs::canonicalize(&runs).unwrap()
    );
    let answer = std::fs::read_to_string(folder.join("files/answer.md")).unwrap();
    assert_eq!(answer, "fake claude: say hi");
    let s2 = store
        .schedule_list()
        .await
        .unwrap()
        .into_iter()
        .find(|x| x.id == s.id)
        .unwrap();
    assert!(s2.last_run.is_some());

    harnessd.kill().await.unwrap();
    // Canary: not a vault value here (harnessd's vault key is the OS keychain, no test
    // override by design), but it is in every process environment, so nothing may print it.
    let c = canary();
    let dump = db.dump().await;
    assert!(!dump.contains(&c), "canary in a table");
    assert!(!add_out.contains(&c) && !add_err.contains(&c));
    assert!(!err.text().contains(&c) && !out.text().contains(&c));
    assert!(files_containing(&runs, &c).is_empty());
    eprintln!("acc4 run {run} parent {parent}");
}

// ------------------------------------------------------------------ D-116 (1), (2)

/// (1) Launcher and Clock are core traits; harness implements ClaudeLauncher and re-exports.
#[test]
fn d116_1_launcher_and_clock_live_in_core() {
    let l: Arc<dyn harness::Launcher> = Arc::new(ClaudeLauncher::print("claude"));
    // Same trait object type: harness::Launcher is the core trait re-exported.
    let _core: Arc<dyn harness_core::sched::Launcher> = l;
    let _c1: Arc<dyn Clock> = Arc::new(SystemClock);
    let _c2: Arc<dyn Clock> = Arc::new(ManualClock::new(ts_now()));
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let own: Vec<PathBuf> = files_containing(&src, "trait Launcher")
        .into_iter()
        .chain(files_containing(&src, "trait Clock"))
        .collect();
    assert!(own.is_empty(), "harness defines its own trait: {own:?}");
    let core =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/src/sched.rs"))
            .unwrap();
    assert!(core.contains("pub trait Launcher") && core.contains("pub trait Clock"));
}

/// (2) One crate, a lib and two bins (harness, harnessd), with the named deps.
#[tokio::test]
async fn d116_2_one_crate_lib_and_two_bins() {
    for (bin, must) in [
        (
            env!("CARGO_BIN_EXE_harness"),
            &[
                "run", "schedule", "vault", "gate", "tools", "ledger", "db", "ask",
            ][..],
        ),
        (env!("CARGO_BIN_EXE_harnessd"), &["harnessd"][..]),
    ] {
        let out = tokio::process::Command::new(bin)
            .arg("--help")
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "{bin} --help");
        let text = String::from_utf8_lossy(&out.stdout);
        for m in must {
            assert!(text.contains(m), "{bin} --help lacks {m}:\n{text}");
        }
    }
    let toml =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    for dep in [
        "clap",
        "rpassword",
        "core-pg",
        "toolstore",
        "notify",
        "sqlx",
    ] {
        assert!(
            toml.lines()
                .any(|l| l.starts_with(&format!("{dep}.")) || l.starts_with(&format!("{dep} "))),
            "harness Cargo.toml lacks {dep}"
        );
    }
    assert!(toml.contains("[lib]") && toml.matches("[[bin]]").count() == 2);
}

// ------------------------------------------------------------------ D-116 (3)

/// (3) The `.sched/changed` marker wakes an idle harnessd and makes it re-read schedules;
/// without it nothing re-reads (no poll interval). harnessd's only deadline is next new year
/// and the clock never moves, so the only thing that can cause a second `schedule_list` is
/// the marker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d116_3_marker_wakes_idle_loop_and_nothing_polls() {
    let (_db, pg) = TestDb::store().await;
    let obs = ObservedStore::new(pg.clone());
    let tmp = tempfile::tempdir().unwrap();
    let runs = tmp.path().join("runs");
    let start = t(12, 0, 30);
    pg.schedule_add(cron_row("0 0 1 1 *", "new year", None))
        .await
        .unwrap();
    let launcher = TestLauncher::new(pg.clone(), |_| Behave::Ok);
    let clock = Arc::new(ManualClock::new(start));
    let lp = LoopUnderTest::daemon(obs.clone(), launcher.clone(), clock.clone(), &runs);
    let new_year = Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap();
    assert_eq!(lp.deadline(|x| x == new_year).await, new_year);
    obs.wait_lists(1).await;
    assert_eq!(obs.lists(), 1);

    // A schedule written straight to the store, without the marker: never seen. The wait
    // is on the list counter; its cap is the expected outcome (no event exists for "did
    // not happen"), 5 s covers any poll interval worth the name.
    pg.schedule_add(cron_row("*/1 * * * *", "unseen", None))
        .await
        .unwrap();
    let mut rx = obs.lists.subscribe();
    let polled = timeout(Duration::from_secs(5), rx.wait_for(|c| *c > 1)).await;
    assert!(
        polled.is_err(),
        "schedule_list was called again with no marker"
    );

    // The marker exactly as `harness schedule add|rm` writes it.
    sched::notify_changed(&runs).unwrap();
    obs.wait_lists(2).await;
    let minute = t(12, 1, 0);
    assert_eq!(lp.deadline(|x| x == minute).await, minute);
    let rest = lp.finish().await;
    assert!(rest.is_empty());
    assert_eq!(launcher.count("unseen"), 0);
}

/// (3) the marker side: `harness schedule add` and `rm` through the real binary write
/// `<runs_root>/.sched/changed`, and the folder harnessd watches is that one.
#[tokio::test]
async fn d116_3_cli_add_and_rm_write_the_marker() {
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("runs/.sched/changed");
    let calls = tmp.path().join("calls");
    let run = |args: Vec<String>| {
        let mut c = tokio::process::Command::new(env!("CARGO_BIN_EXE_harness"));
        bin_env(&mut c, tmp.path(), &db.url, &calls);
        c.args(args);
        async move { timeout(CAP, c.output()).await.unwrap().unwrap() }
    };
    assert!(!marker.exists());
    let o = run(vec![
        "schedule".into(),
        "add".into(),
        "0 9 * * *".into(),
        "check mail".into(),
    ])
    .await;
    assert!(o.status.success());
    let first = std::fs::read_to_string(&marker).expect("marker after add");
    std::fs::remove_file(&marker).unwrap();
    let id = store.schedule_list().await.unwrap()[0].id;
    let o = run(vec!["schedule".into(), "rm".into(), id.to_string()]).await;
    assert!(o.status.success());
    let second = std::fs::read_to_string(&marker).expect("marker after rm");
    assert!(!first.is_empty() && !second.is_empty());
    assert_eq!(
        sched::changed_dir(&tmp.path().join("runs")),
        tmp.path().join("runs").join(sched::CHANGED_DIR)
    );
}

// ------------------------------------------------------------------ D-116 (4)

/// (4) Due rule: next occurrence strictly after max(last_run, first seen, last attempt);
/// missed occurrences coalesce into one firing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d116_4_due_rule_and_coalescing() {
    let (_db, pg) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let runs = tmp.path().join("runs");
    // a: last ran ten minutes before harnessd started (it was down): no burst at start.
    let a = pg
        .schedule_add(cron_row("*/1 * * * *", "due a", Some(t(11, 50, 0))))
        .await
        .unwrap();
    // b: last_run later than first seen: the next occurrence counts from last_run.
    let b = pg
        .schedule_add(cron_row("*/1 * * * *", "due b", Some(t(12, 2, 0))))
        .await
        .unwrap();
    let launcher = TestLauncher::new(pg.clone(), |_| Behave::Ok);
    let clock = Arc::new(ManualClock::new(t(12, 0, 30)));
    let mut lp = LoopUnderTest::daemon(pg.clone(), launcher.clone(), clock.clone(), &runs);

    assert_eq!(lp.deadline(|_| true).await, t(12, 1, 0));
    clock.set(t(12, 1, 0));
    let f = lp.next_fired().await;
    assert_eq!(f.schedule, a);
    assert_eq!(f.result.unwrap().run.0, "2026-10-25T12-01-00-due-a");
    assert_eq!(lp.deadline(|x| x > t(12, 1, 0)).await, t(12, 2, 0));

    // The loop slept through ten minutes (suspend): one firing each, not one per minute.
    clock.set(t(12, 11, 0));
    let f1 = lp.next_fired().await;
    let f2 = lp.next_fired().await;
    let mut got: Vec<(i32, String)> = [f1, f2]
        .into_iter()
        .map(|f| (f.schedule, f.result.unwrap().run.0))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            (a, "2026-10-25T12-11-00-due-a".to_owned()),
            (b, "2026-10-25T12-11-00-due-b".to_owned())
        ]
    );
    assert_eq!(lp.deadline(|x| x > t(12, 11, 0)).await, t(12, 12, 0));
    let rest = lp.finish().await;
    assert!(rest.is_empty(), "extra firings: {}", rest.len());
    assert_eq!(launcher.count("due a"), 2);
    assert_eq!(launcher.count("due b"), 1);
    for s in pg.schedule_list().await.unwrap() {
        assert_eq!(s.last_run, Some(t(12, 11, 0)), "{}", s.prompt);
    }
}

/// (4) A schedule whose previous firing is still running skips.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d116_4_skips_while_previous_firing_runs() {
    let (_db, pg) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let runs = tmp.path().join("runs");
    let slow = pg
        .schedule_add(cron_row("*/1 * * * *", "slow", None))
        .await
        .unwrap();
    let fast = pg
        .schedule_add(cron_row("*/1 * * * *", "fast", None))
        .await
        .unwrap();
    let launcher = TestLauncher::new(pg.clone(), |p| {
        if p == "slow" {
            Behave::BlockFirst
        } else {
            Behave::Ok
        }
    });
    let clock = Arc::new(ManualClock::new(t(12, 0, 30)));
    let mut lp = LoopUnderTest::daemon(pg.clone(), launcher.clone(), clock.clone(), &runs);

    assert_eq!(lp.deadline(|_| true).await, t(12, 1, 0));
    clock.set(t(12, 1, 0));
    assert_eq!(lp.next_fired().await.schedule, fast);
    launcher.wait_launches(2).await;
    assert_eq!(lp.deadline(|x| x > t(12, 1, 0)).await, t(12, 2, 0));
    clock.set(t(12, 2, 0));
    assert_eq!(lp.next_fired().await.schedule, fast);
    assert_eq!(lp.deadline(|x| x > t(12, 2, 0)).await, t(12, 3, 0));
    // slow was due at 12:02 but its 12:01 firing still runs: skipped.
    assert_eq!(launcher.count("slow"), 1);
    assert_eq!(launcher.count("fast"), 2);

    launcher.release.notify_one();
    let f = lp.next_fired().await;
    assert_eq!(f.schedule, slow);
    assert_eq!(f.result.unwrap().run.0, "2026-10-25T12-01-00-slow");
    let _ = lp.finish().await;
    assert_eq!(launcher.max_running("slow"), 1, "two slow firings at once");
    assert!(
        launcher.count("slow") <= 2,
        "skipped occurrences coalesce into at most one"
    );
}

/// (4) schedule_fired is written before the run starts, so a failing firing never hot-loops:
/// (a) a launcher that fails sees last_run already set; (b) when schedule_fired itself fails
/// the run never starts and the loop's last attempt still moves the next due time; (c) a
/// firing that fails before launch (run folder) has last_run set and waits a minute.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d116_4_schedule_fired_before_launch_and_no_hot_loop() {
    let (db, pg) = TestDb::store().await;
    let obs = ObservedStore::new(pg.clone());
    let tmp = tempfile::tempdir().unwrap();
    let runs = tmp.path().join("runs");
    let first = pg
        .schedule_add(cron_row("*/1 * * * *", "fails", None))
        .await
        .unwrap();
    let nofire = pg
        .schedule_add(cron_row("*/1 * * * *", "nofire", None))
        .await
        .unwrap();
    obs.fail_fired.lock().unwrap().insert(nofire);
    let launcher = TestLauncher::new(pg.clone(), |_| Behave::Err);
    let clock = Arc::new(ManualClock::new(t(12, 0, 30)));
    let mut lp = LoopUnderTest::daemon(obs.clone(), launcher.clone(), clock.clone(), &runs);

    assert_eq!(lp.deadline(|_| true).await, t(12, 1, 0));
    clock.set(t(12, 1, 0));
    let mut by_id = HashMap::new();
    for _ in 0..2 {
        let f = lp.next_fired().await;
        by_id.insert(f.schedule, f.result);
    }
    let failed = by_id
        .remove(&first)
        .unwrap()
        .expect("a launch failure is a finished run");
    assert_eq!(failed.status, RunStatus::Failed);
    assert!(matches!(
        by_id.remove(&nofire).unwrap(),
        Err(SchedError::Memory(_))
    ));
    // Not re-fired at 12:01: both wait for 12:02.
    assert_eq!(lp.deadline(|x| x > t(12, 1, 0)).await, t(12, 2, 0));
    let rest = lp.finish().await;
    assert!(rest.is_empty());
    let seen = launcher.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].prompt, "fails");
    assert_eq!(seen[0].run, failed.run);
    assert_eq!(
        seen[0].last_run_at_launch,
        Some(Some(t(12, 1, 0))),
        "last_run was not written before launch"
    );
    let mut conn = db.conn().await;
    let n: i64 = sqlx::query_scalar("select count(*) from runs where prompt = 'nofire'")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(n, 0, "a run started although schedule_fired failed");

    // (c) A runs root under a regular file: the run folder cannot be created.
    let (_db2, pg2) = TestDb::store().await;
    let blocker = tmp.path().join("not-a-dir");
    std::fs::write(&blocker, "x").unwrap();
    let broken = pg2
        .schedule_add(cron_row("*/1 * * * *", "broken", None))
        .await
        .unwrap();
    let l2 = TestLauncher::new(pg2.clone(), |_| Behave::Ok);
    let clock2 = Arc::new(ManualClock::new(t(12, 0, 30)));
    let mut lp2 = LoopUnderTest::scheduler(
        pg2.clone(),
        l2.clone(),
        clock2.clone(),
        &blocker.join("runs"),
    );
    assert_eq!(lp2.deadline(|_| true).await, t(12, 1, 0));
    clock2.set(t(12, 1, 0));
    let f = lp2.next_fired().await;
    assert_eq!(f.schedule, broken);
    assert!(matches!(f.result, Err(SchedError::Io(_))), "{:?}", f.result);
    assert_eq!(lp2.deadline(|x| x > t(12, 1, 0)).await, t(12, 2, 0));
    assert!(lp2.finish().await.is_empty());
    assert_eq!(l2.count("broken"), 0);
    let s = &pg2.schedule_list().await.unwrap()[0];
    assert_eq!(s.last_run, Some(t(12, 1, 0)));
}

// ------------------------------------------------------------------ D-116 (5)

/// (5) watch and reflect rows are stored, the loop skips them, fire() refuses them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d116_5_watch_and_reflect_skipped_by_loop_and_fire() {
    let (_db, pg) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let runs = tmp.path().join("runs");
    let mut w = cron_row("*/1 * * * *", "watch it", None);
    w.kind = ScheduleKind::Watch;
    w.watch_target = Some("jobs".into());
    let mut r = cron_row("*/1 * * * *", "reflect", None);
    r.kind = ScheduleKind::Reflect;
    pg.schedule_add(w).await.unwrap();
    pg.schedule_add(r).await.unwrap();
    let tick = pg
        .schedule_add(cron_row("*/1 * * * *", "tick", None))
        .await
        .unwrap();
    let launcher = TestLauncher::new(pg.clone(), |_| Behave::Ok);
    let clock = Arc::new(ManualClock::new(t(12, 0, 30)));
    let mut lp = LoopUnderTest::daemon(pg.clone(), launcher.clone(), clock.clone(), &runs);
    assert_eq!(lp.deadline(|_| true).await, t(12, 1, 0));
    clock.set(t(12, 1, 0));
    assert_eq!(lp.next_fired().await.schedule, tick);
    assert_eq!(lp.deadline(|x| x > t(12, 1, 0)).await, t(12, 2, 0));
    clock.set(t(12, 2, 0));
    assert_eq!(lp.next_fired().await.schedule, tick);
    assert!(lp.finish().await.is_empty());
    let prompts: Vec<String> = launcher
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.prompt.clone())
        .collect();
    assert_eq!(prompts, ["tick", "tick"]);

    let runner = Runner {
        store: pg.clone(),
        launcher: launcher.clone(),
        clock: clock.clone(),
        runs_root: runs.clone(),
    };
    for s in pg.schedule_list().await.unwrap() {
        match s.kind {
            ScheduleKind::Watch => {
                assert!(matches!(
                    runner.fire(&s).await,
                    Err(SchedError::NotInPhase1("watch"))
                ));
                assert_eq!(s.last_run, None);
            }
            ScheduleKind::Reflect => {
                assert!(matches!(
                    runner.fire(&s).await,
                    Err(SchedError::NotInPhase1("reflect"))
                ));
                assert_eq!(s.last_run, None);
            }
            ScheduleKind::Cron => {}
        }
    }
    // fire() refused before schedule_fired: last_run still unset.
    for s in pg.schedule_list().await.unwrap() {
        if s.kind != ScheduleKind::Cron {
            assert_eq!(s.last_run, None, "{}", s.prompt);
        }
    }
    assert_eq!(launcher.count("watch it") + launcher.count("reflect"), 0);
}

/// (5) "with one log line": the real harnessd prints one line per watch/reflect row, not
/// one per pass. Two more passes are forced (marker writes) and observed through the
/// one-time warning of a bad-cron row inserted before each.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d116_5_harnessd_logs_one_line_per_skipped_row() {
    let (db, pg) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let calls = dir.join("calls");
    let mut w = cron_row("*/1 * * * *", "watch it", None);
    w.kind = ScheduleKind::Watch;
    w.watch_target = Some("jobs".into());
    let mut r = cron_row("*/1 * * * *", "reflect", None);
    r.kind = ScheduleKind::Reflect;
    let wid = pg.schedule_add(w).await.unwrap();
    let rid = pg.schedule_add(r).await.unwrap();

    let mut d = tokio::process::Command::new(env!("CARGO_BIN_EXE_harnessd"));
    bin_env(&mut d, &dir, &db.url, &calls);
    d.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut harnessd = d.spawn().unwrap();
    let mut err = Lines::spawn(harnessd.stderr.take().unwrap());
    err.until(CAP, "scheduling", |l| l.starts_with("harnessd: scheduling"))
        .await;
    for (i, bad) in ["bad one", "bad two"].into_iter().enumerate() {
        let id = pg
            .schedule_add(cron_row(bad, &format!("bad {i}"), None))
            .await
            .unwrap();
        sched::notify_changed(&dir.join("runs")).unwrap();
        let tag = format!("harnessd: schedule {id}: ");
        err.until(CAP, "for the bad cron row", |l| l.starts_with(&tag))
            .await;
    }
    harnessd.kill().await.unwrap();
    let text = err.text();
    let skipped: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("not in Phase 1"))
        .collect();
    assert_eq!(skipped.len(), 2, "{text}");
    assert!(
        skipped
            .iter()
            .any(|l| l.starts_with(&format!("harnessd: schedule {wid}: kind watch"))),
        "{text}"
    );
    assert!(
        skipped
            .iter()
            .any(|l| l.starts_with(&format!("harnessd: schedule {rid}: kind reflect"))),
        "{text}"
    );
    assert!(
        !calls.exists() || std::fs::read_dir(&calls).unwrap().next().is_none(),
        "claude was launched for a skipped row"
    );
}

// ------------------------------------------------------------------ D-116 (6)

/// (6) A firing writes core `sched.fire`; a launcher error or non-zero exit writes a core
/// `launch` event (ok false), sets runs.status failed unless the ledger says stopped, and
/// the report's Result line says failed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d116_6_launch_failure_marks_run_failed() {
    let (_db, pg) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let runs = tmp.path().join("runs");
    let prompts = ["err", "code3", "killed", "stopped"];
    let mut ids = HashMap::new();
    for p in prompts {
        ids.insert(
            pg.schedule_add(cron_row("*/1 * * * *", p, None))
                .await
                .unwrap(),
            p,
        );
    }
    let launcher = TestLauncher::new(pg.clone(), |p| match p {
        "err" => Behave::Err,
        "code3" => Behave::Exit(Some(3)),
        "killed" => Behave::Exit(None),
        _ => Behave::StopThenExit1,
    });
    let clock = Arc::new(ManualClock::new(t(12, 0, 30)));
    let mut lp = LoopUnderTest::daemon(pg.clone(), launcher.clone(), clock.clone(), &runs);
    assert_eq!(lp.deadline(|_| true).await, t(12, 1, 0));
    clock.set(t(12, 1, 0));
    let mut done = HashMap::new();
    for _ in 0..4 {
        let f = lp.next_fired().await;
        let p = ids[&f.schedule];
        done.insert(p, f.result.unwrap_or_else(|e| panic!("{p}: {e}")));
    }
    assert!(lp.finish().await.is_empty());

    for (p, why) in [
        ("err", "launcher: no such program: claude"),
        ("code3", "session exited with code 3"),
        ("killed", "session was killed"),
    ] {
        let f = &done[p];
        assert_eq!(f.status, RunStatus::Failed, "{p}");
        let row = pg.run_get(&f.run).await.unwrap().unwrap();
        assert_eq!(row.status, RunStatus::Failed, "{p} runs.status");
        let report = std::fs::read_to_string(f.folder.join("report.md")).unwrap();
        assert!(
            report.contains(&format!("\nResult: failed ({why})\n")),
            "{p}: {report}"
        );
        let ev: Vec<LedgerEvent> = pg
            .ledger_events(&f.run)
            .await
            .unwrap()
            .into_iter()
            .map(|(_, e)| e)
            .collect();
        assert_eq!(ev.len(), 2, "{p}: {ev:?}");
        assert_eq!(
            (ev[0].server.as_str(), ev[0].tool.as_str(), ev[0].ok),
            ("core", FIRE_TOOL, true)
        );
        assert_eq!(
            (ev[1].server.as_str(), ev[1].tool.as_str(), ev[1].ok),
            ("core", LAUNCH_TOOL, false)
        );
        assert_eq!(ev[1].why.as_deref(), Some(why), "{p}");
        let jsonl = std::fs::read_to_string(f.folder.join("ledger.jsonl")).unwrap();
        assert_eq!(jsonl.lines().count(), 2, "{p}");
    }
    let f = &done["stopped"];
    assert_eq!(f.status, RunStatus::Stopped);
    assert_eq!(
        pg.run_get(&f.run).await.unwrap().unwrap().status,
        RunStatus::Stopped
    );
    let report = std::fs::read_to_string(f.folder.join("report.md")).unwrap();
    assert!(!report.contains("Result: failed"), "{report}");
}

/// (6) and (7) with the real ClaudeLauncher: a fake claude exiting 3, and a missing program.
#[tokio::test]
async fn d116_6_claude_launcher_exit_code_and_missing_program() {
    let tmp = tempfile::tempdir().unwrap();
    run_child(
        "child_claude_launcher_failures",
        &[("CHILD_DIR", tmp.path().as_os_str())],
    )
    .await;
}

#[tokio::test]
async fn child_claude_launcher_failures() {
    if !is_child("child_claude_launcher_failures") {
        return;
    }
    let dir = PathBuf::from(std::env::var("CHILD_DIR").unwrap());
    let runs = dir.join("runs");
    let calls = dir.join("calls");
    std::env::set_var("FAKE_CLAUDE_DIR", &calls);
    std::env::set_var("FAKE_CLAUDE_EXIT", "3");
    let store: Arc<dyn MemoryStore> = Arc::new(harness_core::testing::MemStore::new());
    let runner = |l: ClaudeLauncher| Runner {
        store: store.clone(),
        launcher: Arc::new(l),
        clock: Arc::new(SystemClock),
        runs_root: runs.clone(),
    };
    let spec = RunSpec {
        prompt: "exit three",
        parent_run: None,
        strict: false,
        schedule: None,
    };
    let f = runner(ClaudeLauncher::print(fake_claude()))
        .execute(spec)
        .await
        .unwrap();
    assert_eq!(f.status, RunStatus::Failed);
    assert_eq!(
        store.run_get(&f.run).await.unwrap().unwrap().status,
        RunStatus::Failed
    );
    let report = std::fs::read_to_string(f.folder.join("report.md")).unwrap();
    assert!(
        report.contains("Result: failed (session exited with code 3)"),
        "{report}"
    );
    assert!(calls.join(format!("{}.argv", f.run.0)).is_file());

    let f = runner(ClaudeLauncher::print(dir.join("no-such-claude")))
        .execute(RunSpec {
            prompt: "missing",
            ..spec
        })
        .await
        .unwrap();
    assert_eq!(f.status, RunStatus::Failed);
    let report = std::fs::read_to_string(f.folder.join("report.md")).unwrap();
    assert!(
        report.contains("Result: failed (launcher: cannot start"),
        "{report}"
    );
}

// ------------------------------------------------------------------ D-116 (7)

/// (7) ClaudeLauncher: `claude -p`, the scrubbed prompt on stdin (a prompt starting with '-'
/// and Windows metacharacters arrive intact, never in argv), SB_RUN_ID and an absolute
/// SB_RUNS_ROOT, stdout scrubbed into files/answer.md; `harness run` mode is `claude
/// <prompt>`. The vault canary is planted as `@vault(test)` and leaked by the fake session;
/// the child's stdout and stderr, the run folder and the ledger are grepped.
#[tokio::test]
async fn d116_7_prompt_on_stdin_and_scrubbed_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_child(
        "child_claude_launcher_stdin_scrub",
        &[
            ("CHILD_DIR", tmp.path().as_os_str()),
            ("FAKE_CLAUDE_LEAK", std::ffi::OsStr::new("1")),
        ],
    )
    .await;
    let c = canary();
    assert!(!String::from_utf8_lossy(&out.stdout).contains(&c), "stdout");
    assert!(!String::from_utf8_lossy(&out.stderr).contains(&c), "stderr");
    let runs = tmp.path().join("runs");
    let hits = files_containing(&runs, &c);
    assert!(hits.is_empty(), "canary in runs/: {hits:?}");
    // The fake's own records (outside runs/) prove the canary really was leaked to stdout
    // only by the session: the stdin it received is scrubbed.
    let calls = tmp.path().join("calls");
    for e in std::fs::read_dir(&calls).unwrap().flatten() {
        if e.path().extension().is_some_and(|x| x == "stdin") {
            let s = std::fs::read_to_string(e.path()).unwrap();
            assert!(!s.contains(&c), "prompt on stdin held the value");
        }
    }
}

#[tokio::test]
async fn child_claude_launcher_stdin_scrub() {
    if !is_child("child_claude_launcher_stdin_scrub") {
        return;
    }
    let c = canary();
    let dir = PathBuf::from(std::env::var("CHILD_DIR").unwrap());
    let calls = dir.join("calls");
    std::env::set_var("FAKE_CLAUDE_DIR", &calls);
    Vault::in_memory([("test".to_owned(), c.clone(), EntryKind::Secret)]).install_global();

    // A relative runs root: the session must still get an absolute SB_RUNS_ROOT.
    let cwd = std::env::current_dir().unwrap();
    let runs_abs = dir.join("runs");
    std::fs::create_dir_all(&runs_abs).unwrap();
    let runs_rel = relative_to(&runs_abs, &cwd);
    assert!(runs_rel.is_relative(), "{runs_rel:?}");

    let store: Arc<dyn MemoryStore> = Arc::new(harness_core::testing::MemStore::new());
    let runner = Runner {
        store: store.clone(),
        launcher: Arc::new(ClaudeLauncher::print(fake_claude())),
        clock: Arc::new(SystemClock),
        runs_root: runs_rel.clone(),
    };
    let raw = format!("--version\nsay \"hi\" & | ^ %PATH% to {c} and @vault(test)");
    let sched_row = Schedule {
        id: 7,
        ..cron_row("*/1 * * * *", &raw, None)
    };
    let f = runner
        .execute(RunSpec {
            prompt: &raw,
            parent_run: None,
            strict: false,
            schedule: Some(&sched_row),
        })
        .await
        .unwrap();
    assert_eq!(f.status, RunStatus::Done, "{:?}", f.launch_problem);
    let scrubbed = "--version\nsay \"hi\" & | ^ %PATH% to @vault(test) and @vault(test)";
    let argv = std::fs::read_to_string(calls.join(format!("{}.argv", f.run.0))).unwrap();
    assert_eq!(argv, "-p", "the prompt must not be in argv");
    let stdin = std::fs::read_to_string(calls.join(format!("{}.stdin", f.run.0))).unwrap();
    assert_eq!(stdin, scrubbed);
    let envs = std::fs::read_to_string(calls.join(format!("{}.env", f.run.0))).unwrap();
    assert!(envs.contains(&format!("SB_RUN_ID={}\n", f.run.0)), "{envs}");
    let root = envs
        .lines()
        .find_map(|l| l.strip_prefix("SB_RUNS_ROOT="))
        .unwrap();
    assert!(Path::new(root).is_absolute(), "SB_RUNS_ROOT {root:?}");
    assert_eq!(
        std::fs::canonicalize(root).unwrap(),
        std::fs::canonicalize(&runs_abs).unwrap()
    );
    let answer = std::fs::read_to_string(runs_abs.join(&f.run.0).join("files/answer.md")).unwrap();
    assert!(!answer.contains(&c), "answer.md holds the vault value");
    assert!(answer.starts_with("fake claude: --version\n"), "{answer}");
    assert!(answer.contains("leaked: @vault(test)"), "{answer}");
    let row = store.run_get(&f.run).await.unwrap().unwrap();
    assert_eq!(row.prompt, scrubbed, "runs.prompt");
    for (_, e) in store.ledger_events(&f.run).await.unwrap() {
        let j = serde_json::to_string(&e).unwrap();
        assert!(!j.contains(&c), "ledger row holds the value");
    }

    // `harness run` mode: interactive `claude <prompt>` (a leading '-' gets a space).
    let run = RunFolder::create(&runs_abs, "interactive", "-h").unwrap();
    let exit = ClaudeLauncher::interactive(fake_claude())
        .launch(LaunchRequest {
            run: &run,
            runs_root: &runs_abs,
            prompt: "-h",
        })
        .await
        .unwrap();
    assert!(exit.success());
    let argv = std::fs::read_to_string(calls.join(format!("{}.argv", run.id.0))).unwrap();
    assert_eq!(argv, " -h");
    println!("child ok, run {}", f.run);
}

/// `target` relative to `base` (both absolute, same drive).
fn relative_to(target: &Path, base: &Path) -> PathBuf {
    let t: Vec<_> = target.components().collect();
    let b: Vec<_> = base.components().collect();
    let common = t.iter().zip(&b).take_while(|(x, y)| x == y).count();
    assert!(common > 0, "{target:?} and {base:?} share no root");
    let mut p = PathBuf::new();
    for _ in common..b.len() {
        p.push("..");
    }
    for c in &t[common..] {
        p.push(c.as_os_str());
    }
    p
}

// ------------------------------------------------------------------ H-7 vault set

/// `harness vault set` never echoes or prints the value. (a) The real binary with no
/// console and the canary piped on stdin: it refuses (rpassword reads only the console),
/// never reads stdin, prints no canary. (b) The CLI code path with the canary as the typed
/// value, run in a child process whose whole stdout and stderr are grepped.
#[tokio::test]
async fn vault_set_never_prints_the_value() {
    let c = canary();
    let tmp = tempfile::tempdir().unwrap();

    // (a)
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_harness"));
    cmd.args(["vault", "set", "test"])
        .env("SB_VAULT_PATH", tmp.path().join("bin-vault.toml.age"))
        .env("SB_VAULT_CANARY", &c)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(DETACHED_PROCESS);
    }
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(format!("{c}\n{c}\n").as_bytes()).await;
    drop(stdin);
    let out = timeout(CAP, child.wait_with_output())
        .await
        .expect("vault set without a terminal returns")
        .unwrap();
    let so = String::from_utf8_lossy(&out.stdout).into_owned();
    let se = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!so.contains(&c) && !se.contains(&c), "canary printed");
    assert_eq!(out.status.code(), Some(1), "stdout {so:?} stderr {se:?}");
    assert!(se.starts_with("harness: "), "{se}");
    assert!(
        !tmp.path().join("bin-vault.toml.age").exists(),
        "a value read from stdin was stored"
    );

    // (b)
    let out = run_child(
        "child_vault_set_cli",
        &[("CHILD_DIR", tmp.path().as_os_str())],
    )
    .await;
    let so = String::from_utf8_lossy(&out.stdout).into_owned();
    let se = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(so.contains("vault: test set (secret)\n"), "{so}");
    assert!(so.contains("vault: who set (personal)\n"), "{so}");
    assert!(se.contains("harness: vault name"), "{se}");
    assert!(!so.contains(&c), "canary on stdout");
    assert!(!se.contains(&c), "canary on stderr");
    let hits = files_containing(tmp.path(), &c);
    assert!(hits.is_empty(), "canary in clear on disk: {hits:?}");
    // The value really was stored (so the greps above were meaningful).
    let v = Vault::open(
        &tmp.path().join("vault.toml.age"),
        &KeySource::IdentityFile(tmp.path().join("vault.key")),
    )
    .unwrap();
    assert!(v.resolve_into("@vault(test)", |r| r.expose() == c).unwrap());
}

#[tokio::test]
async fn child_vault_set_cli() {
    if !is_child("child_vault_set_cli") {
        return;
    }
    let c = canary();
    let dir = PathBuf::from(std::env::var("CHILD_DIR").unwrap());
    let env = Env {
        database_url: "postgres://unused".into(),
        runs_root: dir.join("runs"),
        tools_root: dir.join("tools"),
        vault_path: dir.join("vault.toml.age"),
        vault_key: KeySource::IdentityFile(dir.join("vault.key")),
        run_id: None,
        claude: dir.join("no-claude"),
    };
    let value = c.clone();
    let ctx = Ctx {
        env,
        store: Some(Arc::new(harness_core::testing::MemStore::new())),
        read_secret: Box::new(move |prompt| {
            // What rpassword would print as the prompt goes to stderr here so it is grepped.
            eprint!("{prompt}");
            Ok(Zeroizing::new(value.clone()))
        }),
        launcher: None,
    };
    for args in [
        vec!["vault", "set", "test"],
        vec!["vault", "set", "who", "--personal"],
        vec!["vault", "set", "test"],
        vec!["vault", "set", "bad name"],
    ] {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(&args);
        let cli = Cli::try_parse_from(&argv).unwrap();
        let mut out = std::io::stdout();
        // As main.rs does: the scrubbed Display chain on stderr.
        if let Err(e) = execute(cli, &ctx, &mut out).await {
            eprintln!("harness: {}", harness_core::scrub(&format!("{e:#}")));
        }
    }
}

// ------------------------------------------------------------------ H-7 ask (B-177)

/// `harness ask` on a closed ask prints ask::answer's AlreadyExists message as is.
#[tokio::test]
async fn ask_closed_prints_already_exists_message() {
    let tmp = tempfile::tempdir().unwrap();
    let runs = tmp.path().join("runs");
    let run = RunFolder::create(&runs, "ask test", "p").unwrap();
    for n in [1, 2] {
        let card = harness_core::ask::card_path(&run.dir, n);
        std::fs::create_dir_all(card.parent().unwrap()).unwrap();
        std::fs::write(&card, "{}").unwrap();
    }
    // Ask 2 timed out: FileAsker claims the answer file with "timeout".
    std::fs::write(harness_core::ask::answer_path(&run.dir, 2), "timeout\n").unwrap();
    let ask = |n: &str, yn: &str| {
        let mut c = tokio::process::Command::new(env!("CARGO_BIN_EXE_harness"));
        c.args(["ask", &run.id.0, n, yn])
            .current_dir(tmp.path())
            .env("SB_RUNS_ROOT", "runs")
            .env("SB_VAULT_PATH", tmp.path().join("vault.toml.age"))
            .env("SB_VAULT_CANARY", canary())
            .env_remove("SB_RUN_ID")
            .kill_on_drop(true);
        async move { timeout(CAP, c.output()).await.unwrap().unwrap() }
    };
    let o = ask("1", "yes").await;
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!("ask 1 in run {}: yes\n", run.id)
    );
    let o = ask("1", "no").await;
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "harness: ask 1 in run {} is already answered (yes); nothing changed\n",
            run.id
        )
    );
    assert_eq!(
        std::fs::read_to_string(harness_core::ask::answer_path(&run.dir, 1))
            .unwrap()
            .trim(),
        "yes"
    );
    let o = ask("2", "yes").await;
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "harness: ask 2 in run {} timed out before this answer; nothing was done with it\n",
            run.id
        )
    );
}
