//! T-16 package C: the `mem` tool server behind `harness mcp` (docs/02 M-2, M-3 reviews SM-2
//! and contacts, 02-acc-4; docs/01 H-5, H-6, H-1.1; docs/08 Q-1 rungs 6 to 8; D-141).
//!
//! Against a real Postgres: each test gets a throwaway database `sb_t_<random>` on
//! `SB_TEST_DATABASE_URL` (default `postgres://sb@127.0.0.1:5433/postgres`, D-100), dropped
//! at the end. When that server cannot be reached the tests print a SKIPPED line and pass.
//!
//! * rung 6 shape: `review.add` -> `review.grade` -> a new connection -> `review.due` shows
//!   the SM-2 date the grade reported (1 day, then 6, a fail resets to 1);
//! * `schedule.add` is Account and asks (B-187): a yes creates a row whose parent is the
//!   session's run and rewrites the schedules-changed marker, a no adds nothing, and no
//!   answer times out to skip (the seeded `Account:schedule` rule, D-144);
//!   `schedule.rm` is Delete and asks (timeout skips);
//! * a vault value in a review text never reaches a tool reply (in-process and over MCP
//!   stdio framing), the stored row, or the ledger;
//! * `watch.observe` writes `runs/<run>/files/watch.txt` (rung 8's input to the harness);
//! * `harness mcp` (the real binary) answers `tools/list` and `tools/call`.
//!
//! The vault is `Vault::in_memory` with `SB_VAULT_CANARY` (else a fixed test string). No
//! model is called (rule 1); every wait is on a reply or an exit, capped (rule 6).

use core_pg::PgStore;
use harness::mem_server;
use harness_core::gate::AskOutcome;
use harness_core::mcp::{dispatch, serve_io, Harness, ToolError, ToolOutput, ToolRegistry};
use harness_core::memory::*;
use harness_core::run::{RunFolder, RunId};
use harness_core::sched::changed_dir;
use harness_core::testing::ScriptedAsker;
use harness_core::vault::{EntryKind, Vault};
use serde_json::{json, Value};
use sqlx::{Connection, Row};
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;

const CAP: Duration = Duration::from_secs(60);
const DEFAULT_ADMIN_URL: &str = "postgres://sb@127.0.0.1:5433/postgres";

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| c.trim().chars().count() >= 8)
        .unwrap_or_else(|| "canary-t16-mem-not-a-secret".to_owned())
}

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
    /// A migrated throwaway database, or `None` (after a SKIPPED line) when the test
    /// Postgres cannot be reached.
    async fn create(test: &str) -> Option<TestDb> {
        let admin = admin_url();
        let mut conn =
            match timeout(Duration::from_secs(10), sqlx::PgConnection::connect(&admin)).await {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    eprintln!(
                        "SKIPPED {test}: no test Postgres ({e}); run `docker compose up -d --wait \
                     postgres` or set SB_TEST_DATABASE_URL"
                    );
                    return None;
                }
                Err(_) => {
                    eprintln!("SKIPPED {test}: the test Postgres did not answer within 10 s");
                    return None;
                }
            };
        let name = random_name();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database {name}")))
            .execute(&mut conn)
            .await
            .expect("create test database");
        let _ = conn.close().await;
        let db = TestDb {
            url: with_db(&admin, &name),
            name,
        };
        // Migrate once here so every later connect is a plain reconnect.
        PgStore::connect(&db.url)
            .await
            .unwrap_or_else(|e| panic!("connect + migrate {}: {e}", db.name));
        Some(db)
    }

    async fn store(&self) -> Arc<dyn MemoryStore> {
        Arc::new(
            PgStore::connect(&self.url)
                .await
                .unwrap_or_else(|e| panic!("connect {}: {e}", self.name)),
        )
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

// ------------------------------------------------------------------ fixtures

struct Env {
    _tmp: tempfile::TempDir,
    runs_root: PathBuf,
    run: RunFolder,
    db: TestDb,
}

impl Env {
    async fn new(test: &str) -> Option<Env> {
        let db = TestDb::create(test).await?;
        let tmp = tempfile::tempdir().unwrap();
        let runs_root = tmp.path().join("runs");
        let run = RunFolder::create(&runs_root, "t16 mem", "mem tools").unwrap();
        db.store()
            .await
            .run_start(Run {
                id: run.id.clone(),
                started: ts_now(),
                ended: None,
                prompt: "mem tools".into(),
                status: RunStatus::Running,
                folder: run.dir.to_string_lossy().replace('\\', "/"),
                parent_run: None,
                strict: false,
            })
            .await
            .unwrap();
        Some(Env {
            _tmp: tmp,
            runs_root,
            run,
            db,
        })
    }

    /// A fresh connection, as a new `harness mcp` process would have.
    async fn harness(&self, asker: ScriptedAsker) -> Harness {
        Harness {
            store: self.db.store().await,
            vault: Arc::new(Vault::in_memory([(
                "test".to_owned(),
                canary(),
                EntryKind::Secret,
            )])),
            asker: Arc::new(asker),
            runs_root: self.runs_root.clone(),
            run: Some(self.run.id.clone()),
        }
    }
}

async fn call(h: &Harness, tool: &str, args: Value) -> Result<ToolOutput, ToolError> {
    let reg = ToolRegistry::new(mem_server::tools(h));
    dispatch(mem_server::SERVER, &reg, h, tool, args).await
}

async fn ok(h: &Harness, tool: &str, args: Value) -> String {
    match call(h, tool, args).await {
        Ok(out) => {
            assert!(out.stopped.is_none(), "{tool} stopped: {}", out.text);
            out.text
        }
        Err(e) => panic!("{tool} failed: {e}"),
    }
}

/// The number after `prefix` in `text` ("review 12 added" -> 12).
fn id_after(text: &str, prefix: &str) -> i64 {
    let rest = text
        .split_once(prefix)
        .unwrap_or_else(|| panic!("no {prefix:?} in {text:?}"))
        .1;
    rest.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or_else(|_| panic!("no id after {prefix:?} in {text:?}"))
}

/// `YYYY-MM-DD HH:MM` after `marker` in `text`.
fn date_after(text: &str, marker: &str) -> String {
    let rest = text
        .split_once(marker)
        .unwrap_or_else(|| panic!("no {marker:?} in {text:?}"))
        .1;
    rest.chars().take(16).collect()
}

async fn review_row(db: &TestDb, id: i64) -> (String, String, Ts, f32, i32) {
    let mut conn = sqlx::PgConnection::connect(&db.url).await.unwrap();
    let r = sqlx::query(
        "select item, answer, due, interval_days, reps from memory.reviews where id = $1",
    )
    .bind(id)
    .fetch_one(&mut conn)
    .await
    .unwrap();
    let out = (
        r.get("item"),
        r.get("answer"),
        r.get("due"),
        r.get("interval_days"),
        r.get("reps"),
    );
    let _ = conn.close().await;
    out
}

fn minutes_from_now(t: Ts) -> i64 {
    (t - ts_now()).num_minutes()
}

// ------------------------------------------------------------------ rung 6

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rung6_review_add_grade_then_a_new_connection_shows_the_sm2_date() {
    let Some(e) = Env::new("rung6_review_add_grade_then_a_new_connection_shows_the_sm2_date").await
    else {
        return;
    };
    let h = e.harness(ScriptedAsker::new()).await;
    let added = ok(
        &h,
        "review.add",
        json!({"item": "What does SM-2 schedule?", "answer": "the next review of an item"}),
    )
    .await;
    let id = id_after(&added, "review ");
    assert!(added.contains("due now"), "{added}");

    let due_now = ok(&h, "review.due", json!({})).await;
    assert!(due_now.contains(&format!("review {id} due")), "{due_now}");
    assert!(due_now.contains("Q: What does SM-2 schedule?"), "{due_now}");
    assert!(
        due_now.contains("A: the next review of an item"),
        "{due_now}"
    );

    // First pass: 1 day.
    let graded = ok(&h, "review.grade", json!({"id": id, "grade": 5})).await;
    assert!(graded.contains("interval 1 d"), "{graded}");
    assert!(graded.contains("reps 1"), "{graded}");
    let reported = date_after(&graded, "next due ");
    drop(h);

    // A restart: a new store connection sees the same SM-2 state.
    let h = e.harness(ScriptedAsker::new()).await;
    let now = ok(&h, "review.due", json!({})).await;
    assert!(
        !now.contains(&format!("review {id} ")),
        "a graded item is not due now: {now}"
    );
    let soon = ok(&h, "review.due", json!({"days": 2})).await;
    assert!(
        soon.contains(&format!("review {id} due {reported} UTC")),
        "{soon}\nwant {reported}"
    );
    assert!(soon.contains("interval 1 d"), "{soon}");
    let (_, _, due, interval, reps) = review_row(&e.db, id).await;
    assert_eq!((interval, reps), (1.0, 1));
    assert_eq!(due.format("%Y-%m-%d %H:%M").to_string(), reported);
    let m = minutes_from_now(due);
    assert!(
        (24 * 60 - 2..=24 * 60).contains(&m),
        "due in {m} min, want 1 day"
    );

    // Second pass: 6 days (SM-2 step two).
    let graded = ok(&h, "review.grade", json!({"id": id, "grade": 4})).await;
    assert!(graded.contains("interval 6 d"), "{graded}");
    let (_, _, due, interval, reps) = review_row(&e.db, id).await;
    assert_eq!((interval, reps), (6.0, 2));
    let m = minutes_from_now(due);
    assert!(
        (6 * 24 * 60 - 2..=6 * 24 * 60).contains(&m),
        "due in {m} min"
    );
    assert!(ok(&h, "review.due", json!({"days": 2}))
        .await
        .starts_with("no reviews due"));

    // A fail resets to 1 day and reps 0.
    let graded = ok(&h, "review.grade", json!({"id": id, "grade": 1})).await;
    assert!(graded.contains("fail, reset"), "{graded}");
    let (_, _, _, interval, reps) = review_row(&e.db, id).await;
    assert_eq!((interval, reps), (1.0, 0));

    // Bad grades and unknown ids are argument errors, nothing changes.
    assert!(matches!(
        call(&h, "review.grade", json!({"id": id, "grade": 6})).await,
        Err(ToolError::Args(_))
    ));
    assert!(matches!(
        call(&h, "review.grade", json!({"id": id + 999, "grade": 3})).await,
        Err(ToolError::Args(_))
    ));
    let (_, _, _, interval, reps) = review_row(&e.db, id).await;
    assert_eq!((interval, reps), (1.0, 0));

    // Every call is in the ledger at its class (rule 4).
    let events = h.store.ledger_events(&e.run.id).await.unwrap();
    let grades: Vec<_> = events
        .iter()
        .filter(|(_, ev)| ev.server == mem_server::SERVER && ev.tool == "review.grade")
        .collect();
    assert_eq!(grades.len(), 5);
    assert!(grades
        .iter()
        .all(|(_, ev)| ev.class == Some(harness_core::Class::Fill)));
    assert!(events.iter().any(|(_, ev)| ev.tool == "review.due"
        && ev.class == Some(harness_core::Class::Read)
        && ev.ok));
}

// ------------------------------------------------------------------ rung 7 / 8: schedules

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedule_add_creates_a_row_with_this_run_as_parent_and_wakes_harnessd() {
    let Some(e) =
        Env::new("schedule_add_creates_a_row_with_this_run_as_parent_and_wakes_harnessd").await
    else {
        return;
    };
    // Asks in order: add (yes), add (no), five bad adds (yes, then refused on their
    // arguments), watch add (yes), rm (yes); the last rm gets no answer.
    let mut script = vec![AskOutcome::AnsweredProceed, AskOutcome::AnsweredSkip];
    script.extend([AskOutcome::AnsweredProceed; 7]);
    let h = e.harness(ScriptedAsker::with(script)).await;
    let marker = changed_dir(&e.runs_root).join("changed");
    assert!(!marker.exists());

    let out = ok(
        &h,
        "schedule.add",
        json!({"cron": "0  8 * * 1-5", "prompt": "new postings matching my profile, as a file"}),
    )
    .await;
    let id = id_after(&out, "schedule ") as i32;
    assert!(out.contains("cron `0 8 * * 1-5` UTC"), "{out}");
    std::fs::remove_file(&marker).unwrap();
    // B-187: the user says no, nothing is added and harnessd is not woken.
    let no = ok(
        &h,
        "schedule.add",
        json!({"cron": "0 * * * *", "prompt": "planted by a page"}),
    )
    .await;
    assert!(no.starts_with("skipped"), "{no}");
    assert!(!marker.exists());
    assert_eq!(h.store.schedule_list().await.unwrap().len(), 1);
    std::fs::write(&marker, "").unwrap();
    assert!(out.contains(&format!("parent run {}", e.run.id)), "{out}");
    assert!(
        marker.is_file(),
        "schedule.add writes the schedules-changed marker"
    );

    let rows = h.store.schedule_list().await.unwrap();
    assert_eq!(rows.len(), 1);
    let s = &rows[0];
    assert_eq!(s.id, id);
    assert_eq!(s.cron, "0 8 * * 1-5");
    assert_eq!(s.kind, ScheduleKind::Cron);
    assert!(s.enabled);
    assert_eq!(
        s.run_id.as_ref(),
        Some(&e.run.id),
        "parent is the session's run"
    );
    assert_eq!(s.prompt, "new postings matching my profile, as a file");

    // A watch needs its target; a bad cron or kind adds nothing and does not wake harnessd.
    std::fs::remove_file(&marker).unwrap();
    for bad in [
        json!({"cron": "*/5 * * * *", "prompt": "luma", "kind": "watch"}),
        json!({"cron": "every day", "prompt": "x"}),
        json!({"cron": "0 8 * * *", "prompt": "x", "kind": "hourly"}),
        json!({"cron": "0 8 * * *", "prompt": "x", "watch_target": "t"}),
        json!({"cron": "0 8 * * *", "prompt": "  "}),
    ] {
        assert!(
            matches!(
                call(&h, "schedule.add", bad.clone()).await,
                Err(ToolError::Args(_))
            ),
            "{bad}"
        );
    }
    assert!(!marker.exists());
    assert_eq!(h.store.schedule_list().await.unwrap().len(), 1);

    let watch = ok(
        &h,
        "schedule.add",
        json!({"cron": "*/30 * * * *", "prompt": "Tell me when lu.ma/x opens registration",
               "kind": "watch", "watch_target": "luma-x\n  registration"}),
    )
    .await;
    let wid = id_after(&watch, "schedule ") as i32;
    assert!(marker.is_file());
    let w = h
        .store
        .schedule_list()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.id == wid)
        .unwrap();
    assert_eq!(w.kind, ScheduleKind::Watch);
    assert_eq!(
        w.watch_target.as_deref(),
        Some("luma-x registration"),
        "stored on one line"
    );

    let list = ok(&h, "schedule.list", json!({})).await;
    assert!(list.starts_with("2 schedules"), "{list}");
    assert!(
        list.contains(&format!("schedule {id} cron `0 8 * * 1-5`")),
        "{list}"
    );
    assert!(list.contains("watching luma-x registration"), "{list}");

    // B-187: the tool event carries the Account class and the answered ask (rule 4).
    let events = h.store.ledger_events(&e.run.id).await.unwrap();
    let adds: Vec<_> = events
        .iter()
        .filter(|(_, ev)| ev.tool == "schedule.add" && ev.server == mem_server::SERVER && ev.ok)
        .collect();
    assert_eq!(adds.len(), 2);
    assert!(adds
        .iter()
        .all(|(_, ev)| ev.class == Some(harness_core::Class::Account)
            && ev.mode == Some(LedgerMode::AnsweredProceed)));
    let add_asks = events
        .iter()
        .filter(|(_, ev)| ev.server == "core" && ev.tool == "schedule.add")
        .filter(|(_, ev)| ev.class == Some(harness_core::Class::Account))
        .count();
    assert!(add_asks >= 2, "every schedule.add asked: {add_asks}");

    // schedule.rm is Delete: it asks. Answered yes removes it and wakes harnessd.
    std::fs::remove_file(&marker).unwrap();
    let rm = ok(&h, "schedule.rm", json!({"id": id})).await;
    assert_eq!(rm, format!("schedule {id} removed"));
    assert!(marker.is_file());
    // No answer: a Delete ask times out to skip and the row stays.
    let skipped = ok(&h, "schedule.rm", json!({"id": wid})).await;
    assert!(skipped.starts_with("skipped"), "{skipped}");
    let left: Vec<i32> = h
        .store
        .schedule_list()
        .await
        .unwrap()
        .iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(left, vec![wid]);
    let events = h.store.ledger_events(&e.run.id).await.unwrap();
    let asks: Vec<_> = events
        .iter()
        .filter(|(_, ev)| ev.server == "core" && ev.tool == "schedule.rm")
        .collect();
    assert_eq!(asks.len(), 2, "both removals asked");
    assert!(asks
        .iter()
        .all(|(_, ev)| ev.class == Some(harness_core::Class::Delete)));
    assert!(
        asks[0].1.result_summary.contains("0 8 * * 1-5"),
        "the card says which schedule: {}",
        asks[0].1.result_summary
    );

    // B-187, D-144: no answer to a schedule.add (an unattended firing, a page-planted call)
    // times out to skip through the seeded ('*','Account:schedule','ask') rule: no row, no wake.
    std::fs::remove_file(&marker).unwrap();
    let planted = ok(
        &h,
        "schedule.add",
        json!({"cron": "0 * * * *", "prompt": "planted by a page, nobody answers"}),
    )
    .await;
    assert!(
        planted.starts_with("skipped: no answer before the ask timed out"),
        "{planted}"
    );
    assert_eq!(h.store.schedule_list().await.unwrap().len(), 1);
    assert!(!marker.exists());
    let events = h.store.ledger_events(&e.run.id).await.unwrap();
    let last_add = events
        .iter()
        .rev()
        .find(|(_, ev)| ev.server == "core" && ev.tool == "schedule.add")
        .unwrap();
    assert_eq!(last_add.1.mode, Some(LedgerMode::TimeoutSkip));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_observe_writes_the_run_watch_file_and_contact_get_reads_contacts() {
    let Some(e) =
        Env::new("watch_observe_writes_the_run_watch_file_and_contact_get_reads_contacts").await
    else {
        return;
    };
    let h = e.harness(ScriptedAsker::new()).await;
    let file = e.run.dir.join("files").join(mem_server::WATCH_FILE);

    let out = call(
        &h,
        "watch.observe",
        json!({"state": "registration: closed\nspots: 0\n"}),
    )
    .await
    .unwrap();
    assert!(out.text.contains("files/watch.txt"), "{}", out.text);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "registration: closed\nspots: 0\n"
    );
    assert_eq!(out.files, vec![file.clone()]);
    // Last observation wins.
    ok(&h, "watch.observe", json!({"state": "registration: open"})).await;
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "registration: open"
    );
    assert!(matches!(
        call(&h, "watch.observe", json!({"state": " \n"})).await,
        Err(ToolError::Args(_))
    ));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "registration: open"
    );
    let big = "x".repeat(mem_server::STATE_MAX_BYTES + 1);
    assert!(matches!(
        call(&h, "watch.observe", json!({ "state": big })).await,
        Err(ToolError::Args(_))
    ));

    let none = ok(&h, "contact.get", json!({"person": "Vasanth@Example.com"})).await;
    assert!(none.contains("is not a contact"), "{none}");
    h.store
        .touch_contact("vasanth@example.com", "mail.google.com", "")
        .await
        .unwrap();
    let one = ok(&h, "contact.get", json!({"person": " VASANTH@example.com"})).await;
    assert!(
        one.starts_with("contact vasanth@example.com: contacted 1 time,"),
        "{one}"
    );
    assert!(one.contains("via mail.google.com"), "{one}");
}

// ------------------------------------------------------------------ vault canary

/// Every reply line of one MCP session over `serve_io` (as Claude Code would see them).
async fn mcp_session(h: Harness, calls: &[(&str, Value)]) -> Vec<String> {
    let (client, server) = tokio::io::duplex(1 << 16);
    let (sr, sw) = tokio::io::split(server);
    let reg = ToolRegistry::new(mem_server::tools(&h));
    let srv =
        tokio::spawn(
            async move { serve_io(mem_server::SERVER, reg, h, BufReader::new(sr), sw).await },
        );
    let (cr, mut cw) = tokio::io::split(client);
    let mut lines = BufReader::new(cr).lines();
    let mut replies = Vec::new();
    let send = |v: Value| format!("{v}\n");
    cw.write_all(
        send(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}})).as_bytes(),
    )
    .await
    .unwrap();
    replies.push(
        timeout(CAP, lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    );
    for (i, (tool, args)) in calls.iter().enumerate() {
        let req = json!({"jsonrpc": "2.0", "id": i + 1, "method": "tools/call",
                         "params": {"name": tool, "arguments": args}});
        cw.write_all(send(req).as_bytes()).await.unwrap();
        // One call at a time, so the replies come back in order.
        replies.push(
            timeout(CAP, lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        );
    }
    // Both split halves own the client end: drop both so the server reads EOF.
    drop(cw);
    drop(lines);
    timeout(CAP, srv).await.unwrap().unwrap().unwrap();
    replies
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vault_value_in_a_review_never_reaches_a_reply_the_row_or_the_ledger() {
    let Some(e) =
        Env::new("a_vault_value_in_a_review_never_reaches_a_reply_the_row_or_the_ledger").await
    else {
        return;
    };
    let canary = canary();
    let lower = canary.to_ascii_lowercase();
    let h = e.harness(ScriptedAsker::new()).await;

    // In-process: the handler's own replies.
    let added = call(
        &h,
        "review.add",
        json!({"item": format!("What is my code {canary}?"), "answer": format!("it is {canary}")}),
    )
    .await
    .unwrap();
    assert!(!added.text.to_ascii_lowercase().contains(&lower));
    assert!(added.text.contains("@vault(test)"), "{}", added.text);
    let id = id_after(&added.text, "review ");
    let (item, answer, _, _, _) = review_row(&e.db, id).await;
    assert_eq!(item, "What is my code @vault(test)?");
    assert_eq!(answer, "it is @vault(test)");

    // Over MCP framing: due, grade, add again, a schedule with the value, and a watch state.
    // schedule.add is the only ask; the user says yes (no answer would skip it, D-144).
    let replies = mcp_session(
        e.harness(ScriptedAsker::with([AskOutcome::AnsweredProceed])).await,
        &[
            ("review.due", json!({})),
            (
                "review.add",
                json!({"item": canary.clone(), "answer": canary.clone()}),
            ),
            ("review.grade", json!({"id": id, "grade": 3})),
            ("review.due", json!({"days": 3})),
            (
                "schedule.add",
                json!({"cron": "0 9 * * *", "prompt": format!("remind me of {canary}")}),
            ),
            ("schedule.list", json!({})),
            ("contact.get", json!({"person": canary.clone()})),
            ("review.grade", json!({"id": canary.clone(), "grade": 3})),
            ("watch.observe", json!({"state": format!("seen {canary}")})),
        ],
    )
    .await;
    assert_eq!(replies.len(), 10);
    for r in &replies {
        assert!(
            !r.to_ascii_lowercase().contains(&lower),
            "a vault value reached an MCP reply"
        );
    }
    let due: Value = serde_json::from_str(&replies[1]).unwrap();
    let due_text = due["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        due_text.contains("Q: What is my code @vault(test)?"),
        "{due_text}"
    );
    let watch: Value = serde_json::from_str(&replies[9]).unwrap();
    assert_eq!(watch["result"]["isError"], false, "{watch}");
    // D-143: the watch state file is scrubbed before it is written.
    let state =
        std::fs::read_to_string(e.run.dir.join("files").join(mem_server::WATCH_FILE)).unwrap();
    assert!(
        !state.to_ascii_lowercase().contains(&lower),
        "watch.txt holds a vault value"
    );
    assert_eq!(state, "seen @vault(test)");

    // The stored rows and the ledger (table and jsonl) hold only the reference.
    let mut conn = sqlx::PgConnection::connect(&e.db.url).await.unwrap();
    for sql in [
        "select coalesce(string_agg(item || ' ' || answer, ' '), '') from memory.reviews",
        "select coalesce(string_agg(prompt, ' '), '') from schedules",
        "select coalesce(string_agg(args::text || ' ' || result_summary || ' ' || coalesce(why, ''), ' '), '') from ledger.events",
    ] {
        let all: String = sqlx::query_scalar(sql).fetch_one(&mut conn).await.unwrap();
        assert!(!all.to_ascii_lowercase().contains(&lower), "a vault value is stored: {sql}");
        assert!(all.contains("@vault(test)"), "{sql}");
    }
    let _ = conn.close().await;
    let jsonl = std::fs::read_to_string(e.run.ledger_jsonl()).unwrap();
    assert!(!jsonl.to_ascii_lowercase().contains(&lower));
}

// ------------------------------------------------------------------ the real binary

async fn next_reply<R: tokio::io::AsyncBufRead + Unpin>(lines: &mut tokio::io::Lines<R>) -> Value {
    let l = timeout(CAP, lines.next_line())
        .await
        .expect("reply in time");
    serde_json::from_str::<Value>(&l.unwrap().expect("a reply line")).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn harness_mcp_serves_the_mem_tools_over_stdio() {
    let Some(e) = Env::new("harness_mcp_serves_the_mem_tools_over_stdio").await else {
        return;
    };
    let tmp = e.runs_root.parent().unwrap().to_path_buf();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_harness"))
        .arg("mcp")
        .env("DATABASE_URL", &e.db.url)
        .env("SB_RUNS_ROOT", &e.runs_root)
        .env("SB_RUN_ID", &e.run.id.0)
        // A missing vault file is an empty vault; the OS keychain is never touched.
        .env("SB_VAULT_PATH", tmp.join("no-vault.toml.age"))
        .env_remove("SB_VAULT_CANARY")
        .current_dir(&tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn harness mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    // B-187: schedule.add asks through the file channel (D-105); answer it the way
    // `harness ask <run> 1 yes` does once the server prints the card's notice line.
    let mut errs = BufReader::new(child.stderr.take().unwrap()).lines();
    let (runs_root, run_id) = (e.runs_root.clone(), e.run.id.clone());
    let answered = tokio::spawn(async move {
        while let Ok(Some(l)) = errs.next_line().await {
            if l.starts_with("ask 1 (") {
                harness_core::ask::answer(&runs_root, &run_id, 1, true).unwrap();
                return Some(l);
            }
        }
        None
    });
    let rpc = |v: Value| format!("{v}\n");
    stdin
        .write_all(
            rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
                .as_bytes(),
        )
        .await
        .unwrap();
    let init = next_reply(&mut lines).await;
    assert_eq!(init["result"]["serverInfo"]["name"], "mem", "{init}");
    stdin
        .write_all(
            (rpc(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                + &rpc(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})))
                .as_bytes(),
        )
        .await
        .unwrap();
    let list = next_reply(&mut lines).await;
    let mut names: Vec<String> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "contact.get",
            "review.add",
            "review.due",
            "review.grade",
            "schedule.add",
            "schedule.list",
            "schedule.rm",
            "watch.observe"
        ]
    );
    stdin
        .write_all(
            rpc(
                json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
                "name": "schedule.add",
                "arguments": {"cron": "0 8 * * 1-5", "prompt": "weekday postings"}}}),
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let added = next_reply(&mut lines).await;
    let notice = timeout(CAP, answered).await.unwrap().unwrap();
    let notice = notice.expect("schedule.add asked");
    assert!(notice.contains("Account"), "{notice}");
    let text = added["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(added["result"]["isError"], false, "{added}");
    assert!(text.contains(&format!("parent run {}", e.run.id)), "{text}");
    drop(stdin);
    let status = timeout(CAP, child.wait()).await.unwrap().unwrap();
    assert!(status.success());

    assert!(changed_dir(&e.runs_root).join("changed").is_file());
    let rows = e.db.store().await.schedule_list().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].run_id, Some(RunId(e.run.id.0.clone())));
    assert_eq!(rows[0].prompt, "weekday postings");
}
