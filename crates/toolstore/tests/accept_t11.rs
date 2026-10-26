//! docs/03 acceptance for T-11 (T-3, T-4, T-7, T-9, G-4), written by the tester.
//!
//! Everything goes through toolstore's public API with a fake `MemoryStore` and fake
//! `PageHost` / `ApiFetch` / `PlannerCheck`: no network, no Postgres. Row 1 reads the real
//! seed files under `tools/` (read only); every test that mutates files copies
//! `tests/fixtures/tools/` into a tempdir first.

use async_trait::async_trait;
use chrono::{Duration, Utc};
use harness_core::memory::*;
use harness_core::run::RunId;
use harness_core::Class;
use sb_act::ActClass;
use sb_sheet::IndexRow;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use toolstore::*;

// ---------------------------------------------------------------- fake MemoryStore

#[derive(Default)]
struct FakeStore {
    health: Mutex<Vec<ToolHealth>>,
    ledger: Mutex<Vec<LedgerEvent>>,
}

impl FakeStore {
    fn row(&self, domain: &str, name: &str) -> Option<ToolHealth> {
        self.health
            .lock()
            .unwrap()
            .iter()
            .find(|h| h.domain == domain && h.name == name)
            .cloned()
    }
    fn events(&self) -> Vec<LedgerEvent> {
        self.ledger.lock().unwrap().clone()
    }
    fn tools_logged(&self) -> Vec<String> {
        self.events().into_iter().map(|e| e.tool).collect()
    }
    async fn seed(&self, status: ToolStatus, name: &str, version: i32, runs: i32) {
        self.tool_health_set(ToolHealth {
            domain: "luma.com".into(),
            name: name.into(),
            version,
            status,
            runs,
            fails: 0,
            last_ok: None,
            last_fail: None,
            why: None,
            pinned: false,
        })
        .await
        .unwrap();
    }
}

#[async_trait]
impl MemoryStore for FakeStore {
    async fn remember(&self, _: Fact) -> Result<FactId> {
        unimplemented!()
    }
    async fn forget(&self, _: FactId) -> Result<()> {
        unimplemented!()
    }
    async fn link(&self, _: NodeId, _: &str, _: NodeId, _: Source) -> Result<()> {
        unimplemented!()
    }
    async fn node(&self, _: &str, _: &str) -> Result<NodeId> {
        unimplemented!()
    }
    async fn neighbors(&self, _: NodeId, _: Option<&str>, _: u8) -> Result<Vec<Node>> {
        unimplemented!()
    }
    async fn recall(&self, _: &str, _: usize, _: Option<Filter>) -> Result<Vec<Hit>> {
        unimplemented!()
    }
    async fn search(&self, _: &str, _: usize) -> Result<Vec<Hit>> {
        unimplemented!()
    }
    async fn contact(&self, _: &str) -> Result<Option<Contact>> {
        unimplemented!()
    }
    async fn touch_contact(&self, _: &str, _: &str, _: &str) -> Result<()> {
        unimplemented!()
    }
    async fn due_reviews(&self, _: Ts, _: usize) -> Result<Vec<Review>> {
        unimplemented!()
    }
    async fn grade_review(&self, _: ReviewId, _: u8) -> Result<()> {
        unimplemented!()
    }
    async fn baseline_get(&self, _: &str) -> Result<Option<Baseline>> {
        unimplemented!()
    }
    async fn baseline_set(&self, _: &str, _: Baseline) -> Result<()> {
        unimplemented!()
    }
    async fn append(&self, e: LedgerEvent) -> Result<LedgerId> {
        let mut l = self.ledger.lock().unwrap();
        l.push(e);
        Ok(LedgerId(l.len() as i64))
    }
    async fn tool_health(&self, domain: &str) -> Result<Vec<ToolHealth>> {
        Ok(self
            .health
            .lock()
            .unwrap()
            .iter()
            .filter(|h| h.domain == domain)
            .cloned()
            .collect())
    }
    async fn tool_health_set(&self, h: ToolHealth) -> Result<()> {
        let mut v = self.health.lock().unwrap();
        v.retain(|x| !(x.domain == h.domain && x.name == h.name));
        v.push(h);
        Ok(())
    }
    async fn gate_rule(&self, _: &str, _: &str) -> Result<Option<GateRule>> {
        unimplemented!()
    }
    async fn run_start(&self, _: Run) -> Result<()> {
        unimplemented!()
    }
    async fn run_end(&self, _: &RunId, _: RunStatus, _: Ts) -> Result<()> {
        unimplemented!()
    }
    async fn run_get(&self, _: &RunId) -> Result<Option<Run>> {
        unimplemented!()
    }
    async fn schedule_add(&self, _: Schedule) -> Result<i32> {
        unimplemented!()
    }
    async fn schedule_list(&self) -> Result<Vec<Schedule>> {
        unimplemented!()
    }
    async fn schedule_rm(&self, _: i32) -> Result<()> {
        unimplemented!()
    }
    async fn schedule_fired(&self, _: i32, _: Ts) -> Result<()> {
        unimplemented!()
    }
    async fn gate_rule_set(&self, _: &str, _: &str, _: GateRule, _: &str) -> Result<()> {
        unimplemented!()
    }
    async fn review_add(&self, _: &str, _: &str) -> Result<ReviewId> {
        unimplemented!()
    }
    async fn ledger_events(&self, _: &RunId) -> Result<Vec<(LedgerId, LedgerEvent)>> {
        unimplemented!()
    }
}

// ---------------------------------------------------------------- fixtures

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The real seed files (read only).
fn repo_tools() -> ToolStore {
    ToolStore::new(manifest().join("../../tools"))
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            fs::copy(e.path(), dst).unwrap();
        }
    }
}

/// `tests/fixtures/tools/` copied to a tempdir.
fn sandbox() -> (tempfile::TempDir, ToolStore) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tools");
    copy_dir(&manifest().join("tests/fixtures/tools"), &root);
    let ts = ToolStore::new(&root);
    (dir, ts)
}

fn row(id: u32, role: &str, name: &str, form: Option<&str>) -> IndexRow {
    serde_json::from_value(json!({"id": id, "frame": 0, "role": role, "name": name, "form": form}))
        .unwrap()
}

/// The luma registration page; `button` is the submit button's label.
fn luma_page(button: &str) -> Vec<IndexRow> {
    vec![
        row(1, "link", "Home", None),
        row(2, "textbox", "Full name", Some("f2")),
        row(3, "textbox", "Email *", Some("f2")),
        row(4, "button", button, Some("f2")),
        row(5, "textbox", "Newsletter email", Some("f9")),
    ]
}

/// Planted mismatch: the registration form is gone (an error page).
fn broken_page() -> Vec<IndexRow> {
    vec![
        row(1, "link", "Home", None),
        row(9, "heading", "Something went wrong", None),
        row(10, "textbox", "Search", None),
    ]
}

fn tool(ts: &ToolStore, host: &str, name: &str) -> Tool {
    ts.load(host, "/")
        .unwrap()
        .into_iter()
        .find(|t| t.header.name == name)
        .unwrap_or_else(|| panic!("{name} not loaded for {host}"))
}

fn res(name: &str, outcome: RunOutcome, at: Ts) -> ToolRunResult {
    ToolRunResult {
        domain: "luma.com".into(),
        name: name.into(),
        outcome,
        at,
    }
}

fn fp_miss() -> RunOutcome {
    RunOutcome::FingerprintMiss {
        why: "form#f2 not on the page".into(),
    }
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap()
}

/// Every file under `dir`, recursively, as (path, content).
fn tree(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(tree(&p));
        } else {
            out.push((p.clone(), fs::read_to_string(&p).unwrap_or_default()));
        }
    }
    out
}

// ---------------------------------------------------------------- fake page / api / planner check

struct Page {
    url: String,
    finds: Vec<(&'static str, &'static str)>,
}

impl PageHost for Page {
    fn find(&self, q: &str) -> std::result::Result<String, String> {
        self.finds
            .iter()
            .find(|(k, _)| *k == q)
            .map(|(_, v)| v.to_string())
            .ok_or_else(|| format!("find({q:?}): no match"))
    }
    fn find_all(&self, _: &str) -> Vec<String> {
        vec![]
    }
    fn text(&self, _: Option<&str>) -> String {
        String::new()
    }
    fn table(&self, _: &str) -> Value {
        Value::Null
    }
    fn url(&self) -> String {
        self.url.clone()
    }
    fn title(&self) -> String {
        "fixture".into()
    }
}

fn page_at(url: &str) -> Page {
    Page {
        url: url.into(),
        finds: vec![],
    }
}

/// Records every URL and answers with a fixed body.
struct Api {
    body: Value,
    seen: Mutex<Vec<String>>,
}

impl ApiFetch for Api {
    fn api(
        &self,
        url: &str,
        _remaining: std::time::Duration,
    ) -> std::result::Result<Value, String> {
        self.seen.lock().unwrap().push(url.to_string());
        Ok(self.body.clone())
    }
}

/// D-301 fake: returns a fixed class (or rejection) and records the program it saw.
struct Check {
    out: std::result::Result<Class, String>,
    seen: Mutex<Vec<String>>,
}

impl Check {
    fn class(c: Class) -> Check {
        Check {
            out: Ok(c),
            seen: Mutex::new(vec![]),
        }
    }
}

impl PlannerCheck for Check {
    fn check(&self, program: &str, _: &[IndexRow]) -> std::result::Result<ActClass, String> {
        self.seen.lock().unwrap().push(program.to_string());
        self.out.clone().map(|class| ActClass {
            class,
            hints: vec![],
        })
    }
}

// ================================================================ docs/03 acceptance

/// Row 1: list_jobs (@phase 1, api reader) loads for jobs.ashbyhq.com, appears in the skeleton
/// line, and returns a short job list from the Ashby posting API for org `acme`.
#[test]
fn accept_1_ashby_list_jobs_loads_announces_and_reads_api() {
    let ts = repo_tools();
    assert_eq!(ts.folder_for("jobs.ashbyhq.com"), "ashbyhq.com");

    // Host + path, and the full URL a `fetch("https://jobs.ashbyhq.com/acme")` sees.
    for path in ["/acme", "https://jobs.ashbyhq.com/acme"] {
        let report = ts.scan("jobs.ashbyhq.com", path);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        let names: Vec<&str> = report.tools.iter().map(|t| t.namespaced.as_str()).collect();
        assert_eq!(names, ["ashbyhq.list_jobs", "generic.read_table"], "{path}");
        let lj = &report.tools[0];
        assert_eq!(lj.domain, "ashbyhq.com");
        assert_eq!(lj.header.kind, ToolKind::Reader);
        assert_eq!(lj.header.phase, 1);
        assert_eq!(lj.header.gate, Class::Read);
        assert_eq!(
            skeleton_line(&report.tools).unwrap(),
            "tools for this site: ashbyhq.list_jobs(org?) · generic.read_table(id?,max?)"
        );
    }

    let lj = tool(&ts, "jobs.ashbyhq.com", "list_jobs");
    let fixture: Value = serde_json::from_str(&read(
        &manifest().join("tests/fixtures/ashby_job_board_acme.json"),
    ))
    .unwrap();
    let api = Api {
        body: fixture,
        seen: Mutex::new(vec![]),
    };
    let out = run_reader(
        &lj,
        &page_at("https://jobs.ashbyhq.com/acme"),
        &json!({}),
        Some(&api),
    )
    .unwrap();
    assert_eq!(
        api.seen.lock().unwrap().as_slice(),
        ["https://api.ashbyhq.com/posting-api/job-board/acme"]
    );
    assert_eq!(
        out,
        json!({
            "org": "acme", "total": 2, "shown": 2,
            "jobs": [
                {"title": "Senior Backend Engineer", "location": "Berlin (remote)", "department": "Engineering",
                 "url": "https://jobs.ashbyhq.com/acme/0a1b2c3d-0000-4000-8000-000000000001"},
                {"title": "Product Designer", "location": "Paris, France", "department": "Design",
                 "url": "https://jobs.ashbyhq.com/acme/0a1b2c3d-0000-4000-8000-000000000002/application"}
            ]
        })
    );
    // Short text only: no description HTML or body text reaches the model (CLAUDE.md rule 5).
    let s = out.to_string();
    for banned in [
        "FIXTURE_HTML_MARKER",
        "FIXTURE_PLAIN_MARKER",
        "<p>",
        "Internal Referral",
    ] {
        assert!(!s.contains(banned), "{banned} leaked: {s}");
    }
}

/// Row 1 (cap): a large board is cut to a short list, with the total reported.
#[test]
fn accept_1b_ashby_list_jobs_caps_a_large_board() {
    let lj = tool(&repo_tools(), "jobs.ashbyhq.com", "list_jobs");
    let jobs: Vec<Value> = (0..75)
        .map(|i| {
            json!({"title": format!("Role {i}"), "location": "Remote", "department": "Eng",
                   "isListed": true, "jobUrl": format!("https://jobs.ashbyhq.com/acme/{i}"),
                   "descriptionHtml": "<div>".repeat(200)})
        })
        .collect();
    let api = Api {
        body: json!({ "jobs": jobs }),
        seen: Mutex::new(vec![]),
    };
    let out = run_reader(
        &lj,
        &page_at("https://jobs.ashbyhq.com/acme"),
        &json!({}),
        Some(&api),
    )
    .unwrap();
    assert_eq!(out["total"], 75);
    assert_eq!(out["shown"], 60);
    assert_eq!(out["jobs"].as_array().unwrap().len(), 60);
    let s = out.to_string();
    assert!(!s.contains("<div>"));
    assert!(s.len() < 8_000, "{} bytes", s.len());
}

/// Row 2: planted fingerprint mismatch -> needs_repair (why recorded); the page fixed ->
/// relocate -> version 2, @fingerprint patched on disk, nothing else in the file changed.
#[tokio::test]
async fn accept_2_planted_mismatch_needs_repair_then_relocate_version_2() {
    let (_dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::Saved, "register", 1, 4).await;
    let path = ts.try_tool_path("luma.com", "register").unwrap();
    let original = read(&path);
    let reg = tool(&ts, "lu.ma", "register");
    assert_eq!(
        fingerprint(&reg, &luma_page("Register")),
        FingerprintResult::Match
    );

    // Planted mismatch.
    assert_eq!(fingerprint(&reg, &broken_page()), FingerprintResult::Miss);
    let h = ts
        .record(&store, res("register", fp_miss(), Utc::now()))
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::NeedsRepair, 1));
    assert!(
        h.why
            .as_deref()
            .unwrap()
            .contains("form#f2 not on the page"),
        "{:?}",
        h.why
    );
    assert_eq!(store.row("luma.com", "register").unwrap(), h);

    // The page is back with the submit button renamed: partial -> relocate.
    let fixed = luma_page("Sign up");
    let FingerprintResult::Partial { missing } = fingerprint(&reg, &fixed) else {
        panic!("expected partial")
    };
    assert_eq!(missing, parse_fingerprint(r#"button"Register""#).unwrap());
    let fp = relocate(&reg, &fixed).unwrap();
    let h = ts
        .record(
            &store,
            res(
                "register",
                RunOutcome::Relocated { fingerprint: fp },
                Utc::now(),
            ),
        )
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Repaired, 2));
    assert_eq!(store.row("luma.com", "register").unwrap().version, 2);

    // @fingerprint patched on disk; every other line identical.
    let patched = read(&path);
    assert!(patched.contains(r#"// @fingerprint form#f2 button"Sign up" input"Email""#));
    let diff: Vec<(&str, &str)> = original
        .lines()
        .zip(patched.lines())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(diff.len(), 1, "{diff:?}");
    assert!(diff[0].0.starts_with("// @fingerprint"));
    assert_eq!(original.lines().count(), patched.lines().count());
    // The reloaded tool now matches the fixed page.
    let reg2 = tool(&ts, "luma.com", "register");
    assert_eq!(fingerprint(&reg2, &fixed), FingerprintResult::Match);
    assert_eq!(store.tools_logged(), ["tools.relocate"]);
}

/// Row 2 (variant): the page fixed exactly as before -> match -> repaired, version 2 (D-304).
#[tokio::test]
async fn accept_2b_page_restored_exactly_flips_back_at_version_2() {
    let (_dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::Saved, "register", 1, 4).await;
    let reg = tool(&ts, "luma.com", "register");
    ts.record(&store, res("register", fp_miss(), Utc::now()))
        .await
        .unwrap();
    assert_eq!(
        fingerprint(&reg, &luma_page("Register")),
        FingerprintResult::Match
    );
    let h = ts
        .record(&store, res("register", RunOutcome::Ok, Utc::now()))
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Repaired, 2));
}

/// Row 3 (D-303): an ephemeral tool with last_ok 15 days old is retired and archived by the
/// reflect sweep with a ledger event; the archived file is deleted only after 90 days.
#[tokio::test]
async fn accept_3_stale_ephemeral_retired_archived_then_deleted_after_90_days() {
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    let now = Utc::now();
    let h = ts
        .register_forged(&store, "luma.com", "list_events")
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Ephemeral, 1));
    ts.record(
        &store,
        res("list_events", RunOutcome::Ok, now - Duration::days(15)),
    )
    .await
    .unwrap();
    // A fresh ephemeral next to it is left alone.
    store.seed(ToolStatus::Ephemeral, "register", 1, 1).await;
    let mut fresh = store.row("luma.com", "register").unwrap();
    fresh.last_ok = Some(now - Duration::days(2));
    store.tool_health_set(fresh).await.unwrap();

    let file = ts.try_tool_path("luma.com", "list_events").unwrap();
    let archived = ts.try_archive_path("luma.com", "list_events", 1).unwrap();
    let done = ts.sweep(&store, now, &|_| 0).await.unwrap();
    assert_eq!(done.len(), 1, "{done:?}");
    assert!(
        matches!(&done[0], SweepAction::Retired { name, archived: Some(a), why, .. }
            if name == "list_events" && *a == archived && why.contains("14 days")),
        "{done:?}"
    );
    assert!(!file.exists());
    assert!(archived.exists());
    assert_eq!(
        archived,
        dir.path().join("tools/_archive/luma.com/list_events.1.js")
    );
    let row = store.row("luma.com", "list_events").unwrap();
    assert_eq!(row.status, ToolStatus::Retired);
    assert!(row.why.unwrap().contains("unused 14 days"));
    assert_eq!(
        store.row("luma.com", "register").unwrap().status,
        ToolStatus::Ephemeral
    );

    let ev = store.events();
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(
        (ev[0].server.as_str(), ev[0].tool.as_str()),
        ("core", "tools.retire")
    );
    assert!(ev[0].ok);
    assert_eq!(ev[0].args["domain"], "luma.com");
    assert_eq!(ev[0].args["name"], "list_events");
    assert_eq!(
        ev[0].consequences["archived"],
        "_archive/luma.com/list_events.1.js"
    );
    assert!(ev[0].why.as_deref().unwrap().contains("14 days"));

    // Not deleted at 89 days; deleted at 91, with a ledger event. The fresh tool keeps being
    // used meanwhile, so the only archived file is list_events'.
    let touch = |at: Ts| {
        let mut r = store.row("luma.com", "register").unwrap();
        r.last_ok = Some(at);
        r
    };
    store
        .tool_health_set(touch(now + Duration::days(89)))
        .await
        .unwrap();
    let d89 = ts
        .sweep(&store, now + Duration::days(89), &|_| 0)
        .await
        .unwrap();
    assert!(
        !d89.iter().any(|a| matches!(a, SweepAction::Deleted { .. })),
        "{d89:?}"
    );
    assert!(archived.exists());
    store
        .tool_health_set(touch(now + Duration::days(91)))
        .await
        .unwrap();
    let d91 = ts
        .sweep(&store, now + Duration::days(91), &|_| 0)
        .await
        .unwrap();
    assert!(
        d91.iter()
            .any(|a| matches!(a, SweepAction::Deleted { path, .. } if *path == archived)),
        "{d91:?}"
    );
    assert!(!archived.exists());
    assert_eq!(
        store.tools_logged(),
        ["tools.retire", "tools.delete"],
        "{d91:?}"
    );
    let last = store.events().last().unwrap().clone();
    assert_eq!(
        last.consequences["deleted"],
        "_archive/luma.com/list_events.1.js"
    );
}

// ================================================================ T-9 invariants

/// T-9.1: a planner's output class is max(declared floor, classified), with the program the
/// runner produced going through the PlannerCheck (D-301).
#[test]
fn t9_planner_class_is_max_of_floor_and_classified() {
    let (_dir, ts) = sandbox();
    let reg = tool(&ts, "luma.com", "register");
    assert_eq!(reg.header.gate, Class::Submit);
    let host = Page {
        url: "https://lu.ma/e/abc".into(),
        finds: vec![
            ("input \"Full name\"", "2"),
            ("input \"Email\"", "3"),
            ("button \"Register\" SUBMIT", "4"),
        ],
    };
    let program = run_planner(
        &reg,
        &host,
        &json!({"name": "Ada", "email": "ada@example.org"}),
    )
    .unwrap();
    assert_eq!(program, r#"t#2="Ada"; t#3="ada@example.org"; c#4; ?s"#);
    let page = luma_page("Register");

    // classified below the floor -> floor
    let fill = Check::class(Class::Fill);
    assert_eq!(
        planner_class(&reg, &program, &page, &fill).unwrap(),
        Class::Submit
    );
    assert_eq!(*fill.seen.lock().unwrap(), [program.as_str()]);
    // classified above the floor -> classified
    assert_eq!(
        planner_class(&reg, &program, &page, &Check::class(Class::Pay)).unwrap(),
        Class::Pay
    );
    // equal
    assert_eq!(
        planner_class(&reg, &program, &page, &Check::class(Class::Submit)).unwrap(),
        Class::Submit
    );
    // A Read-floor planner still gets the classified class (the floor never caps).
    let mut low = reg.clone();
    low.header.gate = Class::Read;
    assert_eq!(
        planner_class(&low, &program, &page, &Check::class(Class::Submit)).unwrap(),
        Class::Submit
    );
    // sb-act rejecting the program (e.g. no ?s before a Submit) is an invariant error.
    let bad = Check {
        out: Err("Submit not followed by ?s (D-50)".into()),
        seen: Mutex::new(vec![]),
    };
    let e = planner_class(&reg, "c#4", &page, &bad).unwrap_err();
    assert!(
        matches!(&e, StoreError::Invariant(m) if m.contains("?s")),
        "{e}"
    );
    // A reader never yields a classified program.
    let reader = tool(&ts, "luma.com", "list_events");
    assert!(matches!(
        planner_class(&reader, "c#4", &page, &Check::class(Class::Read)),
        Err(StoreError::Invariant(_))
    ));
}

/// T-9.2: `page` (and the global scope) has no document, window, fetch; no eval or
/// string-to-code; `api` only for a phase 1 reader that was given one.
#[test]
fn t9_page_has_no_document_window_fetch() {
    let (_dir, ts) = sandbox();
    let probe = tool(&ts, "example.com", "probe");
    let api = Api {
        body: json!({}),
        seen: Mutex::new(vec![]),
    };
    let out = run_reader(
        &probe,
        &page_at("https://example.com/"),
        &json!({}),
        Some(&api),
    )
    .unwrap();
    for k in [
        "document",
        "window",
        "fetch",
        "globalFetch",
        "xhr",
        "require",
        "eval",
        "setTimeout",
        "pageDocument",
        "pageWindow",
        "pageFetch",
        "pageApi",
    ] {
        assert_eq!(out[k], "undefined", "{k}: {out}");
    }
    let keys: Vec<&str> = out["pageKeys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for banned in ["document", "window", "fetch", "api", "eval"] {
        assert!(!keys.contains(&banned), "page has {banned}: {keys:?}");
    }
    for want in ["find", "findAll", "text", "table", "url", "title"] {
        assert!(keys.contains(&want), "page lacks {want}: {keys:?}");
    }
    assert_eq!(out["pageFrozen"], true);
    assert!(
        out["fnCtor"].as_str().unwrap().starts_with("threw:"),
        "{out}"
    );
    assert!(
        out["globalFn"].as_str().unwrap().starts_with("threw:"),
        "{out}"
    );
    assert!(api.seen.lock().unwrap().is_empty());

    // Phase 1 reader: api is there, fetch/document/window still are not.
    let p1 = tool(&ts, "example.com", "probe_api");
    let out = run_reader(
        &p1,
        &page_at("https://example.com/"),
        &json!({}),
        Some(&api),
    )
    .unwrap();
    assert_eq!(out["pageApi"], "function");
    for k in ["document", "window", "fetch", "globalFetch", "pageFetch"] {
        assert_eq!(out[k], "undefined", "{k}: {out}");
    }
    let out = run_reader(&p1, &page_at("https://example.com/"), &json!({}), None).unwrap();
    assert_eq!(out["pageApi"], "undefined");
}

fn with_line(src: &str, from: &str, to: &str) -> String {
    assert!(src.contains(from), "{from:?} not in fixture");
    src.replacen(from, to, 1)
}

/// T-9.3: a repair that changes @gate or removes ?s is rejected and logged to
/// tools.health.why; the file, version and status stay as they were.
#[tokio::test]
async fn t9_repair_changing_gate_or_removing_stop_rejected_and_logged() {
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::NeedsRepair, "register", 1, 0).await;
    let path = ts.try_tool_path("luma.com", "register").unwrap();
    let old = read(&path);

    let cases = [
        (
            with_line(&old, "// @gate Submit", "// @gate Fill"),
            "@gate lowered",
        ),
        (with_line(&old, "c#${btn}; ?s`", "c#${btn}`"), "?s"),
    ];
    for (new, want) in cases {
        let e = ts
            .reforge(&store, "luma.com", "register", &new, Utc::now())
            .await
            .unwrap_err();
        assert!(
            matches!(&e, StoreError::Invariant(m) if m.contains(want)),
            "{e}"
        );
        let h = store.row("luma.com", "register").unwrap();
        let why = h.why.clone().unwrap();
        assert!(why.starts_with("rejected:") && why.contains(want), "{why}");
        assert_eq!((h.status, h.version), (ToolStatus::NeedsRepair, 1));
        assert_eq!(read(&path), old);
    }
    assert!(!dir.path().join("tools/_archive").exists());
    assert!(store.events().is_empty());

    // A relocate may change only @fingerprint: changing @gate (even raising it) is rejected.
    let raised = with_line(&old, "// @gate Submit", "// @gate Pay");
    let e = check_repair(&old, &raised, RepairKind::Relocate).unwrap_err();
    assert!(e.to_string().contains("relocate changed @gate"), "{e}");
    ts.log_invariant(&store, "luma.com", "register", &e)
        .await
        .unwrap();
    let why = store.row("luma.com", "register").unwrap().why.unwrap();
    assert!(why.contains("relocate changed @gate"), "{why}");
    // Removing ?s is rejected for a relocate too.
    let nostop = with_line(&old, "c#${btn}; ?s`", "c#${btn}`");
    assert!(check_repair(&old, &nostop, RepairKind::Relocate).is_err());
}

/// T-9.4: the loader ignores files without a valid header and reports them (D-306).
#[test]
fn t9_loader_ignores_and_reports_invalid_headers() {
    let (_dir, ts) = sandbox();
    let site = ts.root.join("luma.com");
    let bad = [
        ("no_header.js", "export default function () { return 1; }\n"),
        (
            "missing_kind.js",
            "// @name missing_kind\n// @gate Read\nexport default function () {}\n",
        ),
        (
            "bad_gate.js",
            "// @name bad_gate\n// @kind reader\n// @gate Whatever\nexport default function () {}\n",
        ),
        (
            "bad_kind.js",
            "// @name bad_kind\n// @kind writer\n// @gate Read\nexport default function () {}\n",
        ),
        (
            "wrong_name.js",
            "// @name other\n// @kind reader\n// @gate Read\nexport default function () {}\n",
        ),
        (
            "two_names.js",
            "// @name two_names\n// @name two_names\n// @kind reader\n// @gate Read\nexport default function () {}\n",
        ),
    ];
    for (f, src) in bad {
        fs::write(site.join(f), src).unwrap();
    }
    fs::write(site.join("README.txt"), "not a tool").unwrap();

    let report = ts.scan("lu.ma", "/e/abc");
    let mut names: Vec<&str> = report
        .tools
        .iter()
        .map(|t| t.header.name.as_str())
        .collect();
    names.sort();
    assert_eq!(names, ["list_events", "register"]);
    let mut skipped: Vec<String> = report
        .skipped
        .iter()
        .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    skipped.sort();
    let mut want: Vec<&str> = bad.iter().map(|(f, _)| *f).collect();
    want.sort();
    assert_eq!(skipped, want);
    for (p, why) in &report.skipped {
        assert!(!why.is_empty(), "{p:?}");
    }
    let why_of = |f: &str| {
        report
            .skipped
            .iter()
            .find(|(p, _)| p.ends_with(f))
            .unwrap()
            .1
            .clone()
    };
    assert!(why_of("missing_kind.js").contains("@kind"));
    assert!(why_of("bad_gate.js").contains("@gate"));
    assert!(why_of("wrong_name.js").contains("file name"));
    // load() is not fatal on bad files.
    assert_eq!(ts.load("lu.ma", "/e/abc").unwrap().len(), 2);
}

// ================================================================ G-4

/// G-4: a repair may not add a vault key the tool did not use; keeping an existing key is
/// fine. Vault canary: SB_VAULT_CANARY appears nowhere toolstore writes.
#[tokio::test]
async fn g4_no_new_vault_key() {
    let canary = std::env::var("SB_VAULT_CANARY").unwrap_or_else(|_| {
        format!(
            "sbcanary-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    });
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    let login = "// @name login\n// @kind planner\n// @match /login\n// @args email:string\n// @gate Account\n// @phase 2\nexport default function (page, args) {\n  const p = page.find('input \"Password\"');\n  const b = page.find('button \"Log in\" SUBMIT');\n  return `t#${p}=@vault(luma_password); c#${b}; ?s`;\n}\n";
    fs::write(ts.try_tool_path("luma.com", "login").unwrap(), login).unwrap();
    store.seed(ToolStatus::NeedsRepair, "login", 1, 0).await;
    let reg_path = ts.try_tool_path("luma.com", "register").unwrap();
    let reg_old = read(&reg_path);
    store.seed(ToolStatus::NeedsRepair, "register", 1, 0).await;

    // register used no vault key: adding one is rejected and logged by name only.
    let added = with_line(
        &reg_old,
        "t#${email}=${q(args.email)}",
        "t#${email}=@vault(luma_email)",
    );
    let e = ts
        .reforge(&store, "luma.com", "register", &added, Utc::now())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("@vault(luma_email)"), "{e}");
    let why = store.row("luma.com", "register").unwrap().why.unwrap();
    assert!(
        why.contains("vault key") && why.contains("@vault(luma_email)"),
        "{why}"
    );
    assert_eq!(read(&reg_path), reg_old);

    // login keeps its own key: accepted. A second, new key: rejected.
    let kept = login.replace("c#${b}; ?s", "c#${b};  ?s");
    let h = ts
        .reforge(&store, "luma.com", "login", &kept, Utc::now())
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Repaired, 2));
    // B-31 (3): the canary is fed in. The re-forge adds a new key, @vault(luma_otp_seed), and
    // carries that key's value (the canary) as a trace line in the new source. Only the
    // refusal keeps the value out of the tools tree: without the G-4 check the file is
    // written and the sweep below finds the canary.
    let more = kept.replace(
        "  return `t#${p}=@vault(luma_password);",
        &format!(
            "  // trace: luma_otp_seed = {canary}\n  return `t#${{p}}=@vault(luma_password); t#9=@vault(luma_otp_seed);"
        ),
    );
    assert!(more.contains(&canary) && more.contains("@vault(luma_otp_seed)"));
    let r = ts
        .reforge(&store, "luma.com", "login", &more, Utc::now())
        .await;

    // Canary sweep: health rows, ledger, every file under the tools root, the result.
    let mut haystack = format!(
        "{:?}{:?}{r:?}",
        store.health.lock().unwrap(),
        store.events()
    );
    if let Err(e) = &r {
        haystack.push_str(&e.to_string());
    }
    for (_, c) in tree(dir.path()) {
        haystack.push_str(&c);
    }
    assert!(!haystack.contains(&canary), "vault canary leaked");

    // ...and the re-forge was refused, naming the key only.
    let e = r.unwrap_err();
    assert!(
        matches!(&e, StoreError::Invariant(m) if m.contains("@vault(luma_otp_seed)")),
        "{e}"
    );
    assert_eq!(read(&ts.try_tool_path("luma.com", "login").unwrap()), kept);
    assert_eq!(
        store.row("luma.com", "login").unwrap().version,
        2,
        "no new version"
    );
}

/// G-4: @match may not be widened to another domain (scheme, host, //, ..), on re-forge or
/// on load, and a site's tools never load for another host.
#[tokio::test]
async fn g4_match_not_widened_to_another_domain() {
    let (_dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::NeedsRepair, "register", 1, 0).await;
    let path = ts.try_tool_path("luma.com", "register").unwrap();
    let old = read(&path);
    for wide in [
        "https://evil.example/*",
        "//evil.example/*",
        "evil.example/*",
        "/../../evil.example/*",
        "/* https://evil.example/*",
    ] {
        let new = with_line(&old, "// @match /*", &format!("// @match {wide}"));
        let e = ts
            .reforge(&store, "luma.com", "register", &new, Utc::now())
            .await
            .unwrap_err();
        assert!(
            matches!(&e, StoreError::Invariant(m) if m.contains("@match widened")),
            "{wide}: {e}"
        );
        let why = store.row("luma.com", "register").unwrap().why.unwrap();
        assert!(why.contains("@match widened"), "{wide}: {why}");
        assert_eq!(read(&path), old, "{wide}");
        assert_eq!(store.row("luma.com", "register").unwrap().version, 1);
    }
    // A file already carrying a cross-domain @match is not loaded.
    fs::write(
        ts.try_tool_path("luma.com", "wide").unwrap(),
        "// @name wide\n// @kind reader\n// @match https://evil.example/*\n// @gate Read\nexport default function () { return {}; }\n",
    )
    .unwrap();
    let report = ts.scan("luma.com", "/");
    assert!(report.tools.iter().all(|t| t.header.name != "wide"));
    assert!(report
        .skipped
        .iter()
        .any(|(p, why)| p.ends_with("wide.js") && why.contains("@match")));
    // Site scoping: luma tools never load for other hosts.
    for host in ["evil.example", "luma.com.evil.example", "notluma.com"] {
        assert!(ts.load(host, "/").unwrap().is_empty(), "{host}");
    }
}

// ================================================================ T-7 lifecycle

#[tokio::test]
async fn t7_ephemeral_to_saved_after_three_clean_runs() {
    let (_dir, ts) = sandbox();
    let store = FakeStore::default();
    let h = ts
        .register_forged(&store, "luma.com", "list_events")
        .await
        .unwrap();
    assert_eq!(h.status, ToolStatus::Ephemeral);
    async fn ok(ts: &ToolStore, s: &FakeStore) -> ToolHealth {
        ts.record(s, res("list_events", RunOutcome::Ok, Utc::now()))
            .await
            .unwrap()
    }
    ok(&ts, &store).await;
    let h = ok(&ts, &store).await;
    assert_eq!((h.status, h.runs), (ToolStatus::Ephemeral, 2));
    // A failure resets the clean streak (D-302).
    let h = ts
        .record(
            &store,
            res(
                "list_events",
                RunOutcome::Failed {
                    why: "threw".into(),
                },
                Utc::now(),
            ),
        )
        .await
        .unwrap();
    assert_eq!((h.status, h.runs), (ToolStatus::Ephemeral, 0));
    ok(&ts, &store).await;
    let h = ok(&ts, &store).await;
    assert_eq!((h.status, h.runs), (ToolStatus::Ephemeral, 2));
    let h = ok(&ts, &store).await;
    assert_eq!((h.status, h.runs, h.version), (ToolStatus::Saved, 3, 1));
}

#[tokio::test]
async fn t7_ephemeral_retired_after_ten_site_visits_unused() {
    let (_dir, ts) = sandbox();
    let store = FakeStore::default();
    ts.register_forged(&store, "luma.com", "list_events")
        .await
        .unwrap();
    let now = Utc::now();
    assert!(ts.sweep(&store, now, &|_| 9).await.unwrap().is_empty());
    let done = ts.sweep(&store, now, &|_| 10).await.unwrap();
    assert!(
        matches!(&done[..], [SweepAction::Retired { why, .. }] if why.contains("10 site visits")),
        "{done:?}"
    );
    assert_eq!(store.tools_logged(), ["tools.retire"]);
}

#[tokio::test]
async fn t7_saved_to_needs_repair_on_fingerprint_miss_and_expects_miss() {
    let (_dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::Saved, "register", 1, 6).await;
    store.seed(ToolStatus::Saved, "list_events", 1, 6).await;
    let h = ts
        .record(&store, res("register", fp_miss(), Utc::now()))
        .await
        .unwrap();
    assert_eq!((h.status, h.fails, h.runs), (ToolStatus::NeedsRepair, 1, 0));
    assert!(h.why.unwrap().starts_with("fingerprint miss"));
    let h = ts
        .record(
            &store,
            res(
                "list_events",
                RunOutcome::ExpectsMiss {
                    why: "no navigation to /thanks".into(),
                },
                Utc::now(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(h.status, ToolStatus::NeedsRepair);
    assert!(h
        .why
        .unwrap()
        .contains("expects miss: no navigation to /thanks"));
    // A plain failure is not a miss: status stays saved.
    store.seed(ToolStatus::Saved, "other", 1, 6).await;
    let h = ts
        .record(
            &store,
            res("other", RunOutcome::Failed { why: "x".into() }, Utc::now()),
        )
        .await
        .unwrap();
    assert_eq!(h.status, ToolStatus::Saved);
}

#[tokio::test]
async fn t7_needs_repair_to_repaired_by_relocate_and_by_reforge() {
    // Relocate.
    let (_dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::NeedsRepair, "register", 1, 0).await;
    let reg = tool(&ts, "luma.com", "register");
    let fp = relocate(&reg, &luma_page("Sign up")).unwrap();
    let h = ts
        .record(
            &store,
            res(
                "register",
                RunOutcome::Relocated { fingerprint: fp },
                Utc::now(),
            ),
        )
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Repaired, 2));

    // Re-forge from a successful trace, same floor.
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::NeedsRepair, "register", 1, 0).await;
    let old = read(&ts.try_tool_path("luma.com", "register").unwrap());
    // The trace found the name field under a new label; body changes, header and ?s do not.
    let new = with_line(&old, "input \"Full name\"", "input \"Name\"");
    let h = ts
        .reforge(&store, "luma.com", "register", &new, Utc::now())
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Repaired, 2));
    assert_eq!(
        read(&dir.path().join("tools/_archive/luma.com/register.1.js")),
        old
    );
    assert_eq!(
        read(&ts.try_tool_path("luma.com", "register").unwrap()),
        new
    );
    assert_eq!(store.tools_logged(), ["tools.reforge"]);
}

#[tokio::test]
async fn t7_repaired_to_saved_after_three_clean_runs() {
    let (_dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::NeedsRepair, "register", 1, 0).await;
    let reg = tool(&ts, "luma.com", "register");
    let fp = relocate(&reg, &luma_page("Sign up")).unwrap();
    ts.record(
        &store,
        res(
            "register",
            RunOutcome::Relocated { fingerprint: fp },
            Utc::now(),
        ),
    )
    .await
    .unwrap();
    for i in 1..=3 {
        let h = ts
            .record(&store, res("register", RunOutcome::Ok, Utc::now()))
            .await
            .unwrap();
        let want = if i < 3 {
            ToolStatus::Repaired
        } else {
            ToolStatus::Saved
        };
        assert_eq!((h.status, h.version, h.runs), (want, 2, i), "run {i}");
    }
}

#[tokio::test]
async fn t7_needs_repair_to_retired_at_five_misses() {
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::Saved, "register", 1, 3).await;
    let file = ts.try_tool_path("luma.com", "register").unwrap();
    for i in 1..=4 {
        let h = ts
            .record(&store, res("register", fp_miss(), Utc::now()))
            .await
            .unwrap();
        assert_eq!((h.status, h.fails), (ToolStatus::NeedsRepair, i));
        assert!(file.exists());
    }
    let h = ts
        .record(&store, res("register", fp_miss(), Utc::now()))
        .await
        .unwrap();
    assert_eq!((h.status, h.fails), (ToolStatus::Retired, 5));
    assert!(h.why.unwrap().contains("5 misses"));
    assert!(!file.exists());
    assert!(dir
        .path()
        .join("tools/_archive/luma.com/register.1.js")
        .exists());
    assert_eq!(store.tools_logged(), ["tools.retire"]);
}

#[tokio::test]
async fn t7_retired_to_archive_to_delete() {
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    // Retired (superseded) with its file still in place: the sweep archives it.
    store.seed(ToolStatus::Retired, "register", 3, 0).await;
    let now = Utc::now();
    let done = ts.sweep(&store, now, &|_| 0).await.unwrap();
    let archived = dir.path().join("tools/_archive/luma.com/register.3.js");
    assert!(
        matches!(&done[..], [SweepAction::Archived { version: 3, to, .. }] if *to == archived),
        "{done:?}"
    );
    assert!(!ts.try_tool_path("luma.com", "register").unwrap().exists());
    assert!(archived.exists());
    // Not deleted before 90 days.
    let d = ts
        .sweep(&store, now + Duration::days(60), &|_| 0)
        .await
        .unwrap();
    assert!(d.is_empty(), "{d:?}");
    let d = ts
        .sweep(
            &store,
            now + Duration::days(90) + Duration::hours(1),
            &|_| 0,
        )
        .await
        .unwrap();
    assert!(
        matches!(&d[..], [SweepAction::Deleted { path, .. }] if *path == archived),
        "{d:?}"
    );
    assert!(!archived.exists());
    assert_eq!(store.tools_logged(), ["tools.archive", "tools.delete"]);
}

#[tokio::test]
async fn t7_pinned_never_auto_retired_or_deleted() {
    // Pinned by the user (row flag): misses never retire it.
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::Saved, "register", 1, 3).await;
    ts.set_pinned(&store, "luma.com", "register", true)
        .await
        .unwrap();
    for _ in 0..8 {
        ts.record(&store, res("register", fp_miss(), Utc::now()))
            .await
            .unwrap();
    }
    let h = store.row("luma.com", "register").unwrap();
    assert_eq!(
        (h.status, h.fails, h.pinned),
        (ToolStatus::NeedsRepair, 8, true)
    );
    assert!(ts.try_tool_path("luma.com", "register").unwrap().exists());

    // Pinned tool's re-forge archives its old version; that archive is never deleted, and a
    // re-forge that raises the gate class asks first.
    let old = read(&ts.try_tool_path("luma.com", "register").unwrap());
    let new = with_line(&old, "// @gate Submit", "// @gate Pay");
    let h = ts
        .reforge(&store, "luma.com", "register", &new, Utc::now())
        .await
        .unwrap();
    assert_eq!(h.status, ToolStatus::Ask);
    let archived = dir.path().join("tools/_archive/luma.com/register.1.js");
    assert!(archived.exists());
    let now = Utc::now();
    let d = ts
        .sweep(&store, now + Duration::days(400), &|_| 100)
        .await
        .unwrap();
    assert!(d.is_empty(), "{d:?}");
    assert!(archived.exists());

    // Pinned through _site.yaml: a stale ephemeral is never swept.
    let (_dir2, ts2) = sandbox();
    let store2 = FakeStore::default();
    fs::write(
        ts2.root.join("luma.com/_site.yaml"),
        "aliases: [lu.ma]\npinned: [list_events]\n",
    )
    .unwrap();
    let h = ts2
        .register_forged(&store2, "luma.com", "list_events")
        .await
        .unwrap();
    assert!(h.pinned);
    ts2.record(
        &store2,
        res("list_events", RunOutcome::Ok, now - Duration::days(100)),
    )
    .await
    .unwrap();
    let d = ts2.sweep(&store2, now, &|_| 100).await.unwrap();
    assert!(d.is_empty(), "{d:?}");
    assert!(ts2
        .try_tool_path("luma.com", "list_events")
        .unwrap()
        .exists());
    assert!(store2.events().is_empty());
}

#[tokio::test]
async fn t7_reforge_with_raised_floor_asks_then_confirm() {
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::NeedsRepair, "register", 1, 0).await;
    let old = read(&ts.try_tool_path("luma.com", "register").unwrap());
    // A payment step appeared: floor raised Submit -> Pay, ?s kept.
    let new = with_line(&old, "// @gate Submit", "// @gate Pay");
    let h = ts
        .reforge(&store, "luma.com", "register", &new, Utc::now())
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Ask, 2));
    let why = h.why.unwrap();
    assert!(why.contains("Submit") && why.contains("Pay"), "{why}");
    assert_eq!(
        read(&ts.try_tool_path("luma.com", "register").unwrap()),
        new
    );
    assert_eq!(
        read(&dir.path().join("tools/_archive/luma.com/register.1.js")),
        old
    );
    assert_eq!(tool(&ts, "luma.com", "register").header.gate, Class::Pay);
    assert_eq!(store.tools_logged(), ["tools.reforge"]);
    // Still ask until the user confirms.
    assert_eq!(
        store.row("luma.com", "register").unwrap().status,
        ToolStatus::Ask
    );
    let h = ts.confirm(&store, "luma.com", "register").await.unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Repaired, 2));
    assert_eq!(store.row("luma.com", "register").unwrap(), h);
    // confirm is a no-op on anything that is not ask.
    let again = ts.confirm(&store, "luma.com", "register").await.unwrap();
    assert_eq!(again, h);
}

/// G-4 (D-29): a re-forge that builds `@vault(` by string concatenation must not get a new
/// vault key through. Either the re-forge is rejected, or the program it emits is rejected
/// when classified (review-T-11-core blocking 3, D-309).
#[tokio::test]
async fn g4_vault_key_built_by_concatenation_is_rejected() {
    let (_dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::NeedsRepair, "register", 1, 0).await;
    let old = read(&ts.try_tool_path("luma.com", "register").unwrap());
    let sneaky = with_line(
        &old,
        "t#${email}=${q(args.email)}; c#${btn}; ?s`",
        "t#${email}=@vau` + `lt(luma_email); c#${btn}; ?s`",
    );
    if ts
        .reforge(&store, "luma.com", "register", &sneaky, Utc::now())
        .await
        .is_err()
    {
        return; // rejected at re-forge time: the invariant holds.
    }
    let reg = tool(&ts, "luma.com", "register");
    let host = Page {
        url: "https://lu.ma/e/abc".into(),
        finds: vec![
            ("input \"Full name\"", "2"),
            ("input \"Email\"", "3"),
            ("button \"Register\" SUBMIT", "4"),
        ],
    };
    let program = run_planner(&reg, &host, &json!({"name": "Ada", "email": "x"})).unwrap();
    assert!(program.contains("@vault(luma_email)"), "{program}");
    let r = planner_class(
        &reg,
        &program,
        &luma_page("Register"),
        &Check::class(Class::Submit),
    );
    assert!(
        matches!(r, Err(StoreError::Invariant(_))),
        "re-forge accepted and its program {program:?} classified as {r:?}: a vault key the \
         tool never used reached an act program"
    );
}

/// G-4 (D-307): `@match` spellings that a URL parser reads as another host are rejected:
/// `/\host` (WHATWG treats `\` as `/`) and percent-encoded `.` and `/`.
#[tokio::test]
async fn g4_match_backslash_and_percent_encoded_host_rejected() {
    let (_dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::NeedsRepair, "register", 1, 0).await;
    let old = read(&ts.try_tool_path("luma.com", "register").unwrap());
    let mut accepted = Vec::new();
    for wide in [
        r"/\evil.example/*",
        "/%2e%2e/%2f%2fevil.example/*",
        "/%2F%2Fevil.example/*",
    ] {
        let new = with_line(&old, "// @match /*", &format!("// @match {wide}"));
        if check_repair(&old, &new, RepairKind::Reforge).is_ok() {
            accepted.push(wide);
        }
    }
    assert!(accepted.is_empty(), "accepted as @match: {accepted:?}");
}

/// T-5 / T-7: after a re-forge raises the floor the status is `ask` until the user confirms;
/// no other path leaves `ask` (review-T-11-core blocking 4, D-310).
#[tokio::test]
async fn t7_ask_held_until_confirm() {
    async fn asked() -> (tempfile::TempDir, ToolStore, FakeStore, String) {
        let (dir, ts) = sandbox();
        let store = FakeStore::default();
        store.seed(ToolStatus::NeedsRepair, "register", 1, 0).await;
        let old = read(&ts.try_tool_path("luma.com", "register").unwrap());
        let new = with_line(&old, "// @gate Submit", "// @gate Pay");
        let h = ts
            .reforge(&store, "luma.com", "register", &new, Utc::now())
            .await
            .unwrap();
        assert_eq!(h.status, ToolStatus::Ask);
        (dir, ts, store, new)
    }
    let mut left = Vec::new();

    // (a) a second re-forge at the same raised floor
    let (_d, ts, store, pay) = asked().await;
    let again = with_line(&pay, "input \"Full name\"", "input \"Name\"");
    let h = ts
        .reforge(&store, "luma.com", "register", &again, Utc::now())
        .await
        .unwrap();
    if h.status != ToolStatus::Ask {
        left.push(format!("second reforge -> {:?}", h.status));
    }

    // (b) a relocate
    let (_d, ts, store, _) = asked().await;
    let reg = tool(&ts, "luma.com", "register");
    let fp = relocate(&reg, &luma_page("Sign up")).unwrap();
    let h = ts
        .record(
            &store,
            res(
                "register",
                RunOutcome::Relocated { fingerprint: fp },
                Utc::now(),
            ),
        )
        .await
        .unwrap();
    if h.status != ToolStatus::Ask {
        left.push(format!("relocate -> {:?}", h.status));
    }

    // (c) a miss, then a clean run
    let (_d, ts, store, _) = asked().await;
    ts.record(&store, res("register", fp_miss(), Utc::now()))
        .await
        .unwrap();
    let h = ts
        .record(&store, res("register", RunOutcome::Ok, Utc::now()))
        .await
        .unwrap();
    if h.status != ToolStatus::Ask {
        left.push(format!("miss then ok -> {:?}", h.status));
    }

    // (d) clean runs alone
    let (_d, ts, store, _) = asked().await;
    for _ in 0..4 {
        ts.record(&store, res("register", RunOutcome::Ok, Utc::now()))
            .await
            .unwrap();
    }
    let h = store.row("luma.com", "register").unwrap();
    if h.status != ToolStatus::Ask {
        left.push(format!("4 clean runs -> {:?}", h.status));
    }

    assert!(left.is_empty(), "ask left without confirm(): {left:?}");
}

// ================================================================ re-run additions (tester-2)
//
// D-310 path safety, the git pathspec escape (review-T-11-core blocking 1 and 2) and the D-308
// runner budget and output cap. These went in scope once 4a61935 and f67a09f landed.

/// Domains that are not one safe folder under the tools root.
const HOSTILE_DOMAINS: &[&str] = &[
    "",
    ".",
    "..",
    "../outside",
    "..\\outside",
    "luma.com/../../outside",
    "luma.com\\..\\..\\outside",
    "/etc",
    "C:\\Windows",
    "c:",
    "c:outside",
    "\\\\evil\\share",
    "//evil/share",
    "_archive",
    ".git",
    ":/",
    ":(top)",
    "luma.com:x",
    "con",
    "LUMA.com/..",
];

/// Tool names that are not one safe file name.
const HOSTILE_NAMES: &[&str] = &[
    "",
    ".",
    "..",
    "../../escape",
    "..\\..\\escape",
    "a/b",
    "a\\b",
    "/escape",
    "C:\\escape",
    "x:y",
    ":/escape",
    "register.js",
    "con",
    "../luma.com/register",
    "../../outside/register",
];

/// Bait files where a hostile id would land if it were joined under the root.
fn plant_bait(dir: &Path) {
    fs::write(dir.join("escape.js"), "bait").unwrap();
    fs::create_dir_all(dir.join("outside")).unwrap();
    fs::write(dir.join("outside/register.js"), "bait").unwrap();
}

fn sorted_tree(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut t = tree(dir);
    t.sort();
    t
}

/// Only plain components below `root`.
fn strictly_under(p: &Path, root: &Path) -> bool {
    p.strip_prefix(root).is_ok_and(|rel| {
        rel.components().count() > 0
            && rel
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
    })
}

/// D-310: record, reforge and every other mutator refuse a domain or name that is not a single
/// safe path component, with StoreError::BadPath, and touch neither the disk nor the store.
#[tokio::test]
async fn d310_hostile_domain_or_name_refused_by_record_and_reforge() {
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    plant_bait(dir.path());
    let before = sorted_tree(dir.path());
    let good_src = read(&ts.try_tool_path("luma.com", "register").unwrap());
    let mut pairs: Vec<(&str, &str)> = HOSTILE_DOMAINS.iter().map(|d| (*d, "register")).collect();
    pairs.extend(HOSTILE_NAMES.iter().map(|n| ("luma.com", *n)));
    let bad =
        |r: &std::result::Result<ToolHealth, StoreError>| matches!(r, Err(StoreError::BadPath(_)));
    for (d, n) in pairs {
        for outcome in [
            RunOutcome::Ok,
            RunOutcome::Relocated {
                fingerprint: vec![],
            },
            fp_miss(),
        ] {
            let r = ts
                .record(
                    &store,
                    ToolRunResult {
                        domain: d.into(),
                        name: n.into(),
                        outcome: outcome.clone(),
                        at: Utc::now(),
                    },
                )
                .await;
            assert!(bad(&r), "record({d:?}, {n:?}, {outcome:?}) = {r:?}");
        }
        let r = ts.reforge(&store, d, n, &good_src, Utc::now()).await;
        assert!(bad(&r), "reforge({d:?}, {n:?}) = {r:?}");
        let r = ts.register_forged(&store, d, n).await;
        assert!(bad(&r), "register_forged({d:?}, {n:?}) = {r:?}");
        let r = ts.confirm(&store, d, n).await;
        assert!(bad(&r), "confirm({d:?}, {n:?}) = {r:?}");
        let r = ts.set_pinned(&store, d, n, true).await;
        assert!(bad(&r), "set_pinned({d:?}, {n:?}) = {r:?}");
        let r = ts
            .log_invariant(&store, d, n, &StoreError::Invariant("x".into()))
            .await;
        assert!(
            matches!(r, Err(StoreError::BadPath(_))),
            "log_invariant({d:?}, {n:?}) = {r:?}"
        );
        let r = ts.archive(d, n, 1);
        assert!(
            matches!(r, Err(StoreError::BadPath(_))),
            "archive({d:?}, {n:?}) = {r:?}"
        );
        assert!(
            matches!(ts.try_tool_path(d, n), Err(StoreError::BadPath(_))),
            "try_tool_path({d:?}, {n:?})"
        );
        assert!(
            matches!(ts.try_archive_path(d, n, 1), Err(StoreError::BadPath(_))),
            "try_archive_path({d:?}, {n:?})"
        );
    }
    assert_eq!(sorted_tree(dir.path()), before, "files changed");
    assert!(store.health.lock().unwrap().is_empty(), "rows written");
    assert!(store.events().is_empty(), "ledger written");
}

/// D-310: sweep never acts on a row whose name is not a safe file name (a poisoned row in
/// the store), never visits a domain that is not a folder under the root, and still does its
/// normal work for the good rows next to them.
#[tokio::test]
async fn d310_sweep_never_acts_on_a_hostile_row() {
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    let now = Utc::now();
    plant_bait(dir.path());
    let register_src = read(&ts.try_tool_path("luma.com", "register").unwrap());
    // The good row: a stale ephemeral, which sweep retires.
    ts.register_forged(&store, "luma.com", "list_events")
        .await
        .unwrap();
    ts.record(
        &store,
        res("list_events", RunOutcome::Ok, now - Duration::days(15)),
    )
    .await
    .unwrap();
    let raw = |domain: &str, name: &str, status: ToolStatus| ToolHealth {
        domain: domain.into(),
        name: name.into(),
        version: 1,
        status,
        runs: 0,
        fails: 7,
        last_ok: Some(now - Duration::days(100)),
        last_fail: Some(now - Duration::days(100)),
        why: None,
        pinned: false,
    };
    let mut planted = Vec::new();
    for n in HOSTILE_NAMES.iter().filter(|n| !n.is_empty()) {
        // A stale ephemeral (retire + archive) and a retired row in place (archive).
        planted.push(raw("luma.com", n, ToolStatus::Ephemeral));
        planted.push(raw("example.com", n, ToolStatus::Retired));
    }
    for d in HOSTILE_DOMAINS {
        planted.push(raw(d, "register", ToolStatus::Retired));
        planted.push(raw(d, "escape", ToolStatus::Ephemeral));
    }
    for h in &planted {
        store.tool_health_set(h.clone()).await.unwrap();
    }

    let done = ts.sweep(&store, now, &|_| 100).await;
    let done = done.unwrap_or_else(|e| panic!("sweep failed on a hostile row: {e}"));
    assert_eq!(done.len(), 1, "{done:?}");
    assert!(
        matches!(&done[0], SweepAction::Retired { name, .. } if name == "list_events"),
        "{done:?}"
    );
    // Bait and the real register.js are untouched; the archive holds only list_events.
    assert_eq!(read(&dir.path().join("escape.js")), "bait");
    assert_eq!(read(&dir.path().join("outside/register.js")), "bait");
    assert_eq!(
        read(&ts.try_tool_path("luma.com", "register").unwrap()),
        register_src
    );
    let archived: Vec<PathBuf> = tree(&ts.root.join(ARCHIVE))
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    assert_eq!(
        archived,
        vec![ts.try_archive_path("luma.com", "list_events", 1).unwrap()],
        "archive"
    );
    for (p, _) in tree(dir.path()) {
        assert!(
            strictly_under(&p, &ts.root)
                || p == dir.path().join("escape.js")
                || p == dir.path().join("outside/register.js"),
            "unexpected file {p:?}"
        );
    }
    // Hostile rows are left exactly as planted; one ledger event, for list_events.
    for h in &planted {
        assert_eq!(
            store.row(&h.domain, &h.name).as_ref(),
            Some(h),
            "row changed"
        );
    }
    let ev = store.events();
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0].args["name"], "list_events");
}

/// D-310: folder_for of a hostile host is "" or one safe folder name; nothing built from it
/// (tool_path, archive_path, load) points outside the root, even with a bait folder next to
/// the root and a `_site.yaml` whose aliases try to escape.
#[test]
fn d310_folder_for_hostile_host_never_leaves_root() {
    let (dir, ts) = sandbox();
    let root = ts.root.clone();
    let bait = dir.path().join("evil.com");
    fs::create_dir_all(&bait).unwrap();
    fs::copy(
        manifest().join("tests/fixtures/tools/example.com/probe.js"),
        bait.join("probe.js"),
    )
    .unwrap();
    fs::create_dir_all(root.join("aliasbait.com")).unwrap();
    fs::write(
        root.join("aliasbait.com").join(SITE_FILE),
        "aliases: [\"../evil.com\", \"..\", \"c:\", \"/etc\", \"\\\\\\\\evil\\\\share\"]\n",
    )
    .unwrap();
    let hosts = [
        "",
        ".",
        "..",
        "../..",
        "../evil.com",
        "..\\evil.com",
        "../../etc",
        "/etc/passwd",
        "C:\\Windows\\System32",
        "C:",
        "c:/windows",
        "c:evil.com",
        "\\\\evil\\share",
        "//evil/share",
        "file:///etc/passwd",
        "file://../evil.com",
        "http://../evil.com/x",
        "https://user@../evil.com",
        "luma.com\\..\\..\\evil.com",
        "evil.com\\..\\..",
        "_archive",
        "_ARCHIVE",
        ".git",
        "%2e%2e",
        "%2e%2e%2fevil.com",
        "..%2fevil.com",
        "[::1]",
        "a:b",
        "con",
        "aux.com",
        "\u{fc}n\u{ef}code.com",
        "x\u{0}y",
        " .. ",
        "tools/../../evil.com",
        "../tools/luma.com",
        ":/",
        ":(top)evil.com",
    ];
    for h in hosts {
        let f = ts.folder_for(h);
        assert!(f.is_empty() || valid_domain(&f), "{h:?} -> {f:?}");
        assert!(
            !f.contains(['/', '\\', ':', '\0'])
                && f != "."
                && f != ".."
                && !f.starts_with('.')
                && f != ARCHIVE,
            "{h:?} -> {f:?}"
        );
        if !f.is_empty() {
            assert_eq!(
                root.join(&f).parent(),
                Some(root.as_path()),
                "{h:?} -> {f:?}"
            );
        }
        if let Ok(tp) = ts.try_tool_path(&f, "x") {
            assert!(strictly_under(&tp, &root), "{h:?} -> {f:?} -> {tp:?}");
        }
        if let Ok(ap) = ts.try_archive_path(&f, "x", 1) {
            assert!(strictly_under(&ap, &root), "{h:?} -> {f:?} -> {ap:?}");
        }
        for t in ts.load(h, "/").unwrap() {
            assert!(strictly_under(&t.path, &root), "{h:?} loaded {:?}", t.path);
        }
    }
    // Normal hosts still map.
    assert_eq!(ts.folder_for("https://luma.com/x"), "luma.com");
    assert_eq!(ts.folder_for("LUMA.com"), "luma.com");
    assert_eq!(ts.folder_for("_generic"), GENERIC);
}

fn git_out(repo: &Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git must be installed for this test (it is not skipped)");
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// D-310: git_commit with pathspec magic (`:/`, `:(top)`, `:(glob)`, `:!`), globs, `..`
/// and absolute paths outside the root commits nothing outside tools/, including files that
/// are modified, untracked or already staged at the repo root.
#[test]
fn d310_git_commit_pathspec_magic_commits_nothing_outside_tools() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git_out(repo, &["init", "-q"]);
    git_out(repo, &["config", "user.email", "t@example.invalid"]);
    git_out(repo, &["config", "user.name", "toolstore tester"]);
    git_out(repo, &["config", "commit.gpgsign", "false"]);
    git_out(repo, &["config", "core.autocrlf", "false"]);
    let root = repo.join("tools");
    copy_dir(&manifest().join("tests/fixtures/tools"), &root);
    fs::write(repo.join("README.md"), "v1\n").unwrap();
    git_out(repo, &["add", "-A"]);
    git_out(repo, &["commit", "-q", "-m", "base"]);
    let base = git_out(repo, &["rev-parse", "HEAD"]).trim().to_string();

    // Outside changes in every state, and one change inside tools/.
    fs::write(repo.join("README.md"), "v2 outside\n").unwrap();
    fs::write(repo.join("secret.txt"), "untracked outside\n").unwrap();
    fs::write(repo.join("staged.txt"), "staged outside\n").unwrap();
    git_out(repo, &["add", "staged.txt"]);
    let reg = root.join("luma.com/register.js");
    let reg_src = read(&reg);
    fs::write(&reg, format!("{reg_src}// edit 1\n")).unwrap();

    let ts = ToolStore::new(&root);
    let hostile: Vec<Vec<PathBuf>> = vec![
        vec![":/".into()],
        vec![":/README.md".into()],
        vec![":/secret.txt".into()],
        vec![":(top)README.md".into()],
        vec![":(top)".into()],
        vec![":(glob)**".into()],
        vec![":(top,glob)*".into()],
        vec![":(top,icase)readme.md".into()],
        vec![":!luma.com".into()],
        vec![":^luma.com".into()],
        vec!["*".into()],
        vec!["**".into()],
        vec!["../README.md".into()],
        vec!["../*".into()],
        vec!["luma.com/../../secret.txt".into()],
        vec![repo.join("README.md")],
        vec![repo.join("staged.txt")],
        vec![root.join("../secret.txt")],
        vec![root.join("..")],
        vec![repo.to_path_buf()],
        vec![reg.clone(), ":/".into()],
        vec![reg.clone(), "../README.md".into()],
        vec![reg.clone(), repo.join("secret.txt")],
    ];
    let mut oks = Vec::new();
    for paths in &hostile {
        let r = ts.git_commit(paths, "tools: pathspec probe (T-11)");
        if r.is_ok() {
            oks.push(paths.clone());
        }
        let changed = git_out(repo, &["diff", "--name-only", &base, "HEAD"]);
        let outside: Vec<&str> = changed
            .lines()
            .filter(|l| !l.starts_with("tools/"))
            .collect();
        assert!(
            outside.is_empty(),
            "{paths:?} -> {r:?} committed {outside:?}"
        );
    }
    // Every hostile list is refused; nothing was committed at all.
    assert!(oks.is_empty(), "accepted: {oks:?}");
    assert_eq!(git_out(repo, &["rev-parse", "HEAD"]).trim(), base);
    let status = git_out(repo, &["status", "--porcelain"]);
    assert!(status.contains(" M README.md"), "{status}");
    assert!(status.contains("?? secret.txt"), "{status}");
    assert!(status.contains("A  staged.txt"), "{status}");

    // Control: the same setup commits a tools path, and only that path.
    ts.git_commit(std::slice::from_ref(&reg), "tools: edit (T-11)")
        .unwrap();
    let files = git_out(repo, &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(files.trim(), "tools/luma.com/register.js");
    let status = git_out(repo, &["status", "--porcelain"]);
    assert!(status.contains(" M README.md"), "{status}");
    assert!(status.contains("?? secret.txt"), "{status}");
    assert!(status.contains("A  staged.txt"), "{status}");
}

// ---------------------------------------------------------------- D-308 runner budget and cap

fn mem_tool(src: &str) -> Tool {
    let header = parse_header(src).unwrap();
    Tool {
        domain: "example.com".into(),
        namespaced: format!("example.{}", header.name),
        path: PathBuf::from("example.com/mem.js"),
        header,
        source: src.into(),
    }
}

/// A host whose api call ignores its budget and comes back `extra` after it ran out.
struct SlowApi {
    budgets: Mutex<Vec<std::time::Duration>>,
    extra: std::time::Duration,
}

impl ApiFetch for SlowApi {
    fn api(
        &self,
        _url: &str,
        remaining: std::time::Duration,
    ) -> std::result::Result<Value, String> {
        self.budgets.lock().unwrap().push(remaining);
        // Test fake for a slow network host: overrunning `remaining` is the point, so there
        // is no condition to wait on.
        std::thread::sleep(remaining + self.extra);
        Ok(json!({"jobs": []}))
    }
}

const SLOW_CAUGHT: &str = "// @name slow_caught
// @kind reader
// @match /*
// @gate Read
// @phase 1
export default function (page) {
  let first = 'none';
  let second = 'none';
  try { page.api('https://api.example.com/a'); first = 'ok'; } catch (e) { first = 'caught'; }
  try { page.api('https://api.example.com/b'); second = 'ok'; } catch (e) { second = 'caught'; }
  return { first, second };
}
";

const SLOW_UNCAUGHT: &str = "// @name slow_uncaught
// @kind reader
// @match /*
// @gate Read
// @phase 1
export default function (page) {
  return page.api('https://api.example.com/a');
}
";

/// D-308: an ApiFetch slower than its `remaining` budget makes the run RunError::Timeout,
/// whether or not the tool catches it, and a spent budget never reaches the host again.
#[test]
fn d308_api_slower_than_remaining_is_timeout() {
    let timeout = std::time::Duration::from_millis(300);
    let opts = RunOpts {
        timeout,
        ..RunOpts::default()
    };
    let page = page_at("https://jobs.example.com/acme");
    for src in [SLOW_CAUGHT, SLOW_UNCAUGHT] {
        let t = mem_tool(src);
        let api = SlowApi {
            budgets: Mutex::new(vec![]),
            extra: std::time::Duration::from_millis(100),
        };
        let r = run_reader_with(&t, &page, &json!({}), Some(&api), &opts);
        assert_eq!(r, Err(RunError::Timeout(timeout)), "{}", t.header.name);
        let budgets = api.budgets.lock().unwrap().clone();
        assert_eq!(
            budgets.len(),
            1,
            "{}: host calls {budgets:?}",
            t.header.name
        );
        assert!(
            !budgets[0].is_zero() && budgets[0] <= timeout,
            "{}: remaining {:?}",
            t.header.name,
            budgets[0]
        );
    }
    // Control: a host that answers at once, same caps, same tool.
    let fast = Api {
        body: json!({"jobs": []}),
        seen: Mutex::new(vec![]),
    };
    let r = run_reader_with(
        &mem_tool(SLOW_CAUGHT),
        &page,
        &json!({}),
        Some(&fast),
        &opts,
    );
    assert_eq!(r, Ok(json!({"first": "ok", "second": "ok"})));
    assert_eq!(fast.seen.lock().unwrap().len(), 2);
}

fn big_reader(n: usize) -> Tool {
    mem_tool(&format!(
        "// @name big\n// @kind reader\n// @match /*\n// @gate Read\n// @phase 2\nexport default function () {{\n  return {{ s: 'x'.repeat({n}) }};\n}}\n"
    ))
}

/// D-308: reader output over RunOpts::max_output is RunError::OutputTooLarge (custom and
/// default caps); output under the cap is returned. A planner's program text is capped too.
#[test]
fn d308_reader_output_over_max_output_is_output_too_large() {
    let page = page_at("https://example.com/");
    let small = RunOpts {
        max_output: 1024,
        ..RunOpts::default()
    };
    let r = run_reader_with(&big_reader(2000), &page, &json!({}), None, &small);
    match r {
        Err(RunError::OutputTooLarge { max, got }) => {
            assert_eq!(max, 1024);
            assert!(got >= 2000, "got {got}");
        }
        other => panic!("2000 chars under a 1024 byte cap: {other:?}"),
    }
    let r = run_reader_with(&big_reader(900), &page, &json!({}), None, &small).unwrap();
    assert_eq!(r["s"].as_str().map(str::len), Some(900));

    assert_eq!(RunOpts::default().max_output, 64 * 1024);
    let r = run_reader(&big_reader(70_000), &page, &json!({}), None);
    assert!(
        matches!(r, Err(RunError::OutputTooLarge { max: 65536, got }) if got >= 70_000),
        "{r:?}"
    );

    let planner = mem_tool(
        "// @name big_plan\n// @kind planner\n// @match /*\n// @gate Fill\nexport default function () {\n  return 'c#1; '.repeat(400) + '?s';\n}\n",
    );
    let r = run_planner_with(&planner, &page, &json!({}), &small);
    assert!(
        matches!(r, Err(RunError::OutputTooLarge { max: 1024, got }) if got > 1024),
        "{r:?}"
    );
}

// ================================================================ D-78 batch additions (tester-3)
//
// B-31 items 1 to 4 as fixed in 93a2e4d: lower-case names and the reforge @name check (D-312),
// the @kind and @phase repair check (D-313), the fed vault canary (B-31 item 3) and the
// archive that fails closed (D-314).

fn canary_value() -> String {
    std::env::var("SB_VAULT_CANARY").unwrap_or_else(|_| {
        format!(
            "sbcanary-t3-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    })
}

/// D-312: an upper-case tool name is refused everywhere: the header parser, the loader, the
/// name validator and every mutator. NTFS would make `SignUp.js` and `signup.js` one file.
#[tokio::test]
async fn d312_upper_case_name_refused() {
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    for n in ["Register", "SignUp", "LIST_EVENTS", "listJobs"] {
        let src = format!("// @name {n}\n// @kind reader\n// @gate Read\n");
        let e = parse_header(&src).unwrap_err();
        assert!(e.contains("@name") && e.contains("[a-z0-9_]+"), "{n}: {e}");
        assert!(!valid_name(n), "{n}");
        assert!(
            matches!(check_tool_id("luma.com", n), Err(StoreError::BadPath(_))),
            "{n}"
        );
        assert!(
            matches!(ts.try_tool_path("luma.com", n), Err(StoreError::BadPath(_))),
            "{n}"
        );
        assert!(
            matches!(
                ts.try_archive_path("luma.com", n, 1),
                Err(StoreError::BadPath(_))
            ),
            "{n}"
        );
    }
    // Lower case with digits and _ still passes.
    assert!(parse_header("// @name list_events2\n// @kind reader\n// @gate Read\n").is_ok());
    assert!(valid_name("list_events2"));

    // The loader skips an upper-case tool file (its @name matches its file name) and reports it.
    let site = ts.root.join("luma.com");
    fs::write(
        site.join("SignUp.js"),
        "// @name SignUp\n// @kind reader\n// @match /*\n// @gate Read\nexport default function () { return {}; }\n",
    )
    .unwrap();
    let report = ts.scan("lu.ma", "/e/abc");
    let mut names: Vec<&str> = report
        .tools
        .iter()
        .map(|t| t.header.name.as_str())
        .collect();
    names.sort();
    assert_eq!(names, ["list_events", "register"]);
    let why = report
        .skipped
        .iter()
        .find(|(p, _)| p.ends_with("SignUp.js"))
        .map(|(_, w)| w.clone())
        .unwrap_or_else(|| panic!("SignUp.js not reported: {:?}", report.skipped));
    assert!(why.contains("@name"), "{why}");

    // Every mutator refuses the upper-case spelling of a real tool and writes nothing.
    store.seed(ToolStatus::NeedsRepair, "register", 1, 0).await;
    let before = sorted_tree(dir.path());
    let rows = format!("{:?}", store.health.lock().unwrap());
    let old = read(&ts.try_tool_path("luma.com", "register").unwrap());
    let upper_src = with_line(&old, "// @name register", "// @name Register");
    for outcome in [RunOutcome::Ok, fp_miss()] {
        let r = ts
            .record(&store, res("Register", outcome, Utc::now()))
            .await;
        assert!(matches!(r, Err(StoreError::BadPath(_))), "record: {r:?}");
    }
    let r = ts
        .reforge(&store, "luma.com", "Register", &upper_src, Utc::now())
        .await;
    assert!(matches!(r, Err(StoreError::BadPath(_))), "reforge: {r:?}");
    let r = ts.register_forged(&store, "luma.com", "Register").await;
    assert!(
        matches!(r, Err(StoreError::BadPath(_))),
        "register_forged: {r:?}"
    );
    let r = ts.confirm(&store, "luma.com", "Register").await;
    assert!(matches!(r, Err(StoreError::BadPath(_))), "confirm: {r:?}");
    let r = ts.set_pinned(&store, "luma.com", "Register", true).await;
    assert!(
        matches!(r, Err(StoreError::BadPath(_))),
        "set_pinned: {r:?}"
    );
    let r = ts.archive("luma.com", "Register", 1);
    assert!(matches!(r, Err(StoreError::BadPath(_))), "archive: {r:?}");
    assert_eq!(sorted_tree(dir.path()), before, "files changed");
    assert_eq!(format!("{:?}", store.health.lock().unwrap()), rows);
    assert!(store.events().is_empty(), "ledger written");
}

/// D-312: a re-forge whose new @name is not the name it was called with is refused and writes
/// no file, no archive and no ledger event: (a) a plain rename, caught by check_repair and
/// logged; (b) a hand-edited old file that already carries the other @name, so the old/new
/// rename check cannot see it, also logged; (c) the upper-case spelling of the same name.
#[tokio::test]
async fn d312_reforge_with_a_different_name_refused() {
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::NeedsRepair, "register", 1, 0).await;
    let path = ts.try_tool_path("luma.com", "register").unwrap();
    let old = read(&path);

    // (a) rename register -> signup.
    let renamed = with_line(&old, "// @name register", "// @name signup");
    let e = ts
        .reforge(&store, "luma.com", "register", &renamed, Utc::now())
        .await
        .unwrap_err();
    assert!(
        matches!(&e, StoreError::Invariant(m) if m.contains("renamed") && m.contains("signup")),
        "{e}"
    );
    let h = store.row("luma.com", "register").unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::NeedsRepair, 1));
    assert!(
        h.why.as_deref().is_some_and(|w| w.starts_with("rejected:")),
        "{h:?}"
    );
    assert_eq!(read(&path), old);
    assert!(!ts.try_tool_path("luma.com", "signup").unwrap().exists());

    // (c) Register (upper case): the new header is not valid.
    let upper = with_line(&old, "// @name register", "// @name Register");
    let e = ts
        .reforge(&store, "luma.com", "register", &upper, Utc::now())
        .await
        .unwrap_err();
    assert!(
        matches!(&e, StoreError::Invariant(m) if m.contains("@name")),
        "{e}"
    );
    assert_eq!(read(&path), old);

    // (b) the old file was hand-edited to @name signup; the new one keeps @name signup.
    let hand = renamed.clone();
    fs::write(&path, &hand).unwrap();
    let seeded = store.row("luma.com", "register").unwrap();
    let new = with_line(&hand, "const btn", "const x = 1;\n  const btn");
    let e = ts
        .reforge(&store, "luma.com", "register", &new, Utc::now())
        .await
        .unwrap_err();
    assert!(
        matches!(&e, StoreError::Invariant(m) if m.contains("signup") && m.contains("D-312")),
        "{e}"
    );
    assert_eq!(read(&path), hand);
    // Logged like the check_repair refusals: only `why` changes (T-9 "rejected and logged").
    let logged = store.row("luma.com", "register").unwrap();
    assert!(
        logged
            .why
            .as_deref()
            .is_some_and(|w| w.starts_with("rejected:") && w.contains("D-312")),
        "{logged:?}"
    );
    assert_eq!(
        ToolHealth {
            why: seeded.why.clone(),
            ..logged
        },
        seeded
    );

    assert!(!dir.path().join("tools/_archive").exists(), "archived");
    assert!(store.events().is_empty(), "ledger written");
}

/// D-313: a repair may not change @kind (planner -> reader or reader -> planner). The re-forge
/// is refused and logged; nothing is written. A relocate that changes @kind is refused too.
#[tokio::test]
async fn d313_kind_change_refused() {
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::NeedsRepair, "register", 1, 0).await;
    store
        .seed(ToolStatus::NeedsRepair, "list_events", 1, 0)
        .await;
    let reg = ts.try_tool_path("luma.com", "register").unwrap();
    let le = ts.try_tool_path("luma.com", "list_events").unwrap();
    let (reg_old, le_old) = (read(&reg), read(&le));

    // Header-only change: gate, ?s count, vault keys and @match stay as they were, so @kind is
    // the only thing a re-forge check can object to.
    let cases = [
        (
            "register",
            &reg,
            &reg_old,
            with_line(&reg_old, "// @kind planner", "// @kind reader"),
            "@kind planner -> reader",
        ),
        (
            "list_events",
            &le,
            &le_old,
            with_line(&le_old, "// @kind reader", "// @kind planner"),
            "@kind reader -> planner",
        ),
    ];
    for (name, path, old, new, want) in &cases {
        let e = ts
            .reforge(&store, "luma.com", name, new, Utc::now())
            .await
            .unwrap_err();
        assert!(
            matches!(&e, StoreError::Invariant(m) if m.contains(want)),
            "{name}: {e}"
        );
        let h = store.row("luma.com", name).unwrap();
        assert_eq!(
            (h.status, h.version),
            (ToolStatus::NeedsRepair, 1),
            "{name}"
        );
        let why = h.why.unwrap_or_default();
        assert!(why.starts_with("rejected:") && why.contains(want), "{why}");
        assert_eq!(&read(path), *old, "{name} file changed");
        let e = check_repair(old, new, RepairKind::Relocate).unwrap_err();
        assert!(matches!(&e, StoreError::Invariant(_)), "{name}: {e}");
    }
    assert!(!dir.path().join("tools/_archive").exists(), "archived");
    assert!(store.events().is_empty(), "ledger written");
}

/// D-313: @phase may go 1 -> 2 (narrows: no page.api) but not 2 -> 1 (gains page.api and runs
/// on fetched HTML), including from a tool with no @phase line, which means 2.
#[tokio::test]
async fn d313_phase_lowered_refused_raised_allowed() {
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    store.seed(ToolStatus::NeedsRepair, "register", 1, 0).await;
    let reg = ts.try_tool_path("luma.com", "register").unwrap();
    let reg_old = read(&reg);

    // 2 -> 1, explicit @phase 2.
    let lowered = with_line(&reg_old, "// @phase 2", "// @phase 1");
    let e = ts
        .reforge(&store, "luma.com", "register", &lowered, Utc::now())
        .await
        .unwrap_err();
    assert!(
        matches!(&e, StoreError::Invariant(m) if m.contains("@phase lowered 2 -> 1")),
        "{e}"
    );
    let h = store.row("luma.com", "register").unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::NeedsRepair, 1));
    assert!(h.why.unwrap_or_default().contains("@phase lowered"));
    assert_eq!(read(&reg), reg_old);

    // 2 -> 1 from a tool with no @phase line (default 2).
    let nophase_src = "// @name nophase\n// @kind reader\n// @match /*\n// @gate Read\nexport default function (page) {\n  return { url: page.url };\n}\n";
    let np = ts.try_tool_path("luma.com", "nophase").unwrap();
    fs::write(&np, nophase_src).unwrap();
    store.seed(ToolStatus::NeedsRepair, "nophase", 1, 0).await;
    let np_lowered = with_line(nophase_src, "// @gate Read", "// @gate Read\n// @phase 1");
    let e = ts
        .reforge(&store, "luma.com", "nophase", &np_lowered, Utc::now())
        .await
        .unwrap_err();
    assert!(
        matches!(&e, StoreError::Invariant(m) if m.contains("@phase lowered 2 -> 1")),
        "{e}"
    );
    assert_eq!(read(&np), nophase_src);
    assert!(!dir.path().join("tools/_archive").exists(), "archived");
    assert!(store.events().is_empty(), "ledger written");

    // 1 -> 2 is allowed: list_events is @phase 1.
    store
        .seed(ToolStatus::NeedsRepair, "list_events", 1, 0)
        .await;
    let le = ts.try_tool_path("luma.com", "list_events").unwrap();
    let le_old = read(&le);
    let raised = with_line(&le_old, "// @phase 1", "// @phase 2");
    let h = ts
        .reforge(&store, "luma.com", "list_events", &raised, Utc::now())
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Repaired, 2));
    assert_eq!(read(&le), raised);
    assert_eq!(
        read(&dir.path().join("tools/_archive/luma.com/list_events.1.js")),
        le_old
    );
    assert_eq!(store.tools_logged(), ["tools.reforge"]);
    assert_eq!(tool(&ts, "lu.ma", "list_events").header.phase, 2);
}

/// G-4 vault canary, focused (B-31 item 3): a re-forge that adds a new @vault key and carries
/// that key's value (SB_VAULT_CANARY) in its source is refused, and the canary is in no health
/// row, no ledger event, no file under the tempdir and not in the result. The canary sweep is
/// the first assertion, so with the G-4 vault-key check disabled this test fails on the leak.
#[tokio::test]
async fn g4_canary_fed_new_vault_key_refused_and_absent() {
    let canary = canary_value();
    let (dir, ts) = sandbox();
    let store = FakeStore::default();
    let login = "// @name login\n// @kind planner\n// @match /login\n// @gate Account\n// @phase 2\nexport default function (page) {\n  const p = page.find('input \"Password\"');\n  const b = page.find('button \"Log in\" SUBMIT');\n  return `t#${p}=@vault(luma_password); c#${b}; ?s`;\n}\n";
    let path = ts.try_tool_path("luma.com", "login").unwrap();
    fs::write(&path, login).unwrap();
    store.seed(ToolStatus::NeedsRepair, "login", 1, 0).await;
    let fed = login.replace(
        "  return `t#${p}=@vault(luma_password);",
        &format!(
            "  // seed: {canary}\n  return `t#${{p}}=@vault(luma_password); t#8=@vault(luma_totp);"
        ),
    );
    assert!(fed.contains(&canary) && fed.contains("@vault(luma_totp)"));
    let r = ts
        .reforge(&store, "luma.com", "login", &fed, Utc::now())
        .await;
    let mut haystack = format!(
        "{:?}{:?}{r:?}",
        store.health.lock().unwrap(),
        store.events()
    );
    if let Err(e) = &r {
        haystack.push_str(&e.to_string());
    }
    for (_, c) in tree(dir.path()) {
        haystack.push_str(&c);
    }
    assert!(!haystack.contains(&canary), "vault canary leaked");
    let e = r.unwrap_err();
    assert!(
        matches!(&e, StoreError::Invariant(m) if m.contains("@vault(luma_totp)")),
        "{e}"
    );
    assert_eq!(read(&path), login);
}

/// D-314: archive() sets the tool's mtime to now before the move, so the archived file
/// carries its archive time, never the tool's old mtime, and a read-only tool file (which a
/// rename allows and an open-for-write refuses on Windows) archives like any other. The
/// fail-closed path (set-mtime refused: nothing moves) is covered through the private seam in
/// the lib tests (archive::tests::archive_fails_closed_when_mtime_cannot_be_set).
#[test]
fn d314_read_only_tool_archives_with_its_mtime_set() {
    let (dir, ts) = sandbox();
    let tool_file = ts.try_tool_path("luma.com", "register").unwrap();
    let archived = ts.try_archive_path("luma.com", "register", 1).unwrap();
    let content = read(&tool_file);
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(200 * 86_400);
    fs::File::options()
        .write(true)
        .open(&tool_file)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let writable = fs::metadata(&tool_file).unwrap().permissions();
    let mut ro = writable.clone();
    ro.set_readonly(true);
    fs::set_permissions(&tool_file, ro).unwrap();

    let before = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
    let r = ts.archive("luma.com", "register", 1);
    let moved = !tool_file.exists();
    // Undo the read-only bit wherever the file is, so the tempdir can be removed.
    for p in [&tool_file, &archived] {
        if p.exists() {
            fs::set_permissions(p, writable.clone()).unwrap();
        }
    }
    let to = r
        .unwrap_or_else(|e| panic!("a read-only tool file could not be archived: {e}"))
        .unwrap();
    assert_eq!(to, archived);
    assert!(moved, "the tool is still in place");
    assert_eq!(read(&to), content);
    assert!(
        fs::metadata(&to).unwrap().modified().unwrap() >= before,
        "the archived file kept the tool's old mtime"
    );
    drop(dir);
}
