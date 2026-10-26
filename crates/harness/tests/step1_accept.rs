//! Step-1 acceptance (docs/01-HARNESS-CORE.md "Acceptance for this doc (step 1)", all four
//! bullets; the touch_contact item of docs/02-MEMORY.md; H-6, H-2, H-3, H-5; D-122, D-121).
//!
//! The fake tool server is `examples/echo.rs`, spawned over stdio as Claude Code would spawn
//! a tool server, on a throwaway `sb_t_<random>` database (D-100) of the compose Postgres
//! (SB_TEST_DATABASE_URL, default postgres://sb@127.0.0.1:5433/postgres), dropped at the end.
//! The test creates the run folder and `runs` row the way `harness run` does and hands the
//! run to echo through SB_RUN_ID.
//!
//! Finding the example: Cargo exposes `CARGO_BIN_EXE_<name>` for bins only, and
//! `cargo test --test step1_accept` does not build examples. So the first test that needs
//! echo runs `cargo build -p harness --example echo` once (same profile, same target dir as
//! this test binary, derived from `current_exe()`), then starts
//! `<target>/<profile>/examples/echo[.exe]`. A no-op when it is fresh.
//!
//! The other docs/02 step-1 items (recall why=both, 3-hop neighbors, SM-2) are store-only and
//! live in crates/core-pg/tests/accept_t03.rs; this file covers the one that crosses the gate
//! (touch_contact makes the next Send allow) end to end through echo.
//!
//! No model is called (rule 1): the scheduler item uses a fake Launcher. No fixed sleeps
//! (rule 6): every wait is on a reply, a file, a channel or a clock deadline, with a cap.

use async_trait::async_trait;
use core_pg::PgStore;
use harness::daemon::Daemon;
use harness_core::class::Class;
use harness_core::ledger;
use harness_core::memory::*;
use harness_core::run::{RunFolder, RunId};
use harness_core::sched::{
    next_after, parse_cron, LaunchError, LaunchExit, LaunchRequest, Launcher, ManualClock,
    FIRE_TOOL,
};
use harness_core::vault::{EntryKind, KeySource, Vault, MIN_VALUE_CHARS};
use serde_json::{json, Value};
use sqlx::{Connection, Row};
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;

/// Cap on any single wait (a reply, a firing, the child's exit).
const CAP: Duration = Duration::from_secs(60);
/// echo's ask timer (`--ask-timeout-ms`): long enough to see the card, short enough to wait out.
const ASK_MS: u64 = 1500;
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
    /// A fresh database, migrated by `PgStore::connect`.
    async fn store() -> (TestDb, Arc<dyn MemoryStore>) {
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
        let db = TestDb {
            url: with_db(&admin, &name),
            name,
        };
        let store = PgStore::connect(&db.url)
            .await
            .unwrap_or_else(|e| panic!("connect + migrate {}: {e}", db.name));
        (db, Arc::new(store))
    }

    async fn conn(&self) -> sqlx::PgConnection {
        sqlx::PgConnection::connect(&self.url)
            .await
            .expect("connect to the test database")
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

// ------------------------------------------------------------------ the echo example

/// `<target>/<profile>/examples/echo[.exe]`, built once per test binary.
fn echo_bin() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let exe = std::env::current_exe().expect("test binary path");
        // <target>/<profile>/deps/step1_accept-<hash>[.exe]
        let profile_dir = exe
            .parent()
            .and_then(Path::parent)
            .expect("test binary lives in <target>/<profile>/deps")
            .to_path_buf();
        let target_dir = profile_dir.parent().expect("<target>").to_path_buf();
        let profile = match profile_dir.file_name().and_then(|n| n.to_str()) {
            Some("debug") | None => "dev".to_owned(),
            Some(p) => p.to_owned(),
        };
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        let out = std::process::Command::new(cargo)
            .args(["build", "--quiet", "-p", "harness", "--example", "echo"])
            .arg("--profile")
            .arg(&profile)
            .arg("--target-dir")
            .arg(&target_dir)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("run cargo build --example echo");
        assert!(
            out.status.success(),
            "cargo build --example echo failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bin = profile_dir
            .join("examples")
            .join(format!("echo{}", std::env::consts::EXE_SUFFIX));
        assert!(bin.is_file(), "no echo example at {}", bin.display());
        bin
    })
    .clone()
}

/// One echo server process and its stdio.
struct Echo {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    lines: tokio::sync::mpsc::UnboundedReceiver<String>,
    stdout_task: tokio::task::JoinHandle<Vec<u8>>,
    stderr_task: tokio::task::JoinHandle<Vec<u8>>,
    next_id: u64,
}

/// What the child wrote, collected after it exited.
struct Transcript {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Echo {
    async fn start(s: &Session) -> Echo {
        let mut cmd = tokio::process::Command::new(echo_bin());
        cmd.arg("--vault-key")
            .arg(&s.vault_key)
            .arg("--sink")
            .arg(&s.sink)
            .arg("--ask-timeout-ms")
            .arg(ASK_MS.to_string())
            .env("DATABASE_URL", &s.db.url)
            .env("SB_VAULT_PATH", &s.vault_path)
            .env("SB_RUNS_ROOT", &s.runs_root)
            .env("SB_RUN_ID", &s.run.id.0)
            // The value reaches echo through the vault only.
            .env_remove("SB_VAULT_CANARY")
            .current_dir(s.tmp.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().expect("spawn echo");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("echo stdout");
        let mut stderr = child.stderr.take().expect("echo stderr");
        let (tx, lines) = tokio::sync::mpsc::unbounded_channel();
        let stdout_task = tokio::spawn(async move {
            let mut all = Vec::new();
            let mut r = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                match r.read_until(b'\n', &mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        all.extend_from_slice(&line);
                        let _ = tx.send(String::from_utf8_lossy(&line).into_owned());
                    }
                }
            }
            all
        });
        let stderr_task = tokio::spawn(async move {
            let mut all = Vec::new();
            let _ = stderr.read_to_end(&mut all).await;
            all
        });
        let mut echo = Echo {
            child,
            stdin,
            lines,
            stdout_task,
            stderr_task,
            next_id: 1,
        };
        let init = echo
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "step1_accept", "version": "0"},
                }),
            )
            .await;
        assert_eq!(init["result"]["serverInfo"]["name"], "echo", "{init}");
        echo.send_line(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        let list = echo.request("tools/list", json!({})).await;
        assert_eq!(list["result"]["tools"][0]["name"], "echo.send", "{list}");
        echo
    }

    async fn send_line(&mut self, msg: Value) {
        let mut line = serde_json::to_vec(&msg).unwrap();
        line.push(b'\n');
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin.write_all(&line).await.expect("write to echo");
        stdin.flush().await.expect("flush echo stdin");
    }

    /// Sends a request and waits (capped) for the reply with its id.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send_line(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let line = timeout(CAP, self.lines.recv())
                .await
                .unwrap_or_else(|_| panic!("no reply to {method} within {CAP:?}"))
                .unwrap_or_else(|| panic!("echo closed stdout before answering {method}"));
            let msg: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("echo wrote a non-JSON line ({e}): {line}"));
            if msg["id"] == json!(id) {
                return msg;
            }
        }
    }

    /// `tools/call echo.send`; returns the model-visible text and isError.
    async fn send(&mut self, args: Value) -> (String, bool) {
        let reply = self
            .request(
                "tools/call",
                json!({"name": "echo.send", "arguments": args}),
            )
            .await;
        let result = &reply["result"];
        assert!(result.is_object(), "tools/call failed: {reply}");
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        (text, result["isError"] == json!(true))
    }

    /// Closes stdin, waits for a clean exit, returns everything the child wrote.
    async fn finish(mut self) -> Transcript {
        drop(self.stdin.take());
        let status = timeout(CAP, self.child.wait())
            .await
            .expect("echo exits after stdin closes")
            .expect("wait for echo");
        let stdout = self.stdout_task.await.unwrap();
        let stderr = self.stderr_task.await.unwrap();
        assert!(
            status.success(),
            "echo exited with {status}:\n{}",
            String::from_utf8_lossy(&stderr)
        );
        Transcript { stdout, stderr }
    }
}

// ------------------------------------------------------------------ one run

/// A run as `harness run` leaves it (folder + `runs` row, lenient), its database, and the
/// files echo is pointed at.
struct Session {
    db: TestDb,
    store: Arc<dyn MemoryStore>,
    tmp: tempfile::TempDir,
    runs_root: PathBuf,
    run: RunFolder,
    sink: PathBuf,
    vault_path: PathBuf,
    vault_key: PathBuf,
}

impl Session {
    async fn new(prompt: &str) -> Session {
        let (db, store) = TestDb::store().await;
        let tmp = tempfile::tempdir().unwrap();
        let runs_root = tmp.path().join("runs");
        let run = RunFolder::create(&runs_root, prompt, prompt).unwrap();
        store
            .run_start(Run {
                id: run.id.clone(),
                started: ts_now(),
                ended: None,
                prompt: prompt.to_owned(),
                status: RunStatus::Running,
                folder: run.dir.to_string_lossy().replace('\\', "/"),
                parent_run: None,
                strict: false,
            })
            .await
            .unwrap();
        Session {
            sink: tmp.path().join("sink.jsonl"),
            // Missing until a test plants a value: an empty vault that never needs the key.
            vault_path: tmp.path().join("vault").join("vault.toml.age"),
            vault_key: tmp.path().join("vault").join("vault.key"),
            db,
            store,
            tmp,
            runs_root,
            run,
        }
    }

    async fn events(&self) -> Vec<(LedgerId, LedgerEvent)> {
        self.store.ledger_events(&self.run.id).await.unwrap()
    }

    fn sink_lines(&self) -> Vec<Value> {
        match std::fs::read_to_string(&self.sink) {
            Ok(t) => t
                .lines()
                .map(|l| serde_json::from_str(l).expect("sink line is JSON"))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => panic!("read sink: {e}"),
        }
    }

    fn card(&self, n: u64) -> Value {
        let path = harness_core::ask::card_path(&self.run.dir, n);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("ask card {}: {e}", path.display()));
        serde_json::from_str(&text).expect("ask card is JSON")
    }

    fn answer(&self, n: u64) -> String {
        std::fs::read_to_string(harness_core::ask::answer_path(&self.run.dir, n))
            .unwrap_or_default()
            .trim()
            .to_owned()
    }

    /// ledger.jsonl equals the table row for row (keyed by id, H-3).
    async fn assert_mirror(&self) -> Vec<(LedgerId, LedgerEvent)> {
        let rows = self.events().await;
        let jsonl = ledger::read_jsonl(&self.run.ledger_jsonl()).expect("read ledger.jsonl");
        let text = std::fs::read_to_string(self.run.ledger_jsonl()).unwrap();
        assert_eq!(text.lines().count(), rows.len(), "one line per row");
        assert_eq!(
            jsonl, rows,
            "ledger.jsonl matches ledger.events row for row"
        );
        rows
    }
}

fn mode(e: &LedgerEvent) -> Option<LedgerMode> {
    e.mode
}

fn fnv1a64(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

// ------------------------------------------------------------------ bullet 1: the gate

/// "with `to` in contacts -> runs, ledger has class=Send mode=allow".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b1_send_to_contact_runs_with_mode_allow() {
    let s = Session::new("send to a contact").await;
    s.store
        .touch_contact("Ann@Example.com", "mail", "colleague")
        .await
        .unwrap();
    let mut echo = Echo::start(&s).await;
    let (text, is_err) = echo
        .send(json!({"to": "ann@example.com", "body": "hello Ann"}))
        .await;
    echo.finish().await;

    assert!(!is_err, "{text}");
    assert_eq!(text, "sent to ann@example.com (9 chars)");
    let sink = s.sink_lines();
    assert_eq!(sink.len(), 1, "the tool ran once: {sink:?}");
    assert_eq!(sink[0]["to"], "ann@example.com");
    assert_eq!(sink[0]["class"], "Send");
    assert!(
        !harness_core::ask::asks_dir(&s.run.dir).exists(),
        "no ask for a contact"
    );

    let rows = s.assert_mirror().await;
    assert_eq!(rows.len(), 1, "{rows:#?}");
    let e = &rows[0].1;
    assert_eq!(e.server, "echo");
    assert_eq!(e.tool, "echo.send");
    assert_eq!(e.class, Some(Class::Send));
    assert_eq!(mode(e), Some(LedgerMode::Allow));
    assert!(e.ok);
    assert_eq!(e.run_id.as_ref(), Some(&s.run.id));
    assert_eq!(e.args["to"], "ann@example.com", "D-121: the target is `to`");
    assert_eq!(e.result_summary, "sent to ann@example.com (9 chars)");
}

/// "with unknown `to` -> ask card, timeout -> proceed and ledger shows `timeout_proceed`".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b1_unknown_recipient_asks_and_timeout_proceeds() {
    let s = Session::new("send to a stranger").await;
    let mut echo = Echo::start(&s).await;
    let (text, is_err) = echo
        .send(json!({"to": "new@example.org", "body": "hi there"}))
        .await;
    let t = echo.finish().await;

    assert!(!is_err, "{text}");
    assert_eq!(text, "sent to new@example.org (8 chars)");
    // The card was written (H-1.3, D-105) and closed by the timer.
    let card = s.card(1);
    assert_eq!(card["class"], "Send", "{card}");
    assert_eq!(
        card["on_timeout"], "proceed",
        "Send(new) proceeds on timeout"
    );
    assert_eq!(card["run"], s.run.id.0.as_str());
    assert!(
        card.to_string().contains("new@example.org"),
        "the card names the recipient: {card}"
    );
    assert_eq!(
        s.answer(1),
        "timeout",
        "the asker's own timer closed the ask"
    );
    let stderr = String::from_utf8_lossy(&t.stderr);
    assert!(stderr.contains("ask 1 (Send)"), "the notice line: {stderr}");
    assert_eq!(s.sink_lines().len(), 1, "the tool ran after the timeout");

    let rows = s.assert_mirror().await;
    assert_eq!(rows.len(), 2, "{rows:#?}");
    let (ask, call) = (&rows[0].1, &rows[1].1);
    assert_eq!(
        ask.server, "core",
        "D-121 (3): the ask outcome is a core event"
    );
    assert_eq!(ask.tool, "echo.send");
    assert_eq!(ask.class, Some(Class::Send));
    assert_eq!(mode(ask), Some(LedgerMode::TimeoutProceed));
    assert_eq!(
        ask.why.as_deref(),
        Some("send a message to new@example.org")
    );
    assert_eq!(call.server, "echo");
    assert_eq!(call.class, Some(Class::Send));
    assert_eq!(mode(call), Some(LedgerMode::TimeoutProceed));
    assert!(call.ok);

    let report = ledger::report(
        "send to a stranger",
        &rows.into_iter().map(|r| r.1).collect::<Vec<_>>(),
    );
    assert!(
        report.contains("Asked: 1 (0 answered / 1 timed out)"),
        "{report}"
    );
    assert!(report.contains("Did: 1 actions"), "{report}");
}

/// "with class forced to Pay -> timeout -> skip": `amount` makes the classifier raise the
/// declared Send to Pay, even for a known contact; the tool never runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b1_forced_pay_times_out_to_skip() {
    let s = Session::new("pay a contact").await;
    s.store
        .touch_contact("ann@example.com", "mail", "")
        .await
        .unwrap();
    let mut echo = Echo::start(&s).await;
    let (text, is_err) = echo
        .send(json!({"to": "ann@example.com", "body": "rent", "amount": "20 EUR"}))
        .await;
    echo.finish().await;

    assert!(!is_err, "a skip is not a tool error: {text}");
    assert!(
        text.starts_with("skipped: no answer before the ask timed out"),
        "{text}"
    );
    let card = s.card(1);
    assert_eq!(card["class"], "Pay", "{card}");
    assert_eq!(card["on_timeout"], "skip", "Pay skips on timeout");
    assert_eq!(s.answer(1), "timeout");
    assert!(s.sink_lines().is_empty(), "the tool did not run");

    let rows = s.assert_mirror().await;
    assert_eq!(rows.len(), 1, "only the ask outcome: {rows:#?}");
    let e = &rows[0].1;
    assert_eq!(e.server, "core");
    assert_eq!(e.tool, "echo.send");
    assert_eq!(e.class, Some(Class::Pay));
    assert_eq!(mode(e), Some(LedgerMode::TimeoutSkip));
    assert_eq!(e.why.as_deref(), Some("pay 20 EUR to ann@example.com"));
    let report = ledger::report("pay a contact", std::slice::from_ref(e));
    assert!(report.contains("Did: 0 actions"), "{report}");
    assert!(report.contains("timed out, skipped"), "{report}");
}

/// docs/02 step 1: "`touch_contact` makes a Send to that person `allow` on the next gate
/// check", through the wrapper: asked before, allowed after.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn docs02_touch_contact_makes_the_next_send_allow() {
    let s = Session::new("contact then send").await;
    let mut echo = Echo::start(&s).await;
    echo.send(json!({"to": "bo@example.net", "body": "first"}))
        .await;
    s.store
        .touch_contact("bo@example.net", "mail", "replied")
        .await
        .unwrap();
    echo.send(json!({"to": "bo@example.net", "body": "second"}))
        .await;
    echo.finish().await;

    let rows = s.assert_mirror().await;
    let tool: Vec<_> = rows
        .iter()
        .map(|(_, e)| e)
        .filter(|e| e.server == "echo")
        .collect();
    assert_eq!(tool.len(), 2, "{rows:#?}");
    assert_eq!(mode(tool[0]), Some(LedgerMode::TimeoutProceed));
    assert_eq!(mode(tool[1]), Some(LedgerMode::Allow));
    assert!(harness_core::ask::card_path(&s.run.dir, 1).is_file());
    assert!(
        !harness_core::ask::card_path(&s.run.dir, 2).exists(),
        "no second ask"
    );
}

// ------------------------------------------------------------------ bullet 2: the vault

/// Every way an ASCII-case variant of `needle` could show up: raw, and JSON-escaped.
fn needles(canary: &str) -> Vec<Vec<u8>> {
    let mut out = vec![canary.to_ascii_lowercase().into_bytes()];
    let json = serde_json::to_string(canary).unwrap();
    let inner = json[1..json.len() - 1].to_ascii_lowercase().into_bytes();
    if !out.contains(&inner) {
        out.push(inner);
    }
    out
}

fn hits(hay: &[u8], needles: &[Vec<u8>]) -> bool {
    let hay = hay.to_ascii_lowercase();
    needles
        .iter()
        .any(|n| !n.is_empty() && hay.windows(n.len()).any(|w| w == n.as_slice()))
}

/// Every file and folder under `dir`, recursively.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        out.push(p.clone());
        if p.is_dir() {
            walk(&p, out);
        }
    }
}

/// Every row of every user table in the database, as text.
async fn all_rows(db: &TestDb) -> Vec<(String, String)> {
    let mut conn = db.conn().await;
    let tables = sqlx::query(
        "select table_schema, table_name from information_schema.tables \
         where table_type = 'BASE TABLE' \
         and table_schema not in ('pg_catalog', 'information_schema') \
         order by 1, 2",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    let mut out = Vec::new();
    for t in tables {
        let schema: String = t.get(0);
        let name: String = t.get(1);
        let fq = format!(
            "\"{}\".\"{}\"",
            schema.replace('"', "\"\""),
            name.replace('"', "\"\"")
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("select t::text from {fq} t")))
            .fetch_all(&mut conn)
            .await
            .unwrap_or_else(|e| panic!("read {fq}: {e}"));
        for r in rows {
            out.push((fq.clone(), r.get::<String, _>(0)));
        }
    }
    let _ = conn.close().await;
    out
}

fn flip_case(s: &str) -> String {
    s.chars()
        .enumerate()
        .map(|(i, c)| {
            if i % 2 == 0 {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

/// "`@vault(test)` resolves inside the fake tool's sink; the value appears nowhere in
/// stdout, ledger, DB, or `runs/`. A test greps all of them."
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_vault_value_resolves_in_the_sink_and_nowhere_else() {
    let Some(canary) = std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.trim().is_empty())
    else {
        eprintln!(
            "SKIPPED b2_vault_value_resolves_in_the_sink_and_nowhere_else: SB_VAULT_CANARY is \
             unset; set it to a unique string of at least {MIN_VALUE_CHARS} characters"
        );
        return;
    };
    let s = Session::new("vault canary").await;
    {
        std::fs::create_dir_all(s.vault_path.parent().unwrap()).unwrap();
        let key = KeySource::IdentityFile(s.vault_key.clone());
        let mut v = Vault::open(&s.vault_path, &key).unwrap();
        v.set("test", &canary, EntryKind::Secret)
            .unwrap_or_else(|e| panic!("plant SB_VAULT_CANARY as `test`: {e}"));
        let back = Vault::open(&s.vault_path, &key).unwrap();
        assert!(back
            .resolve_into("@vault(test)", |r| r.expose() == canary)
            .unwrap());
    }
    s.store
        .touch_contact("ann@example.com", "mail", "")
        .await
        .unwrap();

    let mut echo = Echo::start(&s).await;
    // 1. Allowed send: the literal goes to the sink.
    let body1 = "pin @vault(test) end";
    let (t1, e1) = echo
        .send(json!({"to": "ann@example.com", "body": body1}))
        .await;
    // 2. An ask whose card shows the body (values shown as `@vault(name)`, H-1.3).
    let body2 = "for you: @vault(test)";
    let (t2, e2) = echo
        .send(json!({"to": "new@example.org", "body": body2}))
        .await;
    // 3. The raw value in the arguments, in another ASCII case, as a model might paste it.
    let body3 = format!("raw {} here", flip_case(&canary));
    let (t3, e3) = echo
        .send(json!({"to": "ann@example.com", "body": body3}))
        .await;
    // 4. A Pay ask that skips, with the literal in its card.
    let (t4, e4) = echo
        .send(json!({"to": "ann@example.com", "body": "@vault(test)", "amount": 5}))
        .await;
    let transcript = echo.finish().await;
    for (t, e) in [(&t1, e1), (&t2, e2), (&t3, e3), (&t4, e4)] {
        assert!(!e, "{t}");
    }

    // The sink saw the resolved value (length and hash), never a literal.
    let sink = s.sink_lines();
    assert_eq!(sink.len(), 3, "{sink:?}");
    let expect = [
        body1.replace("@vault(test)", &canary),
        body2.replace("@vault(test)", &canary),
        body3.clone(),
    ];
    for (line, want) in sink.iter().zip(&expect) {
        assert_eq!(line["unresolved"], false, "{line}");
        assert_eq!(line["bytes"], want.len(), "{line}");
        assert_eq!(line["fnv1a64"], fnv1a64(want.as_bytes()), "{line}");
    }

    // report.md, so the grep covers it too.
    let rows = s.assert_mirror().await;
    let events: Vec<LedgerEvent> = rows.iter().map(|r| r.1.clone()).collect();
    ledger::write_report(&s.run, "vault canary", &events).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.args.to_string().contains("@vault(test)")),
        "the ledger keeps the literal: {events:#?}"
    );
    assert!(
        s.card(1).to_string().contains("@vault(test)"),
        "{}",
        s.card(1)
    );

    let n = needles(&canary);
    let mut leaks: Vec<String> = Vec::new();
    if hits(&transcript.stdout, &n) {
        leaks.push("echo stdout".into());
    }
    if hits(&transcript.stderr, &n) {
        leaks.push("echo stderr".into());
    }
    for text in [&t1, &t2, &t3, &t4] {
        if hits(text.as_bytes(), &n) {
            leaks.push("a tool result".into());
        }
    }
    // Every row of every table, not only ledger.events.
    let db_rows = all_rows(&s.db).await;
    assert!(
        db_rows.iter().any(|(t, _)| t == "\"ledger\".\"events\""),
        "scanned ledger.events"
    );
    for (table, row) in &db_rows {
        if hits(row.as_bytes(), &n) {
            leaks.push(format!("a row of {table}"));
        }
    }
    // Every file name and file under runs/ (ledger.jsonl, report.md, prompt.md, asks/).
    let mut paths = Vec::new();
    walk(&s.runs_root, &mut paths);
    assert!(paths.contains(&s.run.ledger_jsonl()));
    assert!(paths.contains(&s.run.report_md()));
    for p in &paths {
        let rel = p.strip_prefix(&s.runs_root).unwrap();
        if hits(rel.to_string_lossy().as_bytes(), &n) {
            leaks.push(format!("the name {}", rel.display()));
        }
        if p.is_file() && hits(&std::fs::read(p).unwrap(), &n) {
            leaks.push(format!("the file runs/{}", rel.display()));
        }
    }
    if hits(&std::fs::read(&s.sink).unwrap(), &n) {
        leaks.push("the sink file".into());
    }
    // Never print the value: only where it was found.
    assert!(leaks.is_empty(), "SB_VAULT_CANARY found in: {leaks:?}");
}

// ------------------------------------------------------------------ bullet 3: the ledger

/// "`UPDATE ledger.events` fails. `runs/<id>/ledger.jsonl` matches the table row-for-row."
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b3_ledger_is_append_only_and_mirrored() {
    let s = Session::new("ledger mirror").await;
    s.store
        .touch_contact("ann@example.com", "mail", "")
        .await
        .unwrap();
    let mut echo = Echo::start(&s).await;
    echo.send(json!({"to": "ann@example.com", "body": "one"}))
        .await;
    echo.send(json!({"to": "ann@example.com", "body": "two", "amount": "1 EUR"}))
        .await;
    echo.send(json!({"to": "zed@example.com", "body": "three"}))
        .await;
    echo.finish().await;

    let before = s.assert_mirror().await;
    assert_eq!(
        before.len(),
        4,
        "allow, pay skip, ask + proceed: {before:#?}"
    );

    let mut conn = s.db.conn().await;
    for sql in [
        "update ledger.events set why = 'rewritten'",
        "update ledger.events set ok = false where id = (select min(id) from ledger.events)",
        "delete from ledger.events",
    ] {
        let r = sqlx::query(sql).execute(&mut conn).await;
        assert!(r.is_err(), "{sql} must fail at the database");
    }
    let n: i64 = sqlx::query("select count(*) from ledger.events")
        .fetch_one(&mut conn)
        .await
        .unwrap()
        .get(0);
    let _ = conn.close().await;
    assert_eq!(n as usize, before.len());
    assert_eq!(s.assert_mirror().await, before, "nothing changed");
}

// ------------------------------------------------------------------ bullet 4: the scheduler

/// Records each launch and exits 0. Never calls a model (rule 1).
#[derive(Default)]
struct FakeLauncher {
    seen: Mutex<Vec<(RunId, String)>>,
}

#[async_trait]
impl Launcher for FakeLauncher {
    async fn launch(&self, req: LaunchRequest<'_>) -> Result<LaunchExit, LaunchError> {
        self.seen
            .lock()
            .unwrap()
            .push((req.run.id.clone(), req.prompt.to_owned()));
        Ok(LaunchExit::OK)
    }
}

/// "`harness schedule add "*/1 * * * *" "say hi"` fires, creates a run folder with
/// `report.md`, and `runs` has a row with `parent_run` set." The real `harness` binary adds
/// the schedule; harnessd's loop (in process, ManualClock, fake Launcher) is woken by the
/// change marker the binary writes and fires on the next minute.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b4_schedule_add_through_the_cli_fires_a_child_run() {
    let (db, store) = TestDb::store().await;
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let start = ts_now();
    let minute = next_after(&parse_cron("*/1 * * * *").unwrap(), start).unwrap();
    let clock = Arc::new(ManualClock::new(start));
    let launcher = Arc::new(FakeLauncher::default());
    let (tx, mut fired) = tokio::sync::mpsc::unbounded_channel();
    let daemon = Daemon::new(
        store.clone(),
        launcher.clone(),
        clock.clone(),
        runs_root.clone(),
        Some(tx),
    )
    .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let harnessd = tokio::spawn(daemon.run(async move {
        let _ = stopped.await;
    }));

    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_harness"))
        .args(["schedule", "add", "*/1 * * * *", "say hi"])
        .env("DATABASE_URL", &db.url)
        .env("SB_RUNS_ROOT", &runs_root)
        .env("SB_TOOLS_ROOT", tmp.path().join("tools"))
        // Missing file: an empty vault that never touches the keychain.
        .env("SB_VAULT_PATH", tmp.path().join("vault.toml.age"))
        .env("SB_CLAUDE", tmp.path().join("no-claude-here"))
        .env_remove("SB_RUN_ID")
        .current_dir(tmp.path())
        .output();
    let out = timeout(CAP, out).await.expect("harness exits").unwrap();
    assert!(
        out.status.success(),
        "harness schedule add: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("schedule "),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let s = store
        .schedule_list()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.prompt == "say hi")
        .expect("the schedule row");
    assert_eq!(s.cron, "*/1 * * * *");
    let parent = s
        .run_id
        .clone()
        .expect("the CLI's own run is the schedule's run");

    // The marker woke harnessd, which now sleeps until the next minute.
    let due = timeout(CAP, clock.deadline_where(|t| t == minute))
        .await
        .expect("harnessd re-read schedules after `harness schedule add`");
    clock.set(due);
    let f = timeout(CAP, fired.recv())
        .await
        .expect("the schedule fired within the cap")
        .expect("fired channel open");
    assert_eq!(f.schedule, s.id);
    let f = f.result.expect("the firing succeeded");

    let row = store.run_get(&f.run).await.unwrap().expect("runs row");
    assert_eq!(row.parent_run.as_ref(), Some(&parent), "parent_run is set");
    assert_eq!(row.status, RunStatus::Done);
    assert_eq!(row.prompt, "say hi");
    let dir = runs_root.join(&f.run.0);
    assert_eq!(f.folder, dir);
    let report = std::fs::read_to_string(dir.join("report.md")).expect("report.md");
    assert!(report.starts_with("# say hi\nResult: done\n"), "{report}");
    let folder = RunFolder::open(&runs_root, &f.run).unwrap();
    let jsonl = ledger::read_jsonl(&folder.ledger_jsonl()).unwrap();
    assert_eq!(jsonl, store.ledger_events(&f.run).await.unwrap());
    assert_eq!(jsonl.len(), 1);
    assert_eq!(jsonl[0].1.tool, FIRE_TOOL);
    assert_eq!(
        launcher.seen.lock().unwrap().clone(),
        vec![(f.run.clone(), "say hi".to_owned())]
    );

    stop.send(()).unwrap();
    timeout(CAP, harnessd)
        .await
        .expect("harnessd stops")
        .unwrap()
        .unwrap();
    assert!(fired.try_recv().is_err(), "exactly one firing");
}
