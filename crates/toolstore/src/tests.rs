//! Crate tests that span modules: folder_for, loader precedence, the T-7 lifecycle through a
//! fake `MemoryStore`, the D-303 sweep, re-forge invariants, and the docs/03 acceptance flows.

use crate::fingerprint::tests::page;
use crate::*;
use async_trait::async_trait;
use chrono::{Duration, TimeZone, Utc};
use harness_core::memory::*;
use harness_core::run::RunId;
use harness_core::Class;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

// ---------------------------------------------------------------- fake store

#[derive(Default)]
struct Fake {
    health: Mutex<Vec<ToolHealth>>,
    ledger: Mutex<Vec<LedgerEvent>>,
}

impl Fake {
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
}

#[async_trait]
impl MemoryStore for Fake {
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

const REGISTER: &str = r#"// @name register
// @kind planner
// @match /*
// @args name:string email:string plan?:string
// @gate Submit
// @fingerprint form#f2 button"Register" input"Email"
// @expects navigated:/thanks|/confirm OR text"You're registered"
// @phase 2
export default function (page, args) {
  const email = page.find('input "Email"');
  const name  = page.find('input "Full name"');
  const btn   = page.find('button "Register" SUBMIT');
  return `t#${name}=${q(args.name)}; t#${email}=${q(args.email)}; c#${btn}; ?s`;
}
"#;

fn tool_src(name: &str, kind: &str, gate: &str, extra: &str) -> String {
    format!("// @name {name}\n// @kind {kind}\n// @gate {gate}\n{extra}export default function (page, args) {{ return {{}}; }}\n")
}

fn put(root: &Path, rel: &str, src: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, src).unwrap();
}

fn luma_root() -> (tempfile::TempDir, ToolStore) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    put(
        root,
        "luma.com/_site.yaml",
        "aliases: [lu.ma, app.luma.com]\nnotes: events\n",
    );
    put(root, "luma.com/register.js", REGISTER);
    put(
        root,
        "luma.com/list_events.js",
        &tool_src(
            "list_events",
            "reader",
            "Read",
            "// @args city:string\n// @match /events* /\n",
        ),
    );
    let ts = ToolStore::new(root);
    (dir, ts)
}

fn t0() -> Ts {
    Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap()
}

fn res(name: &str, outcome: RunOutcome, at: Ts) -> ToolRunResult {
    ToolRunResult {
        domain: "luma.com".into(),
        name: name.into(),
        outcome,
        at,
    }
}

fn health(status: ToolStatus, version: i32, runs: i32, fails: i32) -> ToolHealth {
    ToolHealth {
        domain: "luma.com".into(),
        name: "register".into(),
        version,
        status,
        runs,
        fails,
        last_ok: None,
        last_fail: None,
        why: None,
        pinned: false,
    }
}

fn miss() -> RunOutcome {
    RunOutcome::FingerprintMiss {
        why: "button\"Register\" not found".into(),
    }
}

// ---------------------------------------------------------------- T-1

#[test]
fn folder_for_cases() {
    let (dir, ts) = luma_root();
    fs::create_dir_all(dir.path().join("mail.google.com")).unwrap();
    assert_eq!(ts.folder_for("lu.ma"), "luma.com");
    assert_eq!(ts.folder_for("luma.com"), "luma.com");
    assert_eq!(ts.folder_for("app.luma.com"), "luma.com");
    assert_eq!(ts.folder_for("https://www.lu.ma/e/abc"), "luma.com");
    assert_eq!(ts.folder_for("jobs.ashbyhq.com"), "ashbyhq.com");
    assert_eq!(ts.folder_for("mail.google.com"), "mail.google.com");
    assert_eq!(ts.folder_for("docs.google.com"), "google.com");
    assert_eq!(ts.folder_for("_generic"), "_generic");
    // No folders at all: plain eTLD+1.
    let empty = ToolStore::new(dir.path().join("nope"));
    assert_eq!(empty.folder_for("lu.ma"), "lu.ma");
    assert_eq!(empty.folder_for("app.luma.com"), "luma.com");
}

// ---------------------------------------------------------------- T-3

#[test]
fn loader_namespaces_matches_and_skeleton() {
    let (dir, ts) = luma_root();
    put(
        dir.path(),
        "_generic/read_table.js",
        &tool_src("read_table", "reader", "Read", "// @args id?:string\n"),
    );
    let tools = ts.load("lu.ma", "/events/berlin").unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.namespaced.as_str()).collect();
    assert_eq!(
        names,
        vec!["luma.list_events", "luma.register", "generic.read_table"]
    );
    assert_eq!(
        skeleton_line(&tools).unwrap(),
        "tools for this site: luma.list_events(city) · luma.register(name,email,plan?) · generic.read_table(id?)"
    );
    // @match /events* / does not fit /people.
    let tools = ts.load("luma.com", "https://luma.com/people?x=1").unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.namespaced.as_str()).collect();
    assert_eq!(names, vec!["luma.register", "generic.read_table"]);
    assert_eq!(skeleton_line(&[]), None);
}

#[test]
fn generic_has_lower_precedence_on_name_clash() {
    let (dir, ts) = luma_root();
    put(
        dir.path(),
        "_generic/register.js",
        &tool_src("register", "planner", "Submit", ""),
    );
    put(
        dir.path(),
        "_generic/paginate.js",
        &tool_src("paginate", "reader", "Read", ""),
    );
    let tools = ts.load("luma.com", "/").unwrap();
    let reg: Vec<&Tool> = tools
        .iter()
        .filter(|t| t.header.name == "register")
        .collect();
    assert_eq!(reg.len(), 1);
    assert_eq!(reg[0].domain, "luma.com");
    assert_eq!(reg[0].namespaced, "luma.register");
    assert!(tools.iter().any(|t| t.namespaced == "generic.paginate"));
    // On a site without tools, the generic one loads.
    let tools = ts.load("example.org", "/").unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.namespaced.as_str()).collect();
    assert_eq!(names, vec!["generic.paginate", "generic.register"]);
}

#[test]
fn loader_ignores_garbage_archive_and_site_yaml() {
    let (dir, ts) = luma_root();
    let root = dir.path();
    put(
        root,
        "luma.com/broken.js",
        "// just a comment\nexport default () => 1;\n",
    );
    put(
        root,
        "luma.com/nogate.js",
        "// @name nogate\n// @kind reader\nexport default () => 1;\n",
    );
    put(
        root,
        "luma.com/misnamed.js",
        &tool_src("other", "reader", "Read", ""),
    );
    put(
        root,
        "luma.com/evil.js",
        &tool_src("evil", "reader", "Read", "// @match https://evil.com/*\n"),
    );
    put(root, "luma.com/notes.txt", "not a tool");
    put(root, "_archive/luma.com/register.1.js", REGISTER);
    let rep = ts.scan("luma.com", "/");
    let names: Vec<&str> = rep.tools.iter().map(|t| t.header.name.as_str()).collect();
    assert_eq!(names, vec!["list_events", "register"]);
    let mut skipped: Vec<String> = rep
        .skipped
        .iter()
        .map(|(p, _)| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    skipped.sort();
    assert_eq!(
        skipped,
        vec!["broken.js", "evil.js", "misnamed.js", "nogate.js"]
    );
    assert!(rep.skipped.iter().all(|(_, why)| !why.is_empty()));
    assert!(ts.scan("_archive", "/").tools.is_empty());
}

#[test]
fn seed_tools_parse_and_load() {
    let seeds = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools");
    let ts = ToolStore::new(&seeds);
    let rep = ts.scan("jobs.ashbyhq.com", "https://jobs.ashbyhq.com/acme");
    assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
    let names: Vec<&str> = rep.tools.iter().map(|t| t.namespaced.as_str()).collect();
    assert!(names.contains(&"ashbyhq.list_jobs"), "{names:?}");
    assert!(names.contains(&"generic.read_table"), "{names:?}");
    let jobs = rep
        .tools
        .iter()
        .find(|t| t.header.name == "list_jobs")
        .unwrap();
    assert_eq!(jobs.header.phase, 1);
    assert_eq!(jobs.header.gate, Class::Read);
    assert_eq!(jobs.header.kind, ToolKind::Reader);
    assert!(skeleton_line(&rep.tools)
        .unwrap()
        .contains("ashbyhq.list_jobs(org?,offset?,q?)"));
}

// ---------------------------------------------------------------- T-7

#[tokio::test]
async fn unknown_tool_creates_row() {
    let (dir, ts) = luma_root();
    let store = Fake::default();
    let h = ts
        .record(&store, res("register", RunOutcome::Ok, t0()))
        .await
        .unwrap();
    // Hand-written (no @forged): saved.
    assert_eq!(
        (h.status, h.version, h.runs, h.fails),
        (ToolStatus::Saved, 1, 1, 0)
    );
    assert_eq!(h.last_ok, Some(t0()));
    assert_eq!(store.row("luma.com", "register"), Some(h));
    // Forged by toolforge: ephemeral.
    put(
        dir.path(),
        "luma.com/forged.js",
        &tool_src(
            "forged",
            "reader",
            "Read",
            "// @forged 2026-10-22 task=T-31\n",
        ),
    );
    let h = ts
        .record(&store, res("forged", RunOutcome::Ok, t0()))
        .await
        .unwrap();
    assert_eq!(h.status, ToolStatus::Ephemeral);
}

#[tokio::test]
async fn ephemeral_saved_after_three_clean_runs() {
    let (_dir, ts) = luma_root();
    let store = Fake::default();
    let h = ts
        .register_forged(&store, "luma.com", "register")
        .await
        .unwrap();
    assert_eq!(h.status, ToolStatus::Ephemeral);
    assert!(ts
        .register_forged(&store, "luma.com", "register")
        .await
        .is_err());
    ts.record(&store, res("register", RunOutcome::Ok, t0()))
        .await
        .unwrap();
    // A failure resets the streak (D-302).
    let h = ts
        .record(
            &store,
            res(
                "register",
                RunOutcome::Failed {
                    why: "timeout".into(),
                },
                t0(),
            ),
        )
        .await
        .unwrap();
    assert_eq!((h.status, h.runs, h.fails), (ToolStatus::Ephemeral, 0, 1));
    assert_eq!(h.why.as_deref(), Some("failed: timeout"));
    assert_eq!(h.last_fail, Some(t0()));
    ts.record(&store, res("register", RunOutcome::Ok, t0()))
        .await
        .unwrap();
    let h = ts
        .record(&store, res("register", RunOutcome::Ok, t0()))
        .await
        .unwrap();
    assert_eq!(h.status, ToolStatus::Ephemeral);
    let h = ts
        .record(&store, res("register", RunOutcome::Ok, t0()))
        .await
        .unwrap();
    assert_eq!((h.status, h.runs, h.version), (ToolStatus::Saved, 3, 1));
}

#[tokio::test]
async fn saved_to_needs_repair_on_fingerprint_and_expects_miss() {
    let (_dir, ts) = luma_root();
    for outcome in [
        miss(),
        RunOutcome::ExpectsMiss {
            why: "no navigation to /thanks".into(),
        },
    ] {
        let store = Fake::default();
        store
            .tool_health_set(health(ToolStatus::Saved, 1, 7, 0))
            .await
            .unwrap();
        let h = ts
            .record(&store, res("register", outcome, t0()))
            .await
            .unwrap();
        assert_eq!((h.status, h.runs, h.fails), (ToolStatus::NeedsRepair, 0, 1));
        let why = h.why.unwrap();
        assert!(
            why.starts_with("fingerprint miss: ") || why.starts_with("expects miss: "),
            "{why}"
        );
    }
}

#[tokio::test]
async fn relocated_patches_file_and_bumps_version_then_saved() {
    let (dir, ts) = luma_root();
    let store = Fake::default();
    store
        .tool_health_set(health(ToolStatus::Saved, 1, 4, 0))
        .await
        .unwrap();
    let fp = parse_fingerprint(r#"form#f2 button"Sign up" input"Email""#).unwrap();
    let h = ts
        .record(
            &store,
            res(
                "register",
                RunOutcome::Relocated {
                    fingerprint: fp.clone(),
                },
                t0(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        (h.status, h.version, h.runs, h.fails),
        (ToolStatus::Repaired, 2, 0, 0)
    );
    assert!(h.why.as_deref().unwrap().contains("button\"Sign up\""));
    let on_disk = fs::read_to_string(dir.path().join("luma.com/register.js")).unwrap();
    assert_eq!(parse_header(&on_disk).unwrap().fingerprint, fp);
    assert_eq!(on_disk, patch_fingerprint(REGISTER, &fp));
    let ev = store.events();
    assert_eq!(ev.len(), 1);
    assert_eq!(
        (ev[0].server.as_str(), ev[0].tool.as_str()),
        ("core", "tools.relocate")
    );
    for _ in 0..2 {
        let h = ts
            .record(&store, res("register", RunOutcome::Ok, t0()))
            .await
            .unwrap();
        assert_eq!(h.status, ToolStatus::Repaired);
    }
    let h = ts
        .record(&store, res("register", RunOutcome::Ok, t0()))
        .await
        .unwrap();
    assert_eq!((h.status, h.version, h.why), (ToolStatus::Saved, 2, None));
}

#[tokio::test]
async fn needs_repair_retired_after_five_misses_and_archived() {
    let (dir, ts) = luma_root();
    let store = Fake::default();
    store
        .tool_health_set(health(ToolStatus::Saved, 3, 0, 0))
        .await
        .unwrap();
    for i in 1..5 {
        let h = ts
            .record(&store, res("register", miss(), t0()))
            .await
            .unwrap();
        assert_eq!((h.status, h.fails), (ToolStatus::NeedsRepair, i));
    }
    let h = ts
        .record(&store, res("register", miss(), t0()))
        .await
        .unwrap();
    assert_eq!((h.status, h.fails), (ToolStatus::Retired, 5));
    assert!(h.why.unwrap().starts_with("retired after 5 misses"));
    assert!(!dir.path().join("luma.com/register.js").exists());
    assert_eq!(
        fs::read_to_string(dir.path().join("_archive/luma.com/register.3.js")).unwrap(),
        REGISTER
    );
    let ev = store.events();
    assert_eq!(ev.last().unwrap().tool, "tools.retire");
    // A retired tool stays retired on further outcomes.
    let h = ts
        .record(&store, res("register", miss(), t0()))
        .await
        .unwrap();
    assert_eq!(h.status, ToolStatus::Retired);
}

#[tokio::test]
async fn pinned_is_never_auto_retired() {
    let (dir, ts) = luma_root();
    let store = Fake::default();
    let mut row = health(ToolStatus::NeedsRepair, 1, 0, 4);
    row.pinned = true;
    store.tool_health_set(row).await.unwrap();
    for _ in 0..3 {
        let h = ts
            .record(&store, res("register", miss(), t0()))
            .await
            .unwrap();
        assert_eq!(h.status, ToolStatus::NeedsRepair);
    }
    assert!(dir.path().join("luma.com/register.js").exists());
    // Pinned through _site.yaml too.
    put(
        dir.path(),
        "luma.com/_site.yaml",
        "aliases: [lu.ma]\npinned: [list_events]\n",
    );
    let mut row = health(ToolStatus::NeedsRepair, 1, 0, 9);
    row.name = "list_events".into();
    store.tool_health_set(row).await.unwrap();
    let h = ts
        .record(&store, res("list_events", miss(), t0()))
        .await
        .unwrap();
    assert_eq!(h.status, ToolStatus::NeedsRepair);
}

#[tokio::test]
async fn ok_on_needs_repair_is_repaired_next_version() {
    let (_dir, ts) = luma_root();
    let store = Fake::default();
    store
        .tool_health_set(health(ToolStatus::NeedsRepair, 1, 0, 2))
        .await
        .unwrap();
    let h = ts
        .record(&store, res("register", RunOutcome::Ok, t0()))
        .await
        .unwrap();
    assert_eq!(
        (h.status, h.version, h.runs, h.fails),
        (ToolStatus::Repaired, 2, 1, 0)
    );
}

// ---------------------------------------------------------------- T-5 re-forge, G-4

#[tokio::test]
async fn reforge_archives_old_and_bumps_version() {
    let (dir, ts) = luma_root();
    let store = Fake::default();
    store
        .tool_health_set(health(ToolStatus::NeedsRepair, 1, 0, 2))
        .await
        .unwrap();
    let new = REGISTER.replace("const btn", "const _x = 1;\n  const btn");
    let h = ts
        .reforge(&store, "luma.com", "register", &new, t0())
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Repaired, 2));
    assert_eq!(
        fs::read_to_string(dir.path().join("luma.com/register.js")).unwrap(),
        new
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("_archive/luma.com/register.1.js")).unwrap(),
        REGISTER
    );
    assert_eq!(store.events().last().unwrap().tool, "tools.reforge");
}

#[tokio::test]
async fn reforge_raising_floor_sets_ask_until_confirmed() {
    let (_dir, ts) = luma_root();
    let store = Fake::default();
    store
        .tool_health_set(health(ToolStatus::NeedsRepair, 1, 0, 2))
        .await
        .unwrap();
    let new = REGISTER.replace("@gate Submit", "@gate Pay");
    let h = ts
        .reforge(&store, "luma.com", "register", &new, t0())
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Ask, 2));
    assert!(h.why.unwrap().contains("Submit -> Pay"));
    let h = ts.confirm(&store, "luma.com", "register").await.unwrap();
    assert_eq!(h.status, ToolStatus::Repaired);
}

#[tokio::test]
async fn reforge_violations_are_rejected_and_logged() {
    let cases = [
        (
            REGISTER.replace("@gate Submit", "@gate Fill"),
            "@gate lowered",
        ),
        (REGISTER.replace("; ?s`", "`"), "removed ?s"),
        (
            REGISTER.replace("return", "const p = '@vault(card_pin)';\n  return"),
            "vault key",
        ),
        (
            REGISTER.replace("@match /*", "@match https://evil.example/*"),
            "@match widened",
        ),
        (
            REGISTER.replace("@name register", "@name signup"),
            "renamed",
        ),
    ];
    for (new, want) in cases {
        let (dir, ts) = luma_root();
        let store = Fake::default();
        store
            .tool_health_set(health(ToolStatus::NeedsRepair, 1, 0, 2))
            .await
            .unwrap();
        let err = ts
            .reforge(&store, "luma.com", "register", &new, t0())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, StoreError::Invariant(m) if m.contains(want)),
            "{want}: {err}"
        );
        let row = store.row("luma.com", "register").unwrap();
        assert_eq!((row.status, row.version), (ToolStatus::NeedsRepair, 1));
        assert!(row.why.unwrap().contains(want), "{want}");
        // Nothing moved.
        assert_eq!(
            fs::read_to_string(dir.path().join("luma.com/register.js")).unwrap(),
            REGISTER
        );
        assert!(!dir.path().join("_archive").exists());
    }
}

// ---------------------------------------------------------------- D-303 sweep

#[tokio::test]
async fn sweep_retires_stale_ephemeral_then_deletes_after_90_days() {
    let (dir, ts) = luma_root();
    let store = Fake::default();
    let now = Utc::now();
    let mut stale = health(ToolStatus::Ephemeral, 1, 1, 0);
    stale.last_ok = Some(now - Duration::days(15));
    store.tool_health_set(stale).await.unwrap();
    let mut fresh = health(ToolStatus::Ephemeral, 1, 1, 0);
    fresh.name = "list_events".into();
    fresh.last_ok = Some(now - Duration::days(2));
    store.tool_health_set(fresh).await.unwrap();

    let done = ts.sweep(&store, now, &|_| 0).await.unwrap();
    assert_eq!(done.len(), 1, "{done:?}");
    let archived = dir.path().join("_archive/luma.com/register.1.js");
    match &done[0] {
        SweepAction::Retired {
            name,
            archived: a,
            why,
            ..
        } => {
            assert_eq!(name, "register");
            assert_eq!(a.as_ref(), Some(&archived));
            assert!(why.contains("14 days"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    assert!(!dir.path().join("luma.com/register.js").exists());
    assert!(archived.exists());
    assert_eq!(
        store.row("luma.com", "register").unwrap().status,
        ToolStatus::Retired
    );
    assert_eq!(
        store.row("luma.com", "list_events").unwrap().status,
        ToolStatus::Ephemeral
    );
    let ev = store.events();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].tool, "tools.retire");
    assert_eq!(ev[0].server, "core");
    assert!(ev[0].ok);

    // Nothing more to do now; 91 days later the archived file is deleted.
    assert!(ts.sweep(&store, now, &|_| 0).await.unwrap().is_empty());
    let later = now + Duration::days(91);
    let mut fresh = store.row("luma.com", "list_events").unwrap();
    fresh.last_ok = Some(later);
    store.tool_health_set(fresh).await.unwrap();
    let done = ts.sweep(&store, later, &|_| 0).await.unwrap();
    assert!(done
        .iter()
        .any(|a| matches!(a, SweepAction::Deleted { path, .. } if *path == archived)));
    assert!(!archived.exists());
    assert_eq!(store.events().last().unwrap().tool, "tools.delete");
}

#[tokio::test]
async fn sweep_visits_retired_in_place_and_pinned() {
    let (dir, ts) = luma_root();
    let store = Fake::default();
    let now = Utc::now();
    let mut e = health(ToolStatus::Ephemeral, 1, 0, 0);
    e.last_ok = Some(now);
    store.tool_health_set(e).await.unwrap();
    let mut r = health(ToolStatus::Retired, 4, 0, 5);
    r.name = "list_events".into();
    store.tool_health_set(r).await.unwrap();
    let done = ts
        .sweep(&store, now, &|h| if h.name == "register" { 10 } else { 0 })
        .await
        .unwrap();
    assert_eq!(done.len(), 2, "{done:?}");
    assert!(done
        .iter()
        .any(|a| matches!(a, SweepAction::Retired { why, .. } if why.contains("10 site visits"))));
    assert!(done
        .iter()
        .any(|a| matches!(a, SweepAction::Archived { version: 4, .. })));
    assert!(dir
        .path()
        .join("_archive/luma.com/list_events.4.js")
        .exists());

    // Pinned ephemeral: untouched however stale.
    let (_dir2, ts2) = luma_root();
    let store2 = Fake::default();
    let mut p = health(ToolStatus::Ephemeral, 1, 0, 0);
    p.last_ok = Some(now - Duration::days(100));
    p.pinned = true;
    store2.tool_health_set(p).await.unwrap();
    assert!(ts2.sweep(&store2, now, &|_| 50).await.unwrap().is_empty());
}

// ---------------------------------------------------------------- docs/03 acceptance

/// Planted fingerprint mismatch -> needs_repair; page fixed -> relocate -> repaired, version 2.
#[tokio::test]
async fn acceptance_planted_mismatch_then_relocate_to_version_2() {
    let (dir, ts) = luma_root();
    let store = Fake::default();
    store
        .tool_health_set(health(ToolStatus::Saved, 1, 5, 0))
        .await
        .unwrap();
    let tool = ts
        .load("lu.ma", "/e/abc")
        .unwrap()
        .into_iter()
        .find(|t| t.namespaced == "luma.register")
        .unwrap();

    // Planted mismatch: the form is gone.
    let broken = vec![crate::fingerprint::tests::row(1, "link", "Home", None)];
    assert_eq!(fingerprint(&tool, &broken), FingerprintResult::Miss);
    let h = ts
        .record(
            &store,
            res(
                "register",
                RunOutcome::FingerprintMiss {
                    why: "form#f2 gone".into(),
                },
                t0(),
            ),
        )
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::NeedsRepair, 1));
    assert!(h.why.unwrap().contains("form#f2 gone"));

    // Page fixed, with the button renamed: partial -> relocate.
    let fixed = page("Sign up");
    let FingerprintResult::Partial { missing } = fingerprint(&tool, &fixed) else {
        panic!("expected partial");
    };
    assert_eq!(missing, parse_fingerprint(r#"button"Register""#).unwrap());
    let fp = relocate(&tool, &fixed).unwrap();
    let h = ts
        .record(
            &store,
            res("register", RunOutcome::Relocated { fingerprint: fp }, t0()),
        )
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Repaired, 2));
    // The patched file now matches the fixed page.
    let reloaded = ts.load("luma.com", "/").unwrap();
    let tool = reloaded
        .iter()
        .find(|t| t.header.name == "register")
        .unwrap();
    assert_eq!(fingerprint(tool, &fixed), FingerprintResult::Match);
    assert!(dir.path().join("luma.com/register.js").exists());
}

/// The planner-output invariant end to end with a fake sb-act (D-301).
#[test]
fn planner_output_class_uses_floor() {
    struct FillOnly;
    impl PlannerCheck for FillOnly {
        fn check(
            &self,
            _: &str,
            _: &[sb_sheet::IndexRow],
        ) -> std::result::Result<sb_act::ActClass, String> {
            Ok(sb_act::ActClass {
                class: Class::Fill,
                hints: vec![],
            })
        }
    }
    let (_dir, ts) = luma_root();
    let tool = ts
        .load("luma.com", "/")
        .unwrap()
        .into_iter()
        .find(|t| t.header.name == "register")
        .unwrap();
    assert_eq!(
        planner_class(&tool, "t#2=\"a\"", &page("Register"), &FillOnly).unwrap(),
        Class::Submit
    );
}

// ---------------------------------------------------------------- seeds through the runner

struct SeedPage;
impl PageHost for SeedPage {
    fn find(&self, q: &str) -> std::result::Result<String, String> {
        Err(format!("find({q:?}): no match"))
    }
    fn find_all(&self, _: &str) -> Vec<String> {
        vec![]
    }
    fn text(&self, _: Option<&str>) -> String {
        String::new()
    }
    fn table(&self, id: &str) -> serde_json::Value {
        if id == "2" {
            serde_json::json!({"header": ["Name", "Role"], "rows": [["Ada", "Eng"], ["Bo", "PM"], ["Cy", "Ops"]]})
        } else {
            serde_json::Value::Null
        }
    }
    fn url(&self) -> String {
        "https://jobs.ashbyhq.com/acme?utm=x".into()
    }
    fn title(&self) -> String {
        "Jobs at Acme".into()
    }
}

struct Board(Mutex<Vec<String>>);
impl ApiFetch for Board {
    fn api(
        &self,
        url: &str,
        _remaining: std::time::Duration,
    ) -> std::result::Result<serde_json::Value, String> {
        self.0.lock().unwrap().push(url.to_string());
        Ok(serde_json::json!({"jobs": [
            {"title": "Engineer", "location": "Berlin", "department": "R&D", "jobUrl": "https://jobs.ashbyhq.com/acme/1", "isListed": true, "isRemote": true},
            {"title": "Hidden", "isListed": false},
            {"title": "Designer", "address": {"postalAddress": {"addressLocality": "Paris", "addressCountry": "FR"}}, "team": "Design", "applyUrl": "https://jobs.ashbyhq.com/acme/2/application"}
        ]}))
    }
}

fn seed(host: &str, name: &str) -> Tool {
    let ts = ToolStore::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools"));
    ts.load(host, "/acme")
        .unwrap()
        .into_iter()
        .find(|t| t.header.name == name)
        .unwrap()
}

#[test]
fn seed_list_jobs_runs_on_api() {
    let tool = seed("jobs.ashbyhq.com", "list_jobs");
    let board = Board(Mutex::new(vec![]));
    let out = run_reader(&tool, &SeedPage, &serde_json::json!({}), Some(&board)).unwrap();
    assert_eq!(
        board.0.lock().unwrap().as_slice(),
        ["https://api.ashbyhq.com/posting-api/job-board/acme"]
    );
    assert_eq!(out["org"], "acme");
    assert_eq!(out["total"], 2);
    assert_eq!(
        out["jobs"][0],
        serde_json::json!({"title": "Engineer", "location": "Berlin (remote)", "department": "R&D", "url": "https://jobs.ashbyhq.com/acme/1"})
    );
    assert_eq!(out["jobs"][1]["location"], "Paris, FR");
    assert_eq!(out["jobs"][1]["department"], "Design");
    // An explicit org wins over the URL.
    let board = Board(Mutex::new(vec![]));
    run_reader(
        &tool,
        &SeedPage,
        &serde_json::json!({"org": "other co"}),
        Some(&board),
    )
    .unwrap();
    assert!(board.0.lock().unwrap()[0].ends_with("/job-board/other%20co"));
}

#[test]
fn seed_read_table_runs() {
    let tool = seed("example.org", "read_table");
    let out = run_reader(&tool, &SeedPage, &serde_json::json!({"max": 2}), None).unwrap();
    assert_eq!(out["id"], "2");
    assert_eq!(out["total"], 3);
    assert_eq!(out["rows"].as_array().unwrap().len(), 2);
    let out = run_reader(&tool, &SeedPage, &serde_json::json!({"id": "7"}), None).unwrap();
    assert_eq!(out["error"], "no table 7");
}

// ---------------------------------------------------------------- T-13b (D-577, D-578)

fn luma_register(ts: &ToolStore) -> Tool {
    ts.load("lu.ma", "/e/abc")
        .unwrap()
        .into_iter()
        .find(|t| t.namespaced == "luma.register")
        .unwrap()
}

/// luma.register's relocated fingerprint on `page("Sign up")`.
fn relocated_register(ts: &ToolStore) -> Vec<FpItem> {
    relocate(&luma_register(ts), &page("Sign up")).expect("relocatable")
}

/// D-577: two runs that both saw the miss both record Relocated with the same new
/// fingerprint; the second finds the file already patched, writes nothing and counts as a
/// clean run. The row stays at version 2 with one tools.relocate event.
#[tokio::test]
async fn a_second_relocate_to_the_same_fingerprint_stays_at_version_2() {
    let (dir, ts) = luma_root();
    let store = Fake::default();
    store
        .tool_health_set(health(ToolStatus::NeedsRepair, 1, 0, 1))
        .await
        .unwrap();
    let fp = relocated_register(&ts);
    let file = dir.path().join("luma.com/register.js");
    let relocated = || {
        res(
            "register",
            RunOutcome::Relocated {
                fingerprint: fp.clone(),
            },
            t0(),
        )
    };
    let first = ts.record(&store, relocated()).await.unwrap();
    assert_eq!((first.status, first.version), (ToolStatus::Repaired, 2));
    let patched = fs::read_to_string(&file).unwrap();
    assert_ne!(patched, REGISTER);
    let second = ts.record(&store, relocated()).await.unwrap();
    assert_eq!(
        (second.status, second.version, second.runs),
        (ToolStatus::Repaired, 2, 1),
        "{second:?}"
    );
    assert_eq!(second.why, first.why, "no second 'X -> X' relocate");
    assert_eq!(fs::read_to_string(&file).unwrap(), patched);
    let relocates = store
        .events()
        .iter()
        .filter(|e| e.tool == "tools.relocate")
        .count();
    assert_eq!(relocates, 1);
}

/// D-577 on a row left needs_repair after the file was patched (a crash between the write
/// and the row): the Relocated is an Ok, which repairs the row (D-304) without a write.
#[tokio::test]
async fn relocate_to_the_fingerprint_the_file_has_is_an_ok() {
    let (dir, ts) = luma_root();
    let store = Fake::default();
    store
        .tool_health_set(health(ToolStatus::NeedsRepair, 1, 0, 1))
        .await
        .unwrap();
    let file = dir.path().join("luma.com/register.js");
    let current = parse_header(REGISTER).unwrap().fingerprint;
    let h = ts
        .record(
            &store,
            res(
                "register",
                RunOutcome::Relocated {
                    fingerprint: current,
                },
                t0(),
            ),
        )
        .await
        .unwrap();
    assert_eq!((h.status, h.version, h.runs), (ToolStatus::Repaired, 2, 1));
    assert_eq!(h.why.as_deref(), Some("fingerprint matches again"));
    assert_eq!(fs::read_to_string(&file).unwrap(), REGISTER);
    assert!(store.events().iter().all(|e| e.tool != "tools.relocate"));
}

/// The characters D-578 keeps out of headers: JS line terminators Rust's line split does not
/// see, LF itself, and other control characters.
const BAD_IN_HEADER: [&str; 6] = ["\r", "\n", "\u{2028}", "\u{2029}", "\u{85}", "\u{1b}"];

/// D-578 (B-33 family): a page name holding a JS line terminator or control character is
/// never relocated into @fingerprint; the page is a miss, not a partial.
#[test]
fn relocate_skips_names_with_line_terminators_and_controls() {
    let (_d, ts) = luma_root();
    let tool = luma_register(&ts);
    for bad in BAD_IN_HEADER {
        // Without D-578 each of these would relocate: names compare after normalising.
        let name = format!("Sign up{bad}");
        let index = page(&name);
        assert_eq!(relocate(&tool, &index), None, "{}", bad.escape_debug());
        assert_eq!(
            fingerprint(&tool, &index),
            FingerprintResult::Miss,
            "{}",
            bad.escape_debug()
        );
    }
    // The same name without the bad character relocates.
    assert!(relocate(&tool, &page("Sign up")).is_some());
}

/// D-578: a form id that cannot be read back from a header line is no relocation.
#[test]
fn relocate_skips_form_ids_a_header_cannot_hold() {
    use crate::fingerprint::tests::row;
    let (_d, ts) = luma_root();
    let tool = luma_register(&ts);
    let board = |id: &str| {
        vec![
            row(1, "button", "Register", Some(id)),
            row(2, "textbox", "Email", Some(id)),
        ]
    };
    for id in ["f 3", "f\"3", "f\u{2028}3", "f\r3"] {
        assert_eq!(relocate(&tool, &board(id)), None, "{}", id.escape_debug());
    }
    let fp = relocate(&tool, &board("f3")).expect("a plain id relocates");
    assert_eq!(fp[0].id.as_deref(), Some("f3"));
}

/// D-578: record(Relocated) refuses an item a header line cannot hold, whoever built it; the
/// file is untouched, the version stays, and the reason (without the page text) is logged.
#[tokio::test]
async fn record_relocated_refuses_line_terminators_in_names_and_ids() {
    for bad in BAD_IN_HEADER {
        let (dir, ts) = luma_root();
        let store = Fake::default();
        store
            .tool_health_set(health(ToolStatus::NeedsRepair, 1, 0, 1))
            .await
            .unwrap();
        let file = dir.path().join("luma.com/register.js");
        let mut by_name = relocated_register(&ts);
        let b = by_name.iter_mut().find(|i| i.role == "button").unwrap();
        b.name = Some(format!("Sign up{bad}globalThis.x = 1; //"));
        let mut by_id = relocated_register(&ts);
        by_id[0].id = Some(format!("f2{bad}x"));
        for fp in [by_name, by_id] {
            let r = ts
                .record(
                    &store,
                    res("register", RunOutcome::Relocated { fingerprint: fp }, t0()),
                )
                .await;
            assert!(
                matches!(&r, Err(StoreError::Invariant(m)) if m.contains("D-578")),
                "{}: {r:?}",
                bad.escape_debug()
            );
            assert_eq!(fs::read_to_string(&file).unwrap(), REGISTER);
            let row = store.row("luma.com", "register").unwrap();
            assert_eq!((row.status, row.version), (ToolStatus::NeedsRepair, 1));
            let why = row.why.unwrap_or_default();
            assert!(
                why.starts_with("rejected:") && !why.contains("globalThis"),
                "{why}"
            );
        }
        assert!(store.events().iter().all(|e| e.tool != "tools.relocate"));
    }
}

/// D-578: an id with a space or a quote would not read back from the patched line, so it is
/// refused the same way.
#[tokio::test]
async fn record_relocated_refuses_an_id_that_does_not_read_back() {
    let (dir, ts) = luma_root();
    let store = Fake::default();
    for id in ["f 2", "f\"2"] {
        let mut fp = relocated_register(&ts);
        fp[0].id = Some(id.into());
        let r = ts
            .record(
                &store,
                res("register", RunOutcome::Relocated { fingerprint: fp }, t0()),
            )
            .await;
        assert!(matches!(&r, Err(StoreError::Invariant(_))), "{id}: {r:?}");
        assert_eq!(
            fs::read_to_string(dir.path().join("luma.com/register.js")).unwrap(),
            REGISTER
        );
    }
}

/// D-578: check_repair refuses a new file whose header block hides a JS line terminator or
/// control character, for relocate and re-forge alike; CRLF line ends and tabs stay fine,
/// and the body may hold U+2028 in a string.
#[test]
fn check_repair_refuses_hidden_line_terminators_in_the_header() {
    for bad in ["\r", "\u{2028}", "\u{2029}", "\u{1b}"] {
        let new = REGISTER.replace(
            "// @expects navigated",
            &format!("// @expects{bad}globalThis.x = 1; // navigated"),
        );
        for kind in [RepairKind::Relocate, RepairKind::Reforge] {
            let r = check_repair(REGISTER, &new, kind);
            assert!(
                matches!(&r, Err(StoreError::Invariant(m)) if m.contains("header line 7")),
                "{} {kind:?}: {r:?}",
                bad.escape_debug()
            );
        }
    }
    let crlf = REGISTER.replace('\n', "\r\n");
    assert!(check_repair(&crlf, &crlf, RepairKind::Relocate).is_ok());
    let tab = REGISTER.replace("// @phase 2", "//\t@phase 2");
    assert!(check_repair(REGISTER, &tab, RepairKind::Reforge).is_ok());
    let body = REGISTER.replace("  return `t#", "  const s = '\u{2028}';\n  return `t#");
    assert_ne!(body, REGISTER);
    assert!(check_repair(REGISTER, &body, RepairKind::Reforge).is_ok());
}
