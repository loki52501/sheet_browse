//! Acceptance for T-11b (B-33 items 1-3, D-315, D-316; docs/03 T-3, T-7, D-303), written by
//! the tester. Public API only, from outside the crate, with a fake `MemoryStore` whose
//! `tools.health` read can be made to fail per folder.
//!
//! The archive failure here is portable: `tools/_archive` is made a regular file, so the
//! archive step's `create_dir_all` fails after the stamp, with the tool file left in place.
//! (The Windows share-lock variant lives in accept_t11.rs.)

use async_trait::async_trait;
use chrono::{Duration, Utc};
use harness_core::memory::*;
use harness_core::run::RunId;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use toolstore::*;

// ---------------------------------------------------------------- fake MemoryStore

#[derive(Default)]
struct Mem {
    health: Mutex<Vec<ToolHealth>>,
    ledger: Mutex<Vec<LedgerEvent>>,
    /// Folders whose `tool_health` read fails with a backend error; `*` fails every folder.
    fail_read: Mutex<Vec<String>>,
}

impl Mem {
    fn put(&self, domain: &str, name: &str, status: ToolStatus, version: i32) {
        let mut v = self.health.lock().unwrap();
        v.retain(|x| !(x.domain == domain && x.name == name));
        v.push(ToolHealth {
            domain: domain.into(),
            name: name.into(),
            version,
            status,
            runs: 0,
            fails: 0,
            last_ok: None,
            last_fail: None,
            why: None,
            pinned: false,
        });
    }
    fn get(&self, domain: &str, name: &str) -> Option<ToolHealth> {
        self.health
            .lock()
            .unwrap()
            .iter()
            .find(|h| h.domain == domain && h.name == name)
            .cloned()
    }
    fn set(&self, h: ToolHealth) {
        let mut v = self.health.lock().unwrap();
        v.retain(|x| !(x.domain == h.domain && x.name == h.name));
        v.push(h);
    }
    fn events(&self) -> Vec<LedgerEvent> {
        self.ledger.lock().unwrap().clone()
    }
    fn fail_on(&self, folder: &str) {
        self.fail_read.lock().unwrap().push(folder.into());
    }
}

#[async_trait]
impl MemoryStore for Mem {
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
        let fail = self
            .fail_read
            .lock()
            .unwrap()
            .iter()
            .any(|f| f == "*" || f == domain);
        if fail {
            return Err(MemoryError::Invalid(format!(
                "tools.health read for {domain} failed (test hook)"
            )));
        }
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
        self.set(h);
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

/// `tests/fixtures/tools/` (luma.com: register, list_events; example.com: probe, probe_api)
/// copied to a tempdir, plus `_generic/register.js` and `_generic/paginate.js`.
fn sandbox() -> (tempfile::TempDir, ToolStore) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tools");
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tools");
    copy_dir(&fixtures, &root);
    let probe = fs::read_to_string(root.join("example.com/probe.js")).unwrap();
    fs::create_dir_all(root.join(GENERIC)).unwrap();
    for name in ["register", "paginate"] {
        let src = probe.replace("@name probe", &format!("@name {name}"));
        fs::write(root.join(GENERIC).join(format!("{name}.js")), src).unwrap();
    }
    let ts = ToolStore::new(&root);
    (dir, ts)
}

/// Removes the `_generic` folder (row-1 tests look at one site folder only).
fn no_generic(ts: &ToolStore) {
    fs::remove_dir_all(ts.root.join(GENERIC)).unwrap();
}

fn names(tools: &[Tool]) -> Vec<String> {
    let mut v: Vec<String> = tools.iter().map(|t| t.namespaced.clone()).collect();
    v.sort();
    v
}

fn file(ts: &ToolStore, folder: &str, name: &str) -> PathBuf {
    ts.try_tool_path(folder, name).unwrap()
}

/// Makes every archive step fail: `tools/_archive` is a regular file, so the archive's
/// `create_dir_all` fails and the tool file stays where it is.
fn break_archive(ts: &ToolStore) -> PathBuf {
    let p = ts.root.join(ARCHIVE);
    fs::write(&p, "not a directory\n").unwrap();
    p
}

fn miss() -> RunOutcome {
    RunOutcome::FingerprintMiss {
        why: "form#f2 not on the page".into(),
    }
}

// ---------------------------------------------------------------- row 1

/// Row 1 (D-315): a tool whose own row is retired while its file is still in
/// `tools/<domain>/` is not returned by scan_live or load_live and is reported in `skipped`
/// as `retired (v<N>)`; the other site tool still loads; sync `load` still lists it.
#[tokio::test]
async fn r1_retired_row_with_file_in_place_is_skipped_and_reported() {
    let (_dir, ts) = sandbox();
    no_generic(&ts);
    let mem = Mem::default();
    mem.put("luma.com", "register", ToolStatus::Retired, 3);
    let reg = file(&ts, "luma.com", "register");
    assert!(reg.is_file());

    let r = ts.scan_live(&mem, "luma.com", "/").await.unwrap();
    assert_eq!(names(&r.tools), ["luma.list_events"]);
    assert_eq!(r.skipped, vec![(reg.clone(), "retired (v3)".to_string())]);

    for host in ["luma.com", "https://luma.com/events/1"] {
        let l = ts.load_live(&mem, host, "/events/1").await.unwrap();
        assert_eq!(names(&l), ["luma.list_events"], "{host}");
    }
    // Sync load does not read tools.health (documented, D-315).
    assert_eq!(
        names(&ts.load("luma.com", "/").unwrap()),
        ["luma.list_events", "luma.register"]
    );
}

/// Row 1 (D-315): ephemeral, saved, repaired, needs_repair, ask, and no row at all still load;
/// a retired row of the same name in another site folder does not hide this site's tool.
#[tokio::test]
async fn r1_non_retired_statuses_and_no_row_still_load() {
    let (_dir, ts) = sandbox();
    no_generic(&ts);
    for status in [
        None,
        Some(ToolStatus::Ephemeral),
        Some(ToolStatus::Saved),
        Some(ToolStatus::Repaired),
        Some(ToolStatus::NeedsRepair),
        Some(ToolStatus::Ask),
    ] {
        let mem = Mem::default();
        if let Some(st) = status {
            mem.put("luma.com", "register", st, 2);
            mem.put("luma.com", "list_events", st, 5);
        }
        // Another folder's retired row with the same names.
        mem.put("example.com", "register", ToolStatus::Retired, 9);
        mem.put("example.com", "list_events", ToolStatus::Retired, 9);
        let r = ts.scan_live(&mem, "luma.com", "/").await.unwrap();
        assert_eq!(
            names(&r.tools),
            ["luma.list_events", "luma.register"],
            "{status:?}"
        );
        assert!(r.skipped.is_empty(), "{status:?}: {:?}", r.skipped);
        let l = ts.load_live(&mem, "luma.com", "/").await.unwrap();
        assert_eq!(l.len(), 2, "{status:?}");
    }
}

// ---------------------------------------------------------------- row 2

/// Row 2 (D-315): with no rows the site register shadows `_generic/register`; once the site
/// row is retired the `_generic` tool of the same name loads in its place, and the site file
/// is reported as retired. Sync load keeps the old shadowing.
#[tokio::test]
async fn r2_retired_site_tool_does_not_shadow_the_generic_one() {
    let (_dir, ts) = sandbox();
    let mem = Mem::default();
    let before = ts.load_live(&mem, "luma.com", "/").await.unwrap();
    assert_eq!(
        names(&before),
        ["generic.paginate", "luma.list_events", "luma.register"]
    );

    mem.put("luma.com", "register", ToolStatus::Retired, 2);
    let r = ts.scan_live(&mem, "luma.com", "/").await.unwrap();
    assert_eq!(
        names(&r.tools),
        ["generic.paginate", "generic.register", "luma.list_events"]
    );
    let g = r
        .tools
        .iter()
        .find(|t| t.header.name == "register")
        .unwrap();
    assert_eq!(g.domain, GENERIC);
    assert_eq!(g.path, file(&ts, GENERIC, "register"));
    assert_eq!(
        r.skipped,
        vec![(
            file(&ts, "luma.com", "register"),
            "retired (v2)".to_string()
        )]
    );
    assert_eq!(
        names(&ts.load("luma.com", "/").unwrap()),
        ["generic.paginate", "luma.list_events", "luma.register"]
    );
}

/// Row 2 (D-315): a retired `_generic` row is skipped on any site and on `_generic` itself;
/// it does not hide a site tool of the same name, and a site row of the generic tool's name
/// does not hide the generic tool (each folder's own row only).
#[tokio::test]
async fn r2_retired_generic_row_is_skipped() {
    let (_dir, ts) = sandbox();
    let mem = Mem::default();
    mem.put(GENERIC, "paginate", ToolStatus::Retired, 4);
    mem.put(GENERIC, "register", ToolStatus::Retired, 1);
    let pag = file(&ts, GENERIC, "paginate");

    let r = ts.scan_live(&mem, "luma.com", "/").await.unwrap();
    assert_eq!(names(&r.tools), ["luma.list_events", "luma.register"]);
    assert!(
        r.skipped
            .contains(&(pag.clone(), "retired (v4)".to_string())),
        "{:?}",
        r.skipped
    );

    for domain in [GENERIC, "example.com", "nowhere.org"] {
        let r = ts.scan_live(&mem, domain, "/").await.unwrap();
        assert!(
            !names(&r.tools).iter().any(|n| n.starts_with("generic.")),
            "{domain}: {:?}",
            names(&r.tools)
        );
        assert!(
            r.skipped
                .contains(&(pag.clone(), "retired (v4)".to_string())),
            "{domain}: {:?}",
            r.skipped
        );
    }

    // A site row named like the generic tool does not hide the generic tool.
    let mem = Mem::default();
    mem.put("luma.com", "paginate", ToolStatus::Retired, 7);
    let l = ts.load_live(&mem, "luma.com", "/").await.unwrap();
    assert_eq!(
        names(&l),
        ["generic.paginate", "luma.list_events", "luma.register"]
    );
}

// ---------------------------------------------------------------- row 3

/// Row 3 (D-315): a memory error from tool_health, for the site folder, for `_generic` only,
/// or for both, makes scan_live and load_live return Err(Memory), never a list. Also when
/// there are no rows at all and when the folder asked for is `_generic`.
#[tokio::test]
async fn r3_memory_error_is_err_never_a_list() {
    let (_dir, ts) = sandbox();
    for fail in ["luma.com", GENERIC, "*"] {
        for rows in [false, true] {
            let mem = Mem::default();
            if rows {
                mem.put("luma.com", "register", ToolStatus::Retired, 1);
                mem.put(GENERIC, "paginate", ToolStatus::Retired, 1);
            }
            mem.fail_on(fail);
            let s = ts.scan_live(&mem, "luma.com", "/").await;
            assert!(
                matches!(s, Err(StoreError::Memory(_))),
                "{fail} {rows}: {s:?}"
            );
            let l = ts.load_live(&mem, "luma.com", "/").await;
            assert!(
                matches!(l, Err(StoreError::Memory(_))),
                "{fail} {rows}: {l:?}"
            );
        }
    }
    let mem = Mem::default();
    mem.fail_on(GENERIC);
    let l = ts.load_live(&mem, GENERIC, "/").await;
    assert!(matches!(l, Err(StoreError::Memory(_))), "{l:?}");
    let s = ts.scan_live(&mem, GENERIC, "/").await;
    assert!(matches!(s, Err(StoreError::Memory(_))), "{s:?}");
}

// ---------------------------------------------------------------- rows 4 and 5

/// Rows 4 and 5 (D-316) through record: the 5th miss retires a needs_repair tool whose
/// archive step fails. record returns Ok with the row retired, the file stays in
/// `tools/luma.com/`, load_live does not return it (sync load does). Ledger: an ok:true
/// tools.retire with `archived: null` and `archive_failed` set, then an ok:false
/// tools.archive naming the file and the error, nothing else. The next sweep, with the
/// archive fixed, archives the file with an ok:true tools.archive.
#[tokio::test]
async fn r4_r5_record_retire_with_failed_archive() {
    let (_dir, ts) = sandbox();
    no_generic(&ts);
    let mem = Mem::default();
    let now = Utc::now();
    mem.put("luma.com", "register", ToolStatus::NeedsRepair, 1);
    let mut h = mem.get("luma.com", "register").unwrap();
    h.fails = MISSES_TO_RETIRE - 1;
    mem.set(h);
    let reg = file(&ts, "luma.com", "register");
    let content = fs::read_to_string(&reg).unwrap();
    let blocker = break_archive(&ts);

    let res = ToolRunResult {
        domain: "luma.com".into(),
        name: "register".into(),
        outcome: miss(),
        at: now,
    };
    let h = ts
        .record(&mem, res)
        .await
        .expect("record returns Ok (D-316)");
    assert_eq!(h.status, ToolStatus::Retired);
    assert_eq!(h.fails, MISSES_TO_RETIRE);
    assert_eq!(
        mem.get("luma.com", "register").unwrap().status,
        ToolStatus::Retired
    );
    assert!(reg.is_file(), "the file left tools/luma.com/");
    assert_eq!(fs::read_to_string(&reg).unwrap(), content);

    // Row 4: load_live skips it, sync load still has it.
    let r = ts.scan_live(&mem, "luma.com", "/").await.unwrap();
    assert_eq!(names(&r.tools), ["luma.list_events"]);
    assert_eq!(r.skipped, vec![(reg.clone(), "retired (v1)".to_string())]);
    assert_eq!(
        names(&ts.load_live(&mem, "luma.com", "/").await.unwrap()),
        ["luma.list_events"]
    );
    assert_eq!(
        names(&ts.load("luma.com", "/").unwrap()),
        ["luma.list_events", "luma.register"]
    );

    // Row 5: the ledger.
    let ev = mem.events();
    assert_eq!(ev.len(), 2, "{ev:#?}");
    let (r, a) = (&ev[0], &ev[1]);
    assert_eq!(
        (r.server.as_str(), r.tool.as_str()),
        ("core", "tools.retire")
    );
    assert!(r.ok, "{r:?}");
    assert_eq!(
        r.args,
        json!({"domain": "luma.com", "name": "register", "version": 1})
    );
    assert!(r.consequences["archived"].is_null(), "{r:?}");
    assert!(
        r.consequences.as_object().unwrap().contains_key("archived"),
        "{r:?}"
    );
    assert_eq!(
        r.consequences["archive_failed"], "luma.com/register.js",
        "{r:?}"
    );
    assert!(
        r.why.as_deref().is_some_and(|w| w.contains("misses")),
        "{r:?}"
    );
    assert_eq!(
        (a.server.as_str(), a.tool.as_str()),
        ("core", "tools.archive")
    );
    assert!(!a.ok, "{a:?}");
    assert_eq!(a.args["name"], "register");
    assert_eq!(a.consequences["failed"], "luma.com/register.js", "{a:?}");
    let err = a.why.as_deref().unwrap_or_default();
    assert!(!err.is_empty(), "{a:?}");
    assert!(
        a.result_summary.contains("luma.com/register.js") && a.result_summary.contains(err),
        "{a:?}"
    );
    assert!(
        !ev.iter().any(|e| e.tool == "tools.retire" && !e.ok),
        "{ev:?}"
    );

    // The next sweep, archive fixed, archives the retired file in place.
    fs::remove_file(&blocker).unwrap();
    let done = ts.sweep(&mem, now, &|_| 0).await.unwrap();
    let to = ts.try_archive_path("luma.com", "register", 1).unwrap();
    assert!(
        matches!(&done[..], [SweepAction::Archived { name, version: 1, to: t, .. }]
            if name == "register" && *t == to),
        "{done:?}"
    );
    assert!(!reg.exists());
    assert_eq!(fs::read_to_string(&to).unwrap(), content);
    let ev = mem.events();
    assert_eq!(ev.len(), 3, "{ev:#?}");
    assert_eq!(ev[2].tool, "tools.archive");
    assert!(ev[2].ok, "{:?}", ev[2]);
    assert_eq!(
        ev[2].consequences["archived"],
        "_archive/luma.com/register.1.js"
    );
}

/// Rows 4 and 5 (D-316, D-303) through sweep: an idle ephemeral tool whose archive step
/// fails is reported as Retired { archived: None }, the row is saved retired, the file stays,
/// load_live does not return it (sync load does). Ledger as for record. The next sweep, with
/// the archive fixed, archives the file with an ok:true tools.archive.
#[tokio::test]
async fn r4_r5_sweep_retire_with_failed_archive() {
    let (_dir, ts) = sandbox();
    no_generic(&ts);
    let mem = Mem::default();
    let now = Utc::now();
    mem.put("luma.com", "list_events", ToolStatus::Ephemeral, 1);
    let mut h = mem.get("luma.com", "list_events").unwrap();
    h.last_ok = Some(now - Duration::days(EPHEMERAL_IDLE_DAYS + 1));
    mem.set(h);
    let le = file(&ts, "luma.com", "list_events");
    let blocker = break_archive(&ts);

    let done = ts.sweep(&mem, now, &|_| 0).await.expect("sweep is Ok");
    assert!(
        matches!(&done[..], [SweepAction::Retired { domain, name, version: 1, archived: None, why }]
            if domain == "luma.com" && name == "list_events" && why.contains("unused")),
        "{done:?}"
    );
    assert_eq!(
        mem.get("luma.com", "list_events").unwrap().status,
        ToolStatus::Retired
    );
    assert!(le.is_file());

    let r = ts.scan_live(&mem, "luma.com", "/").await.unwrap();
    assert_eq!(names(&r.tools), ["luma.register"]);
    assert_eq!(r.skipped, vec![(le.clone(), "retired (v1)".to_string())]);
    assert_eq!(
        names(&ts.load("luma.com", "/").unwrap()),
        ["luma.list_events", "luma.register"]
    );

    let ev = mem.events();
    assert_eq!(ev.len(), 2, "{ev:#?}");
    let (r, a) = (&ev[0], &ev[1]);
    assert_eq!((r.tool.as_str(), r.ok), ("tools.retire", true), "{r:?}");
    assert!(r.consequences["archived"].is_null(), "{r:?}");
    assert_eq!(r.consequences["archive_failed"], "luma.com/list_events.js");
    assert_eq!((a.tool.as_str(), a.ok), ("tools.archive", false), "{a:?}");
    assert_eq!(a.consequences["failed"], "luma.com/list_events.js");
    assert!(a.why.as_deref().is_some_and(|w| !w.is_empty()), "{a:?}");
    assert!(
        a.result_summary.contains("luma.com/list_events.js"),
        "{a:?}"
    );

    // Still broken: the next sweep tries again (archive step only, no second retire).
    let again = ts.sweep(&mem, now, &|_| 0).await.unwrap();
    assert!(again.is_empty(), "{again:?}");
    let ev = mem.events();
    assert_eq!(ev.len(), 3, "{ev:#?}");
    assert_eq!((ev[2].tool.as_str(), ev[2].ok), ("tools.archive", false));
    assert_eq!(ev.iter().filter(|e| e.tool == "tools.retire").count(), 1);

    fs::remove_file(&blocker).unwrap();
    let fixed = ts.sweep(&mem, now, &|_| 0).await.unwrap();
    let to = ts.try_archive_path("luma.com", "list_events", 1).unwrap();
    assert!(
        matches!(&fixed[..], [SweepAction::Archived { name, to: t, .. }]
            if name == "list_events" && *t == to),
        "{fixed:?}"
    );
    assert!(!le.exists() && to.is_file());
    let last = mem.events().pop().unwrap();
    assert_eq!(
        (last.tool.as_str(), last.ok),
        ("tools.archive", true),
        "{last:?}"
    );
    assert_eq!(
        last.consequences["archived"],
        "_archive/luma.com/list_events.1.js"
    );
}

/// Row 5 (D-316): when the archive step succeeds, record's retire writes exactly one ok:true
/// tools.retire whose consequences are `{archived: <path>}` (no archive_failed, no
/// tools.archive event), and sweep's retire likewise reports Retired { archived: Some }.
#[tokio::test]
async fn r5_successful_archive_is_one_ok_retire_with_the_path() {
    let (_dir, ts) = sandbox();
    no_generic(&ts);
    let mem = Mem::default();
    let now = Utc::now();
    mem.put("luma.com", "register", ToolStatus::NeedsRepair, 1);
    let mut h = mem.get("luma.com", "register").unwrap();
    h.fails = MISSES_TO_RETIRE - 1;
    mem.set(h);
    let res = ToolRunResult {
        domain: "luma.com".into(),
        name: "register".into(),
        outcome: miss(),
        at: now,
    };
    let h = ts.record(&mem, res).await.unwrap();
    assert_eq!(h.status, ToolStatus::Retired);
    let to = ts.try_archive_path("luma.com", "register", 1).unwrap();
    assert!(to.is_file() && !file(&ts, "luma.com", "register").exists());
    let ev = mem.events();
    assert_eq!(ev.len(), 1, "{ev:#?}");
    assert_eq!((ev[0].tool.as_str(), ev[0].ok), ("tools.retire", true));
    assert_eq!(
        ev[0].consequences,
        json!({"archived": "_archive/luma.com/register.1.js"})
    );

    mem.put("luma.com", "list_events", ToolStatus::Ephemeral, 1);
    let mut h = mem.get("luma.com", "list_events").unwrap();
    h.last_ok = Some(now - Duration::days(EPHEMERAL_IDLE_DAYS + 1));
    mem.set(h);
    let done = ts.sweep(&mem, now, &|_| 0).await.unwrap();
    let to = ts.try_archive_path("luma.com", "list_events", 1).unwrap();
    assert!(
        matches!(&done[..], [SweepAction::Retired { name, archived: Some(a), .. }]
            if name == "list_events" && *a == to),
        "{done:?}"
    );
    let ev = mem.events();
    assert_eq!(ev.len(), 2, "{ev:#?}");
    assert_eq!((ev[1].tool.as_str(), ev[1].ok), ("tools.retire", true));
    assert_eq!(
        ev[1].consequences,
        json!({"archived": "_archive/luma.com/list_events.1.js"})
    );
    // Archived, so neither loader lists it and nothing is reported skipped.
    let r = ts.scan_live(&mem, "luma.com", "/").await.unwrap();
    assert!(r.tools.is_empty() && r.skipped.is_empty(), "{r:?}");
}

// ---------------------------------------------------------------- row 6

/// Row 6 (IC-305): scan_live / load_live take `&dyn MemoryStore` and return
/// `Result<LoadReport, StoreError>` / `Result<Vec<Tool>, StoreError>`, from outside the crate.
#[tokio::test]
async fn r6_live_loader_signatures() {
    let (_dir, ts) = sandbox();
    let mem = Mem::default();
    let store: &dyn MemoryStore = &mem;
    let s: std::result::Result<LoadReport, StoreError> = ts.scan_live(store, "luma.com", "/").await;
    let l: std::result::Result<Vec<Tool>, StoreError> = ts.load_live(store, "luma.com", "/").await;
    assert_eq!(s.unwrap().tools, l.unwrap());
}
