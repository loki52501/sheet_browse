//! core-pg against a real Postgres (docs/02 acceptance, step 1). Each test gets a throwaway
//! database `sb_t_<random>` on SB_TEST_DATABASE_URL (D-100), migrated by `PgStore::connect`
//! and dropped when the test ends, pass or panic. Never the shared `sb` database.

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

/// `url` with its database name replaced by `db`.
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
    store: PgStore,
    name: String,
}

impl TestDb {
    async fn new() -> TestDb {
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
        TestDb {
            store: PgStore::connect(&with_db(&admin, &name))
                .await
                .unwrap_or_else(|e| panic!("connect + migrate {name}: {e}")),
            name,
        }
    }
}

impl Drop for TestDb {
    /// Drops the database on a fresh runtime in its own thread (Drop cannot await, and the
    /// test's runtime may be shutting down). `with (force)` ends the store's pooled sessions.
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

// ------------------------------------------------------------------ M-4 recall / search

#[tokio::test]
async fn recall_finds_chromiumoxide_by_meaning_and_words() {
    let db = TestDb::new().await;
    let s = &db.store;
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
        ("Vasanth", "works_at", "Vasanth is at Rapidflare"),
        (
            "sourdough",
            "recipe",
            "Feed the starter twice a day with rye flour",
        ),
        ("dentist", "appointment", "Teeth cleaning on Tuesday at 9am"),
        (
            "tokyo trip",
            "hotel",
            "Booked a ryokan near Asakusa for April",
        ),
    ] {
        s.remember(Fact::new(subj, key, val, src(SourceKind::Email, "msg-1")))
            .await
            .unwrap();
    }

    let hits = s.recall("Rust CDP crate", 3, None).await.unwrap();
    assert!(!hits.is_empty());
    assert!(!"Rust CDP crate".contains("chromiumoxide"));
    assert_eq!(hits[0].id, target, "top hit: {:?}", hits[0].fact.subject);
    assert!(hits[0].fact.value.contains("chromiumoxide"));
    assert_eq!(hits[0].why, Why::Both);
    assert!(hits.len() <= 3);
    // The others share no word with the query: vector only, and ranked below.
    for h in &hits[1..] {
        assert_eq!(h.why, Why::Vector, "{:?}", h.fact.subject);
        assert!(h.score < hits[0].score);
    }

    // Filters: source kind, subject prefix, since.
    let page_only = Filter {
        source_kind: Some(SourceKind::Email),
        ..Filter::default()
    };
    let hits = s
        .recall("Rust CDP crate", 10, Some(page_only))
        .await
        .unwrap();
    assert!(hits.iter().all(|h| h.id != target));
    assert_eq!(hits.len(), 4);
    let prefix = Filter {
        subject_prefix: Some("chrom".into()),
        ..Filter::default()
    };
    let hits = s
        .recall("browser automation", 10, Some(prefix))
        .await
        .unwrap();
    assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![target]);
    let future = Filter {
        since: Some(Utc::now() + Duration::hours(1)),
        ..Filter::default()
    };
    assert!(s.recall("Rust", 10, Some(future)).await.unwrap().is_empty());

    // A query that is only stop words still answers through the vector side.
    let hits = s.recall("the of and", 2, None).await.unwrap();
    assert!(hits.iter().all(|h| h.why == Why::Vector));
    assert!(s.recall("anything", 0, None).await.unwrap().is_empty());

    // forget removes it from both paths.
    s.forget(target).await.unwrap();
    let hits = s.recall("Rust CDP crate", 10, None).await.unwrap();
    assert!(hits.iter().all(|h| h.id != target));
    assert!(matches!(
        s.forget(target).await,
        Err(MemoryError::NotFound(_))
    ));
}

#[tokio::test]
async fn search_is_exact_then_trigram() {
    let db = TestDb::new().await;
    let s = &db.store;
    let rapid = s
        .remember(Fact::new(
            "Rapidflare",
            "domain",
            "rapidflare.ai",
            src(SourceKind::User, "cli"),
        ))
        .await
        .unwrap();
    let vas = s
        .remember(Fact::new(
            "Vasanth",
            "employer",
            "Vasanth works at Rapidflare",
            src(SourceKind::Email, "msg-2"),
        ))
        .await
        .unwrap();

    let hits = s.search("rapidflare", 10).await.unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, rapid, "equality on subject ranks first");
    assert!(hits.iter().all(|h| h.why == Why::Exact));
    assert!(hits.iter().any(|h| h.id == vas));
    assert!(hits[0].score >= hits[1].score);

    // A typo: no substring match, trigram on subject finds it.
    let hits = s.search("Rapidflair", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, rapid);
    assert_eq!(hits[0].why, Why::Trigram);
    assert!(hits[0].score > 0.0 && hits[0].score < 1.0);

    assert_eq!(s.search("rapidflare", 1).await.unwrap().len(), 1);
    assert!(s.search("   ", 5).await.unwrap().is_empty());
    assert!(s.search("zzzqqq", 5).await.unwrap().is_empty());
}

// ------------------------------------------------------------------ M-5 relations

#[tokio::test]
async fn neighbors_walks_three_hops_both_ways() {
    let db = TestDb::new().await;
    let s = &db.store;
    let user = src(SourceKind::User, "test");
    let alice = s.node(node_kind::PERSON, "alice").await.unwrap();
    assert_eq!(s.node(node_kind::PERSON, "alice").await.unwrap(), alice);
    let conf = s.node(node_kind::EVENT, "RustConf 2026").await.unwrap();
    let bob = s.node(node_kind::PERSON, "bob").await.unwrap();
    let company = s.node(node_kind::COMPANY, "Rapidflare").await.unwrap();
    let role = s
        .node(node_kind::ROLE, "Founding Engineer, Rapidflare")
        .await
        .unwrap();
    let thread = s.node(node_kind::THREAD, "offer thread").await.unwrap();
    let tool = s
        .node(node_kind::TOOL, "rapidflare.ai/apply")
        .await
        .unwrap();

    // alice -met_at-> conf <-attended- bob -works_at-> company, bob -works_at-> role
    s.link(alice, rel::MET_AT, conf, user.clone())
        .await
        .unwrap();
    s.link(bob, rel::ATTENDED, conf, user.clone())
        .await
        .unwrap();
    s.link(bob, rel::WORKS_AT, company, user.clone())
        .await
        .unwrap();
    s.link(bob, rel::WORKS_AT, role, user.clone())
        .await
        .unwrap();
    // 4 hops: role <-applied_to- thread; 5 hops: thread -about_tool-> tool
    s.link(thread, rel::APPLIED_TO, role, user.clone())
        .await
        .unwrap();
    s.link(thread, rel::ABOUT_TOOL, tool, user.clone())
        .await
        .unwrap();
    // Linking twice is a no-op; a missing node is NotFound.
    s.link(alice, rel::MET_AT, conf, user.clone())
        .await
        .unwrap();
    assert!(matches!(
        s.link(alice, rel::MET_AT, NodeId(999_999), user.clone())
            .await,
        Err(MemoryError::NotFound(_))
    ));

    let three = s.neighbors(alice, None, 3).await.unwrap();
    let ids: Vec<NodeId> = three.iter().map(|n| n.id).collect();
    assert!(three
        .iter()
        .any(|n| n.kind == node_kind::ROLE && n.id == role));
    for want in [conf, bob, company, role] {
        assert!(ids.contains(&want), "missing {want:?} in {three:?}");
    }
    assert!(!ids.contains(&alice), "n itself is excluded");
    assert!(!ids.contains(&thread), "4 hops away");
    assert_eq!(ids[0], conf, "nearest first");

    let four = s.neighbors(alice, None, 4).await.unwrap();
    assert!(four.iter().any(|n| n.id == thread));
    assert!(!four.iter().any(|n| n.id == tool));
    // depth is capped at 4.
    let capped = s.neighbors(alice, None, 200).await.unwrap();
    assert_eq!(capped, four);
    assert!(s.neighbors(alice, None, 0).await.unwrap().is_empty());

    // rel applies to every hop.
    let met = s.neighbors(alice, Some(rel::MET_AT), 4).await.unwrap();
    assert_eq!(met.iter().map(|n| n.id).collect::<Vec<_>>(), vec![conf]);
    let works = s.neighbors(company, Some(rel::WORKS_AT), 4).await.unwrap();
    let mut got: Vec<NodeId> = works.iter().map(|n| n.id).collect();
    got.sort();
    assert_eq!(got, vec![bob, role]);
}

// ------------------------------------------------------------------ SM-2

#[tokio::test]
async fn grade_review_follows_sm2() {
    let db = TestDb::new().await;
    let s = &db.store;
    let id = s
        .review_add("capital of Australia", "Canberra")
        .await
        .unwrap();
    let due = s.due_reviews(Utc::now(), 10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(
        (due[0].reps, due[0].interval_days, due[0].ease),
        (0, 1.0, 2.5)
    );

    s.grade_review(id, 5).await.unwrap(); // reps 1, 1 day
    s.grade_review(id, 5).await.unwrap(); // reps 2, 6 days
    let r = review(s, id).await;
    assert_eq!((r.reps, r.interval_days), (2, 6.0));
    let before = r.clone();

    s.grade_review(id, 5).await.unwrap(); // reps 3, ~6 * ease
    let r = review(s, id).await;
    assert_eq!(r.reps, 3);
    let want = before.interval_days * before.ease;
    assert!(
        (r.interval_days - want).abs() <= 1.0,
        "{} vs {want}",
        r.interval_days
    );
    assert!(r.ease > before.ease);
    let days = (r.due - Utc::now()).num_hours() as f32 / 24.0;
    assert!((days - r.interval_days).abs() < 0.1, "due in {days} days");
    assert!(s.due_reviews(Utc::now(), 10).await.unwrap().is_empty());

    s.grade_review(id, 2).await.unwrap(); // fail: back to 1 day
    let r = review(s, id).await;
    assert_eq!((r.reps, r.interval_days), (0, 1.0));
    assert!(r.ease >= 1.3);

    assert!(matches!(
        s.grade_review(id, 6).await,
        Err(MemoryError::Invalid(_))
    ));
    assert!(matches!(
        s.grade_review(ReviewId(424_242), 4).await,
        Err(MemoryError::NotFound(_))
    ));
}

async fn review(s: &PgStore, id: ReviewId) -> Review {
    s.due_reviews(Utc::now() + Duration::days(3650), 100)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.id == id)
        .expect("review exists")
}

// ------------------------------------------------------------------ ledger (H-3)

fn event(run: &RunId, tool: &str) -> LedgerEvent {
    LedgerEvent {
        run_id: Some(run.clone()),
        ts: ts_now(),
        server: "web".into(),
        tool: tool.into(),
        class: Some(Class::Submit),
        mode: Some(LedgerMode::TimeoutProceed),
        args: json!({"password": "@vault(site_pw)"}),
        result_summary: "submitted".into(),
        consequences: json!(["form posted"]),
        duration_ms: 42,
        ok: true,
        why: Some("ask timed out".into()),
    }
}

#[tokio::test]
async fn ledger_is_append_only() {
    let db = TestDb::new().await;
    let s = &db.store;
    let run = RunId("2026-10-23T10-00-00-test".into());
    let e1 = event(&run, "click");
    let id1 = s.append(e1.clone()).await.unwrap();
    let mut e2 = event(&run, "fill");
    e2.class = None;
    e2.mode = None;
    e2.why = None;
    let id2 = s.append(e2.clone()).await.unwrap();
    s.append(event(&RunId("other".into()), "x")).await.unwrap();
    assert!(id2 > id1);

    let got = s.ledger_events(&run).await.unwrap();
    assert_eq!(got, vec![(id1, e1), (id2, e2)]);

    let mut neg = event(&run, "bad");
    neg.duration_ms = -1;
    assert!(matches!(s.append(neg).await, Err(MemoryError::Invalid(_))));

    for sql in [
        "update ledger.events set ok = false",
        "update ledger.events set ok = false where id = -1",
        "delete from ledger.events",
        "truncate ledger.events",
    ] {
        let err = sqlx::query(sql)
            .execute(s.pool())
            .await
            .expect_err("append-only guard");
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
    }
    assert_eq!(s.ledger_events(&run).await.unwrap().len(), 2);
}

// ------------------------------------------------------------------ gate rules, contacts

#[tokio::test]
async fn gate_rules_and_contacts() {
    let db = TestDb::new().await;
    let s = &db.store;
    let ask = Some(GateRule {
        mode: Mode::Ask,
        strict: false,
    });
    assert_eq!(s.gate_rule("*", "Pay").await.unwrap(), ask);
    assert_eq!(
        s.gate_rule("*", "Submit:job_application").await.unwrap(),
        ask
    );
    assert_eq!(
        s.gate_rule("*", "Challenge").await.unwrap().map(|r| r.mode),
        Some(Mode::Stop)
    );
    assert_eq!(s.gate_rule("example.com", "Pay").await.unwrap(), None);

    let allow = GateRule {
        mode: Mode::Allow,
        strict: false,
    };
    assert!(matches!(
        s.gate_rule_set("*", "Challenge", allow, "no").await,
        Err(MemoryError::Invalid(_))
    ));
    assert!(matches!(
        s.gate_rule_set("example.com", "Challenge:hcaptcha", allow, "")
            .await,
        Err(MemoryError::Invalid(_))
    ));
    assert_eq!(
        s.gate_rule("*", "Challenge").await.unwrap().map(|r| r.mode),
        Some(Mode::Stop)
    );
    let strict_ask = GateRule {
        mode: Mode::Ask,
        strict: true,
    };
    s.gate_rule_set("shop.example", "Pay", allow, "trusted")
        .await
        .unwrap();
    s.gate_rule_set("shop.example", "Pay", strict_ask, "changed my mind")
        .await
        .unwrap();
    assert_eq!(
        s.gate_rule("shop.example", "Pay").await.unwrap(),
        Some(strict_ask)
    );

    // D-110: keys are canonicalised on write and on lookup.
    s.gate_rule_set("Bank.COM.", "submit", strict_ask, "")
        .await
        .unwrap();
    assert_eq!(
        s.gate_rule("bank.com", "Submit").await.unwrap(),
        Some(strict_ask)
    );
    assert_eq!(
        s.gate_rule(" BANK.com. ", "SUBMIT").await.unwrap(),
        Some(strict_ask)
    );
    s.gate_rule_set("*", "Submit: Wire ", strict_ask, "")
        .await
        .unwrap();
    assert_eq!(
        s.gate_rule("*", "Submit:wire").await.unwrap(),
        Some(strict_ask)
    );
    let stored: Vec<(String, String)> =
        sqlx::query_as("select domain, class from gate.rules where note = '' and strict")
            .fetch_all(s.pool())
            .await
            .unwrap();
    assert!(stored.contains(&("bank.com".into(), "Submit".into())));
    assert!(stored.contains(&("*".into(), "Submit:wire".into())));
    for (d, c) in [("bank com", "Pay"), ("*", "submit:"), ("*", "Wire")] {
        assert!(matches!(
            s.gate_rule_set(d, c, strict_ask, "").await,
            Err(MemoryError::Invalid(_))
        ));
    }
    assert_eq!(s.gate_rule("*", "Teleport").await.unwrap(), None);

    assert_eq!(s.contact("Vasanth@Example.com").await.unwrap(), None);
    s.touch_contact("  Vasanth@Example.com ", "email", "met at RustConf")
        .await
        .unwrap();
    let c = s.contact("vasanth@example.com").await.unwrap().unwrap();
    assert_eq!(c.person, "vasanth@example.com");
    assert_eq!(c.count, 1);
    assert_eq!(c.channel.as_deref(), Some("email"));
    assert_eq!(c.note.as_deref(), Some("met at RustConf"));
    assert!(c.last.is_some());
    s.touch_contact("VASANTH@example.com", "", "")
        .await
        .unwrap();
    let c2 = s.contact("vasanth@example.com").await.unwrap().unwrap();
    assert_eq!(c2.count, 2);
    assert_eq!(c2.note.as_deref(), Some("met at RustConf"));
    assert!(c2.last >= c.last);
}

// ------------------------------------------------------------------ runs, schedules, baselines, health

#[tokio::test]
async fn runs_schedules_baselines_health() {
    let db = TestDb::new().await;
    let s = &db.store;
    let parent = RunId("2026-10-23T09-00-00-schedule-add".into());
    let run = Run {
        id: RunId("2026-10-23T10-00-00-watch".into()),
        started: ts_now(),
        ended: None,
        prompt: "check the price".into(),
        status: RunStatus::Running,
        folder: "runs/2026-10-23T10-00-00-watch".into(),
        parent_run: Some(parent.clone()),
        strict: true,
    };
    s.run_start(run.clone()).await.unwrap();
    assert!(matches!(
        s.run_start(run.clone()).await,
        Err(MemoryError::Invalid(_))
    ));
    assert_eq!(s.run_get(&run.id).await.unwrap(), Some(run.clone()));
    let ended = ts_now();
    s.run_end(&run.id, RunStatus::Partial, ended).await.unwrap();
    let got = s.run_get(&run.id).await.unwrap().unwrap();
    assert_eq!((got.status, got.ended), (RunStatus::Partial, Some(ended)));
    assert!(matches!(
        s.run_end(&RunId("nope".into()), RunStatus::Done, ended)
            .await,
        Err(MemoryError::NotFound(_))
    ));
    assert_eq!(s.run_get(&RunId("nope".into())).await.unwrap(), None);

    let sched = Schedule {
        id: 9999,
        cron: "*/1 * * * *".into(),
        prompt: "watch the price".into(),
        kind: ScheduleKind::Watch,
        watch_target: Some("shop:price".into()),
        enabled: true,
        last_run: None,
        run_id: Some(parent.clone()),
    };
    let sid = s.schedule_add(sched.clone()).await.unwrap();
    assert_ne!(sid, 9999, "schedule_add ignores s.id");
    let fired = ts_now();
    s.schedule_fired(sid, fired).await.unwrap();
    let list = s.schedule_list().await.unwrap();
    assert_eq!(
        list,
        vec![Schedule {
            id: sid,
            last_run: Some(fired),
            ..sched
        }]
    );
    s.schedule_rm(sid).await.unwrap();
    assert!(s.schedule_list().await.unwrap().is_empty());
    assert!(matches!(
        s.schedule_rm(sid).await,
        Err(MemoryError::NotFound(_))
    ));
    assert!(matches!(
        s.schedule_fired(sid, fired).await,
        Err(MemoryError::NotFound(_))
    ));

    assert_eq!(s.baseline_get("shop:price").await.unwrap(), None);
    let b = Baseline {
        hash: "abc".into(),
        snapshot: json!({"price": "19.99"}),
        updated: ts_now(),
    };
    s.baseline_set("shop:price", b.clone()).await.unwrap();
    assert_eq!(s.baseline_get("shop:price").await.unwrap(), Some(b));
    let b2 = Baseline {
        hash: "def".into(),
        snapshot: json!({"price": "17.99"}),
        updated: ts_now(),
    };
    s.baseline_set("shop:price", b2.clone()).await.unwrap();
    assert_eq!(s.baseline_get("shop:price").await.unwrap(), Some(b2));

    let h = ToolHealth {
        domain: "linkedin.com".into(),
        name: "apply".into(),
        version: 2,
        status: ToolStatus::NeedsRepair,
        runs: 10,
        fails: 3,
        last_ok: Some(ts_now()),
        last_fail: None,
        why: Some("selector moved".into()),
        pinned: true,
    };
    s.tool_health_set(h.clone()).await.unwrap();
    let mut h2 = h.clone();
    h2.name = "search".into();
    h2.status = ToolStatus::Ask;
    h2.pinned = false;
    s.tool_health_set(h2.clone()).await.unwrap();
    let mut h3 = h.clone();
    h3.status = ToolStatus::Repaired;
    h3.version = 3;
    s.tool_health_set(h3.clone()).await.unwrap();
    assert_eq!(s.tool_health("linkedin.com").await.unwrap(), vec![h3, h2]);
    assert!(s.tool_health("_generic").await.unwrap().is_empty());
}
