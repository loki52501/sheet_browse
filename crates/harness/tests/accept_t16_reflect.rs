//! T-16 package B: a `reflect` schedule fired through `harness_core::sched::Runner` with
//! harnessd's `ReflectLauncher` (docs/01 H-5 reflect, docs/06 G-3, docs/03 T-7, T-8; build
//! order step 4 "reflect retires a planted stale ephemeral tool"; D-142).
//!
//! The planted tools folder:
//! * `example.com/stale_list` ephemeral, last ok 15 days ago: retired, archived, a `tools.retire`
//!   event in the reflect run's ledger and jsonl, and `tools.health` says retired;
//! * `example.com/fresh_list` ephemeral, last ok yesterday: kept;
//! * `example.com/pinned_old` pinned, last ok 30 days ago: kept, and a T-7 retire proposal;
//! * `visits.com/busy` ephemeral, unused over 10 later site visits (read from earlier runs'
//!   ledger.jsonl): retired; `few.com/quiet` with 9 visits: kept;
//! * `a.com/search`, `b.com/find`, `c.com/lookup`: alike under 3 domains, one T-8 merge;
//! * `example.com/list_items` (`@phase 1` reader) and `example.com/plan_buy` (planner), both
//!   with a good call in an earlier run: only the reader is replayed, through the readers-only
//!   launcher; the scheduled prompt's launcher never runs.
//!
//! Proposals land under the trailing `proposals:` of a scratch blockers.yaml, once (a second
//! pass adds nothing). A vault value planted raw in an earlier run's jsonl never reaches the
//! replay prompt, the proposals, the ledger or any file reflect writes.
//!
//! The scenario runs on `harness_core::testing::MemStore` and, when the compose Postgres is
//! reachable (`SB_TEST_DATABASE_URL`, default `postgres://sb@127.0.0.1:5433/postgres`, D-100),
//! again on a throwaway `sb_t_*` database. No model and no `claude` are started (rule 1); no
//! sleeps (rule 6).

use async_trait::async_trait;
use chrono::Duration as ChronoDuration;
use core_pg::PgStore;
use harness::reflect::{ReflectLauncher, PROMPT_FILE, SUMMARY_FILE, SWEEP_TOOL};
use harness_core::ledger::{read_jsonl, Ledger};
use harness_core::memory::*;
use harness_core::run::RunFolder;
use harness_core::sched::{Clock, LaunchError, LaunchExit, LaunchRequest, Launcher, Runner};
use harness_core::testing::MemStore;
use harness_core::vault::{EntryKind, Vault};
use serde_json::{json, Value};
use sqlx::Connection;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::time::timeout;

const DEFAULT_ADMIN_URL: &str = "postgres://sb@127.0.0.1:5433/postgres";
const VAULT_NAME: &str = "t16reflect";

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| c.trim().chars().count() >= 8)
        .unwrap_or_else(|| "canary-t16-reflect-not-a-secret".to_owned())
}

/// Installs the process-wide scrubber once (both tests use the same entry).
fn install_vault() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        Vault::in_memory([(VAULT_NAME.to_owned(), canary(), EntryKind::Secret)]).install_global();
    });
}

// ------------------------------------------------------------------ seams

struct FixedClock(Ts);

#[async_trait]
impl Clock for FixedClock {
    fn now(&self) -> Ts {
        self.0
    }
    async fn sleep_until(&self, _t: Ts) {}
}

/// Records every prompt it is launched with.
#[derive(Default)]
struct Recorder {
    prompts: Mutex<Vec<String>>,
}

impl Recorder {
    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

#[async_trait]
impl Launcher for Recorder {
    async fn launch(&self, req: LaunchRequest<'_>) -> Result<LaunchExit, LaunchError> {
        self.prompts.lock().unwrap().push(req.prompt.to_owned());
        Ok(LaunchExit::OK)
    }
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
    format!("sb_t_{:016x}", h.finish())
}

struct TestDb {
    name: String,
    url: String,
}

impl TestDb {
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
        Some(TestDb {
            url: with_db(&admin, &name),
            name,
        })
    }

    async fn store(&self) -> Arc<dyn MemoryStore> {
        Arc::new(
            PgStore::connect(&self.url)
                .await
                .unwrap_or_else(|e| panic!("connect + migrate {}: {e}", self.name)),
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

fn reader(name: &str, args: &str, fingerprint: &str) -> String {
    format!(
        "// @name {name}\n// @kind reader\n// @match /*\n// @args {args}\n// @gate Read\n\
         // @phase 1\n// @fingerprint {fingerprint}\n// T-16 reflect fixture.\n\
         export default function (page, args) {{ return {{ ok: true }}; }}\n"
    )
}

fn planner(name: &str) -> String {
    format!(
        "// @name {name}\n// @kind planner\n// @match /*\n// @args item:string\n// @gate Fill\n\
         // @phase 1\n// T-16 reflect fixture.\n\
         export default function (page, args) {{ return []; }}\n"
    )
}

fn plant(tools: &Path, domain: &str, name: &str, src: &str) {
    let dir = tools.join(domain);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.js")), src).unwrap();
}

fn row(domain: &str, name: &str, status: ToolStatus, last_ok: Ts, pinned: bool) -> ToolHealth {
    ToolHealth {
        domain: domain.into(),
        name: name.into(),
        version: 1,
        status,
        runs: 1,
        fails: 0,
        last_ok: Some(last_ok),
        last_fail: None,
        why: None,
        pinned,
    }
}

fn web(tool: &str, args: Value, ts: Ts) -> LedgerEvent {
    LedgerEvent {
        run_id: None,
        ts,
        server: "web".into(),
        tool: tool.into(),
        class: None,
        mode: None,
        args,
        result_summary: "ok".into(),
        consequences: json!({}),
        duration_ms: 1,
        ok: true,
        why: None,
    }
}

/// An earlier run at `at` with `events` in its ledger (store and jsonl).
async fn prior_run(
    store: &Arc<dyn MemoryStore>,
    runs: &Path,
    at: Ts,
    events: Vec<LedgerEvent>,
) -> RunFolder {
    let folder = RunFolder::create_at(runs, "prior", "prior", at).unwrap();
    store
        .run_start(Run {
            id: folder.id.clone(),
            started: at,
            ended: Some(at),
            prompt: "prior".into(),
            status: RunStatus::Done,
            folder: folder.dir.to_string_lossy().replace('\\', "/"),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    let ledger = Ledger::new(store.clone(), folder.clone());
    for e in events {
        ledger.write(e).await.unwrap();
    }
    folder
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            files_under(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn micros(t: Ts) -> Ts {
    chrono::DateTime::from_timestamp_micros(t.timestamp_micros()).unwrap()
}

// ------------------------------------------------------------------ the scenario

async fn scenario(store: Arc<dyn MemoryStore>) {
    install_vault();
    let canary = canary();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let tools = root.join("tools");
    let runs = root.join("runs");
    let proposals = root.join("blockers.yaml");
    let blockers_head = "blockers:\n  - id: B-1\n    text: keep me\nproposals:\n";
    std::fs::write(&proposals, blockers_head).unwrap();
    let now = micros(ts_now());
    let day = ChronoDuration::days(1);

    // Tools and their rows.
    let fp = "form input button\"Search\"";
    plant(
        &tools,
        "example.com",
        "stale_list",
        &reader("stale_list", "q:string", "ul li"),
    );
    plant(
        &tools,
        "example.com",
        "fresh_list",
        &reader("fresh_list", "q:string", "ul li"),
    );
    plant(
        &tools,
        "example.com",
        "pinned_old",
        &reader("pinned_old", "q:string", "ol li"),
    );
    plant(
        &tools,
        "example.com",
        "list_items",
        &reader("list_items", "q:string", "table tr"),
    );
    plant(&tools, "example.com", "plan_buy", &planner("plan_buy"));
    plant(
        &tools,
        "visits.com",
        "busy",
        &reader("busy", "n:number", "main"),
    );
    plant(
        &tools,
        "few.com",
        "quiet",
        &reader("quiet", "n:number", "main"),
    );
    plant(&tools, "a.com", "search", &reader("search", "q:string", fp));
    plant(&tools, "b.com", "find", &reader("find", "query:string", fp));
    plant(
        &tools,
        "c.com",
        "lookup",
        &reader("lookup", "term:string", fp),
    );
    let rows = [
        row(
            "example.com",
            "stale_list",
            ToolStatus::Ephemeral,
            now - day * 15,
            false,
        ),
        row(
            "example.com",
            "fresh_list",
            ToolStatus::Ephemeral,
            now - day,
            false,
        ),
        row(
            "example.com",
            "pinned_old",
            ToolStatus::Ephemeral,
            now - day * 30,
            true,
        ),
        row(
            "example.com",
            "list_items",
            ToolStatus::Saved,
            now - day,
            false,
        ),
        row(
            "example.com",
            "plan_buy",
            ToolStatus::Saved,
            now - day,
            false,
        ),
        row(
            "visits.com",
            "busy",
            ToolStatus::Ephemeral,
            now - day * 3,
            false,
        ),
        row(
            "few.com",
            "quiet",
            ToolStatus::Ephemeral,
            now - day * 3,
            false,
        ),
    ];
    for r in rows {
        store.tool_health_set(r).await.unwrap();
    }

    // Earlier runs: 10 visits to visits.com and 9 to few.com after their tools' last use.
    for i in 0..10 {
        let at = now - day * 2 + ChronoDuration::minutes(i);
        let mut ev = vec![web(
            "fetch",
            json!({"url": format!("https://www.visits.com/p/{i}")}),
            at,
        )];
        if i < 9 {
            ev.push(web("fetch", json!({"url": "https://few.com/"}), at));
        }
        prior_run(&store, &runs, at, ev).await;
    }
    // A run that fetched example.com and called a reader and a planner there.
    let at = now - day + ChronoDuration::hours(1);
    let shop = prior_run(
        &store,
        &runs,
        at,
        vec![
            web("fetch", json!({"url": "https://shop.example.com/list"}), at),
            web("example.plan_buy", json!({"item": "socks"}), at),
        ],
    )
    .await;
    // The reader's good call goes into the jsonl raw, with a vault value in its args: what
    // reflect replays must be scrubbed by reflect itself.
    let mut call = serde_json::to_value(web(
        "example.list_items",
        json!({"q": format!("shoes {canary}")}),
        at + ChronoDuration::seconds(1),
    ))
    .unwrap();
    call["run_id"] = json!(shop.id.0);
    call["id"] = json!(999_999);
    let planted_jsonl = shop.ledger_jsonl();
    let mut text = std::fs::read_to_string(&planted_jsonl).unwrap();
    text.push_str(&call.to_string());
    text.push('\n');
    std::fs::write(&planted_jsonl, text).unwrap();

    // harnessd's wiring with recording launchers in place of `claude -p`.
    let inner = Arc::new(Recorder::default());
    let readers = Arc::new(Recorder::default());
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(now));
    let launcher = Arc::new(ReflectLauncher {
        store: store.clone(),
        tools_root: tools.clone(),
        proposals_file: Some(proposals.clone()),
        clock: clock.clone(),
        commit: false,
        inner: inner.clone(),
        readers: readers.clone(),
    });
    let runner = Runner {
        store: store.clone(),
        launcher: launcher.clone(),
        clock,
        runs_root: runs.clone(),
    };
    let schedule = |kind: ScheduleKind, prompt: &str| Schedule {
        id: 0,
        cron: "0 3 * * *".into(),
        prompt: prompt.into(),
        kind,
        watch_target: None,
        enabled: true,
        last_run: None,
        run_id: None,
    };
    let mut reflect = schedule(ScheduleKind::Reflect, "nightly reflect");
    reflect.id = store.schedule_add(reflect.clone()).await.unwrap();

    let done = runner.fire(&reflect).await.unwrap();
    assert_eq!(done.launch_problem, None);
    assert!(
        inner.prompts().is_empty(),
        "the scheduled prompt never runs"
    );

    // 1. The planted stale ephemeral tool is retired and archived; the fresh one is not.
    let health = |domain: &'static str| {
        let store = store.clone();
        async move { store.tool_health(domain).await.unwrap() }
    };
    let ex = health("example.com").await;
    let status = |rows: &[ToolHealth], name: &str| {
        rows.iter()
            .find(|h| h.name == name)
            .map(|h| h.status)
            .unwrap()
    };
    assert_eq!(status(&ex, "stale_list"), ToolStatus::Retired);
    assert_eq!(status(&ex, "fresh_list"), ToolStatus::Ephemeral);
    assert_eq!(
        status(&ex, "pinned_old"),
        ToolStatus::Ephemeral,
        "pinned: never"
    );
    assert!(!tools.join("example.com/stale_list.js").exists());
    assert!(tools.join("example.com/fresh_list.js").is_file());
    let archived: Vec<_> = std::fs::read_dir(tools.join("_archive/example.com"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        archived.iter().any(|n| n.starts_with("stale_list")),
        "{archived:?}"
    );
    // ...and the visit count from earlier runs' ledgers retires busy (10), not quiet (9).
    assert_eq!(
        status(&health("visits.com").await, "busy"),
        ToolStatus::Retired
    );
    assert_eq!(
        status(&health("few.com").await, "quiet"),
        ToolStatus::Ephemeral
    );

    // 2. The ledger: tools.retire events in the reflect run (store and jsonl), and a summary.
    let events = store.ledger_events(&done.run).await.unwrap();
    let retires: Vec<String> = events
        .iter()
        .filter(|(_, e)| e.server == "core" && e.tool == "tools.retire")
        .map(|(_, e)| e.args["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(retires.len(), 2, "{retires:?}");
    assert!(retires.contains(&"stale_list".to_owned()));
    assert!(retires.contains(&"busy".to_owned()));
    let jsonl = read_jsonl(&done.folder.join("ledger.jsonl")).unwrap();
    assert!(jsonl.iter().any(|(_, e)| e.tool == "tools.retire"));
    let sweep = events
        .iter()
        .find(|(_, e)| e.tool == SWEEP_TOOL)
        .map(|(_, e)| e.clone())
        .expect("reflect.sweep event");
    assert!(sweep.ok);
    assert_eq!(sweep.args["runs_read"], json!(11));
    assert!(done.folder.join("files").join(SUMMARY_FILE).is_file());

    // 3. Proposals: one T-8 merge and one T-7 retire, appended under `proposals:` once.
    let text = std::fs::read_to_string(&proposals).unwrap();
    assert!(
        text.starts_with(blockers_head),
        "existing entries untouched"
    );
    let items: Vec<Value> = text[blockers_head.len()..]
        .lines()
        .map(|l| serde_json::from_str(l.trim().trim_start_matches("- ")).unwrap())
        .collect();
    assert_eq!(items.len(), 2, "{text}");
    let merge = items.iter().find(|i| i["kind"] == "merge").unwrap();
    assert_eq!(
        merge["tools"],
        json!(["a.com/search", "b.com/find", "c.com/lookup"])
    );
    let retire = items.iter().find(|i| i["kind"] == "retire").unwrap();
    assert_eq!(retire["tool"], json!("example.com/pinned_old"));
    assert!(done.folder.join("files/proposals.yaml").is_file());

    // 4. Health refresh: only the phase-1 reader is replayed, readers-only, scrubbed.
    let replays = readers.prompts();
    assert_eq!(replays.len(), 1);
    let p = &replays[0];
    assert!(p.contains("example.list_items"), "{p}");
    assert!(p.contains("https://shop.example.com/list"), "{p}");
    assert!(p.contains(&format!("@vault({VAULT_NAME})")), "{p}");
    assert!(!p.contains(&canary), "vault value in the replay prompt");
    assert!(!p.contains("plan_buy"), "a planner is never replayed: {p}");
    assert!(
        !p.contains("stale_list"),
        "a retired tool is never replayed"
    );
    assert_eq!(
        std::fs::read_to_string(done.folder.join("files").join(PROMPT_FILE)).unwrap(),
        *p
    );

    // 5. A second pass proposes nothing new and retires nothing more.
    let again = runner.fire(&reflect).await.unwrap();
    assert_eq!(std::fs::read_to_string(&proposals).unwrap(), text);
    let events = store.ledger_events(&again.run).await.unwrap();
    assert!(!events.iter().any(|(_, e)| e.tool == "tools.retire"));

    // 6. Every other schedule goes to the inner launcher with its own prompt.
    let mut cron = schedule(ScheduleKind::Cron, "check the weather");
    cron.id = store.schedule_add(cron.clone()).await.unwrap();
    runner.fire(&cron).await.unwrap();
    assert_eq!(inner.prompts(), ["check the weather"]);
    assert_eq!(readers.prompts().len(), 2);

    // 7. The planted vault value appears nowhere reflect wrote.
    let mut all = Vec::new();
    files_under(root, &mut all);
    for f in all.iter().filter(|f| **f != planted_jsonl) {
        let bytes = std::fs::read(f).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(&canary),
            "vault value in {}",
            f.display()
        );
    }
    for run in [&done.run, &again.run] {
        for (_, e) in store.ledger_events(run).await.unwrap() {
            let line = serde_json::to_string(&e).unwrap();
            assert!(!line.contains(&canary), "vault value in the ledger");
        }
    }
}

#[tokio::test]
async fn reflect_retires_stale_tools_on_memstore() {
    scenario(Arc::new(MemStore::new())).await;
}

#[tokio::test]
async fn reflect_retires_stale_tools_on_postgres() {
    let Some(db) = TestDb::create("reflect_retires_stale_tools_on_postgres").await else {
        return;
    };
    scenario(db.store().await).await;
}

/// With `commit: true`, the sweep's moves are committed with a T-16 message (D-560).
#[tokio::test]
async fn reflect_commits_the_sweep() {
    install_vault();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "reflect@test.invalid"]);
    git(&["config", "user.name", "reflect test"]);
    git(&["config", "commit.gpgsign", "false"]);
    let tools = root.join("tools");
    plant(
        &tools,
        "example.com",
        "stale_list",
        &reader("stale_list", "q:string", "ul li"),
    );
    plant(
        &tools,
        "example.com",
        "kept",
        &reader("kept", "q:string", "ul li"),
    );
    git(&["add", "tools"]);
    git(&["commit", "-q", "-m", "plant (T-16)"]);

    let store: Arc<dyn MemoryStore> = Arc::new(MemStore::new());
    let now = micros(ts_now());
    store
        .tool_health_set(row(
            "example.com",
            "stale_list",
            ToolStatus::Ephemeral,
            now - ChronoDuration::days(20),
            false,
        ))
        .await
        .unwrap();
    let launcher = ReflectLauncher {
        store: store.clone(),
        tools_root: tools.clone(),
        proposals_file: None,
        clock: Arc::new(FixedClock(now)),
        commit: true,
        inner: Arc::new(Recorder::default()),
        readers: Arc::new(Recorder::default()),
    };
    let runs = root.join("runs");
    let run = RunFolder::create_at(&runs, "reflect", "reflect", now).unwrap();
    let s = Schedule {
        id: 1,
        cron: "0 3 * * *".into(),
        prompt: "reflect".into(),
        kind: ScheduleKind::Reflect,
        watch_target: None,
        enabled: true,
        last_run: None,
        run_id: None,
    };
    let done = launcher
        .reflect(&LaunchRequest {
            run: &run,
            runs_root: &runs,
            prompt: "reflect",
            schedule: Some(&s),
        })
        .await
        .unwrap();
    assert_eq!(done.commit_error, None);
    let head = done.commit.expect("a commit");
    assert_eq!(git(&["rev-parse", "HEAD"]).trim(), head);
    let msg = git(&["log", "-1", "--format=%s"]);
    assert!(msg.contains("T-16"), "{msg}");
    let status = git(&["status", "--porcelain", "--", "tools"]);
    assert!(status.trim().is_empty(), "tools left uncommitted: {status}");
    assert!(done.prompt.is_none(), "nothing to replay, no session");
}
