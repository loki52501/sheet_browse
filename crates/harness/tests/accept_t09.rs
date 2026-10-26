//! Tester acceptance for T-09 (docs/01-HARNESS-CORE.md "Acceptance for this doc (step 1)",
//! bullet 2, the vault fixture; H-2, H-6; D-122). Independent of the coder's
//! `step1_accept.rs`: a wider set of adversarial calls and a wider set of encoded forms.
//!
//! `t09_vault_canary_*` plants `SB_VAULT_CANARY` as the vault entry `test` of a temp vault,
//! spawns `examples/echo.rs` over stdio and makes calls that put the value (as a literal, raw,
//! flipped case, percent-encoded, JSON-escaped, as the recipient, inside a bad argument) into
//! every path a value could take: the sink, an ask card, the ledger, a tool result, an
//! argument error, a tool error. It then greps, ASCII case-insensitively, for the raw value,
//! its JSON-escaped (once and twice), Rust Debug and percent-encoded (every byte, URI
//! component, form with `+`, path) spellings in: echo's stdout and stderr, every JSON-RPC
//! reply, every row of every base table, every file and file name under `runs/`, the sink
//! file and the stdout and stderr of `harness ledger <run>` on the same database.
//!
//! `SB_T09_KEEP_DIR=<dir>` keeps everything for an external grep: the run root, the sink,
//! `echo.stdout`, `echo.stderr`, `harness-ledger.stdout`, `harness-ledger.stderr`, and the
//! database (its name in `db.txt`; not dropped). Without it, a temp dir and a dropped
//! database (D-100). The test skips with a printed SKIPPED line when SB_VAULT_CANARY is unset.
//!
//! No model is called (rule 1). No fixed sleeps (rule 6): every wait is on a reply or an exit.

use core_pg::PgStore;
use harness_core::memory::*;
use harness_core::run::RunFolder;
use harness_core::vault::{EntryKind, KeySource, Vault};
use serde_json::{json, Value};
use sqlx::{Connection, Row};
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;

const CAP: Duration = Duration::from_secs(60);
const ASK_MS: u64 = 1200;
const DEFAULT_ADMIN_URL: &str = "postgres://sb@127.0.0.1:5433/postgres";

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
    format!("sb_t_a09_{:012x}", h.finish() & 0xffff_ffff_ffff)
}

struct TestDb {
    name: String,
    url: String,
    keep: bool,
}

impl TestDb {
    async fn create(keep: bool) -> (TestDb, Arc<dyn MemoryStore>) {
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
            keep,
        };
        let store = PgStore::connect(&db.url)
            .await
            .unwrap_or_else(|e| panic!("connect + migrate {}: {e}", db.name));
        (db, Arc::new(store))
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let name = self.name.clone();
        let admin = admin_url();
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("cleanup runtime");
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

/// `<target>/<profile>/examples/echo[.exe]`, built once per test binary (as step1_accept does).
fn echo_bin() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let exe = std::env::current_exe().expect("test binary path");
        let profile_dir = exe
            .parent()
            .and_then(Path::parent)
            .expect("<target>/<profile>/deps")
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
            .expect("cargo build --example echo");
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

// ------------------------------------------------------------------ encoded forms

/// Percent-encodes every byte not alphanumeric or in `keep` (upper hex; the grep ignores case).
fn pct(s: &str, keep: &str, space_plus: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || keep.as_bytes().contains(&b) {
            out.push(b as char);
        } else if space_plus && b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn json_inner(s: &str) -> String {
    let j = serde_json::to_string(s).unwrap();
    j[1..j.len() - 1].to_owned()
}

/// Every spelling of the value the grep looks for (lowercased; matched case-insensitively).
fn forms(canary: &str) -> Vec<(&'static str, Vec<u8>)> {
    let dbg = format!("{canary:?}");
    let all = vec![
        ("raw", canary.to_owned()),
        ("json", json_inner(canary)),
        ("json2", json_inner(&json_inner(canary))),
        ("debug", dbg[1..dbg.len() - 1].to_owned()),
        ("pct-all", pct(canary, "", false)),
        ("pct-component", pct(canary, "-_.!~*'()", false)),
        ("pct-form", pct(canary, "*-._", true)),
        ("pct-path", pct(canary, "-._~!$&'()*+,;=:@/", false)),
    ];
    let mut out: Vec<(&'static str, Vec<u8>)> = Vec::new();
    for (k, v) in all {
        let v = v.to_ascii_lowercase().into_bytes();
        if !out.iter().any(|(_, o)| *o == v) {
            out.push((k, v));
        }
    }
    out
}

fn hit(hay: &[u8], forms: &[(&'static str, Vec<u8>)]) -> Option<&'static str> {
    let hay = hay.to_ascii_lowercase();
    forms
        .iter()
        .find(|(_, n)| !n.is_empty() && hay.windows(n.len()).any(|w| w == n.as_slice()))
        .map(|(k, _)| *k)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        out.push(p.clone());
        if p.is_dir() {
            walk(&p, out);
        }
    }
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

fn fnv1a64(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

// ------------------------------------------------------------------ the test

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t09_vault_canary_never_leaves_the_sink() {
    let Some(canary) = std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| c.trim().chars().count() >= 4)
    else {
        eprintln!("SKIPPED t09_vault_canary_never_leaves_the_sink: SB_VAULT_CANARY is unset");
        return;
    };
    let keep_dir = std::env::var("SB_T09_KEEP_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .map(PathBuf::from);
    let _tmp_guard;
    let root: PathBuf = match &keep_dir {
        Some(d) => {
            std::fs::create_dir_all(d).unwrap();
            d.clone()
        }
        None => {
            let t = tempfile::tempdir().unwrap();
            let p = t.path().to_path_buf();
            _tmp_guard = t;
            p
        }
    };
    let (db, store) = TestDb::create(keep_dir.is_some()).await;
    std::fs::write(root.join("db.txt"), format!("{}\n", db.name)).unwrap();
    println!("t09: db={} root={}", db.name, root.display());

    let runs_root = root.join("runs");
    let run = RunFolder::create(&runs_root, "vault canary t09", "vault canary t09").unwrap();
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "vault canary t09".into(),
            status: RunStatus::Running,
            folder: run.dir.to_string_lossy().replace('\\', "/"),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    store
        .touch_contact("ann@example.com", "mail", "")
        .await
        .unwrap();

    let vault_path = root.join("vault").join("vault.toml.age");
    let vault_key = root.join("vault").join("vault.key");
    std::fs::create_dir_all(vault_path.parent().unwrap()).unwrap();
    {
        let key = KeySource::IdentityFile(vault_key.clone());
        let mut v = Vault::open(&vault_path, &key).unwrap();
        v.set("test", &canary, EntryKind::Secret)
            .unwrap_or_else(|e| panic!("plant the canary as `test`: {e}"));
    }
    let sink = root.join("sink.jsonl");

    // ---- echo over stdio
    let mut child = tokio::process::Command::new(echo_bin())
        .arg("--vault-key")
        .arg(&vault_key)
        .arg("--sink")
        .arg(&sink)
        .arg("--ask-timeout-ms")
        .arg(ASK_MS.to_string())
        .env("DATABASE_URL", &db.url)
        .env("SB_VAULT_PATH", &vault_path)
        .env("SB_RUNS_ROOT", &runs_root)
        .env("SB_RUN_ID", &run.id.0)
        .env_remove("SB_VAULT_CANARY")
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn echo");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let (tx, mut lines) = tokio::sync::mpsc::unbounded_channel::<String>();
    let out_task = tokio::spawn(async move {
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
    let err_task = tokio::spawn(async move {
        let mut all = Vec::new();
        let _ = stderr.read_to_end(&mut all).await;
        all
    });

    let mut next_id = 0u64;
    macro_rules! request {
        ($method:expr, $params:expr) => {{
            next_id += 1;
            let id = next_id;
            let mut line = serde_json::to_vec(
                &json!({"jsonrpc": "2.0", "id": id, "method": $method, "params": $params}),
            )
            .unwrap();
            line.push(b'\n');
            stdin.write_all(&line).await.unwrap();
            stdin.flush().await.unwrap();
            loop {
                let l = timeout(CAP, lines.recv())
                    .await
                    .expect("echo answers within the cap")
                    .expect("echo stdout open");
                let msg: Value = serde_json::from_str(&l).expect("JSON-RPC line");
                if msg["id"] == json!(id) {
                    break msg;
                }
            }
        }};
    }
    let init = request!(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {},
               "clientInfo": {"name": "accept_t09", "version": "0"}})
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "echo");
    {
        let mut l =
            serde_json::to_vec(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                .unwrap();
        l.push(b'\n');
        stdin.write_all(&l).await.unwrap();
    }

    let body_pct = format!("enc {} end", pct(&canary, "*-._", true));
    let body_json = format!("esc {} end", json_inner(&canary));
    let calls: Vec<(&str, Value)> = vec![
        // allowed, literal resolved in the sink
        (
            "allow-literal",
            json!({"to": "ann@example.com", "body": "pin @vault(test) end"}),
        ),
        // unknown recipient: ask card, timeout proceed, literal resolved
        (
            "ask-literal",
            json!({"to": "new@example.org", "body": "for you: @vault(test)"}),
        ),
        // the raw value pasted in another ASCII case
        (
            "raw-flipped",
            json!({"to": "ann@example.com", "body": format!("raw {} here", flip_case(&canary))}),
        ),
        // Pay skip with the literal
        (
            "pay-skip",
            json!({"to": "ann@example.com", "body": "@vault(test)", "amount": 5}),
        ),
        // the raw value as the recipient: card, why, ledger args, result text, sink `to`
        ("raw-recipient", json!({"to": canary, "body": "hello"})),
        // percent-encoded and JSON-escaped spellings pasted into the body
        (
            "pct-body",
            json!({"to": "ann@example.com", "body": body_pct}),
        ),
        (
            "json-body",
            json!({"to": "ann@example.com", "body": body_json}),
        ),
        // a bad argument carrying the raw value (argument error path)
        (
            "bad-amount",
            json!({"to": "ann@example.com", "body": canary, "amount": {"k": canary}}),
        ),
        // an unknown vault name next to the raw value (tool error path)
        (
            "unknown-ref",
            json!({"to": "ann@example.com", "body": format!("{canary} @vault(nope)")}),
        ),
    ];
    let mut results: Vec<(String, Value)> = Vec::new();
    for (label, args) in calls {
        let reply = request!(
            "tools/call",
            json!({"name": "echo.send", "arguments": args})
        );
        let is_err = reply["result"]["isError"] == json!(true) || reply.get("error").is_some();
        println!("t09: call {label}: isError={is_err}");
        results.push((label.to_owned(), reply));
    }
    drop(stdin);
    let status = timeout(CAP, child.wait())
        .await
        .expect("echo exits")
        .unwrap();
    let echo_out = out_task.await.unwrap();
    let echo_err = err_task.await.unwrap();
    std::fs::write(root.join("echo.stdout"), &echo_out).unwrap();
    std::fs::write(root.join("echo.stderr"), &echo_err).unwrap();
    assert!(status.success(), "echo exited with {status}");

    // ---- the sink saw the resolved value (length + hash), never a literal
    let sink_text = std::fs::read_to_string(&sink).unwrap();
    let sink_lines: Vec<Value> = sink_text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let want1 = format!("pin {canary} end");
    let want2 = format!("for you: {canary}");
    assert!(sink_lines.len() >= 2, "{sink_lines:?}");
    for (line, want) in sink_lines.iter().zip([&want1, &want2]) {
        assert_eq!(line["unresolved"], false, "{line}");
        assert_eq!(line["bytes"], want.len(), "{line}");
        assert_eq!(line["fnv1a64"], fnv1a64(want.as_bytes()), "{line}");
    }

    // ---- report.md, so runs/ holds every artefact a run leaves
    let events = store.ledger_events(&run.id).await.unwrap();
    let evs: Vec<LedgerEvent> = events.iter().map(|e| e.1.clone()).collect();
    harness_core::ledger::write_report(&run, "vault canary t09", &evs).unwrap();
    let jsonl = harness_core::ledger::read_jsonl(&run.ledger_jsonl()).unwrap();
    assert_eq!(jsonl, events, "ledger.jsonl mirrors the table");

    // ---- `harness ledger <run>` on the same database (CLI stdout)
    let hl = tokio::process::Command::new(env!("CARGO_BIN_EXE_harness"))
        .args(["ledger", &run.id.0])
        .env("DATABASE_URL", &db.url)
        .env("SB_RUNS_ROOT", &runs_root)
        .env("SB_TOOLS_ROOT", root.join("tools"))
        .env("SB_VAULT_PATH", root.join("no-vault.toml.age"))
        .env_remove("SB_VAULT_CANARY")
        .env_remove("SB_RUN_ID")
        .current_dir(&root)
        .output();
    let hl = timeout(CAP, hl)
        .await
        .expect("harness ledger exits")
        .unwrap();
    std::fs::write(root.join("harness-ledger.stdout"), &hl.stdout).unwrap();
    std::fs::write(root.join("harness-ledger.stderr"), &hl.stderr).unwrap();
    assert!(hl.status.success(), "harness ledger failed");
    assert!(!hl.stdout.is_empty());

    // ---- grep
    let f = forms(&canary);
    let mut leaks: Vec<String> = Vec::new();
    let mut check = |what: String, bytes: &[u8]| {
        if let Some(k) = hit(bytes, &f) {
            leaks.push(format!("{what} [{k}]"));
        }
    };
    check("echo stdout".into(), &echo_out);
    check("echo stderr".into(), &echo_err);
    for (label, r) in &results {
        check(format!("reply to {label}"), r.to_string().as_bytes());
    }
    check("harness ledger stdout".into(), &hl.stdout);
    check("harness ledger stderr".into(), &hl.stderr);
    check("sink file".into(), sink_text.as_bytes());

    let mut conn = sqlx::PgConnection::connect(&db.url).await.unwrap();
    let tables = sqlx::query(
        "select table_schema, table_name from information_schema.tables \
         where table_type = 'BASE TABLE' \
         and table_schema not in ('pg_catalog', 'information_schema') order by 1, 2",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    let mut scanned = 0usize;
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
            .unwrap();
        for r in rows {
            scanned += 1;
            check(format!("a row of {fq}"), r.get::<String, _>(0).as_bytes());
        }
    }
    let _ = conn.close().await;
    assert!(scanned > 0);

    let mut paths = Vec::new();
    walk(&runs_root, &mut paths);
    assert!(paths.contains(&run.ledger_jsonl()));
    assert!(paths.contains(&run.report_md()));
    for p in &paths {
        let rel = p.strip_prefix(&runs_root).unwrap().to_path_buf();
        check(
            format!("the name runs/{}", rel.display()),
            rel.to_string_lossy().as_bytes(),
        );
        if p.is_file() {
            check(
                format!("the file runs/{}", rel.display()),
                &std::fs::read(p).unwrap(),
            );
        }
    }
    println!(
        "t09: scanned {} forms, {} db rows, {} paths under runs/, {} replies",
        f.len(),
        scanned,
        paths.len(),
        results.len()
    );
    assert!(leaks.is_empty(), "SB_VAULT_CANARY found in: {leaks:?}");
}
