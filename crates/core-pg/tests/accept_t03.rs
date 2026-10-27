//! T-03 acceptance (docs/02-MEMORY.md step-1 items, docs/01-HARNESS-CORE.md H-3, D-110).
//! One test per acceptance row, each on its own throwaway `sb_t_<random>` database (D-100)
//! on SB_TEST_DATABASE_URL, dropped when the test ends. Never the shared `sb` database.

use chrono::{Duration, Utc};
use core_pg::PgStore;
use harness_core::class::{Class, Mode};
use harness_core::memory::*;
use harness_core::run::RunId;
use serde_json::json;
use sqlx::Connection;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU32, Ordering};

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
    format!("sb_t_a03_{:016x}", h.finish())
}

struct TestDb {
    store: PgStore,
    name: String,
    url: String,
}

impl TestDb {
    async fn new() -> TestDb {
        let admin = admin_url();
        let name = random_name();
        assert!(name.starts_with("sb_t_"), "D-100: throwaway databases only");
        let mut conn = sqlx::PgConnection::connect(&admin)
            .await
            .unwrap_or_else(|e| panic!("cannot reach the test Postgres ({e})"));
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database {name}")))
            .execute(&mut conn)
            .await
            .expect("create test database");
        let _ = conn.close().await;
        let url = with_db(&admin, &name);
        TestDb {
            store: PgStore::connect(&url)
                .await
                .unwrap_or_else(|e| panic!("connect + migrate {name}: {e}")),
            name,
            url,
        }
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

fn src(kind: SourceKind, r: &str) -> Source {
    Source {
        kind,
        reference: r.into(),
        run_id: None,
    }
}

// Row 1 (docs/02): remember + recall("Rust CDP crate") returns the fact with why=both when
// the fact text contains "chromiumoxide" and the query does not.
#[tokio::test]
async fn a03_recall_chromiumoxide_why_both() {
    let db = TestDb::new().await;
    let s = &db.store;
    let q = "Rust CDP crate";
    assert!(!q.to_lowercase().contains("chromiumoxide"));
    let target = s
        .remember(Fact::new(
            "chromiumoxide",
            "what",
            "chromiumoxide is a Rust crate that drives Chrome over CDP",
            src(SourceKind::Page, "https://crates.io/crates/chromiumoxide"),
        ))
        .await
        .unwrap();
    for (subj, key, val) in [
        ("garden", "todo", "Water the tomatoes every morning"),
        ("flight", "booking", "Lufthansa to Munich departs at 7"),
        ("gym", "plan", "Leg day on Thursdays"),
    ] {
        s.remember(Fact::new(subj, key, val, src(SourceKind::User, "cli")))
            .await
            .unwrap();
    }
    let hits = s.recall(q, 5, None).await.unwrap();
    let hit = hits
        .iter()
        .find(|h| h.id == target)
        .unwrap_or_else(|| panic!("chromiumoxide fact not recalled: {hits:?}"));
    assert!(hit.fact.value.contains("chromiumoxide"));
    assert_eq!(hit.why, Why::Both, "why for the chromiumoxide fact");
    assert_eq!(hits[0].id, target, "chromiumoxide fact ranks first");
    println!(
        "a03 recall: top={:?} why={:?} score={}",
        hits[0].fact.subject, hits[0].why, hits[0].score
    );
}

// Row 2 (docs/02): 3-hop neighbors returns roles reachable from a person via
// met_at -> attended -> works_at.
#[tokio::test]
async fn a03_neighbors_three_hops_reach_role() {
    let db = TestDb::new().await;
    let s = &db.store;
    let u = src(SourceKind::User, "test");
    let me = s.node(node_kind::PERSON, "Priya").await.unwrap();
    let meetup = s
        .node(node_kind::EVENT, "Rust Bangalore meetup")
        .await
        .unwrap();
    let ravi = s.node(node_kind::PERSON, "Ravi").await.unwrap();
    let role = s
        .node(node_kind::ROLE, "Staff Engineer, Acme")
        .await
        .unwrap();
    let far_role = s.node(node_kind::ROLE, "Far Role").await.unwrap();
    let far_person = s.node(node_kind::PERSON, "Far Person").await.unwrap();

    s.link(me, rel::MET_AT, meetup, u.clone()).await.unwrap();
    s.link(ravi, rel::ATTENDED, meetup, u.clone())
        .await
        .unwrap();
    s.link(ravi, rel::WORKS_AT, role, u.clone()).await.unwrap();
    // A role 4 hops away must not appear at depth 3.
    s.link(far_person, rel::WORKS_AT, role, u.clone())
        .await
        .unwrap();
    s.link(far_person, rel::WORKS_AT, far_role, u.clone())
        .await
        .unwrap();

    let got = s.neighbors(me, None, 3).await.unwrap();
    let roles: Vec<&Node> = got.iter().filter(|n| n.kind == node_kind::ROLE).collect();
    println!("a03 neighbors depth 3: {got:?}");
    assert!(roles.iter().any(|n| n.id == role), "role missing: {got:?}");
    assert!(!got.iter().any(|n| n.id == far_role), "4-hop role leaked");
    assert!(!got.iter().any(|n| n.id == me), "start node excluded");
}

// Row 3 (docs/02): grade 5 roughly multiplies interval by ease; grade < 3 resets to 1 day.
#[tokio::test]
async fn a03_grade_review_sm2() {
    let db = TestDb::new().await;
    let s = &db.store;
    for fail_grade in [0u8, 1, 2] {
        let id = s
            .review_add(&format!("item {fail_grade}"), "answer")
            .await
            .unwrap();
        for _ in 0..3 {
            s.grade_review(id, 5).await.unwrap();
        }
        let before = get(s, id).await;
        assert!(before.interval_days > 6.0, "{before:?}");
        s.grade_review(id, 5).await.unwrap();
        let after = get(s, id).await;
        let want = before.interval_days * before.ease;
        println!(
            "a03 sm2: {} * {} = {want}, got {}",
            before.interval_days, before.ease, after.interval_days
        );
        assert!(
            (after.interval_days - want).abs() <= 1.0,
            "grade 5: {} vs {want}",
            after.interval_days
        );
        s.grade_review(id, fail_grade).await.unwrap();
        let reset = get(s, id).await;
        assert_eq!(reset.interval_days, 1.0, "grade {fail_grade} resets");
        let hours = (reset.due - Utc::now()).num_minutes() as f32 / 60.0;
        assert!((hours - 24.0).abs() < 0.5, "due in {hours} h");
    }
}

async fn get(s: &PgStore, id: ReviewId) -> Review {
    s.due_reviews(Utc::now() + Duration::days(36_500), 1000)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.id == id)
        .expect("review exists")
}

// Row 4 (docs/01 H-3): UPDATE ledger.events fails, and DELETE and TRUNCATE fail.
#[tokio::test]
async fn a03_ledger_update_delete_truncate_fail() {
    let db = TestDb::new().await;
    let s = &db.store;
    let run = RunId("a03-ledger".into());
    let e = LedgerEvent {
        run_id: Some(run.clone()),
        ts: ts_now(),
        server: "core".into(),
        tool: "gate".into(),
        class: Some(Class::Send),
        mode: Some(LedgerMode::Allow),
        args: json!({"to": "a@example.com"}),
        result_summary: "sent".into(),
        consequences: json!([]),
        duration_ms: 3,
        ok: true,
        why: None,
    };
    let id = s.append(e.clone()).await.unwrap();
    let id_sql = id.0;

    // Through the store's pool and through a fresh direct connection.
    let mut direct = sqlx::PgConnection::connect(&db.url).await.unwrap();
    let stmts = [
        format!("update ledger.events set ok = false where id = {id_sql}"),
        "update ledger.events set result_summary = 'x'".to_string(),
        format!("delete from ledger.events where id = {id_sql}"),
        "delete from ledger.events".to_string(),
        "truncate ledger.events".to_string(),
        "truncate table ledger.events restart identity cascade".to_string(),
    ];
    for sql in &stmts {
        let err = sqlx::raw_sql(sqlx::AssertSqlSafe(sql.clone()))
            .execute(s.pool())
            .await
            .expect_err(sql);
        println!("a03 ledger pool: {sql} -> {err}");
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
        let err = sqlx::raw_sql(sqlx::AssertSqlSafe(sql.clone()))
            .execute(&mut direct)
            .await
            .expect_err(sql);
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
    }
    let _ = direct.close().await;
    assert_eq!(s.ledger_events(&run).await.unwrap(), vec![(id, e)]);
}

// Row 5 (docs/02, store half): touch_contact then contact() returns it.
#[tokio::test]
async fn a03_touch_contact_then_contact() {
    let db = TestDb::new().await;
    let s = &db.store;
    assert_eq!(s.contact("ravi@example.com").await.unwrap(), None);
    s.touch_contact("ravi@example.com", "email", "meetup")
        .await
        .unwrap();
    let c = s
        .contact("ravi@example.com")
        .await
        .unwrap()
        .expect("contact after touch");
    println!("a03 contact: {c:?}");
    assert_eq!(c.person, "ravi@example.com");
    assert_eq!(c.count, 1);
    assert!(c.last.is_some());
}

// Row 5 (docs/02, gate half, review T-03 should-fix): over the real store, a Send to a
// person asks before touch_contact and is allowed on the next gate check after it.
#[tokio::test]
async fn a03_touch_contact_makes_send_allow_through_the_gate() {
    use harness_core::gate::{Call, Decision, Gate, OnTimeout};
    use std::sync::Arc;
    let db = TestDb::new().await;
    let store = Arc::new(PgStore::connect(&db.url).await.expect("second store"));
    let gate = Gate::new(store.clone());
    let send = Call {
        run_id: None,
        server: "echo".into(),
        tool: "echo.send".into(),
        class: Class::Send,
        domain: Some("mail.example".into()),
        recipients: vec!["  Ravi@Example.com ".into()],
        hints: Vec::new(),
        args: json!({"to": "Ravi@Example.com", "body": "hi"}),
        what: None,
        values: Vec::new(),
        strict: false,
    };
    match gate.check(&send).await.unwrap() {
        Decision::Ask(card) => assert_eq!(card.on_timeout, OnTimeout::Proceed),
        other => panic!("Send(new) must ask, got {other:?}"),
    }
    store
        .touch_contact("ravi@example.com", "email", "")
        .await
        .unwrap();
    let d = gate.check(&send).await.unwrap();
    println!("a03 gate after touch_contact: {d:?}");
    assert_eq!(d, Decision::Allow);
    // An IDN rule written by the user matches the A-label host a URL carries (D-120).
    store
        .gate_rule_set(
            "BÜCHER.DE",
            "Send",
            GateRule {
                mode: Mode::Stop,
                strict: false,
            },
            "",
        )
        .await
        .unwrap();
    let mut idn = send.clone();
    idn.domain = Some("xn--bcher-kva.de".into());
    assert!(matches!(gate.check(&idn).await.unwrap(), Decision::Stop(_)));
}

// Row 6 (docs/02 seeds, H-1.2, D-5): gate_rule finds the seeded rules; gate_rule_set
// rejects a Challenge allow, in any spelling.
#[tokio::test]
async fn a03_seeded_rules_and_challenge_allow_rejected() {
    let db = TestDb::new().await;
    let s = &db.store;
    let ask = Some(GateRule {
        mode: Mode::Ask,
        strict: false,
    });
    for class in [
        "Pay",
        "Account",
        "Delete",
        "Submit:job_application",
        "Submit:signature",
        "Account:schedule",
    ] {
        assert_eq!(s.gate_rule("*", class).await.unwrap(), ask, "{class}");
    }
    assert_eq!(
        s.gate_rule("*", "Challenge").await.unwrap().map(|r| r.mode),
        Some(Mode::Stop)
    );
    for mode in [Mode::Allow, Mode::Ask] {
        let rule = GateRule {
            mode,
            strict: false,
        };
        for (d, c) in [
            ("*", "Challenge"),
            ("*", "challenge"),
            ("*", " CHALLENGE "),
            ("example.com", "Challenge"),
            ("Example.COM.", "challenge"),
            ("*", "Challenge:turnstile"),
            ("*", "challenge:hcaptcha"),
        ] {
            let r = s.gate_rule_set(d, c, rule, "bypass").await;
            println!("a03 gate_rule_set({d:?}, {c:?}, {mode:?}) -> {r:?}");
            assert!(
                matches!(r, Err(MemoryError::Invalid(_))),
                "({d:?}, {c:?}, {mode:?}) accepted"
            );
        }
    }
    assert_eq!(
        s.gate_rule("*", "Challenge").await.unwrap().map(|r| r.mode),
        Some(Mode::Stop)
    );
    let non_stop: i64 = sqlx::query_scalar(
        "select count(*) from gate.rules where lower(class) like 'challenge%' and mode <> 'stop'",
    )
    .fetch_one(s.pool())
    .await
    .unwrap();
    assert_eq!(non_stop, 0);
    // A Challenge stop is still accepted.
    s.gate_rule_set(
        "example.com",
        "Challenge",
        GateRule {
            mode: Mode::Stop,
            strict: false,
        },
        "",
    )
    .await
    .unwrap();
}

// Row 7 (D-110): gate_rule_set('Bank.COM.', 'submit', ask) is found by
// gate_rule('bank.com', 'Submit').
#[tokio::test]
async fn a03_d110_canonical_keys() {
    let db = TestDb::new().await;
    let s = &db.store;
    let rule = GateRule {
        mode: Mode::Ask,
        strict: false,
    };
    assert_eq!(s.gate_rule("bank.com", "Submit").await.unwrap(), None);
    s.gate_rule_set("Bank.COM.", "submit", rule, "")
        .await
        .unwrap();
    let got = s.gate_rule("bank.com", "Submit").await.unwrap();
    println!("a03 d110: gate_rule(bank.com, Submit) -> {got:?}");
    assert_eq!(got, Some(rule));
    let rows: Vec<(String, String)> =
        sqlx::query_as("select domain, class from gate.rules where domain <> '*'")
            .fetch_all(s.pool())
            .await
            .unwrap();
    assert_eq!(rows, vec![("bank.com".to_string(), "Submit".to_string())]);
}
