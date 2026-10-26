//! T-03 acceptance, round 3: the 10cb4b5 behaviours (D-112, D-113, D-114, D-115, D-120, rule
//! 2) over the real store. Each test runs on its own throwaway `sb_t_<random>` database
//! (D-100) on SB_TEST_DATABASE_URL, dropped when the test ends. Never the shared `sb`
//! database. MemStore expectations for the same fixtures are in
//! crates/core/tests/gate_accept_t04_r3.rs (the core-pg tests do not get harness-core's
//! `testing` feature).

use chrono::{Duration, TimeZone, Utc};
use core_pg::PgStore;
use harness_core::class::{Class, Mode};
use harness_core::gate::{Call, Decision, Gate, OnTimeout};
use harness_core::memory::*;
use serde_json::json;
use sqlx::Connection;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

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
    format!("sb_t_a03r3_{:016x}", h.finish())
}

struct TestDb {
    store: Arc<PgStore>,
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
            store: Arc::new(
                PgStore::connect(&url)
                    .await
                    .unwrap_or_else(|e| panic!("connect + migrate {name}: {e}")),
            ),
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

fn brief(hits: &[Hit]) -> Vec<(String, f32, Why)> {
    hits.iter()
        .map(|h| (h.fact.subject.clone(), h.score, h.why))
        .collect()
}

fn error_chain(err: &MemoryError) -> String {
    let mut out = format!("{err} | {err:?}");
    let mut cur: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(err);
    while let Some(e) = cur {
        out.push_str(" <- ");
        out.push_str(&format!("{e} | {e:?}"));
        cur = e.source();
    }
    out
}

const ASK: GateRule = GateRule {
    mode: Mode::Ask,
    strict: false,
};

// ------------------------------------------------------------------ D-114

#[tokio::test]
async fn r3_d114_edges_dst_index_from_migration_0002() {
    let db = TestDb::new().await;
    let pool = db.store.pool();
    let defs: Vec<String> = sqlx::query_scalar(
        "select indexdef from pg_indexes where schemaname = 'memory' and tablename = 'edges'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    println!("r3 edges indexes: {defs:?}");
    assert!(
        defs.iter().any(|d| d.ends_with("(dst)")),
        "no index leading on memory.edges(dst): {defs:?}"
    );
    let applied: Vec<(i64, bool)> =
        sqlx::query_as("select version, success from _sqlx_migrations order by version")
            .fetch_all(pool)
            .await
            .unwrap();
    println!("r3 migrations: {applied:?}");
    assert!(applied.contains(&(1, true)) && applied.contains(&(2, true)));
    // The planner can use it for the reverse hop of neighbors().
    let plan: Vec<String> =
        sqlx::query_scalar("explain (costs off) select src from memory.edges where dst = 42")
            .fetch_all(pool)
            .await
            .unwrap();
    println!("r3 plan: {plan:?}");
}

#[tokio::test]
async fn r3_d114_connect_ignores_a_migration_only_another_worktree_has() {
    let db = TestDb::new().await;
    sqlx::query(
        "insert into _sqlx_migrations (version, description, success, checksum, execution_time)
         values (99990101000000, 'from a newer worktree', true, '\\x00'::bytea, 0)",
    )
    .execute(db.store.pool())
    .await
    .unwrap();
    let again = PgStore::connect(&db.url).await;
    assert!(
        again.is_ok(),
        "connect refused a database with a newer applied migration: {:?}",
        again.err()
    );
    // And the store still works on it.
    let s = again.unwrap();
    s.touch_contact("x@y.example", "email", "").await.unwrap();
    assert!(s.contact("x@y.example").await.unwrap().is_some());
}

// ------------------------------------------------------------------ D-115 search

#[tokio::test]
async fn r3_d115_search_equality_substring_and_score_order() {
    let db = TestDb::new().await;
    let s = &db.store;
    let mut exact = Fact::new(
        "Foo-Bar Inc",
        "domain",
        "foobar.example",
        src(SourceKind::User, "t"),
    );
    exact.seen_at = Utc::now() - Duration::days(30);
    s.remember(exact).await.unwrap();
    s.remember(Fact::new(
        "weekly notes",
        "met",
        "lunch with Foo-Bar Inc people",
        src(SourceKind::User, "t"),
    ))
    .await
    .unwrap();
    s.remember(Fact::new(
        "Vasanth Raghavan",
        "role",
        "staff engineer",
        src(SourceKind::User, "t"),
    ))
    .await
    .unwrap();
    s.remember(Fact::new(
        "reading list",
        "next",
        "raghavan vasanth biography",
        src(SourceKind::User, "t"),
    ))
    .await
    .unwrap();

    // Case- and punctuation-insensitive equality on subject is 1.0.
    let hits = s.search("foo bar, INC.", 5).await.unwrap();
    println!("r3 pg search foo: {:?}", brief(&hits));
    assert_eq!(hits[0].fact.subject, "Foo-Bar Inc");
    assert_eq!(hits[0].score, 1.0);
    assert_eq!(hits[0].why, Why::Exact);

    // Equality beats a newer fact that merely mentions it (0.9 substring).
    let hits = s.search("Foo-Bar Inc", 5).await.unwrap();
    println!("r3 pg search Foo-Bar Inc: {:?}", brief(&hits));
    assert_eq!(hits[0].fact.subject, "Foo-Bar Inc");
    assert_eq!(hits[0].score, 1.0);
    assert_eq!(hits[1].fact.subject, "weekly notes");
    assert!((hits[1].score - 0.9).abs() < 1e-6);
    assert_eq!(hits[1].why, Why::Exact);

    // A typo inside a longer subject that clears the 0.6 word_similarity bar.
    let hits = s.search("Vasanh", 5).await.unwrap();
    println!("r3 pg search Vasanh: {:?}", brief(&hits));
    assert_eq!(hits[0].fact.subject, "Vasanth Raghavan");
    assert_eq!(hits[0].why, Why::Trigram);
    assert!((hits[0].score - 0.714_285_7).abs() < 1e-4);

    // Ordered by score, tier only breaking ties: a 1.0 trigram hit beats a 0.9 substring.
    let hits = s.search("Raghavan Vasanth", 5).await.unwrap();
    println!("r3 pg search rv: {:?}", brief(&hits));
    assert_eq!(hits[0].fact.subject, "Vasanth Raghavan");
    assert_eq!(hits[0].why, Why::Trigram);
    assert!(hits[0].score > 0.9);
    assert_eq!(hits[1].fact.subject, "reading list");
    assert_eq!(hits[1].why, Why::Exact);
}

/// D-115's own example: "a typo inside a longer subject ('Vasnth' in 'Vasanth Raghavan')
/// must still hit". word_similarity('Vasnth', 'Vasanth Raghavan') is 0.5 on the compose
/// Postgres, under the default pg_trgm.word_similarity_threshold of 0.6.
#[tokio::test]
async fn r3_d115_vasnth_typo_inside_longer_subject_hits() {
    let db = TestDb::new().await;
    let s = &db.store;
    s.remember(Fact::new(
        "Vasanth Raghavan",
        "role",
        "staff engineer",
        src(SourceKind::User, "t"),
    ))
    .await
    .unwrap();
    let sim: f32 = sqlx::query_scalar("select word_similarity('Vasnth', 'Vasanth Raghavan')")
        .fetch_one(s.pool())
        .await
        .unwrap();
    let hits = s.search("Vasnth", 5).await.unwrap();
    println!(
        "r3 pg search Vasnth: {:?} (word_similarity {sim})",
        brief(&hits)
    );
    assert_eq!(
        hits.first().map(|h| h.fact.subject.as_str()),
        Some("Vasanth Raghavan"),
        "D-115: 'Vasnth' must hit 'Vasanth Raghavan'"
    );
}

// ------------------------------------------------------------------ D-115 SM-2

async fn review(s: &PgStore, id: ReviewId) -> Review {
    s.due_reviews(Utc::now() + Duration::days(400), 100)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.id == id)
        .expect("review due within 400 days")
}

#[tokio::test]
async fn r3_d115_sm2_rounds_up_like_memstore() {
    let db = TestDb::new().await;
    let s = &db.store;
    let id = s.review_add("q", "a").await.unwrap();
    let mut seen = Vec::new();
    let (mut reps, mut iv, mut ease) = (0i32, 1.0f32, 2.5f32);
    for g in [5u8, 5, 5, 5] {
        let before = Utc::now();
        s.grade_review(id, g).await.unwrap();
        let r = review(s, id).await;
        (reps, iv, ease) = core_pg::sm2(reps, iv, ease, g);
        assert_eq!(
            (r.reps, r.interval_days),
            (reps, iv),
            "pure sm2 and the row agree"
        );
        assert!((r.ease - ease).abs() < 1e-5);
        let due = before + Duration::seconds((f64::from(iv) * 86_400.0) as i64);
        assert!(
            (r.due - due).num_seconds().abs() < 60,
            "due {} vs {due}",
            r.due
        );
        seen.push(r.interval_days);
    }
    println!("r3 pg sm2: {seen:?}");
    // The same sequence MemStore gives (gate_accept_t04_r3::r3_memstore_sm2_rounds_up).
    assert_eq!(seen, vec![1.0, 6.0, 17.0, 48.0]);
    // An exact product is not bumped by f32 noise: 6 * 2.5 = 15.
    let id = s.review_add("q2", "a").await.unwrap();
    for g in [4u8, 4, 4] {
        s.grade_review(id, g).await.unwrap();
    }
    assert_eq!(review(s, id).await.interval_days, 15.0);
    // A fail still resets to one day.
    s.grade_review(id, 2).await.unwrap();
    let r = review(s, id).await;
    assert_eq!((r.reps, r.interval_days), (0, 1.0));
}

// ------------------------------------------------------------------ D-112, timestamps

#[tokio::test]
async fn r3_d112_edge_since_comes_from_the_process_clock() {
    let db = TestDb::new().await;
    let s = &db.store;
    let a = s.node(node_kind::PERSON, "a").await.unwrap();
    let b = s.node(node_kind::EVENT, "b").await.unwrap();
    let before = Utc::now();
    s.link(a, rel::ATTENDED, b, src(SourceKind::User, "t"))
        .await
        .unwrap();
    let after = Utc::now();
    let (since, db_now): (chrono::DateTime<Utc>, chrono::DateTime<Utc>) =
        sqlx::query_as("select since, now() from memory.edges where src = $1 and dst = $2")
            .bind(a.0)
            .bind(b.0)
            .fetch_one(s.pool())
            .await
            .unwrap();
    println!(
        "r3 edges.since {since} host window [{before}, {after}], db clock skew {} ms",
        (db_now - after).num_milliseconds()
    );
    assert!(
        since >= before - Duration::milliseconds(1) && since <= after + Duration::milliseconds(1),
        "since {since} outside the host window [{before}, {after}]"
    );
}

#[tokio::test]
async fn r3_timestamps_truncate_toward_zero_from_2000_like_memstore() {
    let db = TestDb::new().await;
    let s = &db.store;
    // 500 ns before 2000-01-01: toward zero from 2000 is 2000-01-01T00:00:00.
    let pre = Utc.with_ymd_and_hms(1999, 12, 31, 23, 59, 59).unwrap()
        + Duration::nanoseconds(999_999_500);
    let post = Utc.with_ymd_and_hms(2026, 10, 25, 12, 0, 0).unwrap() + Duration::nanoseconds(1_900);
    for (subject, at, want) in [
        (
            "pre2000 stamp",
            pre,
            Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap(),
        ),
        (
            "post2000 stamp",
            post,
            Utc.with_ymd_and_hms(2026, 10, 25, 12, 0, 0).unwrap() + Duration::microseconds(1),
        ),
    ] {
        let mut f = Fact::new(subject, "k", "v", src(SourceKind::User, "t"));
        f.seen_at = at;
        s.remember(f).await.unwrap();
        let hit = s.search(subject, 1).await.unwrap().remove(0);
        println!("r3 pg ts {at:?} -> {:?}", hit.fact.seen_at);
        assert_eq!(hit.fact.seen_at, want, "{subject}");
    }
}

// ------------------------------------------------------------------ hnsw.iterative_scan

#[tokio::test]
async fn r3_iterative_scan_is_on_every_session_and_filtered_recall_finds_far_rows() {
    let db = TestDb::new().await;
    let s = &db.store;
    for i in 0..45 {
        s.remember(Fact::new(
            format!("decoy {i:02}"),
            "about",
            format!("Rust crate number {i} that drives Chrome over the DevTools Protocol"),
            src(SourceKind::Page, "t"),
        ))
        .await
        .unwrap();
    }
    s.remember(Fact::new(
        "zz-target",
        "kitchen",
        "banana bread with walnuts and brown sugar",
        src(SourceKind::User, "t"),
    ))
    .await
    .unwrap();
    // Every pooled session carries the option, without a SET round trip.
    let mut conns = Vec::new();
    for _ in 0..3 {
        conns.push(s.pool().acquire().await.unwrap());
    }
    for c in conns.iter_mut() {
        let v: String = sqlx::query_scalar("select current_setting('hnsw.iterative_scan')")
            .fetch_one(&mut **c)
            .await
            .unwrap();
        assert_eq!(v, "relaxed_order");
    }
    drop(conns);
    let hits = s
        .recall(
            "Rust CDP crate",
            5,
            Some(Filter {
                subject_prefix: Some("zz-".into()),
                ..Filter::default()
            }),
        )
        .await
        .unwrap();
    println!("r3 filtered recall: {:?}", brief(&hits));
    assert_eq!(hits.len(), 1, "{:?}", brief(&hits));
    assert_eq!(hits[0].fact.subject, "zz-target");
    assert_eq!(hits[0].why, Why::Vector);
}

// ------------------------------------------------------------------ rule 2

#[tokio::test]
async fn r3_rule2_constraint_errors_carry_no_bound_canary() {
    let canary = std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "sbcanary-a03r3-default-91x".to_string());
    let db = TestDb::new().await;
    let s = &db.store;
    // Check constraints whose violation detail ("Failing row contains (...)") would echo the
    // bound row, canary included, if the store passed server messages through.
    for ddl in [
        "alter table memory.facts add constraint no_canary_f check (strpos(value, 'sbcanary') = 0)",
        "alter table memory.contacts add constraint no_canary_c check (strpos(coalesce(note, ''), 'sbcanary') = 0)",
        "alter table gate.rules add constraint no_canary_g check (strpos(note, 'sbcanary') = 0)",
        "alter table ledger.events add constraint no_canary_l check (strpos(result_summary, 'sbcanary') = 0)",
        "alter table memory.baselines add constraint no_canary_b check (strpos(hash, 'sbcanary') = 0)",
    ] {
        sqlx::raw_sql(ddl).execute(s.pool()).await.unwrap();
    }
    assert!(
        canary.contains("sbcanary"),
        "SB_VAULT_CANARY must contain 'sbcanary' here"
    );
    let errors = vec![
        s.remember(Fact::new("subj", "k", &canary, src(SourceKind::User, "t")))
            .await
            .unwrap_err(),
        s.touch_contact("p@x.example", "email", &canary)
            .await
            .unwrap_err(),
        s.gate_rule_set("x.example", "Submit", ASK, &canary)
            .await
            .unwrap_err(),
        s.append(LedgerEvent {
            run_id: None,
            ts: ts_now(),
            server: "echo".into(),
            tool: "echo.send".into(),
            class: Some(Class::Send),
            mode: Some(LedgerMode::Allow),
            args: json!({}),
            result_summary: canary.clone(),
            consequences: json!([]),
            duration_ms: 0,
            ok: true,
            why: None,
        })
        .await
        .unwrap_err(),
        s.baseline_set(
            "w",
            Baseline {
                hash: canary.clone(),
                snapshot: json!({}),
                updated: ts_now(),
            },
        )
        .await
        .unwrap_err(),
    ];
    for err in &errors {
        let chain = error_chain(err);
        println!("r3 rule2 error: {chain}");
        assert!(matches!(err, MemoryError::Backend(_)), "{chain}");
        assert!(!chain.contains(&canary), "bound value in error: {chain}");
        assert!(!chain.contains("Failing row"), "{chain}");
    }
}

// ------------------------------------------------------------------ D-113 / D-120 over PgStore

#[tokio::test]
async fn r3_d113_d120_pg_gate_rule_set_stores_rule_key() {
    let db = TestDb::new().await;
    let s = &db.store;
    let count = || async {
        sqlx::query_scalar::<_, i64>("select count(*) from gate.rules")
            .fetch_one(s.pool())
            .await
            .unwrap()
    };
    let n0 = count().await;
    s.gate_rule_set("BÜCHER.DE", "send", ASK, "").await.unwrap();
    s.gate_rule_set(
        "\u{ff22}\u{ff21}\u{ff2e}\u{ff2b}\u{ff0e}\u{ff23}\u{ff2f}\u{ff2d}",
        " submit : Wire ",
        ASK,
        "",
    )
    .await
    .unwrap();
    s.gate_rule_set("例え\u{3002}テスト", "Delete", ASK, "")
        .await
        .unwrap();
    let rows: Vec<(String, String)> =
        sqlx::query_as("select domain, class from gate.rules where domain <> '*' order by domain")
            .fetch_all(s.pool())
            .await
            .unwrap();
    println!("r3 pg rules: {rows:?}");
    assert_eq!(
        rows,
        vec![
            ("bank.com".to_string(), "Submit:wire".to_string()),
            ("xn--bcher-kva.de".to_string(), "Send".to_string()),
            ("xn--r8jz45g.xn--zckzah".to_string(), "Delete".to_string()),
        ]
    );
    // Lookups canonicalise too.
    assert_eq!(s.gate_rule("Bücher.de.", "SEND").await.unwrap(), Some(ASK));
    assert_eq!(
        s.gate_rule("BANK.com", "submit:WIRE").await.unwrap(),
        Some(ASK)
    );
    // What the canonicaliser refuses is refused and not written.
    for (d, c) in [
        ("bu\u{0308}cher.de", "Send"),
        ("ban\u{200b}k.com", "Send"),
        ("\u{fb01}le.com", "Send"),
        ("\u{1d41b}ank.com", "Send"),
        ("bank.com:443", "Send"),
        ("bank.com", "Submit:\u{212a}elvin"),
        ("bank.com", "Submit:\u{ff4a}ob"),
        ("bank.com", "Submit:job application"),
        ("*", "Challenge:x"),
    ] {
        match s.gate_rule_set(d, c, ASK, "").await {
            Err(MemoryError::Invalid(_)) => {}
            other => panic!("gate_rule_set({d:?}, {c:?}) should be Invalid, got {other:?}"),
        }
    }
    assert_eq!(count().await, n0 + 3);
}

fn send(to: &[&str]) -> Call {
    Call {
        run_id: None,
        server: "echo".into(),
        tool: "echo.send".into(),
        class: Class::Send,
        domain: Some("mail.example".into()),
        recipients: to.iter().map(|s| s.to_string()).collect(),
        hints: Vec::new(),
        args: json!({"body": "hi"}),
        what: None,
        values: Vec::new(),
        strict: false,
    }
}

#[tokio::test]
async fn r3_d120_gate_over_pgstore_row_dedupe_and_hint_timeout() {
    let db = TestDb::new().await;
    let s = db.store.clone();
    let gate = Gate::new(s.clone());
    // Blank recipients ignored, duplicates by contact_key counted once.
    let c = send(&["New@X.com", " new@x.com ", "   ", ""]);
    let card = match gate.check(&c).await.unwrap() {
        Decision::Ask(card) => card,
        other => panic!("{other:?}"),
    };
    println!("r3 pg send card: {:?}", card.values);
    assert_eq!(
        card.values[0],
        ("new recipients (1)".to_string(), "New@X.com".to_string())
    );
    assert_eq!(card.on_timeout, OnTimeout::Proceed);
    s.touch_contact("new@x.com", "email", "").await.unwrap();
    assert_eq!(gate.check(&c).await.unwrap(), Decision::Allow);
    // D-120 (4): a lower-class allow hint that loses to the class level keeps its timeout.
    s.gate_rule_set(
        "*",
        "Read:newsletter",
        GateRule {
            mode: Mode::Allow,
            strict: false,
        },
        "",
    )
    .await
    .unwrap();
    let acct = Call {
        class: Class::Account,
        tool: "web.signup".into(),
        recipients: Vec::new(),
        hints: vec!["newsletter".into()],
        domain: Some("news.example".into()),
        ..send(&[])
    };
    match gate.check(&acct).await.unwrap() {
        Decision::Ask(card) => assert_eq!(card.on_timeout, OnTimeout::Proceed),
        other => panic!("{other:?}"),
    }
    let pay = Call {
        class: Class::Pay,
        ..acct.clone()
    };
    match gate.check(&pay).await.unwrap() {
        Decision::Ask(card) => assert_eq!(card.on_timeout, OnTimeout::Skip),
        other => panic!("{other:?}"),
    }
}
