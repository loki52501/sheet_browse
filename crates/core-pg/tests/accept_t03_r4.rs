//! T-03 acceptance, round 4: D-125 (trigram cut-off 0.5 on every pooled session) and the
//! D-126 key that PgStore stores for a domain idna maps. Written by the tester. Each test
//! runs on its own throwaway `sb_t_a03r4_<random>` database (D-100) on
//! SB_TEST_DATABASE_URL, dropped when the test ends. Never the shared `sb` database.
//!
//! `cargo test -p core-pg --test accept_t03_r4`

use core_pg::PgStore;
use harness_core::class::{Class, Mode};
use harness_core::gate::{Call, Decision, Gate};
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
    format!("sb_t_a03r4_{:016x}", h.finish())
}

struct TestDb {
    store: Arc<PgStore>,
    name: String,
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

fn src() -> Source {
    Source {
        kind: SourceKind::User,
        reference: "t03-r4".into(),
        run_id: None,
    }
}

const STOP: GateRule = GateRule {
    mode: Mode::Stop,
    strict: false,
};

const THRESHOLD_SQL: &str =
    "select pg_backend_pid(), current_setting('pg_trgm.word_similarity_threshold')";

// ------------------------------------------------------------------ D-125

/// D-125: every pooled session has pg_trgm.word_similarity_threshold = 0.5, including a
/// session the pool opens later (8 held at once and detached, then a fresh one).
#[tokio::test]
async fn r4_d125_every_pooled_session_has_threshold_0_5() {
    let db = TestDb::new().await;
    let pool = db.store.pool();
    let mut held = Vec::new();
    let mut seen = Vec::new();
    for _ in 0..8 {
        let mut c = pool.acquire().await.unwrap();
        let (pid, t): (i32, String) = sqlx::query_as(THRESHOLD_SQL)
            .fetch_one(&mut *c)
            .await
            .unwrap();
        seen.push((pid, t));
        held.push(c);
    }
    let pids: std::collections::BTreeSet<i32> = seen.iter().map(|(p, _)| *p).collect();
    // Detach every held session so the pool has to open a new one.
    for c in held {
        let _ = c.detach();
    }
    let mut c = pool.acquire().await.unwrap();
    let (pid, t): (i32, String) = sqlx::query_as(THRESHOLD_SQL)
        .fetch_one(&mut *c)
        .await
        .unwrap();
    println!("r4 pg sessions: {seen:?}, after detach: ({pid}, {t})");
    assert_eq!(pids.len(), 8, "8 distinct backends");
    assert!(seen.iter().all(|(_, t)| t == "0.5"), "{seen:?}");
    assert!(!pids.contains(&pid), "a new backend after detach");
    assert_eq!(t, "0.5");
}

/// D-125 on the server: search's trigram tier admits word_similarity >= 0.5 and nothing
/// under it, scored by the server's word_similarity. The printed scores feed MemStore's
/// table in crates/core/tests/gate_accept_t04_r4.rs (SERVER_SCORES).
#[tokio::test]
async fn r4_d125_server_cutoff_is_0_5() {
    let db = TestDb::new().await;
    let s = &db.store;
    s.remember(Fact::new(
        "Vasanth Raghavan",
        "role",
        "staff engineer",
        src(),
    ))
    .await
    .unwrap();
    s.remember(Fact::new("reading list", "item", "rust book", src()))
        .await
        .unwrap();
    let (mut at_half, mut under) = (false, false);
    for q in ["Vasnth", "Vasanh", "Raghvn", "Vsnth", "Vasnath", "Rghavan"] {
        let ws: f32 = sqlx::query_scalar("select word_similarity($1, 'Vasanth Raghavan')")
            .bind(q)
            .fetch_one(s.pool())
            .await
            .unwrap();
        let hits = s.search(q, 5).await.unwrap();
        let hit = hits.iter().find(|h| h.fact.subject == "Vasanth Raghavan");
        println!(
            "r4 pg {q:?}: word_similarity {ws}, hit {:?}",
            hit.map(|h| (h.score, h.why))
        );
        if ws >= 0.5 {
            let h = hit.unwrap_or_else(|| panic!("{q:?} ({ws}) must hit"));
            assert_eq!(h.why, Why::Trigram);
            assert!((h.score - ws).abs() < 1e-6);
            at_half |= ws == 0.5;
        } else {
            assert!(hit.is_none(), "{q:?} ({ws}) is under 0.5 and must not hit");
            under |= ws >= 0.3;
        }
    }
    // The fixture brackets the cut-off: a query at exactly 0.5 hits, one in [0.3, 0.5)
    // does not.
    assert!(at_half && under, "fixture must bracket 0.5");
}

/// D-125: the `%>` operator itself uses 0.5 on a pooled session, and `subject %> $1`
/// stays on the gin trigram index.
#[tokio::test]
async fn r4_d125_trigram_operator_uses_0_5_and_the_index() {
    let db = TestDb::new().await;
    db.store
        .remember(Fact::new(
            "Vasanth Raghavan",
            "role",
            "staff engineer",
            src(),
        ))
        .await
        .unwrap();
    let mut c = db.store.pool().acquire().await.unwrap();
    let hit: bool = sqlx::query_scalar("select 'Vasanth Raghavan' %> 'Vasnth'")
        .fetch_one(&mut *c)
        .await
        .unwrap();
    sqlx::raw_sql("set enable_seqscan = off")
        .execute(&mut *c)
        .await
        .unwrap();
    let plan: Vec<String> = sqlx::query_scalar(
        "explain (costs off) select id from memory.facts where subject %> 'Vasnth'",
    )
    .fetch_all(&mut *c)
    .await
    .unwrap();
    sqlx::raw_sql("reset enable_seqscan")
        .execute(&mut *c)
        .await
        .unwrap();
    println!("r4 pg %> at 0.5: {hit}; plan {plan:?}");
    assert!(hit);
    assert!(
        plan.iter()
            .any(|l| l.contains("Index Scan") && l.contains("facts_subject")),
        "{plan:?}"
    );
}

// ------------------------------------------------------------------ D-126 over PgStore

/// The same inputs as crates/core/tests/gate_accept_t04_r4.rs: the ones b829add removed
/// from the tester's refusal lists, plus D-126's examples, with the WHATWG URL host
/// (idna::domain_to_ascii_from_cow(.., AsciiDenyList::URL), minus one trailing dot) that
/// the core test checks against idna. core-pg has no idna dependency, so it is spelled out.
const BROWSER_HOST: [(&str, &str); 18] = [
    ("bu\u{0308}cher.de", "xn--bcher-kva.de"),
    ("ban\u{200b}k.com", "bank.com"),
    ("ban\u{00ad}k.com", "bank.com"),
    ("ban\u{2060}k.com", "bank.com"),
    ("bank\u{2800}.com", "xn--bank-yj5b.com"),
    ("\u{fb01}le.com", "file.com"),
    ("x\u{00b2}.com", "x2.com"),
    ("\u{1d41b}ank.com", "bank.com"),
    ("\u{24d1}ank.com", "bank.com"),
    ("\u{ff8a}\u{ff9f}.jp", "xn--odk.jp"),
    ("\u{17f}hop.example", "shop.example"),
    ("\u{133}ssel.nl", "ijssel.nl"),
    ("b\u{1d2c}nk.example", "bank.example"),
    ("example.\u{391}\u{3a3}", "example.xn--mxa0b"),
    ("stra\u{1e9e}e.de", "xn--strae-oqa.de"),
    ("\u{f900}.kr", "xn--oh3a.kr"),
    ("\u{3131}.kr", "xn--ypd.kr"),
    ("BÜCHER.DE.", "xn--bcher-kva.de"),
];

/// D-126 over PgStore: a rule written in any spelling idna maps is stored under the key a
/// browser host carries (read back from gate.rules), and a Submit on that host stops.
#[tokio::test]
async fn r4_d126_pg_stores_the_browser_host_key() {
    let db = TestDb::new().await;
    let s = db.store.clone();
    let gate = Gate::new(s.clone());
    for (input, host) in BROWSER_HOST {
        sqlx::query("delete from gate.rules where domain <> '*'")
            .execute(s.pool())
            .await
            .unwrap();
        s.gate_rule_set(input, "Submit", STOP, "").await.unwrap();
        let rows: Vec<(String, String)> =
            sqlx::query_as("select domain, class from gate.rules where domain <> '*'")
                .fetch_all(s.pool())
                .await
                .unwrap();
        println!("r4 pg {} -> {rows:?}", input.escape_unicode());
        assert_eq!(
            rows,
            vec![(host.to_string(), "Submit".to_string())],
            "{}",
            input.escape_unicode()
        );
        assert_eq!(s.gate_rule(host, "Submit").await.unwrap(), Some(STOP));
        let mut c = Call {
            run_id: None,
            server: "browser".into(),
            tool: "browser.submit".into(),
            class: Class::Submit,
            domain: Some(host.to_string()),
            recipients: Vec::new(),
            hints: Vec::new(),
            args: json!({}),
            what: None,
            values: Vec::new(),
            strict: false,
        };
        assert!(matches!(gate.check(&c).await.unwrap(), Decision::Stop(_)));
        c.domain = Some("other.example".into());
        assert_eq!(gate.check(&c).await.unwrap(), Decision::Allow);
    }
}
