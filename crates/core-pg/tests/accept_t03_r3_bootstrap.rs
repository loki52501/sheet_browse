//! T-03 acceptance, round 3, B-175: `core_pg::bootstrap()` and SB_RUN_ID, as a tool server's
//! `main` calls it. Its own test binary with one test, because bootstrap reads the process
//! environment and installs the process-wide scrubber. Runs on a throwaway `sb_t_<random>`
//! database (D-100), a missing vault file (empty vault, no keychain access) and a temporary
//! runs root.
//!
//! Cases: no SB_RUN_ID starts its own run (folder + `running` row); an SB_RUN_ID whose folder
//! is missing is refused; an SB_RUN_ID with a folder but no runs row is refused, naming the
//! id, and writes no row; with the row it is accepted and no other run is created.

use core_pg::PgStore;
use harness_core::memory::{MemoryError, MemoryStore, Run, RunStatus};
use harness_core::run::{RunFolder, RunId};
use sqlx::Connection;
use std::hash::{BuildHasher, Hasher};

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

fn random_suffix() -> String {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    );
    format!("{:016x}", h.finish())
}

struct Fixture {
    name: String,
    url: String,
    dir: std::path::PathBuf,
}

impl Fixture {
    async fn new() -> Fixture {
        let suffix = random_suffix();
        let name = format!("sb_t_a03r3b_{suffix}");
        let admin = admin_url();
        let mut conn = sqlx::PgConnection::connect(&admin)
            .await
            .unwrap_or_else(|e| panic!("cannot reach the test Postgres ({e})"));
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database {name}")))
            .execute(&mut conn)
            .await
            .expect("create test database");
        let _ = conn.close().await;
        let dir = std::env::temp_dir().join(format!("sb-a03r3b-{suffix}"));
        std::fs::create_dir_all(&dir).expect("temp dir");
        Fixture {
            url: with_db(&admin, &name),
            name,
            dir,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let name = self.name.clone();
        let admin = admin_url();
        let _ = std::fs::remove_dir_all(&self.dir);
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

async fn run_ids(store: &PgStore) -> Vec<String> {
    sqlx::query_scalar("select id from runs order by id")
        .fetch_all(store.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn r3_b175_bootstrap_and_sb_run_id() {
    let fx = Fixture::new().await;
    let runs_root = fx.dir.join("runs");
    std::fs::create_dir_all(&runs_root).unwrap();
    // One test in this binary, so nothing else reads the environment while it changes.
    std::env::set_var("DATABASE_URL", &fx.url);
    std::env::set_var("SB_RUNS_ROOT", &runs_root);
    std::env::set_var("SB_VAULT_PATH", fx.dir.join("no-vault.toml.age"));
    std::env::remove_var("SB_RUN_ID");
    let store = PgStore::connect(&fx.url).await.expect("connect + migrate");

    // 1. No SB_RUN_ID: the server starts its own run, folder and `running` row.
    let h = core_pg::bootstrap()
        .await
        .unwrap_or_else(|e| panic!("bootstrap without SB_RUN_ID: {e}"));
    let own = h.run.clone().expect("a run");
    let row = store.run_get(&own).await.unwrap().expect("own runs row");
    assert_eq!(row.status, RunStatus::Running);
    assert!(RunFolder::open(&runs_root, &own).is_ok(), "own run folder");
    drop(h);
    assert_eq!(run_ids(&store).await, vec![own.0.clone()]);

    // 2. SB_RUN_ID naming no folder: refused.
    std::env::set_var("SB_RUN_ID", "2099-12-31T00-00-00-nofolder");
    match core_pg::bootstrap().await {
        Err(MemoryError::Invalid(m)) => println!("r3 b175 no folder: {m}"),
        Err(e) => panic!("expected Invalid, got {e:?}"),
        Ok(_) => panic!("bootstrap accepted an SB_RUN_ID with no folder"),
    }

    // 3. A folder with no runs row: refused, naming the id; no row is written.
    let folder = RunFolder::create(&runs_root, "a03r3b", "launched elsewhere\n").unwrap();
    std::env::set_var("SB_RUN_ID", format!("  {}  ", folder.id.0));
    match core_pg::bootstrap().await {
        Err(MemoryError::Invalid(m)) => {
            println!("r3 b175 no row: {m}");
            assert!(m.contains(&folder.id.0), "{m}");
        }
        Err(e) => panic!("expected Invalid, got {e:?}"),
        Ok(_) => panic!("bootstrap accepted an SB_RUN_ID with no runs row"),
    }
    assert!(store.run_get(&folder.id).await.unwrap().is_none());
    assert_eq!(run_ids(&store).await, vec![own.0.clone()]);

    // 4. With the row the launching session writes: accepted, on that run, nothing new.
    store
        .run_start(Run {
            id: folder.id.clone(),
            started: harness_core::memory::ts_now(),
            ended: None,
            prompt: "launched elsewhere".into(),
            status: RunStatus::Running,
            folder: folder.dir.to_string_lossy().replace('\\', "/"),
            parent_run: None,
            strict: true,
        })
        .await
        .unwrap();
    let h = core_pg::bootstrap()
        .await
        .unwrap_or_else(|e| panic!("bootstrap with a runs row: {e}"));
    assert_eq!(h.run.as_ref(), Some(&RunId(folder.id.0.clone())));
    let mut want = vec![own.0.clone(), folder.id.0.clone()];
    want.sort();
    assert_eq!(run_ids(&store).await, want);
    std::env::remove_var("SB_RUN_ID");
}
