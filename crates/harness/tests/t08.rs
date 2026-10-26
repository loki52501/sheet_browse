//! T-08 against a real Postgres: docs/01 step-1 acceptance item 4 and the H-7 CLI.
//!
//! "`harness schedule add "*/1 * * * *" "say hi"` fires, creates a run folder with
//! `report.md`, and `runs` has a row with `parent_run` set."
//!
//! Each test gets a throwaway database `sb_t_<random>` on SB_TEST_DATABASE_URL (D-100),
//! dropped at the end. The launcher is a fake that records the request and exits 0 (no model,
//! CLAUDE.md rule 1). harnessd's clock is a `ManualClock`: the test waits until the loop
//! sleeps on the deadline it computed, then moves the clock there. No fixed sleeps: every wait
//! is on a condition with a cap.

use async_trait::async_trait;
use clap::Parser;
use harness::cli::{execute, Cli, Ctx};
use harness::daemon::Daemon;
use harness::{CliError, Env};
use harness_core::class::Mode;
use harness_core::memory::*;
use harness_core::run::{RunFolder, RunId};
use harness_core::sched::{
    next_after, parse_cron, LaunchError, LaunchExit, LaunchRequest, Launcher, ManualClock,
    FIRE_TOOL,
};
use harness_core::vault::{KeySource, Vault};
use sqlx::Connection;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::timeout;
use zeroize::Zeroizing;

const CAP: Duration = Duration::from_secs(30);
const DEFAULT_ADMIN_URL: &str = "postgres://sb@127.0.0.1:5433/postgres";

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
    /// An empty database (no migrations applied).
    async fn empty() -> TestDb {
        let admin = admin_url();
        let name = random_name();
        let mut conn = sqlx::PgConnection::connect(&admin)
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "cannot reach the test Postgres ({e}); run `docker compose up -d --wait postgres` \
                     at the repo root or set SB_TEST_DATABASE_URL"
                )
            });
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database {name}")))
            .execute(&mut conn)
            .await
            .expect("create test database");
        let _ = conn.close().await;
        let url = with_db(&admin, &name);
        TestDb { name, url }
    }

    /// A migrated database and its store.
    async fn store() -> (TestDb, Arc<dyn MemoryStore>) {
        let db = TestDb::empty().await;
        let store = core_pg::PgStore::connect(&db.url)
            .await
            .unwrap_or_else(|e| panic!("connect + migrate {}: {e}", db.name));
        (db, Arc::new(store))
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
                    if let Err(e) = sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                        .execute(&mut conn)
                        .await
                    {
                        eprintln!("could not drop test database {name}: {e}");
                    }
                    let _ = conn.close().await;
                }
            });
        })
        .join();
    }
}

// ------------------------------------------------------------------ fixtures

fn env(tmp: &Path, url: &str) -> Env {
    Env {
        database_url: url.to_owned(),
        runs_root: tmp.join("runs"),
        tools_root: tmp.join("tools"),
        // Missing file: an empty vault that never touches a key.
        vault_path: tmp.join("vault.toml.age"),
        vault_key: KeySource::IdentityFile(tmp.join("vault.key")),
        run_id: None,
        claude: tmp.join("no-claude-here"),
    }
}

fn ctx(env: Env, store: Arc<dyn MemoryStore>) -> Ctx {
    Ctx {
        env,
        store: Some(store),
        read_secret: Box::new(|_| Err(std::io::Error::other("no terminal in this test"))),
        launcher: None,
    }
}

async fn run_cli(ctx: &Ctx, args: &[&str]) -> Result<String, CliError> {
    let mut argv = vec!["harness"];
    argv.extend_from_slice(args);
    let cli = Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
    let mut out = Vec::new();
    execute(cli, ctx, &mut out).await?;
    Ok(String::from_utf8(out).expect("utf-8 output"))
}

/// Records each request and exits 0. Never calls a model (rule 1).
#[derive(Default)]
struct FakeLauncher {
    seen: Mutex<Vec<(RunId, PathBuf, String)>>,
}

#[async_trait]
impl Launcher for FakeLauncher {
    async fn launch(&self, req: LaunchRequest<'_>) -> Result<LaunchExit, LaunchError> {
        self.seen.lock().unwrap().push((
            req.run.id.clone(),
            req.runs_root.to_path_buf(),
            req.prompt.to_owned(),
        ));
        Ok(LaunchExit::OK)
    }
}

// ------------------------------------------------------------------ acceptance

/// docs/01 acceptance 4. harnessd is already idle on a far deadline when the CLI adds the
/// schedule, so the firing also proves the change marker wakes it (no poll interval).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedule_add_fires_run_with_report_and_parent_run() {
    let (_db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path(), &_db.url);
    let start = ts_now();
    let clock = Arc::new(ManualClock::new(start));
    let launcher = Arc::new(FakeLauncher::default());

    // A yearly schedule so harnessd has a deadline to sleep on before the add.
    let yearly = "0 0 1 1 *";
    store
        .schedule_add(Schedule {
            id: 0,
            cron: yearly.into(),
            prompt: "new year".into(),
            kind: ScheduleKind::Cron,
            watch_target: None,
            enabled: true,
            last_run: None,
            run_id: None,
        })
        .await
        .unwrap();
    let new_year = next_after(&parse_cron(yearly).unwrap(), start).unwrap();
    let minute = next_after(&parse_cron("*/1 * * * *").unwrap(), start).unwrap();

    let (tx, mut fired) = tokio::sync::mpsc::unbounded_channel();
    let daemon = Daemon::new(
        store.clone(),
        launcher.clone(),
        clock.clone(),
        env.runs_root.clone(),
        Some(tx),
    )
    .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let harnessd = tokio::spawn(daemon.run(async move {
        let _ = stopped.await;
    }));
    let idle = timeout(CAP, clock.deadline_where(|t| t == new_year))
        .await
        .expect("harnessd sleeps until the yearly schedule");
    assert_eq!(idle, new_year);

    // The CLI exactly as the acceptance writes it.
    let c = ctx(env.clone(), store.clone());
    let out = run_cli(&c, &["schedule", "add", "*/1 * * * *", "say hi"])
        .await
        .unwrap();
    assert!(out.starts_with("schedule "), "{out}");

    let s = store
        .schedule_list()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.prompt == "say hi")
        .expect("schedule row");
    assert_eq!(s.cron, "*/1 * * * *");
    assert_eq!(s.kind, ScheduleKind::Cron);
    let parent = s
        .run_id
        .clone()
        .expect("D-107: the CLI's own run is the schedule's run");
    assert!(parent.0.ends_with("-schedule-add"), "{parent}");
    let prow = store.run_get(&parent).await.unwrap().expect("parent row");
    assert_eq!(prow.status, RunStatus::Done);
    assert!(prow.ended.is_some());
    assert!(env.runs_root.join(&parent.0).join("report.md").is_file());
    let pevents = store.ledger_events(&parent).await.unwrap();
    assert_eq!(pevents.len(), 1);
    assert_eq!(pevents[0].1.tool, harness::cli::SCHEDULE_ADD_TOOL);

    // The marker woke harnessd: it now sleeps until the next minute, not the new year.
    let due = timeout(CAP, clock.deadline_where(|t| t == minute))
        .await
        .expect("harnessd re-read schedules after `schedule add`");
    clock.set(due);
    let f = timeout(CAP, fired.recv())
        .await
        .expect("the schedule fired within the cap")
        .expect("fired channel open");
    assert_eq!(f.schedule, s.id);
    let f = f.result.expect("firing succeeded");

    // runs has a row with parent_run set.
    let row = store.run_get(&f.run).await.unwrap().expect("runs row");
    assert_eq!(row.parent_run.as_ref(), Some(&parent));
    assert_eq!(row.status, RunStatus::Done);
    assert_eq!(row.prompt, "say hi");
    assert!(row.ended.is_some_and(|e| e >= row.started));
    assert!(row.id.0.ends_with("-say-hi"), "{}", row.id);

    // A run folder with report.md (H-4 layout).
    let dir = env.runs_root.join(&f.run.0);
    assert_eq!(f.folder, dir);
    for sub in ["files", "frames", "handoff"] {
        assert!(dir.join(sub).is_dir(), "{sub}");
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("prompt.md")).unwrap(),
        "say hi"
    );
    let report = std::fs::read_to_string(dir.join("report.md")).unwrap();
    assert!(report.starts_with("# say hi\nResult: done\n"), "{report}");
    let jsonl = std::fs::read_to_string(dir.join("ledger.jsonl")).unwrap();
    assert_eq!(jsonl.lines().count(), 1);
    assert!(jsonl.contains(FIRE_TOOL));

    // schedules.last_run moved; the launcher got this run and the runs root.
    let s2 = store
        .schedule_list()
        .await
        .unwrap()
        .into_iter()
        .find(|x| x.id == s.id)
        .unwrap();
    assert_eq!(s2.last_run, Some(due));
    let seen = launcher.seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![(f.run.clone(), env.runs_root.clone(), "say hi".to_owned())]
    );

    // `harness ledger <run>` prints the firing.
    let table = run_cli(&c, &["ledger", &f.run.0]).await.unwrap();
    assert!(table.contains(FIRE_TOOL), "{table}");
    assert!(table.contains(&format!("parent {parent}")), "{table}");
    assert!(table.ends_with("1 events\n"), "{table}");

    stop.send(()).unwrap();
    timeout(CAP, harnessd)
        .await
        .expect("harnessd stops")
        .unwrap()
        .unwrap();
    assert!(fired.try_recv().is_err(), "exactly one firing");
}

// ------------------------------------------------------------------ the rest of H-7

#[tokio::test]
async fn schedule_list_rm_and_bad_input() {
    let (_db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let c = ctx(env(tmp.path(), &_db.url), store.clone());

    run_cli(&c, &["schedule", "add", "0 9 * * 1-5", "check mail"])
        .await
        .unwrap();
    let out = run_cli(
        &c,
        &[
            "schedule",
            "add",
            "0 3 * * *",
            "watch it",
            "--kind",
            "watch",
            "--watch-target",
            "jobs",
        ],
    )
    .await
    .unwrap();
    assert!(out.contains("not in Phase 1"), "{out}");
    let list = run_cli(&c, &["schedule", "list"]).await.unwrap();
    assert!(list.starts_with("id  kind"), "{list}");
    assert!(
        list.contains("0 9 * * 1-5") && list.contains("check mail"),
        "{list}"
    );
    assert!(list.contains("T-16"), "{list}");

    // A 6-field cron, a watch without target, an empty prompt: refused, nothing stored.
    for args in [
        vec!["schedule", "add", "0 * * * * *", "x"],
        vec!["schedule", "add", "0 * * * *", "x", "--kind", "watch"],
        vec!["schedule", "add", "0 * * * *", "  "],
    ] {
        assert!(run_cli(&c, &args).await.is_err(), "{args:?}");
    }
    assert_eq!(store.schedule_list().await.unwrap().len(), 2);

    let id = store.schedule_list().await.unwrap()[0].id.to_string();
    let out = run_cli(&c, &["schedule", "rm", &id]).await.unwrap();
    assert_eq!(out, format!("schedule {id} removed\n"));
    assert_eq!(store.schedule_list().await.unwrap().len(), 1);
    assert!(run_cli(&c, &["schedule", "rm", &id]).await.is_err());
    // Every change rewrote the marker harnessd watches.
    assert!(tmp.path().join("runs/.sched/changed").is_file());
}

/// Inside a session (SB_RUN_ID) the schedule's run is the session's run.
#[tokio::test]
async fn schedule_add_inside_a_run_uses_that_run() {
    let (_db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let mut e = env(tmp.path(), &_db.url);
    let folder = RunFolder::create(&e.runs_root, "plan week", "plan week").unwrap();
    store
        .run_start(Run {
            id: folder.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "plan week".into(),
            status: RunStatus::Running,
            folder: folder.dir.to_string_lossy().into_owned(),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    e.run_id = Some(folder.id.clone());
    let c = ctx(e, store.clone());
    run_cli(&c, &["schedule", "add", "*/5 * * * *", "say hi"])
        .await
        .unwrap();
    let s = &store.schedule_list().await.unwrap()[0];
    assert_eq!(s.run_id.as_ref(), Some(&folder.id));
    let ev = store.ledger_events(&folder.id).await.unwrap();
    assert_eq!(ev.len(), 1);
    // The session's run is not ended by the CLI.
    assert_eq!(
        store.run_get(&folder.id).await.unwrap().unwrap().status,
        RunStatus::Running
    );
}

#[tokio::test]
async fn gate_set_ask_and_tools() {
    let (_db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let c = ctx(env(tmp.path(), &_db.url), store.clone());

    // gate set
    let out = run_cli(
        &c,
        &["gate", "set", "Luma.com.", "Submit", "ask", "--strict"],
    )
    .await
    .unwrap();
    assert!(out.starts_with("gate: "), "{out}");
    let rule = store.gate_rule("luma.com", "Submit").await.unwrap();
    assert_eq!(
        rule,
        Some(GateRule {
            mode: Mode::Ask,
            strict: true
        })
    );
    assert!(run_cli(&c, &["gate", "set", "*", "Send", "maybe"])
        .await
        .is_err());
    // D-5: a Challenge rule other than stop is refused by the store.
    assert!(run_cli(&c, &["gate", "set", "*", "Challenge", "allow"])
        .await
        .is_err());

    // ask: answers a card written by a tool server's FileAsker.
    let run = RunFolder::create(&c.env.runs_root, "ask test", "p").unwrap();
    let card = harness_core::ask::card_path(&run.dir, 1);
    std::fs::create_dir_all(card.parent().unwrap()).unwrap();
    std::fs::write(&card, "{}").unwrap();
    let out = run_cli(&c, &["ask", &run.id.0, "1", "yes"]).await.unwrap();
    assert_eq!(out, format!("ask 1 in run {}: yes\n", run.id));
    let answer = std::fs::read_to_string(harness_core::ask::answer_path(&run.dir, 1)).unwrap();
    assert_eq!(answer.trim(), "yes");
    assert!(run_cli(&c, &["ask", &run.id.0, "2", "no"]).await.is_err());

    // tools: list, pin refuses retire, unpin, retire --no-commit archives.
    let site = c.env.tools_root.join("luma.com");
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(site.join("register.js"), "// a tool\n").unwrap();
    let list = run_cli(&c, &["tools", "list"]).await.unwrap();
    assert!(list.contains("luma.com/register"), "{list}");
    assert!(list.contains("no row"), "{list}");
    run_cli(&c, &["tools", "pin", "luma.com/register"])
        .await
        .unwrap();
    let h = store.tool_health("luma.com").await.unwrap();
    assert!(h[0].pinned);
    let e = run_cli(&c, &["tools", "retire", "luma.com/register", "--no-commit"])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("pinned"), "{e}");
    run_cli(&c, &["tools", "unpin", "luma.com/register"])
        .await
        .unwrap();
    let out = run_cli(
        &c,
        &[
            "tools",
            "retire",
            "luma.com/register",
            "--why",
            "superseded",
            "--no-commit",
        ],
    )
    .await
    .unwrap();
    assert!(out.contains("archived"), "{out}");
    assert!(!site.join("register.js").exists());
    let archived = c.env.tools_root.join("_archive/luma.com");
    assert_eq!(std::fs::read_dir(&archived).unwrap().count(), 1);
    let h = &store.tool_health("luma.com").await.unwrap()[0];
    assert_eq!(h.status, ToolStatus::Retired);
    assert_eq!(h.why.as_deref(), Some("superseded"));
    let health = run_cli(&c, &["tools", "health"]).await.unwrap();
    assert!(
        health.contains("retired") && health.contains("superseded"),
        "{health}"
    );
    // Paths go through try_tool_path (D-74): traversal is refused.
    for bad in ["../x/register", "luma.com/..", "luma.com", "C:/register"] {
        assert!(run_cli(&c, &["tools", "pin", bad]).await.is_err(), "{bad}");
    }
}

#[tokio::test]
async fn db_status_and_migrate() {
    let db = TestDb::empty().await;
    let tmp = tempfile::tempdir().unwrap();
    // `db` always talks to env.database_url, never a preset store.
    let c = Ctx {
        store: None,
        ..ctx(
            env(tmp.path(), &db.url),
            Arc::new(harness_core::testing::MemStore::new()),
        )
    };
    let before = run_cli(&c, &["db", "status"]).await.unwrap();
    assert!(
        before.contains("pending") && !before.contains("applied"),
        "{before}"
    );
    let after = run_cli(&c, &["db", "migrate"]).await.unwrap();
    assert!(after.starts_with("migrations applied\n"), "{after}");
    assert!(
        after.contains("applied") && !after.contains("pending"),
        "{after}"
    );
    let again = run_cli(&c, &["db", "status"]).await.unwrap();
    assert!(!again.contains("pending"), "{again}");
}

/// `vault set` reads the value through the no-echo reader only; it is never in the output,
/// the error text, or the vault file in clear.
#[tokio::test]
async fn vault_set_reads_value_from_terminal_only() {
    let tmp = tempfile::tempdir().unwrap();
    let canary = std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| c.len() >= 8)
        .unwrap_or_else(|| {
            format!(
                "sbcanary-t08-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )
        });
    let e = env(tmp.path(), "postgres://unused");
    let value = canary.clone();
    let c = Ctx {
        env: e.clone(),
        store: Some(Arc::new(harness_core::testing::MemStore::new())),
        read_secret: Box::new(move |prompt| {
            assert!(prompt.starts_with("value for "), "{prompt}");
            Ok(Zeroizing::new(value.clone()))
        }),
        launcher: None,
    };
    let out = run_cli(&c, &["vault", "set", "test"]).await.unwrap();
    assert_eq!(out, "vault: test set (secret)\n");
    let out = run_cli(&c, &["vault", "set", "who", "--personal"])
        .await
        .unwrap();
    assert_eq!(out, "vault: who set (personal)\n");
    let bytes = std::fs::read(&e.vault_path).unwrap();
    assert!(
        !String::from_utf8_lossy(&bytes).contains(&canary),
        "vault file holds the value in clear"
    );
    let v = Vault::open(&e.vault_path, &e.vault_key).unwrap();
    let same = v
        .resolve_into("@vault(test)", |r| r.expose() == canary)
        .unwrap();
    assert!(same, "the stored value resolves");
    let err = run_cli(&c, &["vault", "set", "bad name"])
        .await
        .unwrap_err();
    assert!(
        !err.to_string().contains(&canary),
        "error text holds the value"
    );
}

/// The `harness` binary end to end: env vars in, table out.
#[tokio::test]
async fn harness_binary_schedule_add_and_list() {
    let (db, _store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_harness");
    let run = |args: &[&str]| {
        std::process::Command::new(bin)
            .args(args)
            .env("DATABASE_URL", &db.url)
            .env("SB_RUNS_ROOT", tmp.path().join("runs"))
            .env("SB_TOOLS_ROOT", tmp.path().join("tools"))
            .env("SB_VAULT_PATH", tmp.path().join("vault.toml.age"))
            .env_remove("SB_RUN_ID")
            .output()
            .unwrap()
    };
    let add = run(&["schedule", "add", "*/1 * * * *", "say hi"]);
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let list = run(&["schedule", "list"]);
    let text = String::from_utf8_lossy(&list.stdout);
    assert!(
        text.contains("*/1 * * * *") && text.contains("say hi"),
        "{text}"
    );
    let bad = run(&["schedule", "add", "* * *", "x"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).starts_with("harness: "));
}

// ------------------------------------------------------------------ round 2 (review-T-08)

/// Exits with `code`, recording each request. Never calls a model (rule 1).
struct ExitLauncher {
    code: Option<i32>,
    seen: Mutex<Vec<(RunId, String)>>,
}

#[async_trait]
impl Launcher for ExitLauncher {
    async fn launch(&self, req: LaunchRequest<'_>) -> Result<LaunchExit, LaunchError> {
        self.seen
            .lock()
            .unwrap()
            .push((req.run.id.clone(), req.prompt.to_owned()));
        Ok(LaunchExit { code: self.code })
    }
}

/// Should fix 6: `harness run` (H-7) through the Ctx.launcher seam: strict on the row,
/// parent_run from SB_RUN_ID, and the printed status and report line.
#[tokio::test]
async fn harness_run_uses_the_launcher_seam() {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn MemoryStore> = Arc::new(harness_core::testing::MemStore::new());
    let mut e = env(tmp.path(), "postgres://unused");
    let parent = RunId("2026-10-25T08-00-00-plan-week".into());
    e.run_id = Some(parent.clone());
    let launcher = Arc::new(ExitLauncher {
        code: Some(0),
        seen: Mutex::new(vec![]),
    });
    let c = Ctx {
        launcher: Some(launcher.clone()),
        ..ctx(e.clone(), store.clone())
    };
    let out = run_cli(&c, &["run", "say hi", "--strict"]).await.unwrap();
    let seen = launcher.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let (id, prompt) = &seen[0];
    assert_eq!(prompt, "say hi");
    let dir = e.runs_root.join(&id.0);
    assert_eq!(
        out,
        format!(
            "run {id}: done\nreport: {}\n",
            dir.join("report.md").display()
        )
    );
    let row = store.run_get(id).await.unwrap().expect("runs row");
    assert!(row.strict);
    assert_eq!(row.parent_run.as_ref(), Some(&parent));
    assert_eq!(row.status, RunStatus::Done);
    assert!(dir.join("report.md").is_file());

    // A session that exits non-zero: the run is failed and the line says why.
    let failing = Arc::new(ExitLauncher {
        code: Some(3),
        seen: Mutex::new(vec![]),
    });
    let mut e2 = e.clone();
    e2.run_id = None;
    let c = Ctx {
        launcher: Some(failing.clone()),
        ..ctx(e2, store.clone())
    };
    let out = run_cli(&c, &["run", "again"]).await.unwrap();
    let id = failing.seen.lock().unwrap()[0].0.clone();
    assert!(
        out.starts_with(&format!(
            "run {id}: failed (session exited with code 3)\nreport: "
        )),
        "{out}"
    );
    let row = store.run_get(&id).await.unwrap().unwrap();
    assert!(!row.strict);
    assert_eq!(row.parent_run, None);
    assert_eq!(row.status, RunStatus::Failed);
}

/// Should fix 2 (D-79): answering the same ask twice gives ask::answer's AlreadyExists
/// message as is, from the library and on the binary's stderr.
#[tokio::test]
async fn ask_answered_twice_prints_the_already_exists_message() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), "postgres://unused");
    let c = ctx(e.clone(), Arc::new(harness_core::testing::MemStore::new()));
    let run = RunFolder::create(&e.runs_root, "ask twice", "p").unwrap();
    let card = harness_core::ask::card_path(&run.dir, 1);
    std::fs::create_dir_all(card.parent().unwrap()).unwrap();
    std::fs::write(&card, "{}").unwrap();
    let want = format!(
        "ask 1 in run {} is already answered (yes); nothing changed",
        run.id
    );

    run_cli(&c, &["ask", &run.id.0, "1", "yes"]).await.unwrap();
    let err = run_cli(&c, &["ask", &run.id.0, "1", "no"])
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), want);
    let answer = std::fs::read_to_string(harness_core::ask::answer_path(&run.dir, 1)).unwrap();
    assert_eq!(answer.trim(), "yes", "the second answer changed nothing");

    // The binary: the same text after `harness: `, exit code 1.
    let card2 = harness_core::ask::card_path(&run.dir, 2);
    std::fs::write(&card2, "{}").unwrap();
    let bin = env!("CARGO_BIN_EXE_harness");
    let ask = |answer: &str| {
        std::process::Command::new(bin)
            .args(["ask", &run.id.0, "2", answer])
            .env("SB_RUNS_ROOT", &e.runs_root)
            .env("SB_TOOLS_ROOT", &e.tools_root)
            .env("SB_VAULT_PATH", &e.vault_path)
            .env_remove("SB_RUN_ID")
            .output()
            .unwrap()
    };
    let first = ask("yes");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&first.stdout),
        format!("ask 2 in run {}: yes\n", run.id)
    );
    let second = ask("no");
    assert_eq!(second.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&second.stderr),
        format!(
            "harness: ask 2 in run {} is already answered (yes); nothing changed\n",
            run.id
        )
    );
    assert!(second.stdout.is_empty());
}

/// A MemStore whose ledger append fails (the database went away mid-command).
struct FailAppend(harness_core::testing::MemStore);

#[async_trait]
impl MemoryStore for FailAppend {
    async fn remember(&self, f: Fact) -> Result<FactId> {
        self.0.remember(f).await
    }
    async fn forget(&self, id: FactId) -> Result<()> {
        self.0.forget(id).await
    }
    async fn link(&self, src: NodeId, rel: &str, dst: NodeId, source: Source) -> Result<()> {
        self.0.link(src, rel, dst, source).await
    }
    async fn node(&self, kind: &str, name: &str) -> Result<NodeId> {
        self.0.node(kind, name).await
    }
    async fn neighbors(&self, n: NodeId, rel: Option<&str>, depth: u8) -> Result<Vec<Node>> {
        self.0.neighbors(n, rel, depth).await
    }
    async fn recall(&self, q: &str, k: usize, filter: Option<Filter>) -> Result<Vec<Hit>> {
        self.0.recall(q, k, filter).await
    }
    async fn search(&self, q: &str, k: usize) -> Result<Vec<Hit>> {
        self.0.search(q, k).await
    }
    async fn contact(&self, person: &str) -> Result<Option<Contact>> {
        self.0.contact(person).await
    }
    async fn touch_contact(&self, person: &str, channel: &str, note: &str) -> Result<()> {
        self.0.touch_contact(person, channel, note).await
    }
    async fn due_reviews(&self, now: Ts, k: usize) -> Result<Vec<Review>> {
        self.0.due_reviews(now, k).await
    }
    async fn grade_review(&self, id: ReviewId, grade: u8) -> Result<()> {
        self.0.grade_review(id, grade).await
    }
    async fn baseline_get(&self, w: &str) -> Result<Option<Baseline>> {
        self.0.baseline_get(w).await
    }
    async fn baseline_set(&self, w: &str, b: Baseline) -> Result<()> {
        self.0.baseline_set(w, b).await
    }
    async fn append(&self, _: LedgerEvent) -> Result<LedgerId> {
        Err(MemoryError::Invalid("test: ledger append refused".into()))
    }
    async fn tool_health(&self, domain: &str) -> Result<Vec<ToolHealth>> {
        self.0.tool_health(domain).await
    }
    async fn tool_health_set(&self, h: ToolHealth) -> Result<()> {
        self.0.tool_health_set(h).await
    }
    async fn gate_rule(&self, domain: &str, class: &str) -> Result<Option<GateRule>> {
        self.0.gate_rule(domain, class).await
    }
    async fn run_start(&self, run: Run) -> Result<()> {
        self.0.run_start(run).await
    }
    async fn run_end(&self, id: &RunId, status: RunStatus, ended: Ts) -> Result<()> {
        self.0.run_end(id, status, ended).await
    }
    async fn run_get(&self, id: &RunId) -> Result<Option<Run>> {
        self.0.run_get(id).await
    }
    async fn schedule_add(&self, s: Schedule) -> Result<i32> {
        self.0.schedule_add(s).await
    }
    async fn schedule_list(&self) -> Result<Vec<Schedule>> {
        self.0.schedule_list().await
    }
    async fn schedule_rm(&self, id: i32) -> Result<()> {
        self.0.schedule_rm(id).await
    }
    async fn schedule_fired(&self, id: i32, at: Ts) -> Result<()> {
        self.0.schedule_fired(id, at).await
    }
    async fn gate_rule_set(&self, d: &str, c: &str, r: GateRule, note: &str) -> Result<()> {
        self.0.gate_rule_set(d, c, r, note).await
    }
    async fn review_add(&self, item: &str, answer: &str) -> Result<ReviewId> {
        self.0.review_add(item, answer).await
    }
    async fn ledger_events(&self, run: &RunId) -> Result<Vec<(LedgerId, LedgerEvent)>> {
        self.0.ledger_events(run).await
    }
}

/// Should fix 5: `schedule add` whose ledger write fails after the schedule row exists ends
/// its own run failed and still rewrites the marker harnessd watches.
#[tokio::test]
async fn schedule_add_failing_after_the_row_ends_its_run_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(FailAppend(harness_core::testing::MemStore::new()));
    let e = env(tmp.path(), "postgres://unused");
    let c = ctx(e.clone(), store.clone());
    let err = run_cli(&c, &["schedule", "add", "*/5 * * * *", "say hi"])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("ledger append refused"), "{err}");
    let s = store.schedule_list().await.unwrap();
    assert_eq!(s.len(), 1, "the schedule row was written");
    let parent = s[0].run_id.clone().expect("schedule's run");
    let row = store.run_get(&parent).await.unwrap().unwrap();
    assert_eq!(row.status, RunStatus::Failed);
    assert!(row.ended.is_some());
    assert!(
        e.runs_root.join(".sched/changed").is_file(),
        "harnessd is told the schedules changed"
    );
}
