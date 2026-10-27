//! Tester acceptance for T-08 round 2 (docs/01 H-4, H-5, H-7; D-116 (7) as refined by D-124;
//! the B-192 default). One test (or more) per D-124 clause:
//!
//! * `d124_1_*`: `harness` installs the vault before every command except `vault set` (output
//!   that would carry a vault value shows `@vault(test)`), fails closed with nothing printed
//!   or written when the vault cannot be opened, and `vault set` repairs a short entry
//!   (`Vault::open_for_set`).
//! * `d124_2_*`: harnessd (`daemon::ReopenVault`) re-opens the vault per firing.
//! * `d124_3_*`: the `claude -p` session cap kills a hung session and the launch is recorded
//!   failed.
//! * `d124_4_*`: stdout is capped at 1 MiB and scrubbed before the cut.
//! * `d124_5_b192_*`: every session's argv carries `--disallowedTools` with the three rules,
//!   and the prompt still arrives (stdin for `-p`, positional for interactive), also through a
//!   `claude.cmd` shim (how npm installs Claude Code on Windows).
//!
//! The process-wide scrubber is global, so every test holds `LOCK`. Postgres: a throwaway
//! `sb_t_<random>` database per test on SB_TEST_DATABASE_URL (default the compose Postgres on
//! 127.0.0.1:5433), dropped at the end (D-100). The vault value is `SB_VAULT_CANARY` (set it
//! to a fresh value per run; one is generated and exported when unset). No assertion message
//! prints it: texts are shown with it replaced by `<CANARY>`. `claude` is
//! bench/fixtures/fake_claude.rs compiled with plain rustc (rule 1). No fixed sleeps: every
//! wait is a condition with a cap.

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use clap::Parser;
use harness::cli::{execute, Cli, Ctx};
use harness::daemon::{Daemon, ReopenVault};
use harness::launcher::{ClaudeLauncher, ANSWER_FILE, DISALLOWED_TOOLS, HARNESS_ALLOWED_TOOLS};
use harness::{CliError, Env};
use harness_core::memory::*;
use harness_core::run::{is_run_id, RunFolder, RunId};
use harness_core::sched::{
    self, LaunchError, LaunchExit, LaunchRequest, Launcher, ManualClock, RunSpec, Runner,
    SchedError, SystemClock, LAUNCH_TOOL,
};
use harness_core::vault::{EntryKind, KeySource, Vault, VaultError};
use sqlx::{Connection, Row};
use std::collections::BTreeMap;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use zeroize::Zeroizing;

const CAP: Duration = Duration::from_secs(60);
const LITERAL: &str = "@vault(test)";
const DEFAULT_ADMIN_URL: &str = "postgres://sb@127.0.0.1:5433/postgres";

/// The task part of a `claude -p` session's stdin: what follows the run preamble and its
/// blank line (B-563, D-140 (10)). Asserts the preamble names the run.
fn task_of(stdin: &str, run: &str) -> String {
    assert!(
        stdin.starts_with(&format!("Harness run {run}.\n")),
        "stdin lacks the run preamble"
    );
    stdin
        .split_once("\n\n")
        .map(|(_, t)| t.to_owned())
        .unwrap_or_default()
}
const MIB: usize = 1024 * 1024;
const SLACK: usize = 64 * 1024;

static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

fn random_hex() -> String {
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
    format!("{:016x}", h.finish())
}

struct TestDb {
    name: String,
    url: String,
}

impl TestDb {
    async fn store() -> (TestDb, Arc<dyn MemoryStore>) {
        let admin = admin_url();
        let name = format!("sb_t_{}", random_hex());
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

    /// Every row of `tables`, as text, for the canary grep and before/after comparisons.
    async fn dump_tables(&self, tables: &[&str]) -> String {
        let mut conn = sqlx::PgConnection::connect(&self.url).await.unwrap();
        let mut all = String::new();
        for t in tables {
            let sql = format!("select t::text from {t} t order by 1");
            let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
                .fetch_all(&mut conn)
                .await
                .unwrap_or_else(|e| panic!("dump {t}: {e}"));
            all.push_str(&format!("== {t}\n"));
            for r in rows {
                all.push_str(&r.get::<String, _>(0));
                all.push('\n');
            }
        }
        let _ = conn.close().await;
        all
    }

    async fn dump(&self) -> String {
        self.dump_tables(&[
            "ledger.events",
            "runs",
            "schedules",
            "gate.rules",
            "tools.health",
        ])
        .await
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

// ------------------------------------------------------------------ canary and greps

/// `SB_VAULT_CANARY` when it is set and fits a run-id slug, else a fresh value, exported so
/// the fake claude (`[[emit]]`) prints the same value.
fn canary() -> &'static str {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| {
        let ok = |c: &str| c.len() >= 12 && is_run_id(&format!("2026-10-25T00-00-00-{c}"));
        match std::env::var("SB_VAULT_CANARY") {
            Ok(c) if ok(&c) => c,
            _ => {
                let c = format!("sbcanary-t08r2acc-{}", random_hex());
                std::env::set_var("SB_VAULT_CANARY", &c);
                c
            }
        }
    })
}

fn redact(s: &str) -> String {
    s.replace(canary(), "<CANARY>")
}

/// Panics (without printing the value) when `text` holds the vault value; checks it shows
/// the literal instead when `literal` is set.
fn assert_clean(text: &str, literal: bool, what: &str) {
    assert!(
        !text.contains(canary()),
        "{what} holds the vault value: {}",
        redact(text)
    );
    if literal {
        assert!(text.contains(LITERAL), "{what} lacks {LITERAL}: {text}");
    }
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

/// Every path under `dir` with its length and modification time.
fn tree(dir: &Path) -> BTreeMap<PathBuf, (u64, Option<std::time::SystemTime>)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            // Directory times are updated lazily on NTFS; a directory counts by name only.
            let m = e.metadata().ok().filter(|m| m.is_file());
            out.insert(
                p.clone(),
                (
                    m.as_ref().map_or(0, |m| m.len()),
                    m.and_then(|m| m.modified().ok()),
                ),
            );
            if p.is_dir() {
                stack.push(p);
            }
        }
    }
    out
}

// ------------------------------------------------------------------ fixtures

fn empty_global_vault() {
    Vault::in_memory(Vec::<(String, String, EntryKind)>::new()).install_global();
}

fn installed() -> bool {
    harness_core::scrub(canary()) == LITERAL
}

fn env(tmp: &Path, db: &TestDb) -> Env {
    Env {
        database_url: db.url.clone(),
        runs_root: tmp.join("runs"),
        tools_root: tmp.join("tools"),
        vault_path: tmp.join("vault.toml.age"),
        vault_key: KeySource::IdentityFile(tmp.join("vault.key")),
        run_id: None,
        claude: tmp.join("no-claude-here"),
    }
}

/// A vault file at `e.vault_path` holding `test` = the canary.
fn vault_with_canary(e: &Env) {
    let mut v = Vault::open(&e.vault_path, &e.vault_key).unwrap();
    v.set("test", canary(), EntryKind::Secret).unwrap();
}

fn ctx(env: Env, store: Arc<dyn MemoryStore>, launcher: Option<Arc<dyn Launcher>>) -> Ctx {
    Ctx {
        env,
        store: Some(store),
        read_secret: Box::new(|_| Ok(Zeroizing::new(canary().to_owned()))),
        launcher,
    }
}

async fn run_cli(ctx: &Ctx, args: &[&str]) -> (Result<(), CliError>, String) {
    let mut argv = vec!["harness"];
    argv.extend_from_slice(args);
    let cli = Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
    let mut out = Vec::new();
    let r = timeout(CAP, execute(cli, ctx, &mut out))
        .await
        .unwrap_or_else(|_| panic!("{argv:?} within the cap"));
    (r, String::from_utf8(out).expect("utf-8 output"))
}

/// What harness's main prints for an error.
fn binary_error_line(e: &CliError) -> String {
    format!("harness: {}", harness_core::scrub(&format!("{e:#}")))
}

/// Re-encrypts the vault at `path` with exactly `entries` (name, value, kind), as an older
/// build or a hand edit could have, with the identity in `key_file`.
fn write_vault_file(path: &Path, key_file: &Path, entries: &[(&str, &str, &str)]) {
    let text = std::fs::read_to_string(key_file).unwrap();
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap();
    let id: age::x25519::Identity = line.parse().unwrap();
    let mut t = toml::Table::new();
    for (name, value, kind) in entries {
        let mut e = toml::Table::new();
        e.insert("value".into(), toml::Value::String((*value).into()));
        e.insert("kind".into(), toml::Value::String((*kind).into()));
        t.insert((*name).into(), toml::Value::Table(e));
    }
    let ct = age::encrypt(&id.to_public(), toml::to_string(&t).unwrap().as_bytes()).unwrap();
    std::fs::write(path, ct).unwrap();
}

fn health_row(domain: &str, name: &str, why: Option<String>) -> ToolHealth {
    ToolHealth {
        domain: domain.into(),
        name: name.into(),
        version: 1,
        status: ToolStatus::NeedsRepair,
        runs: 2,
        fails: 1,
        last_ok: None,
        last_fail: None,
        why,
        pinned: false,
    }
}

fn cron_row(prompt: String) -> Schedule {
    Schedule {
        id: 0,
        cron: "*/1 * * * *".into(),
        prompt,
        kind: ScheduleKind::Cron,
        watch_target: None,
        enabled: true,
        last_run: None,
        run_id: Some(RunId("2026-10-25T11-00-00-schedule-add".into())),
    }
}

fn spec(prompt: &str) -> RunSpec<'_> {
    RunSpec {
        prompt,
        parent_run: None,
        strict: false,
        schedule: None,
    }
}

/// bench/fixtures/fake_claude.rs compiled with plain rustc (once per test process).
fn fake_claude_build() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/fixtures/fake_claude.rs");
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("accept_t08_r2_fake_claude")
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

/// A copy of the fake in its own folder `dir`, recording into `dir` (a `record` file).
fn fake_claude(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("record"), b"").unwrap();
    let to = dir.join(format!("fake-claude{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(fake_claude_build(), &to).unwrap();
    to
}

/// `dir/claude.cmd` running the fake (npm's shim shape), and the extension-less program
/// path `dir/claude` the launcher resolves to it.
fn fake_claude_cmd_shim(dir: &Path) -> PathBuf {
    let exe = fake_claude(dir);
    std::fs::write(
        dir.join("claude.cmd"),
        format!("@echo off\r\n\"{}\" %*\r\n", exe.display()),
    )
    .unwrap();
    dir.join("claude")
}

fn read(p: impl AsRef<Path>) -> String {
    std::fs::read_to_string(p.as_ref()).unwrap_or_else(|e| panic!("{}: {e}", p.as_ref().display()))
}

/// A `-p` session also allows the harness servers (review T-16 r1 should-fix 1); an
/// interactive one does not.
fn want_argv(first: &str) -> Vec<String> {
    let mut v = vec![first.to_owned()];
    if first == "-p" {
        // D-144: every -p session loads no built-in tools.
        v.push("--tools".to_owned());
        v.push(String::new());
        v.push("--allowedTools".to_owned());
        v.extend(HARNESS_ALLOWED_TOOLS.iter().map(|s| s.to_string()));
    }
    v.push("--disallowedTools".to_owned());
    v.extend(DISALLOWED_TOOLS.iter().map(|s| s.to_string()));
    v
}

/// Whether a process with this id is still alive (Windows: tasklist).
fn process_alive(pid: u32) -> bool {
    if cfg!(windows) {
        let o = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .expect("tasklist");
        String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\""))
    } else {
        Path::new(&format!("/proc/{pid}")).exists()
    }
}

/// Waits (a condition with a cap) until process `pid` is gone. No OS event says "this
/// unrelated pid exited" without a handle, so it re-checks every 100 ms.
async fn gone_within(pid: u32, cap: Duration) -> bool {
    let t0 = Instant::now();
    while process_alive(pid) {
        if t0.elapsed() > cap {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    true
}

/// Records each request and exits 0. Never calls a model (rule 1).
#[derive(Default)]
struct RecordingLauncher {
    seen: Mutex<Vec<String>>,
}

#[async_trait]
impl Launcher for RecordingLauncher {
    async fn launch(&self, req: LaunchRequest<'_>) -> Result<LaunchExit, LaunchError> {
        self.seen.lock().unwrap().push(req.prompt.to_owned());
        Ok(LaunchExit::OK)
    }
}

// ------------------------------------------------------------------ D-124 (1)

/// Output from rows that hold a raw vault value (written by a tool server, or before the
/// value was in the vault) prints @vault(test), for every reading command; each command
/// installed the file vault (the global scrubber starts empty before each).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d124_1_reading_commands_print_the_literal() {
    let _g = LOCK.lock().await;
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), &db);
    vault_with_canary(&e);
    let v = canary();
    let c = ctx(e.clone(), store.clone(), None);

    store
        .schedule_add(cron_row(format!("{v} check mail")))
        .await
        .unwrap();
    store
        .tool_health_set(health_row(
            "luma.com",
            "register",
            Some(format!("page said: wrong password {v}")),
        ))
        .await
        .unwrap();
    let run = RunId("2026-10-25T10-00-00-ledger-test".into());
    store
        .run_start(Run {
            id: run.clone(),
            started: Utc.with_ymd_and_hms(2026, 10, 25, 10, 0, 0).unwrap(),
            ended: None,
            prompt: "p".into(),
            status: RunStatus::Running,
            folder: "runs/x".into(),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    store
        .append(LedgerEvent {
            run_id: Some(run.clone()),
            ts: Utc.with_ymd_and_hms(2026, 10, 25, 10, 0, 1).unwrap(),
            server: "mail".into(),
            tool: "mail.read".into(),
            class: None,
            mode: None,
            args: serde_json::json!({}),
            result_summary: format!("inbox: {v}"),
            consequences: serde_json::json!({}),
            duration_ms: 3,
            ok: true,
            why: Some(format!("why {v}")),
        })
        .await
        .unwrap();
    // An ask card in a run whose id carries the value (created before it was in the vault).
    let ask_run = format!("2026-10-25T10-00-00-{v}");
    let ask_dir = e.runs_root.join(&ask_run);
    let card = harness_core::ask::card_path(&ask_dir, 1);
    std::fs::create_dir_all(card.parent().unwrap()).unwrap();
    std::fs::write(&card, "{}").unwrap();
    // A tool folder named with the value (made before it was in the vault).
    let tool_dir = format!("{v}.com");
    std::fs::create_dir_all(e.tools_root.join(&tool_dir)).unwrap();
    std::fs::write(e.tools_root.join(&tool_dir).join("probe.js"), "// t\n").unwrap();

    let cases: Vec<(Vec<String>, bool)> = vec![
        (vec!["schedule".into(), "list".into()], true),
        (
            vec!["tools".into(), "health".into(), "luma.com".into()],
            true,
        ),
        (
            vec!["tools".into(), "list".into(), "luma.com".into()],
            false,
        ),
        (vec!["tools".into(), "list".into()], true),
        (vec!["tools".into(), "list".into(), tool_dir.clone()], true),
        (vec!["ledger".into(), run.0.clone()], true),
        (
            vec!["ask".into(), ask_run.clone(), "1".into(), "yes".into()],
            true,
        ),
    ];
    for (args, literal) in cases {
        empty_global_vault();
        assert!(!installed());
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let (r, out) = run_cli(&c, &a).await;
        let what = redact(&args.join(" "));
        r.unwrap_or_else(|e| panic!("{what}: {}", redact(&e.to_string())));
        assert!(installed(), "{what} did not install the vault");
        assert_clean(&out, literal, &what);
        println!("d124_1 read ok: {what}");
    }

    // Error lines (as main prints them) for a value in an argument.
    for args in [
        vec!["ledger".to_owned(), format!("2026-10-25T10-00-00-{v}")],
        vec![
            "ask".to_owned(),
            ask_run.clone(),
            "1".to_owned(),
            "no".to_owned(),
        ],
        vec!["ledger".to_owned(), format!("not-a-run-{v}")],
    ] {
        empty_global_vault();
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let (r, out) = run_cli(&c, &a).await;
        let what = redact(&args.join(" "));
        let err = r.expect_err(&what);
        assert!(installed(), "{what} did not install the vault");
        assert!(out.is_empty(), "{what}: {}", redact(&out));
        assert_clean(&binary_error_line(&err), true, &what);
        println!("d124_1 error line ok: {what}");
    }

    // `vault set` never installs (its handle is open_for_set's).
    empty_global_vault();
    let (r, out) = run_cli(&c, &["vault", "set", "test"]).await;
    r.unwrap();
    assert_eq!(out, "vault: test set (secret)\n");
    assert!(!installed(), "vault set installed a scrubber");
}

/// `harness ledger <run>` prints the run id and its parent from the runs row; a value in
/// either (a run started before the value was added) must print as the literal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d124_1_ledger_header_prints_run_ids_as_literals() {
    let _g = LOCK.lock().await;
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), &db);
    vault_with_canary(&e);
    let v = canary();
    let c = ctx(e.clone(), store.clone(), None);
    let parent = RunId(format!("2026-10-25T09-00-00-{v}"));
    let child = RunId("2026-10-25T09-30-00-child".into());
    let own = RunId(format!("2026-10-25T09-40-00-{v}"));
    for (id, parent_run) in [
        (parent.clone(), None),
        (child.clone(), Some(parent.clone())),
        (own.clone(), None),
    ] {
        store
            .run_start(Run {
                id,
                started: Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap(),
                ended: None,
                prompt: "p".into(),
                status: RunStatus::Running,
                folder: "runs/x".into(),
                parent_run,
                strict: false,
            })
            .await
            .unwrap();
    }
    let mut leaks = Vec::new();
    for (id, what) in [
        (&child, "ledger <child>: the parent column"),
        (&own, "ledger <run>: the run id"),
    ] {
        empty_global_vault();
        let (r, out) = run_cli(&c, &["ledger", &id.0]).await;
        r.unwrap();
        assert!(installed());
        if out.contains(canary()) || !out.contains(LITERAL) {
            leaks.push(format!("{what} holds the vault value: {}", redact(&out)));
        }
    }
    assert!(leaks.is_empty(), "{}", leaks.join(" | "));
}

/// `harness tools pin|unpin|retire <domain>/<name>` echo the tool id on success. A tool whose
/// name became a vault value after its `tools.health` row and file existed (as the ledger
/// header test seeds run ids) must echo the literal from all three commands.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d124_1_tools_commands_echo_the_tool_id_as_literal() {
    let _g = LOCK.lock().await;
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), &db);
    vault_with_canary(&e);
    let v = canary();
    let c = ctx(e.clone(), store.clone(), None);
    store
        .tool_health_set(health_row("luma.com", v, None))
        .await
        .unwrap();
    let dir = e.tools_root.join("luma.com");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{v}.js")), "// tool\n").unwrap();
    let tool = format!("luma.com/{v}");
    let mut leaks = Vec::new();
    for (a, word) in [
        (vec!["tools", "pin", &tool], "pinned"),
        (vec!["tools", "unpin", &tool], "unpinned"),
        (vec!["tools", "retire", &tool, "--no-commit"], "retired"),
    ] {
        empty_global_vault();
        let (r, out) = run_cli(&c, &a).await;
        let what = redact(&a.join(" "));
        r.unwrap_or_else(|e| panic!("{what}: {}", redact(&binary_error_line(&e))));
        assert!(installed(), "{what} did not install the vault");
        println!("d124_1 tools: {what} -> {}", redact(&out).trim());
        let echo = format!("luma.com/{LITERAL}: {word}");
        if out.contains(v) || !out.contains(&echo) {
            leaks.push(format!("{what} did not echo `{echo}`: {}", redact(&out)));
        }
    }
    assert!(leaks.is_empty(), "{}", leaks.join(" | "));
    let h = store.tool_health("luma.com").await.unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].status, ToolStatus::Retired);
    assert!(!h[0].pinned);
}

/// Pin and unpin of an id with no `tools.health` row and no file are refused with NotFound
/// and create no row, with or without a vault value in the id (D-124 (1)); the error line
/// shows the literal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d124_1_tools_pin_unpin_of_an_unknown_id_is_not_found() {
    let _g = LOCK.lock().await;
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), &db);
    vault_with_canary(&e);
    let v = canary();
    let c = ctx(e.clone(), store.clone(), None);
    let with_value = format!("luma.com/{v}");
    for (tool, literal) in [
        ("luma.com/unknown-tool", false),
        (with_value.as_str(), true),
    ] {
        for cmd in ["pin", "unpin"] {
            empty_global_vault();
            let (r, out) = run_cli(&c, &["tools", cmd, tool]).await;
            let what = redact(&format!("tools {cmd} {tool}"));
            let err = r.expect_err(&what);
            assert!(installed(), "{what} did not install the vault");
            assert!(
                matches!(err, CliError::NotFound(_)),
                "{what}: want NotFound, got {}",
                redact(&binary_error_line(&err))
            );
            assert!(out.is_empty(), "{what} printed {}", redact(&out));
            let line = binary_error_line(&err);
            assert_clean(&line, literal, &what);
            assert!(
                line.contains("no tool luma.com/"),
                "{what}: {}",
                redact(&line)
            );
        }
    }
    let health = db.dump_tables(&["tools.health"]).await;
    assert_eq!(
        health.trim(),
        "== tools.health",
        "a refused pin or unpin wrote a row: {}",
        redact(&health)
    );
}

/// `harness gate set <domain> <class>`: the domain and class are the rule's key, so a vault
/// value in either is refused (B-186 default), the error names the entry and never the value,
/// and gate.rules stays empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b186_gate_set_refuses_a_vault_value_in_the_domain_or_class() {
    let _g = LOCK.lock().await;
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), &db);
    vault_with_canary(&e);
    let v = canary();
    let c = ctx(e.clone(), store.clone(), None);
    let domain = format!("{v}.com");
    let class = format!("Send{v}");
    for (args, what) in [
        (vec!["gate", "set", &domain, "Send", "ask"], "domain"),
        (vec!["gate", "set", "example.com", &class, "ask"], "class"),
    ] {
        empty_global_vault();
        let (r, out) = run_cli(&c, &args).await;
        let label = redact(&args.join(" "));
        let err = r.expect_err(&label);
        assert!(installed(), "{label} did not install the vault");
        assert!(
            matches!(err, CliError::Usage(_)),
            "{label}: want Usage, got {}",
            redact(&binary_error_line(&err))
        );
        assert!(out.is_empty(), "{label} printed {}", redact(&out));
        // The unscrubbed message itself already names the entry, not the value.
        let raw = format!("{err:#}");
        assert_clean(&raw, true, &label);
        assert!(
            raw.contains(&format!("the {what} ")) && raw.contains("B-186"),
            "{label}: {}",
            redact(&raw)
        );
    }
    let rules = db.dump_tables(&["gate.rules"]).await;
    assert!(
        !rules.contains(v),
        "gate.rules holds the value: {}",
        redact(&rules)
    );
    assert!(
        !rules.contains("example.com"),
        "a refused rule was stored: {}",
        redact(&rules)
    );
    // A clean rule still goes in.
    empty_global_vault();
    let (r, _) = run_cli(&c, &["gate", "set", "example.com", "Send", "ask"]).await;
    r.unwrap();
}

/// `harness schedule add` under SB_RUN_ID prints the parent run id; a run started before its
/// id's text was in the vault must print as the literal (like the ledger header).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d124_1_schedule_add_prints_the_parent_run_as_literal() {
    let _g = LOCK.lock().await;
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let mut e = env(tmp.path(), &db);
    vault_with_canary(&e);
    let v = canary();
    let parent = RunId(format!("2026-10-25T09-00-00-{v}"));
    std::fs::create_dir_all(e.runs_root.join(&parent.0)).unwrap();
    store
        .run_start(Run {
            id: parent.clone(),
            started: Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap(),
            ended: None,
            prompt: "p".into(),
            status: RunStatus::Running,
            folder: "runs/x".into(),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    e.run_id = Some(parent.clone());
    let c = ctx(e.clone(), store.clone(), None);
    empty_global_vault();
    let (r, out) = run_cli(&c, &["schedule", "add", "*/5 * * * *", "say hi"]).await;
    r.unwrap_or_else(|e| panic!("schedule add: {}", redact(&binary_error_line(&e))));
    assert!(installed());
    assert_clean(&out, true, "schedule add output");
    assert!(
        out.contains(&format!("parent run 2026-10-25T09-00-00-{LITERAL}")),
        "{}",
        redact(&out)
    );
}

/// Commands that write (rows, run folders, files) store no vault value, print none, and each
/// installs the vault first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d124_1_writing_commands_store_and_print_no_value() {
    let _g = LOCK.lock().await;
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), &db);
    vault_with_canary(&e);
    let v = canary();
    let fake_dir = tmp.path().join("fake");
    let program = fake_claude(&fake_dir);
    let c = ctx(
        e.clone(),
        store.clone(),
        Some(Arc::new(ClaudeLauncher::interactive(program))),
    );
    store
        .tool_health_set(health_row("luma.com", "register", None))
        .await
        .unwrap();
    store
        .tool_health_set(health_row("luma.com", "login", None))
        .await
        .unwrap();
    let ask_run = RunFolder::create(&e.runs_root, "ask here", "p").unwrap();
    let card = harness_core::ask::card_path(&ask_run.dir, 1);
    std::fs::create_dir_all(card.parent().unwrap()).unwrap();
    std::fs::write(&card, "{}").unwrap();

    let add_prompt = format!("{v} check mail");
    let why = format!("{v} broke it");
    let run_prompt = format!("log in as {v} please");
    let watch_target = format!("https://example.com/{v}/inbox");
    let cases: Vec<Vec<&str>> = vec![
        vec!["schedule", "add", "*/1 * * * *", &add_prompt],
        vec![
            "schedule",
            "add",
            "*/2 * * * *",
            "watch the inbox",
            "--kind",
            "watch",
            "--watch-target",
            &watch_target,
        ],
        vec!["schedule", "list"],
        vec!["gate", "set", "example.com", "Send", "ask"],
        vec!["tools", "pin", "luma.com/login"],
        vec!["tools", "unpin", "luma.com/login"],
        vec![
            "tools",
            "retire",
            "luma.com/register",
            "--why",
            &why,
            "--no-commit",
        ],
        vec!["ask", &ask_run.id.0, "1", "yes"],
        vec!["run", &run_prompt],
        vec!["db", "status"],
        vec!["db", "migrate"],
    ];
    let mut outs = String::new();
    for a in &cases {
        empty_global_vault();
        let (r, out) = run_cli(&c, a).await;
        let what = redact(&a.join(" "));
        r.unwrap_or_else(|e| panic!("{what}: {}", redact(&e.to_string())));
        assert!(installed(), "{what} did not install the vault");
        assert_clean(&out, a[..2] == ["schedule", "list"], &what);
        outs.push_str(&out);
        println!("d124_1 write ok: {what}");
    }
    let list = store.schedule_list().await.unwrap();
    assert_eq!(list.len(), 2);
    let cron = list.iter().find(|s| s.kind == ScheduleKind::Cron).unwrap();
    assert_eq!(cron.prompt, format!("{LITERAL} check mail"));
    let watch = list.iter().find(|s| s.kind == ScheduleKind::Watch).unwrap();
    assert_eq!(
        watch.watch_target.as_deref().map(redact),
        Some(format!("https://example.com/{LITERAL}/inbox")),
        "schedules.watch_target is stored scrubbed"
    );
    empty_global_vault();
    let id = cron.id.to_string();
    let (r, out) = run_cli(&c, &["schedule", "rm", &id]).await;
    r.unwrap();
    assert!(installed(), "schedule rm did not install the vault");
    assert_clean(&out, false, "schedule rm");

    // `harness run`: the interactive session got the scrubbed prompt as its first argument.
    let argv_files: Vec<_> = std::fs::read_dir(&fake_dir)
        .unwrap()
        .flatten()
        .map(|d| d.path())
        .filter(|p| p.extension().is_some_and(|x| x == "argv"))
        .collect();
    assert_eq!(argv_files.len(), 1);
    let argv: Vec<String> = read(&argv_files[0]).lines().map(str::to_owned).collect();
    assert_eq!(argv, want_argv(&format!("log in as {LITERAL} please")));

    let health = store.tool_health("luma.com").await.unwrap();
    let reg = health.iter().find(|h| h.name == "register").unwrap();
    assert_eq!(
        reg.why.as_deref(),
        Some(format!("{LITERAL} broke it").as_str())
    );

    let dump = db.dump().await;
    assert_clean(&dump, true, "the database");
    for dir in [&e.runs_root, &e.tools_root, &fake_dir] {
        let hits = files_containing(dir, v);
        assert!(hits.is_empty(), "files holding the value: {hits:?}");
    }
    assert_clean(&outs, false, "all output");
}

/// `harness gate set --note` stores the note in gate.rules; a vault value typed into it must
/// be stored as the literal (rule 2: outgoing strings are scrubbed), like schedule add's
/// prompt and tools retire's why.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d124_1_gate_set_note_is_scrubbed_before_the_database() {
    let _g = LOCK.lock().await;
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), &db);
    vault_with_canary(&e);
    let c = ctx(e.clone(), store.clone(), None);
    let note = format!("card pin is {}", canary());
    empty_global_vault();
    let (r, out) = run_cli(
        &c,
        &["gate", "set", "example.com", "Send", "ask", "--note", &note],
    )
    .await;
    r.unwrap();
    assert!(installed());
    assert_clean(&out, false, "gate set output");
    let rules = db.dump_tables(&["gate.rules"]).await;
    assert_clean(&rules, true, "gate.rules");
}

/// When the vault cannot be opened every command except `vault set` fails before it prints
/// or writes anything, and the error line holds no value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d124_1_every_command_fails_closed_on_a_broken_vault() {
    let _g = LOCK.lock().await;
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), &db);
    let v = canary();
    let rec = Arc::new(RecordingLauncher::default());
    let c = ctx(e.clone(), store.clone(), Some(rec.clone()));
    store
        .tool_health_set(health_row("luma.com", "register", None))
        .await
        .unwrap();
    let sid = store
        .schedule_add(cron_row("say hi".into()))
        .await
        .unwrap()
        .to_string();
    let ask_run = RunFolder::create(&e.runs_root, "ask here", "p").unwrap();
    let card = harness_core::ask::card_path(&ask_run.dir, 1);
    std::fs::create_dir_all(card.parent().unwrap()).unwrap();
    std::fs::write(&card, "{}").unwrap();
    std::fs::create_dir_all(&e.tools_root).unwrap();
    let add_prompt = format!("{v} check mail");
    let cases: Vec<Vec<&str>> = vec![
        vec!["schedule", "add", "*/1 * * * *", &add_prompt],
        vec!["schedule", "list"],
        vec!["schedule", "rm", &sid],
        vec!["gate", "set", "example.com", "Send", "allow"],
        vec!["tools", "list"],
        vec!["tools", "health"],
        vec!["tools", "pin", "luma.com/register"],
        vec!["tools", "unpin", "luma.com/register"],
        vec!["tools", "retire", "luma.com/register", "--no-commit"],
        vec!["ledger", &ask_run.id.0],
        vec!["ask", &ask_run.id.0, "1", "yes"],
        vec!["run", "say hi"],
        vec!["db", "status"],
        vec!["db", "migrate"],
    ];

    for (label, broken) in [
        ("not an age file", None),
        ("an entry under the D-118 floor", Some(())),
    ] {
        match broken {
            None => std::fs::write(&e.vault_path, b"not an age file").unwrap(),
            Some(()) => {
                let _ = std::fs::remove_file(&e.vault_path);
                vault_with_canary(&e);
                let KeySource::IdentityFile(key) = &e.vault_key else {
                    unreachable!()
                };
                write_vault_file(
                    &e.vault_path,
                    key,
                    &[("test", v, "secret"), ("pin", "abc", "secret")],
                );
            }
        }
        let db_before = db.dump().await;
        let tree_before = tree(tmp.path());
        for a in &cases {
            let (r, out) = run_cli(&c, a).await;
            let what = format!("{label}: {}", redact(&a.join(" ")));
            let err = r.expect_err(&what);
            assert!(
                matches!(err, CliError::Vault(_)),
                "{what}: {}",
                redact(&err.to_string())
            );
            assert!(out.is_empty(), "{what} printed: {}", redact(&out));
            let line = binary_error_line(&err);
            assert_clean(&line, false, &what);
            if broken.is_some() {
                assert!(line.contains("harness vault set pin"), "{what}: {line}");
            }
        }
        assert_eq!(db.dump().await, db_before, "{label}: the database changed");
        assert_eq!(tree(tmp.path()), tree_before, "{label}: files changed");
        assert!(rec.seen.lock().unwrap().is_empty(), "a session started");
        println!("d124_1 fail closed ok: {label}");
    }
}

/// The real binaries fail closed too: SB_VAULT_PATH is a folder, so the vault cannot be read
/// (and the OS keychain is never reached). Every command exits 1 with nothing on stdout and
/// writes nothing; harnessd refuses to start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d124_1_binaries_fail_closed_printing_nothing() {
    let _g = LOCK.lock().await;
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let vault_dir = tmp.path().join("vault-is-a-folder");
    std::fs::create_dir_all(&vault_dir).unwrap();
    let fake_dir = tmp.path().join("fake");
    let program = fake_claude(&fake_dir);
    store
        .tool_health_set(health_row("luma.com", "register", None))
        .await
        .unwrap();
    let sid = store
        .schedule_add(cron_row("say hi".into()))
        .await
        .unwrap()
        .to_string();
    let runs = tmp.path().join("runs");
    let ask_run = RunFolder::create(&runs, "ask here", "p").unwrap();
    let card = harness_core::ask::card_path(&ask_run.dir, 1);
    std::fs::create_dir_all(card.parent().unwrap()).unwrap();
    std::fs::write(&card, "{}").unwrap();
    std::fs::create_dir_all(tmp.path().join("tools")).unwrap();
    let db_before = db.dump().await;
    let tree_before = tree(tmp.path());
    let v = canary().to_owned();
    let add_prompt = format!("{v} check mail");
    let cases: Vec<Vec<&str>> = vec![
        vec!["schedule", "add", "*/1 * * * *", &add_prompt],
        vec!["schedule", "list"],
        vec!["schedule", "rm", &sid],
        vec!["gate", "set", "example.com", "Send", "allow"],
        vec!["tools", "list"],
        vec!["tools", "health"],
        vec!["tools", "pin", "luma.com/register"],
        vec!["tools", "retire", "luma.com/register", "--no-commit"],
        vec!["ledger", &ask_run.id.0],
        vec!["ask", &ask_run.id.0, "1", "yes"],
        vec!["run", "say hi"],
        vec!["db", "status"],
        vec!["db", "migrate"],
    ];
    let cmd = |bin: &str| {
        let mut c = tokio::process::Command::new(bin);
        c.current_dir(tmp.path())
            .env("DATABASE_URL", &db.url)
            .env("SB_RUNS_ROOT", "runs")
            .env("SB_TOOLS_ROOT", "tools")
            .env("SB_VAULT_PATH", &vault_dir)
            .env("SB_CLAUDE", &program)
            .env("SB_VAULT_CANARY", &v)
            .env_remove("SB_RUN_ID")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        c
    };
    for a in &cases {
        let mut c = cmd(env!("CARGO_BIN_EXE_harness"));
        c.args(a);
        let o = timeout(CAP, c.output()).await.unwrap().unwrap();
        let what = redact(&a.join(" "));
        let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
        assert_eq!(o.status.code(), Some(1), "{what}: {}", redact(&stderr));
        assert!(
            o.stdout.is_empty(),
            "{what} printed: {}",
            redact(&String::from_utf8_lossy(&o.stdout))
        );
        assert!(stderr.starts_with("harness: vault: "), "{what}: {stderr}");
        assert_clean(&stderr, false, &what);
        println!("d124_1 binary ok: {what}: {}", stderr.trim());
    }
    let o = timeout(CAP, cmd(env!("CARGO_BIN_EXE_harnessd")).output())
        .await
        .unwrap()
        .unwrap();
    let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
    assert_eq!(o.status.code(), Some(1), "harnessd: {stderr}");
    assert!(
        stderr.starts_with("harnessd: vault: "),
        "harnessd: {stderr}"
    );
    assert!(o.stdout.is_empty());
    println!("d124_1 harnessd refused: {}", stderr.trim());
    assert_eq!(db.dump().await, db_before, "the database changed");
    assert_eq!(tree(tmp.path()), tree_before, "files changed");
}

/// A file `Vault::open` refuses (an entry under the D-118 floor) is repaired by `harness
/// vault set <name>` (open_for_set); other entries survive, nothing is printed in clear.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d124_1_vault_set_repairs_a_short_entry() {
    let _g = LOCK.lock().await;
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), &db);
    let KeySource::IdentityFile(key) = e.vault_key.clone() else {
        unreachable!()
    };
    vault_with_canary(&e);
    write_vault_file(
        &e.vault_path,
        &key,
        &[("test", canary(), "secret"), ("pin", "abc", "secret")],
    );
    assert!(matches!(
        Vault::open(&e.vault_path, &e.vault_key),
        Err(VaultError::ValueTooShort { ref name, .. }) if name == "pin"
    ));
    let pin_value = format!("sbpin-{}", random_hex());
    let pv = pin_value.clone();
    let c = Ctx {
        env: e.clone(),
        store: Some(store.clone()),
        read_secret: Box::new(move |_| Ok(Zeroizing::new(pv.clone()))),
        launcher: None,
    };
    let (r, out) = run_cli(&c, &["schedule", "list"]).await;
    let err = r.expect_err("open refuses the file");
    assert!(out.is_empty());
    let line = binary_error_line(&err);
    assert!(line.contains("harness vault set pin"), "{}", redact(&line));
    assert_clean(&line, false, "the refusal");

    empty_global_vault();
    let (r, out) = run_cli(&c, &["vault", "set", "pin"]).await;
    r.unwrap_or_else(|e| panic!("vault set pin: {}", redact(&e.to_string())));
    assert_eq!(out, "vault: pin set (secret)\n");
    assert!(!installed(), "vault set installed its handle");
    let fixed = Vault::open(&e.vault_path, &e.vault_key).expect("repaired");
    assert!(fixed
        .resolve_into("@vault(pin)", |r| r.expose() == pin_value)
        .unwrap());
    assert!(fixed
        .resolve_into(LITERAL, |r| r.expose() == canary())
        .unwrap());
    let (r, out) = run_cli(&c, &["schedule", "list"]).await;
    r.unwrap();
    assert!(out.starts_with("id  kind"), "{out}");
    for needle in [canary(), pin_value.as_str()] {
        let hits = files_containing(tmp.path(), needle);
        assert!(hits.is_empty(), "files holding a value in clear: {hits:?}");
    }
}

// ------------------------------------------------------------------ D-124 (2)

/// harnessd's launcher (ReopenVault over the real ClaudeLauncher) re-opens the vault per
/// firing: started with an empty vault, a value added afterwards with `harness vault set`
/// is scrubbed from the next firing's prompt.md, runs.prompt, session stdin and answer.md;
/// a vault that breaks stops the next firing before anything is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d124_2_harnessd_reopens_the_vault_per_firing() {
    let _g = LOCK.lock().await;
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), &db);
    let v = canary();
    let fake_dir = tmp.path().join("fake");
    let program = fake_claude(&fake_dir);
    // harnessd's startup install, with no entries yet.
    e.install_vault().unwrap();
    assert!(!installed());

    let start = Utc.with_ymd_and_hms(2026, 10, 25, 12, 0, 30).unwrap();
    let clock = Arc::new(ManualClock::new(start));
    let (tx, mut fired) = tokio::sync::mpsc::unbounded_channel();
    let daemon = Daemon::new(
        store.clone(),
        Arc::new(ReopenVault {
            env: e.clone(),
            inner: Arc::new(ClaudeLauncher::print(program)),
        }),
        clock.clone(),
        e.runs_root.clone(),
        Some(tx),
    )
    .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let harnessd = tokio::spawn(daemon.run(async move {
        let _ = stopped.await;
    }));

    // `harness vault set test` after harnessd started.
    let c = ctx(e.clone(), store.clone(), None);
    let (r, _) = run_cli(&c, &["vault", "set", "test"]).await;
    r.unwrap();
    assert!(!installed(), "vault set installs nothing");
    let id = store
        .schedule_add(cron_row(format!("{v} check mail [[emit 7 7]]")))
        .await
        .unwrap();
    sched::notify_changed(&e.runs_root).unwrap();

    let due = timeout(CAP, clock.next_deadline()).await.unwrap();
    clock.set(due);
    let f = timeout(CAP, fired.recv()).await.unwrap().unwrap();
    assert_eq!(f.schedule, id);
    let f = f
        .result
        .unwrap_or_else(|e| panic!("firing: {}", redact(&e.to_string())));
    assert_eq!(f.status, RunStatus::Done, "{:?}", f.launch_problem);
    let scrubbed = format!("{LITERAL} check mail [[emit 7 7]]");
    assert_clean(&f.run.0, false, "the run id");
    assert_eq!(read(f.folder.join("prompt.md")).trim_end(), scrubbed);
    let row = store.run_get(&f.run).await.unwrap().unwrap();
    assert_eq!(row.prompt, scrubbed);
    assert_eq!(
        task_of(&read(fake_dir.join(format!("{}.stdin", f.run.0))), &f.run.0),
        scrubbed
    );
    let answer = read(f.folder.join("files").join(ANSWER_FILE));
    assert_eq!(answer, format!("aaaaaaa{LITERAL}bbbbbbb"));
    assert_clean(&read(f.folder.join("report.md")), false, "report.md");

    // The vault breaks: the next firing does not start, and writes nothing.
    std::fs::write(&e.vault_path, b"not an age file").unwrap();
    let runs_before = std::fs::read_dir(&e.runs_root).unwrap().count();
    let rows_before = db.dump_tables(&["runs", "ledger.events"]).await;
    let due = timeout(CAP, clock.next_deadline()).await.unwrap();
    clock.set(due);
    let f2 = timeout(CAP, fired.recv()).await.unwrap().unwrap();
    match f2.result {
        Err(SchedError::BeforeRun(e)) => assert_clean(&e.0, false, "the refusal"),
        other => panic!("expected BeforeRun, got {:?}", other.map(|f| f.run)),
    }
    assert_eq!(
        std::fs::read_dir(&e.runs_root).unwrap().count(),
        runs_before
    );
    assert_eq!(
        db.dump_tables(&["runs", "ledger.events"]).await,
        rows_before
    );

    stop.send(()).unwrap();
    timeout(CAP, harnessd).await.unwrap().unwrap().unwrap();
    // The schedules row was planted with the raw value; everything harnessd wrote is clean.
    assert_clean(
        &db.dump_tables(&["runs", "ledger.events"]).await,
        true,
        "runs and ledger.events",
    );
    for dir in [&e.runs_root, &fake_dir] {
        let hits = files_containing(dir, v);
        assert!(hits.is_empty(), "files holding the value: {hits:?}");
    }
}

// ------------------------------------------------------------------ D-124 (3)

/// A hung `claude -p` session (the exe, and the claude.cmd shim whose child must die too) is
/// killed at the cap; the run is failed with a core `launch` event (ok false) in the table
/// and the jsonl, and report.md says why. The wait is the launcher's own cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d124_3_session_cap_kills_a_hung_session() {
    let _g = LOCK.lock().await;
    let (_db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    for shim in [false, true] {
        let label = if shim { "claude.cmd shim" } else { "exe" };
        if shim && !cfg!(windows) {
            continue;
        }
        let fake_dir = tmp.path().join(if shim { "fake-shim" } else { "fake-exe" });
        let program = if shim {
            fake_claude_cmd_shim(&fake_dir)
        } else {
            fake_claude(&fake_dir)
        };
        let mut l = ClaudeLauncher::print(program);
        l.max_session = Duration::from_secs(2);
        let r = Runner {
            store: store.clone(),
            launcher: Arc::new(l),
            clock: Arc::new(SystemClock),
            runs_root: tmp.path().join("runs"),
        };
        let t0 = Instant::now();
        let f = timeout(CAP, r.execute(spec("wait forever [[hang]]")))
            .await
            .unwrap_or_else(|_| panic!("{label}: the cap did not end the session"))
            .unwrap();
        let took = t0.elapsed();
        let pid: u32 = read(fake_dir.join(format!("{}.hanging", f.run.0)))
            .trim()
            .parse()
            .unwrap();
        assert!(
            took >= Duration::from_secs(2),
            "{label}: ended early {took:?}"
        );
        assert!(took < Duration::from_secs(15), "{label}: took {took:?}");
        assert!(
            gone_within(pid, Duration::from_secs(10)).await,
            "{label}: the hung session {pid} survived"
        );
        assert_eq!(f.status, RunStatus::Failed, "{label}");
        let why = f.launch_problem.clone().unwrap();
        assert!(
            why.contains("past its 2s cap and was killed"),
            "{label}: {why}"
        );
        let row = store.run_get(&f.run).await.unwrap().unwrap();
        assert_eq!(row.status, RunStatus::Failed, "{label}");
        assert!(row.ended.is_some(), "{label}");
        let ev = store.ledger_events(&f.run).await.unwrap();
        assert!(
            ev.iter().any(|(_, e)| e.server == "core"
                && e.tool == LAUNCH_TOOL
                && !e.ok
                && e.why.as_deref() == Some(why.as_str())),
            "{label}: launch event {ev:?}"
        );
        let jsonl = harness_core::ledger::read_jsonl(&f.folder.join("ledger.jsonl")).unwrap();
        assert_eq!(jsonl.len(), ev.len(), "{label}: jsonl rows");
        assert!(
            jsonl.iter().any(|(_, e)| e.tool == LAUNCH_TOOL && !e.ok),
            "{label}: jsonl launch event"
        );
        let report = read(f.folder.join("report.md"));
        assert!(
            report.contains(&format!("Result: failed ({why})")),
            "{label}: {report}"
        );
        println!("d124_3 {label}: killed after {took:?}, pid {pid} gone, why: {why}");
    }
}

// ------------------------------------------------------------------ D-124 (4)

/// stdout is capped at 1 MiB (the default) and scrubbed before the cut: a value across the
/// 1 MiB cut, across the read limit (1 MiB + 64 KiB) or inside the kept part is never
/// written in clear or in part.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d124_4_answer_capped_at_1_mib_and_scrubbed_before_the_cut() {
    let _g = LOCK.lock().await;
    let v = canary();
    Vault::in_memory([("test".to_owned(), v.to_owned(), EntryKind::Secret)]).install_global();
    let (_db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let fake_dir = tmp.path().join("fake");
    let l = ClaudeLauncher::print(fake_claude(&fake_dir));
    assert_eq!(l.max_answer_bytes, MIB, "the default cap");
    let r = Runner {
        store,
        launcher: Arc::new(l),
        clock: Arc::new(SystemClock),
        runs_root: tmp.path().join("runs"),
    };
    let keep = MIB + SLACK;
    for (case, a, p) in [
        ("across the 1 MiB cut", MIB - 6, 1000usize),
        ("across the read limit", keep - 6, 1000),
        ("inside the kept part", 10, 2 * MIB),
    ] {
        let prompt = format!("print a lot [[emit {a} {p}]]");
        let f = timeout(CAP, r.execute(spec(&prompt)))
            .await
            .expect("run within the cap")
            .unwrap();
        assert_eq!(f.status, RunStatus::Done, "{case}");
        let answer = read(f.folder.join("files").join(ANSWER_FILE));
        let total = a + v.len() + p;
        let (body, note) = answer
            .rsplit_once("\n\n[answer cut: kept ")
            .unwrap_or_else(|| panic!("{case}: no cut note"));
        assert!(body.len() <= MIB, "{case}: {} bytes kept", body.len());
        assert!(
            note.ends_with(&format!(" of {total} bytes]\n")),
            "{case}: {note}"
        );
        assert_clean(&answer, case == "inside the kept part", case);
        for k in 2..v.len() {
            assert!(
                !body.ends_with(&v[..k]),
                "{case}: {k} bytes of the value written"
            );
        }
        assert!(!body.contains(&v[..4]), "{case}: part of the value written");
        println!(
            "d124_4 {case}: total {total}, kept {} bytes, note `[answer cut: kept {}",
            body.len(),
            note.trim_end()
        );
    }
}

// ------------------------------------------------------------------ D-124 (5), B-192

/// Every session's argv is the mode argument (`-p`, or the prompt when interactive), then
/// `--disallowedTools` and the three rules as separate arguments; the prompt arrives whole
/// (stdin for `-p`, the first positional argument for interactive), also through claude.cmd.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d124_5_b192_argv_denies_user_only_commands_and_the_prompt_arrives() {
    let _g = LOCK.lock().await;
    empty_global_vault();
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), &db);
    let prompt = "-h say \"hi\" & | ^ %PATH% < > (done)";
    for shim in [false, true] {
        if shim && !cfg!(windows) {
            continue;
        }
        let label = if shim { "claude.cmd shim" } else { "exe" };
        let fake_dir = tmp.path().join(if shim { "fake-shim" } else { "fake-exe" });
        let program = if shim {
            fake_claude_cmd_shim(&fake_dir)
        } else {
            fake_claude(&fake_dir)
        };
        // Scheduled: `-p`, the rules, the prompt on stdin.
        let r = Runner {
            store: store.clone(),
            launcher: Arc::new(ClaudeLauncher::print(program.clone())),
            clock: Arc::new(SystemClock),
            runs_root: e.runs_root.clone(),
        };
        let f = timeout(CAP, r.execute(spec(prompt)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(f.status, RunStatus::Done, "{label}: {:?}", f.launch_problem);
        let argv: Vec<String> = read(fake_dir.join(format!("{}.argv", f.run.0)))
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(argv, want_argv("-p"), "{label}: -p argv");
        assert_eq!(
            task_of(&read(fake_dir.join(format!("{}.stdin", f.run.0))), &f.run.0),
            prompt,
            "{label}: stdin"
        );
        println!("d124_5 {label} -p argv {argv:?}");

        // Interactive `harness run`: the prompt first, then the rules.
        for p in [prompt, "say hi to the team"] {
            let c = ctx(
                e.clone(),
                store.clone(),
                Some(Arc::new(ClaudeLauncher::interactive(program.clone()))),
            );
            let (res, out) = run_cli(&c, &["run", "--", p]).await;
            res.unwrap();
            let run = out
                .strip_prefix("run ")
                .and_then(|s| s.split(':').next())
                .unwrap()
                .to_owned();
            let argv: Vec<String> = read(fake_dir.join(format!("{run}.argv")))
                .lines()
                .map(str::to_owned)
                .collect();
            let first = if p.starts_with('-') {
                format!(" {p}")
            } else {
                p.to_owned()
            };
            assert_eq!(argv, want_argv(&first), "{label}: interactive argv");
            println!("d124_5 {label} interactive argv {argv:?}");
        }
    }
}
