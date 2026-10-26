//! B-175: `core_pg::bootstrap()` refuses an `SB_RUN_ID` whose `runs` row does not exist, and
//! accepts it once the row is there. Its own test binary, because bootstrap reads the
//! process environment and installs the process-wide scrubber. Runs on a throwaway
//! `sb_t_<random>` database (D-100) with an empty vault (a missing vault file, so no
//! keychain access) and a temporary runs root.

use core_pg::PgStore;
use harness_core::memory::{MemoryError, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
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

/// The throwaway database and temp folder, removed when the test ends, pass or panic.
struct Fixture {
    name: String,
    url: String,
    dir: std::path::PathBuf,
}

impl Fixture {
    async fn new() -> Fixture {
        let suffix = random_suffix();
        let name = format!("sb_t_b175_{suffix}");
        let admin = admin_url();
        let mut conn = sqlx::PgConnection::connect(&admin)
            .await
            .unwrap_or_else(|e| panic!("cannot reach the test Postgres ({e})"));
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database {name}")))
            .execute(&mut conn)
            .await
            .expect("create test database");
        let _ = conn.close().await;
        let dir = std::env::temp_dir().join(format!("sb-b175-{suffix}"));
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

#[tokio::test]
async fn bootstrap_requires_a_runs_row_for_sb_run_id() {
    let fx = Fixture::new().await;
    let runs_root = fx.dir.join("runs");
    // This binary has one test, so nothing else reads the environment while it changes.
    std::env::set_var("DATABASE_URL", &fx.url);
    std::env::set_var("SB_RUNS_ROOT", &runs_root);
    std::env::set_var("SB_VAULT_PATH", fx.dir.join("no-vault.toml.age"));

    // Migrate first so the runs table exists for the second half.
    let store = PgStore::connect(&fx.url).await.expect("connect + migrate");
    let folder = RunFolder::create(&runs_root, "b175", "check bootstrap\n").expect("run folder");
    std::env::set_var("SB_RUN_ID", &folder.id.0);

    // A folder with no runs row: refused, naming the id.
    let err = match core_pg::bootstrap().await {
        Ok(_) => panic!("bootstrap accepted an SB_RUN_ID with no runs row"),
        Err(e) => e,
    };
    println!("b175 without row: {err}");
    assert!(matches!(err, MemoryError::Invalid(_)), "{err:?}");
    assert!(err.to_string().contains(&folder.id.0), "{err}");

    // With the row the launching session writes: accepted, and the harness is on that run.
    store
        .run_start(Run {
            id: folder.id.clone(),
            started: harness_core::memory::ts_now(),
            ended: None,
            prompt: "check bootstrap".into(),
            status: RunStatus::Running,
            folder: folder.dir.to_string_lossy().replace('\\', "/"),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    let harness = core_pg::bootstrap()
        .await
        .unwrap_or_else(|e| panic!("bootstrap with a runs row: {e}"));
    assert_eq!(harness.run.as_ref(), Some(&folder.id));
}
