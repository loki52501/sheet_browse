//! Regression tests for review-T-11-core: each blocking item with the reviewer's failing
//! input (literal pathspecs, D-310 path validation, D-309 runtime vault check, D-310 ask held
//! until confirm), and the should-fix items (stop count, sweep idle clock, @match encodings,
//! read_table caps, retire/reforge ordering).

use crate::*;
use async_trait::async_trait;
use chrono::{Duration, TimeZone, Utc};
use harness_core::memory::*;
use harness_core::run::RunId;
use harness_core::Class;
use sb_act::ActClass;
use sb_sheet::IndexRow;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

// ---------------------------------------------------------------- fake store

#[derive(Default)]
struct Store {
    health: Mutex<Vec<ToolHealth>>,
    ledger: Mutex<Vec<LedgerEvent>>,
    /// When set, `tool_health_set` fails (to test write ordering).
    fail_set: AtomicBool,
    /// When set, the next `tool_health_set` first turns this file into a directory, so a
    /// rollback that writes the file back fails too (a fault injected after the archive move).
    block_path: Mutex<Option<PathBuf>>,
    /// When set, `tool_health` fails (a memory error while loading, D-315).
    fail_get: AtomicBool,
    /// When set, the next `tool_health` call first writes this file (path, source): a tool
    /// file that changes while the rows are being read (D-315 read order).
    late_file: Mutex<Option<(PathBuf, String)>>,
}

impl Store {
    fn row(&self, domain: &str, name: &str) -> Option<ToolHealth> {
        self.health
            .lock()
            .unwrap()
            .iter()
            .find(|h| h.domain == domain && h.name == name)
            .cloned()
    }
    fn put(&self, h: ToolHealth) {
        let mut v = self.health.lock().unwrap();
        v.retain(|x| !(x.domain == h.domain && x.name == h.name));
        v.push(h);
    }
    fn ledger_len(&self) -> usize {
        self.ledger.lock().unwrap().len()
    }
}

#[async_trait]
impl MemoryStore for Store {
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
        if let Some((p, src)) = self.late_file.lock().unwrap().take() {
            fs::write(p, src).unwrap();
        }
        if self.fail_get.load(Ordering::SeqCst) {
            return Err(MemoryError::Invalid("tool_health refused (test)".into()));
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
        if let Some(p) = self.block_path.lock().unwrap().take() {
            fs::remove_file(&p).unwrap();
            fs::create_dir(&p).unwrap();
        }
        if self.fail_set.load(Ordering::SeqCst) {
            return Err(MemoryError::Invalid(
                "tool_health_set refused (test)".into(),
            ));
        }
        self.put(h);
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
// @gate Submit
// @fingerprint button"Register" input"Email"
export default function (page, args) {
  const email = page.find('input "Email"');
  const btn = page.find('button "Register"');
  return `t#${email}=${q(args.email)}; c#${btn}; ?s`;
}
"#;

fn put(root: &Path, rel: &str, src: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, src).unwrap();
}

fn luma() -> (tempfile::TempDir, ToolStore) {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), "luma.com/register.js", REGISTER);
    let ts = ToolStore::new(dir.path());
    (dir, ts)
}

fn row(status: ToolStatus, version: i32) -> ToolHealth {
    ToolHealth {
        domain: "luma.com".into(),
        name: "register".into(),
        version,
        status,
        runs: 0,
        fails: 0,
        last_ok: None,
        last_fail: None,
        why: None,
        pinned: false,
    }
}

fn res(name: &str, outcome: RunOutcome) -> ToolRunResult {
    ToolRunResult {
        domain: "luma.com".into(),
        name: name.into(),
        outcome,
        at: Utc::now(),
    }
}

fn miss() -> RunOutcome {
    RunOutcome::FingerprintMiss {
        why: "button\"Register\" not found".into(),
    }
}

fn bad_path<T: std::fmt::Debug>(r: std::result::Result<T, StoreError>) -> bool {
    matches!(r, Err(StoreError::BadPath(_)))
}

// ---------------------------------------------------------------- blocking 1: git pathspecs

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

#[test]
fn git_commit_refuses_pathspec_magic_and_never_commits_outside_root() {
    let repo = tempfile::tempdir().unwrap();
    let r = repo.path();
    if git(r, &["init", "-q"]).is_none() {
        eprintln!("skipped: git not available");
        return;
    }
    git(r, &["config", "user.email", "t@example.invalid"]).unwrap();
    git(r, &["config", "user.name", "toolstore test"]).unwrap();
    git(r, &["config", "commit.gpgsign", "false"]).unwrap();
    // The reviewer's setup: tools/ is the root, an untracked file sits next to it.
    fs::write(r.join("outside.txt"), "not a tool").unwrap();
    put(r, "tools/luma.com/a.js", "a");
    put(r, "tools/luma.com/ab.js", "ab");
    let root = r.join("tools");
    let ts = ToolStore::new(&root);

    for magic in [":/", ":(top)x", ":(glob)**", "luma.com/a.js:x"] {
        let e = ts.git_commit(&[PathBuf::from(magic)], "x (T-1)");
        assert!(matches!(e, Err(StoreError::Git(_))), "{magic}: {e:?}");
        assert!(
            git(r, &["rev-parse", "--verify", "-q", "HEAD"]).is_none(),
            "{magic}"
        );
    }
    // --literal-pathspecs: a glob is a file name, which does not exist.
    let e = ts.git_commit(&[root.join("luma.com/a*")], "x (T-1)");
    assert!(e.is_err(), "{e:?}");
    assert!(git(r, &["rev-parse", "--verify", "-q", "HEAD"]).is_none());

    ts.git_commit(&[root.join("luma.com/a.js")], "tools: add (T-1)")
        .unwrap();
    let files = git(r, &["show", "--name-only", "--format=", "HEAD"]).unwrap();
    assert_eq!(files.trim(), "tools/luma.com/a.js");
    let status = git(r, &["status", "--porcelain", "-uall"]).unwrap();
    assert!(status.contains("?? outside.txt"), "{status}");
    assert!(status.contains("?? tools/luma.com/ab.js"), "{status}");
}

// ---------------------------------------------------------------- blocking 2: D-310 paths

#[test]
fn validators_accept_real_names_and_refuse_traversal() {
    for d in [
        "luma.com",
        "ashbyhq.com",
        "mail.google.com",
        "user.github.io",
        "127.0.0.1",
        "_generic",
    ] {
        assert!(valid_domain(d), "{d}");
    }
    for d in [
        "",
        ".",
        "..",
        "../x",
        "..\\..",
        "\\windows",
        "C:\\windows",
        "c:",
        "\\\\server\\share",
        "/etc",
        "_archive",
        "_x",
        ".git",
        "Luma.com",
        "bücher.de",
        "luma.com.",
        "a/b",
        ":/",
        "con",
        "nul.txt",
    ] {
        assert!(!valid_domain(d), "{d:?}");
    }
    for n in ["list_jobs", "read_table", "register", "a-b", "x2"] {
        assert!(valid_name(n), "{n}");
    }
    for n in [
        // D-312: lower case only.
        "listJobs",
        "Register",
        "REGISTER",
        "",
        "../../evil",
        "..",
        "a.b",
        "a/b",
        "a\\b",
        ":x",
        "con",
        "a b",
    ] {
        assert!(!valid_name(n), "{n:?}");
    }
}

/// D-72: the deprecated builders keep their wave-0 behaviour, the empty path on bad input
/// (including an upper-case name, D-312). The one test that still calls them.
#[test]
#[allow(deprecated)]
fn deprecated_path_builders_return_the_empty_path_on_bad_input() {
    let (dir, ts) = luma();
    assert_eq!(ts.tool_path("luma.com", "../../evil"), PathBuf::new());
    assert_eq!(ts.tool_path("..\\..", "evil"), PathBuf::new());
    assert_eq!(ts.tool_path("\\windows", "x"), PathBuf::new());
    assert_eq!(ts.tool_path("luma.com", "Register"), PathBuf::new());
    assert_eq!(ts.archive_path("..", "x", 1), PathBuf::new());
    assert_eq!(ts.archive_path("luma.com", "Register", 1), PathBuf::new());
    assert_eq!(
        ts.tool_path("luma.com", "register"),
        dir.path().join("luma.com").join("register.js")
    );
    assert_eq!(
        ts.archive_path("luma.com", "register", 3),
        dir.path()
            .join("_archive")
            .join("luma.com")
            .join("register.3.js")
    );
}

#[test]
fn path_builders_never_leave_the_root() {
    let (dir, ts) = luma();
    // Reviewer probes.
    assert!(bad_path(ts.try_tool_path("luma.com", "../../evil")));
    assert!(bad_path(ts.try_tool_path("..\\..", "evil")));
    assert!(bad_path(ts.try_tool_path("\\windows", "x")));
    assert!(bad_path(ts.try_tool_path("luma.com", "Register")));
    assert!(bad_path(ts.try_archive_path("..", "x", 1)));
    assert!(bad_path(ts.try_tool_path("C:\\windows", "x")));
    assert!(bad_path(ts.try_archive_path("_archive", "x", 1)));
    assert!(bad_path(ts.archive("..", "evil", 1)));
    assert_eq!(
        ts.try_tool_path("luma.com", "register").unwrap(),
        dir.path().join("luma.com").join("register.js")
    );
}

#[test]
fn folder_for_returns_only_generic_or_a_site_folder() {
    let (dir, ts) = luma();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    fs::write(
        dir.path().join(".git/hook.js"),
        "// @name hook\n// @kind reader\n// @gate Read\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("_archive/luma.com")).unwrap();
    for bad in [
        "\\windows",
        "..\\..\\x",
        "../../x",
        "\\\\server\\share",
        ".git",
        "_archive",
        "_anything",
        "BÜCHER.de",
        "bücher.de",
        "[::1]",
        "",
    ] {
        assert_eq!(ts.folder_for(bad), "", "{bad:?}");
    }
    let c = ts.folder_for("C:\\windows");
    assert!(c.is_empty() || valid_domain(&c), "{c:?}");
    assert_eq!(ts.folder_for("_generic"), "_generic");
    assert_eq!(ts.folder_for("LUMA.com"), "luma.com");
    // Private public-suffix section (D-300): user.github.io is its own site.
    assert_eq!(ts.folder_for("user.github.io"), "user.github.io");
    assert_eq!(ts.folder_for("https://user.github.io/x"), "user.github.io");
    // scan never reads a reserved folder.
    assert!(ts.scan(".git", "/").tools.is_empty());
    assert!(ts.scan("_archive", "/").tools.is_empty());
    assert!(ts.scan("..\\..", "/").tools.is_empty());
}

#[tokio::test]
async fn mutators_refuse_bad_domain_or_name() {
    let (dir, ts) = luma();
    let store = Store::default();
    let r = ts
        .record(
            &store,
            ToolRunResult {
                domain: "luma.com".into(),
                name: "../../evil".into(),
                outcome: RunOutcome::Ok,
                at: Utc::now(),
            },
        )
        .await;
    assert!(bad_path(r));
    let r = ts
        .reforge(&store, "luma.com", "../evil", REGISTER, Utc::now())
        .await;
    assert!(bad_path(r));
    let r = ts
        .reforge(&store, "..\\..", "register", REGISTER, Utc::now())
        .await;
    assert!(bad_path(r));
    assert!(bad_path(ts.register_forged(&store, "\\windows", "x").await));
    assert!(bad_path(
        ts.register_forged(&store, "luma.com", "a/b").await
    ));
    assert!(bad_path(ts.confirm(&store, "_archive", "register").await));
    assert!(bad_path(
        ts.set_pinned(&store, "luma.com", "..", true).await
    ));
    assert!(store.health.lock().unwrap().is_empty());
    assert_eq!(store.ledger_len(), 0);
    assert!(!dir.path().join("evil.js").exists());
    assert!(!dir.path().parent().unwrap().join("evil.js").exists());

    // sweep skips a row whose name is not a file name instead of acting on it.
    let mut h = row(ToolStatus::Retired, 1);
    h.name = "../evil".into();
    store.put(h);
    let done = ts
        .sweep(&store, Utc::now() + Duration::days(400), &|_| 0)
        .await
        .unwrap();
    assert!(done.is_empty(), "{done:?}");
}

// ---------------------------------------------------------------- blocking 3: D-309

struct Submit;
impl PlannerCheck for Submit {
    fn check(&self, _: &str, _: &[IndexRow]) -> std::result::Result<ActClass, String> {
        Ok(ActClass {
            class: Class::Submit,
            hints: vec![],
        })
    }
}

fn planner(src: &str) -> Tool {
    Tool {
        domain: "luma.com".into(),
        namespaced: "luma.register".into(),
        path: PathBuf::from("register.js"),
        header: parse_header(src).unwrap(),
        source: src.to_string(),
    }
}

#[test]
fn concatenated_vault_key_passes_static_check_but_not_planner_class() {
    // The reviewer's re-forge: `@vau` + `lt(bank_pin)`.
    let new = REGISTER.replace(
        "return `t#${email}=${q(args.email)}; c#${btn}; ?s`;",
        "return `t#1=@vau` + `lt(bank_pin); c#${btn}; ?s`;",
    );
    assert_ne!(new, REGISTER);
    // The text check cannot see it (it stays as the early check, D-309)...
    check_repair(REGISTER, &new, RepairKind::Reforge).unwrap();
    // ...the emitted program is checked against the source's literal keys.
    let program = "t#1=@vault(bank_pin); c#4; ?s";
    let e = planner_class(&planner(&new), program, &[], &Submit);
    assert!(
        matches!(&e, Err(StoreError::Invariant(m)) if m.contains("@vault(bank_pin)") && m.contains("D-309")),
        "{e:?}"
    );
    // A key the source spells out is fine.
    let lit = REGISTER.replace("const btn", "const pw = '@vault(luma_pw)';\n  const btn");
    assert_eq!(
        planner_class(&planner(&lit), "t#1=@vault(luma_pw); c#4; ?s", &[], &Submit).unwrap(),
        Class::Submit
    );
    // A key taken from args (`@vault(${args.k})`) is not a literal either.
    let from_args = REGISTER.replace("const btn", "const pw = `@vault(${args.k})`;\n  const btn");
    assert!(planner_class(
        &planner(&from_args),
        "t#1=@vault(luma_pw); c#4; ?s",
        &[],
        &Submit
    )
    .is_err());
}

// ---------------------------------------------------------------- blocking 4: D-310 ask

async fn asked() -> (tempfile::TempDir, ToolStore, Store, String) {
    let (dir, ts) = luma();
    let store = Store::default();
    store.put(row(ToolStatus::NeedsRepair, 1));
    let pay = REGISTER.replace("@gate Submit", "@gate Pay");
    let h = ts
        .reforge(&store, "luma.com", "register", &pay, Utc::now())
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Ask, 2));
    (dir, ts, store, pay)
}

#[tokio::test]
async fn ask_survives_a_second_reforge_at_the_same_floor() {
    let (_d, ts, store, pay) = asked().await;
    let again = pay.replace("const btn", "const x = 1;\n  const btn");
    let h = ts
        .reforge(&store, "luma.com", "register", &again, Utc::now())
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Ask, 3));
    let h = ts.confirm(&store, "luma.com", "register").await.unwrap();
    assert_eq!(h.status, ToolStatus::Repaired);
}

#[tokio::test]
async fn ask_survives_relocate() {
    let (_d, ts, store, _) = asked().await;
    let fp = vec![
        FpItem {
            role: "button".into(),
            name: Some("Sign up".into()),
            id: None,
        },
        FpItem {
            role: "input".into(),
            name: Some("Email".into()),
            id: None,
        },
    ];
    let h = ts
        .record(
            &store,
            res("register", RunOutcome::Relocated { fingerprint: fp }),
        )
        .await
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Ask, 3));
    let src = fs::read_to_string(ts.try_tool_path("luma.com", "register").unwrap()).unwrap();
    assert!(src.contains("button\"Sign up\""), "{src}");
}

#[tokio::test]
async fn ask_survives_miss_then_clean_runs() {
    let (_d, ts, store, _) = asked().await;
    let h = ts.record(&store, res("register", miss())).await.unwrap();
    assert_eq!((h.status, h.fails), (ToolStatus::Ask, 1));
    for _ in 0..4 {
        ts.record(&store, res("register", RunOutcome::Ok))
            .await
            .unwrap();
    }
    let h = store.row("luma.com", "register").unwrap();
    assert_eq!((h.status, h.runs), (ToolStatus::Ask, 4));
    // Misses past the retire threshold do not retire an ask row either.
    for _ in 0..6 {
        ts.record(&store, res("register", miss())).await.unwrap();
    }
    assert_eq!(
        store.row("luma.com", "register").unwrap().status,
        ToolStatus::Ask
    );
    assert!(ts.try_tool_path("luma.com", "register").unwrap().is_file());
}

#[tokio::test]
async fn relocate_on_retired_row_is_refused() {
    let (_d, ts) = luma();
    let store = Store::default();
    store.put(row(ToolStatus::Retired, 4));
    let r = ts
        .record(
            &store,
            res(
                "register",
                RunOutcome::Relocated {
                    fingerprint: vec![],
                },
            ),
        )
        .await;
    assert!(matches!(r, Err(StoreError::Invariant(_))), "{r:?}");
    assert_eq!(store.row("luma.com", "register").unwrap().version, 4);
}

// ---------------------------------------------------------------- should-fix: ?s count

#[test]
fn stop_count_ignores_comments_and_query_strings() {
    assert_eq!(stop_count(REGISTER), 1);
    // The stop moved into a comment no longer counts.
    let commented = REGISTER.replace("c#${btn}; ?s`;", "c#${btn}`; // ?s");
    assert_eq!(stop_count(&commented), 0);
    assert!(matches!(
        check_repair(REGISTER, &commented, RepairKind::Reforge),
        Err(StoreError::Invariant(m)) if m.contains("removed ?s")
    ));
    let block = REGISTER.replace("c#${btn}; ?s`;", "c#${btn}`; /* ?s */");
    assert_eq!(stop_count(&block), 0);
    // A query string is not a stop, so it cannot stand in for one.
    let query = REGISTER.replace("c#${btn}; ?s`;", "c#${btn}`;\n  const u = '/list?sort=1';");
    assert_eq!(stop_count(&query), 0);
    assert!(check_repair(REGISTER, &query, RepairKind::Reforge).is_err());
    // `//` inside a string is not a comment; `?s` at the end of a template counts.
    let url = REGISTER.replace("const btn", "const u = 'https://lu.ma/x';\n  const btn");
    assert_eq!(stop_count(&url), 1);
    assert_eq!(
        stop_count("// @name x\nconst a = `c#${f({a: 1})}; ?s`;\n"),
        1
    );
}

// ---------------------------------------------------------------- should-fix: sweep clock

fn ephemeral(name: &str) -> ToolHealth {
    ToolHealth {
        name: name.into(),
        ..row(ToolStatus::Ephemeral, 1)
    }
}

#[tokio::test]
async fn sweep_idle_clock_is_the_later_of_last_ok_and_last_fail() {
    let (dir, ts) = luma();
    let store = Store::default();
    let now = Utc::now();
    let mut h = ephemeral("register");
    h.last_ok = Some(now - Duration::days(20));
    h.last_fail = Some(now - Duration::days(1));
    store.put(h);
    let done = ts.sweep(&store, now, &|_| 0).await.unwrap();
    assert!(done.is_empty(), "used yesterday, retired: {done:?}");

    // Never run: the later of the @forged date and the file mtime counts.
    let file = |name: &str, forged: &str, mtime: Option<Ts>| {
        put(
            dir.path(),
            &format!("luma.com/{name}.js"),
            &format!(
                "// @name {name}\n// @kind reader\n// @gate Read\n// @forged {forged} task=T-31\n"
            ),
        );
        if let Some(t) = mtime {
            fs::File::options()
                .write(true)
                .open(ts.try_tool_path("luma.com", name).unwrap())
                .unwrap()
                .set_modified(t.into())
                .unwrap();
        }
        store.put(ephemeral(name));
    };
    let long_ago = Utc.with_ymd_and_hms(2021, 3, 4, 0, 0, 0).unwrap();
    let today = now.format("%Y-%m-%d").to_string();
    // Both old: retired, dated by the later one (the mtime).
    file("old", "2020-01-02", Some(long_ago));
    // Old @forged, freshly written file: not idle.
    file("rewritten", "2020-01-02", None);
    // Fresh @forged, old mtime (copied with its times kept): not idle.
    file("fresh", &today, Some(long_ago));
    let done = ts.sweep(&store, now, &|_| 0).await.unwrap();
    assert_eq!(done.len(), 1, "{done:?}");
    assert!(
        matches!(&done[0], SweepAction::Retired { name, why, .. } if name == "old" && why.contains("2021-03-04")),
        "{done:?}"
    );
}

// ---------------------------------------------------------------- should-fix: @match

#[test]
fn match_rejects_backslash_and_percent_encodings() {
    for m in [
        "/\\evil.com/*",
        "/%2e%2e/%2f%2fevil.com",
        "/%2E%2E/x",
        "/%2F%2Fevil.com/*",
        "/a%5cb",
        "/a%5Cb",
    ] {
        let new = REGISTER.replace("@match /*", &format!("@match {m}"));
        assert!(
            matches!(check_repair(REGISTER, &new, RepairKind::Reforge),
                Err(StoreError::Invariant(e)) if e.contains("@match widened")),
            "{m}"
        );
        assert!(parse_header(&new).is_err(), "{m}");
    }
    let ok = REGISTER.replace("@match /*", "@match /events/%20x/*");
    check_repair(REGISTER, &ok, RepairKind::Reforge).unwrap();
}

// ---------------------------------------------------------------- should-fix: read_table caps

/// A 30-column, 80-row table whose cells are `.0` characters long.
struct WideTable(usize);
impl PageHost for WideTable {
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
        if id != "0" {
            return serde_json::Value::Null;
        }
        let long = "x".repeat(self.0);
        let header: Vec<String> = (0..30).map(|i| format!("h{i}")).collect();
        let rows: Vec<Vec<String>> = (0..80).map(|_| vec![long.clone(); 30]).collect();
        serde_json::json!({"header": header, "rows": rows})
    }
    fn url(&self) -> String {
        "https://example.org/t".into()
    }
    fn title(&self) -> String {
        "t".into()
    }
}

fn read_table() -> Tool {
    let ts = ToolStore::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools"));
    ts.load("example.org", "/t")
        .unwrap()
        .into_iter()
        .find(|t| t.header.name == "read_table")
        .unwrap()
}

#[test]
fn read_table_caps_rows_columns_and_cells() {
    let tool = read_table();
    // Long cells: each cut to 200 chars, 20 columns, and rows stop at the 8000-char budget.
    let out = run_reader(
        &tool,
        &WideTable(500),
        &serde_json::json!({"max": 500}),
        None,
    )
    .unwrap();
    assert_eq!(out["truncated"], true);
    assert_eq!(out["total"], 80);
    assert_eq!(out["columns"], 30);
    assert_eq!(out["header"].as_array().unwrap().len(), 20);
    let rows = out["rows"].as_array().unwrap();
    assert!(!rows.is_empty() && rows.len() < 50, "{}", rows.len());
    for r in rows {
        let cells = r.as_array().unwrap();
        assert_eq!(cells.len(), 20);
        assert!(cells
            .iter()
            .all(|c| c.as_str().unwrap().chars().count() <= 200));
    }
    assert!(out.to_string().len() < 10_000, "{}", out.to_string().len());
    // Short cells: rows capped at 50 even when max asks for more.
    let out = run_reader(&tool, &WideTable(3), &serde_json::json!({"max": 500}), None).unwrap();
    assert_eq!(out["truncated"], true);
    assert_eq!(out["rows"].as_array().unwrap().len(), 50);
    assert_eq!(out["rows"][0], serde_json::json!(vec!["xxx"; 20]));
}

/// A one-row table whose only cell is `.0`.
struct OneCell(String);
impl PageHost for OneCell {
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
        if id != "0" {
            return serde_json::Value::Null;
        }
        serde_json::json!({"header": ["c"], "rows": [[self.0]]})
    }
    fn url(&self) -> String {
        "https://example.org/t".into()
    }
    fn title(&self) -> String {
        "t".into()
    }
}

/// Review-T-11 should-fix 2: the 200-unit cell cut landed between the halves of U+1F600,
/// and the lone surrogate made the whole read a `BadReturn`.
#[test]
fn read_table_cell_cut_keeps_surrogate_pairs_whole() {
    let tool = read_table();
    let cell = |host: OneCell| {
        let out = run_reader(&tool, &host, &serde_json::json!({}), None).unwrap();
        assert_eq!(out["truncated"], true);
        out["rows"][0][0].as_str().unwrap().to_string()
    };
    // The pair sits at UTF-16 units 198..200: cutting at 199 would split it, so it goes.
    let split = format!("{}\u{1F600}tail{}", "a".repeat(198), "b".repeat(50));
    assert_eq!(cell(OneCell(split)), format!("{}…", "a".repeat(198)));
    // The pair sits at units 197..199: it fits whole and stays.
    let whole = format!("{}\u{1F600}tail{}", "a".repeat(197), "b".repeat(50));
    assert_eq!(
        cell(OneCell(whole)),
        format!("{}\u{1F600}…", "a".repeat(197))
    );
}

// ---------------------------------------------------------------- should-fix: write order

#[tokio::test]
async fn retire_persists_the_row_before_moving_the_file() {
    let (dir, ts) = luma();
    let store = Store::default();
    let mut h = row(ToolStatus::NeedsRepair, 1);
    h.fails = MISSES_TO_RETIRE - 1;
    store.put(h);
    // A file where the archive folder should be: the move fails.
    fs::write(dir.path().join("_archive"), "blocker").unwrap();
    // D-316: the retire happened (the row says so), only the move failed: record is Ok.
    let r = ts.record(&store, res("register", miss())).await;
    let r = r.unwrap_or_else(|e| panic!("a failed archive step failed the retire: {e}"));
    assert_eq!(r.status, ToolStatus::Retired);
    let h = store.row("luma.com", "register").unwrap();
    assert_eq!(h.status, ToolStatus::Retired);
    assert!(ts.try_tool_path("luma.com", "register").unwrap().is_file());
    assert_retired_with_failed_archive(&store, "register", "luma.com/register.js", None);
    // Recoverable: the next sweep archives the retired file in place.
    fs::remove_file(dir.path().join("_archive")).unwrap();
    let done = ts.sweep(&store, Utc::now(), &|_| 0).await.unwrap();
    assert!(
        matches!(&done[..], [SweepAction::Archived { name, .. }] if name == "register"),
        "{done:?}"
    );
    assert!(!ts.try_tool_path("luma.com", "register").unwrap().exists());
    let last = store.ledger.lock().unwrap().last().cloned().unwrap();
    assert_eq!((last.tool.as_str(), last.ok), ("tools.archive", true));
}

/// D-316: the events of a retire whose archive step failed, for tool `name` in the ledger: one
/// ok:true `tools.retire` (`archived: null`, `archive_failed: <rel>`), followed by one ok:false
/// `tools.archive` naming the file (and `err`, when given), and no ok:false `tools.retire`.
fn assert_retired_with_failed_archive(store: &Store, name: &str, rel: &str, err: Option<&str>) {
    let ev: Vec<LedgerEvent> = store
        .ledger
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.args["name"] == name)
        .cloned()
        .collect();
    let retire: Vec<usize> = (0..ev.len())
        .filter(|&i| ev[i].tool == "tools.retire")
        .collect();
    assert_eq!(retire.len(), 1, "{ev:?}");
    let r = &ev[retire[0]];
    assert!(r.ok, "{r:?}");
    assert_eq!(r.server, "core");
    assert_eq!(
        r.consequences.get("archived"),
        Some(&serde_json::Value::Null),
        "{r:?}"
    );
    assert_eq!(r.consequences["archive_failed"], rel, "{r:?}");
    assert!(r.result_summary.contains(rel), "{r:?}");
    assert!(
        r.why
            .as_deref()
            .is_some_and(|w| w.contains("retired") || w.contains("unused")),
        "{r:?}"
    );
    let a = ev
        .get(retire[0] + 1)
        .unwrap_or_else(|| panic!("no tools.archive after the retire: {ev:?}"));
    assert_eq!(a.tool, "tools.archive", "{ev:?}");
    assert!(!a.ok, "{a:?}");
    assert_eq!(a.consequences["failed"], rel, "{a:?}");
    assert!(a.result_summary.contains(rel), "{a:?}");
    if let Some(err) = err {
        assert!(a.result_summary.contains(err), "{a:?}");
        assert!(a.why.as_deref().is_some_and(|w| w.contains(err)), "{a:?}");
    }
}

/// D-316: record() reaching 5 misses retires the tool even when the archive's set-mtime
/// step fails: Ok with the row retired and saved, the file in place, an ok:true
/// `tools.retire` and an ok:false `tools.archive` event; the next sweep archives the file.
#[tokio::test]
async fn record_retire_with_a_failed_archive_is_ok() {
    let (dir, ts) = luma();
    let store = Store::default();
    let mut h = row(ToolStatus::NeedsRepair, 2);
    h.fails = MISSES_TO_RETIRE - 1;
    store.put(h);
    let refuse = |_: &Path| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "set mtime refused (test)",
        ))
    };
    let r = ts
        .record_with(&store, res("register", miss()), &refuse)
        .await;
    let r = r.unwrap_or_else(|e| panic!("a failed archive step failed record: {e}"));
    assert_eq!(r.status, ToolStatus::Retired);
    assert_eq!(r.fails, MISSES_TO_RETIRE);
    assert_eq!(store.row("luma.com", "register").unwrap(), r);
    let file = ts.try_tool_path("luma.com", "register").unwrap();
    assert!(file.is_file());
    assert!(!dir.path().join("_archive/luma.com/register.2.js").exists());
    assert_retired_with_failed_archive(
        &store,
        "register",
        "luma.com/register.js",
        Some("set mtime refused"),
    );
    assert_eq!(store.ledger_len(), 2);

    let done = ts.sweep(&store, Utc::now(), &|_| 0).await.unwrap();
    assert!(
        matches!(&done[..], [SweepAction::Archived { name, version: 2, .. }] if name == "register"),
        "{done:?}"
    );
    assert!(!file.exists());
    assert!(dir.path().join("_archive/luma.com/register.2.js").is_file());
    let last = store.ledger.lock().unwrap().last().cloned().unwrap();
    assert_eq!((last.tool.as_str(), last.ok), ("tools.archive", true));
}

#[tokio::test]
async fn reforge_rolls_back_when_the_row_cannot_be_written() {
    let (dir, ts) = luma();
    let store = Store::default();
    store.put(row(ToolStatus::NeedsRepair, 1));
    store.fail_set.store(true, Ordering::SeqCst);
    let new = REGISTER.replace("const btn", "const x = 1;\n  const btn");
    let r = ts
        .reforge(&store, "luma.com", "register", &new, Utc::now())
        .await;
    assert!(matches!(r, Err(StoreError::Memory(_))), "{r:?}");
    assert_eq!(
        fs::read_to_string(ts.try_tool_path("luma.com", "register").unwrap()).unwrap(),
        REGISTER
    );
    assert!(!ts
        .try_archive_path("luma.com", "register", 1)
        .unwrap()
        .exists());
    assert!(dir
        .path()
        .join("_archive/luma.com")
        .read_dir()
        .unwrap()
        .next()
        .is_none());
    assert_eq!(store.ledger_len(), 0);
    assert_eq!(store.row("luma.com", "register").unwrap().version, 1);
}

/// Review-T-11 should-fix 3: when the row write fails and putting the old file back fails
/// too, the archived copy is the only one left. It must stay, and the error must name both
/// failures and where the old version is.
#[tokio::test]
async fn reforge_keeps_the_archive_when_the_rollback_fails() {
    let (dir, ts) = luma();
    let store = Store::default();
    store.put(row(ToolStatus::NeedsRepair, 1));
    store.fail_set.store(true, Ordering::SeqCst);
    let path = ts.try_tool_path("luma.com", "register").unwrap();
    *store.block_path.lock().unwrap() = Some(path.clone());
    let new = REGISTER.replace(
        "const btn",
        "const x = 1;
  const btn",
    );
    let r = ts
        .reforge(&store, "luma.com", "register", &new, Utc::now())
        .await;
    let msg = match r {
        Err(StoreError::Io(e)) => e.to_string(),
        r => panic!("{r:?}"),
    };
    assert!(msg.contains("tool_health_set refused (test)"), "{msg}");
    assert!(
        msg.contains("restoring luma.com/register.js failed"),
        "{msg}"
    );
    assert!(msg.contains("_archive/luma.com/register.1.js"), "{msg}");
    assert!(path.is_dir(), "the injected fault is still in place");
    let archived = ts.try_archive_path("luma.com", "register", 1).unwrap();
    assert_eq!(fs::read_to_string(&archived).unwrap(), REGISTER);
    assert_eq!(store.ledger_len(), 0);
    assert_eq!(store.row("luma.com", "register").unwrap().version, 1);
    drop(dir);
}

/// D-312: a re-forge whose new @name is not the name it was called with is refused, even
/// when the old file (hand-edited) already carries that other @name, so check_repair's
/// old-vs-new rename check cannot catch it. No file or ledger write; the reason is logged to
/// `tools.health.why` like every check_repair refusal (T-9).
#[tokio::test]
async fn reforge_refuses_a_new_name_that_is_not_the_called_name() {
    let (dir, ts) = luma();
    let other = REGISTER.replace("@name register", "@name signup");
    let path = ts.try_tool_path("luma.com", "register").unwrap();
    fs::write(&path, &other).unwrap();
    let store = Store::default();
    store.put(row(ToolStatus::NeedsRepair, 1));
    let new = other.replace("const btn", "const x = 1;\n  const btn");
    let r = ts
        .reforge(&store, "luma.com", "register", &new, Utc::now())
        .await;
    assert!(
        matches!(&r, Err(StoreError::Invariant(m)) if m.contains("signup") && m.contains("D-312")),
        "{r:?}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), other);
    assert!(!dir.path().join("_archive").exists());
    let logged = store.row("luma.com", "register").unwrap();
    let why = logged.why.clone().unwrap_or_default();
    assert!(
        why.starts_with("rejected: ") && why.contains("signup") && why.contains("D-312"),
        "{why}"
    );
    assert_eq!(
        ToolHealth {
            why: None,
            ..logged
        },
        row(ToolStatus::NeedsRepair, 1),
        "status and counters untouched"
    );
    assert_eq!(store.ledger_len(), 0);
}

// ---------------------------------------------------------------- D-78 batch review

/// A stale ephemeral row (unused 20 days) for `name` in `domain`, with its tool file.
fn stale(root: &Path, domain: &str, name: &str, now: Ts) -> ToolHealth {
    put(
        root,
        &format!("{domain}/{name}.js"),
        &REGISTER.replace("@name register", &format!("@name {name}")),
    );
    ToolHealth {
        domain: domain.into(),
        name: name.into(),
        last_ok: Some(now - Duration::days(20)),
        ..row(ToolStatus::Ephemeral, 1)
    }
}

fn set_readonly(p: &Path, ro: bool) {
    let mut perm = fs::metadata(p).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(ro);
    fs::set_permissions(p, perm).unwrap();
}

/// Review should-fix 1 (b): an archive that fails for one tool is a ledger failure, not the
/// end of the sweep. Later tools in the same domain, later domains and the 90-day deletes are
/// still swept, and the next sweep archives the tools that failed. D-316: the idle ephemeral
/// tool whose archive step fails is still retired (ok:true `tools.retire`, ok:false
/// `tools.archive`, `SweepAction::Retired { archived: None }`).
#[tokio::test]
async fn sweep_records_a_failed_archive_and_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let ts = ToolStore::new(root);
    let store = Store::default();
    let now = Utc::now();
    // Rows in this order: the failing ones come first in their domain.
    store.put(stale(root, "luma.com", "aaa", now));
    put(root, "luma.com/bbb.js", REGISTER);
    store.put(ToolHealth {
        name: "bbb".into(),
        ..row(ToolStatus::Retired, 2)
    });
    store.put(stale(root, "luma.com", "register", now));
    store.put(stale(root, "zeta.com", "list", now));
    put(root, "_archive/luma.com/gone.1.js", "old");
    let gone = root.join("_archive/luma.com/gone.1.js");
    fs::File::options()
        .write(true)
        .open(&gone)
        .unwrap()
        .set_modified((now - Duration::days(100)).into())
        .unwrap();

    let fails = |p: &Path| {
        let f = p.file_name().unwrap().to_string_lossy();
        if f == "aaa.js" || f == "bbb.js" {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "set mtime refused (test)",
            ))
        } else {
            crate::archive::touch(p)
        }
    };
    let done = ts.sweep_with(&store, now, &|_| 0, &fails).await;
    let done = done.unwrap_or_else(|e| panic!("one failed archive stopped the sweep: {e}"));
    let retired: Vec<_> = done
        .iter()
        .filter_map(|a| match a {
            SweepAction::Retired {
                domain,
                name,
                archived,
                ..
            } => Some((format!("{domain}/{name}"), archived.is_some())),
            _ => None,
        })
        .collect();
    assert_eq!(
        retired,
        [
            ("luma.com/aaa".to_string(), false),
            ("luma.com/register".to_string(), true),
            ("zeta.com/list".to_string(), true)
        ],
        "{done:?}"
    );
    assert!(
        done.iter()
            .any(|a| matches!(a, SweepAction::Deleted { path, .. } if *path == gone)),
        "{done:?}"
    );
    assert!(!gone.exists());
    assert_eq!(done.len(), 4, "{done:?}");

    // The failures: one ok:false tools.archive event each, files untouched in place, aaa's
    // row retired with an ok:true tools.retire.
    let failed: Vec<(String, String)> = store
        .ledger
        .lock()
        .unwrap()
        .iter()
        .filter(|e| !e.ok)
        .map(|e| (e.tool.clone(), e.result_summary.clone()))
        .collect();
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert_eq!(failed[0].0, "tools.archive");
    assert!(failed[0].1.contains("luma.com/aaa.js"), "{failed:?}");
    assert!(failed[0].1.contains("set mtime refused"), "{failed:?}");
    assert_eq!(failed[1].0, "tools.archive");
    assert!(failed[1].1.contains("luma.com/bbb.js"), "{failed:?}");
    assert_retired_with_failed_archive(&store, "aaa", "luma.com/aaa.js", Some("set mtime refused"));
    assert!(root.join("luma.com/aaa.js").is_file());
    assert!(root.join("luma.com/bbb.js").is_file());
    assert_eq!(
        store.row("luma.com", "aaa").unwrap().status,
        ToolStatus::Retired
    );

    // Next sweep, the step works: both are archived.
    let done = ts.sweep(&store, now, &|_| 0).await.unwrap();
    let mut archived: Vec<_> = done
        .iter()
        .filter_map(|a| match a {
            SweepAction::Archived { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    archived.sort();
    assert_eq!(archived, ["aaa", "bbb"], "{done:?}");
    assert!(root.join("_archive/luma.com/aaa.1.js").is_file());
    assert!(root.join("_archive/luma.com/bbb.2.js").is_file());
    let l = store.ledger.lock().unwrap();
    for n in ["aaa", "bbb"] {
        let last = l.iter().rev().find(|e| e.args["name"] == n).unwrap();
        assert_eq!(
            (last.tool.as_str(), last.ok),
            ("tools.archive", true),
            "{n}"
        );
    }
}

/// Review should-fix 1 (a): a read-only tool file is retired and archived by sweep, and the
/// archived copy carries the archive time (so it is not deleted early).
#[tokio::test]
async fn sweep_archives_a_read_only_tool_with_its_mtime_set() {
    let (dir, ts) = luma();
    let store = Store::default();
    let now = Utc::now();
    let tool = ts.try_tool_path("luma.com", "register").unwrap();
    fs::File::options()
        .write(true)
        .open(&tool)
        .unwrap()
        .set_modified((now - Duration::days(200)).into())
        .unwrap();
    store.put(ToolHealth {
        last_ok: Some(now - Duration::days(20)),
        ..row(ToolStatus::Ephemeral, 1)
    });
    set_readonly(&tool, true);
    let before = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
    let done = ts.sweep(&store, now, &|_| 0).await;
    let to = ts.try_archive_path("luma.com", "register", 1).unwrap();
    for p in [&tool, &to] {
        if p.exists() {
            set_readonly(p, false);
        }
    }
    let done = done.unwrap();
    assert!(
        matches!(&done[..], [SweepAction::Retired { archived: Some(a), .. }] if *a == to),
        "{done:?}"
    );
    assert!(!tool.exists());
    assert!(fs::metadata(&to).unwrap().modified().unwrap() >= before);
    assert!(store.ledger.lock().unwrap().iter().all(|e| e.ok));
    // 89 days on, the archived file is still there.
    let done = ts
        .sweep(&store, now + Duration::days(89), &|_| 0)
        .await
        .unwrap();
    assert!(done.is_empty(), "{done:?}");
    assert!(to.is_file());
    drop(dir);
}

/// A 90-day delete that fails (the archived file is held open without share-delete, which
/// Windows refuses to delete) is an `ok: false` ledger event; the next file is still deleted.
#[cfg(windows)]
#[tokio::test]
async fn sweep_records_a_failed_delete_and_goes_on() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let ts = ToolStore::new(root);
    let store = Store::default();
    let now = Utc::now();
    let old = (now - Duration::days(100)).into();
    for n in ["a", "b"] {
        let p = root.join(format!("_archive/luma.com/{n}.1.js"));
        put(root, &format!("_archive/luma.com/{n}.1.js"), "old");
        fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let a = root.join("_archive/luma.com/a.1.js");
    let b = root.join("_archive/luma.com/b.1.js");
    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&a)
        .unwrap();
    let done = ts.sweep(&store, now, &|_| 0).await;
    drop(lock);
    let done = done.unwrap_or_else(|e| panic!("one failed delete stopped the sweep: {e}"));
    assert!(
        matches!(&done[..], [SweepAction::Deleted { path, .. }] if *path == b),
        "{done:?}"
    );
    assert!(a.is_file() && !b.exists());
    let l = store.ledger.lock().unwrap();
    let failed: Vec<_> = l.iter().filter(|e| !e.ok).collect();
    assert_eq!(failed.len(), 1, "{l:?}");
    assert_eq!(failed[0].tool, "tools.delete");
    assert!(failed[0]
        .result_summary
        .contains("_archive/luma.com/a.1.js"));
}

// ---------------------------------------------------------------- B-33 (3): live loader

fn names(tools: &[Tool]) -> Vec<&str> {
    tools.iter().map(|t| t.namespaced.as_str()).collect()
}

/// D-315: a retired row whose file is still in place (its archive step failed) is not
/// loaded; the file is reported as skipped with the retired version. The sync `load`, which
/// does not read `tools.health`, still lists it.
#[tokio::test]
async fn load_live_skips_a_retired_row_whose_file_is_in_place() {
    let (_dir, ts) = luma();
    let store = Store::default();
    store.put(row(ToolStatus::Retired, 3));
    let file = ts.try_tool_path("luma.com", "register").unwrap();
    assert!(file.is_file());
    let r = ts.scan_live(&store, "luma.com", "/").await.unwrap();
    assert!(r.tools.is_empty(), "{:?}", names(&r.tools));
    assert_eq!(r.skipped, vec![(file, "retired (v3)".to_string())]);
    assert!(ts
        .load_live(&store, "https://luma.com/x", "/x")
        .await
        .unwrap()
        .is_empty());
    assert_eq!(names(&ts.load("luma.com", "/").unwrap()), ["luma.register"]);
}

/// D-315: every status other than retired, and no row at all, still loads.
#[tokio::test]
async fn load_live_keeps_every_status_but_retired() {
    let (_dir, ts) = luma();
    for status in [
        None,
        Some(ToolStatus::Ephemeral),
        Some(ToolStatus::Saved),
        Some(ToolStatus::Repaired),
        Some(ToolStatus::NeedsRepair),
        Some(ToolStatus::Ask),
    ] {
        let store = Store::default();
        if let Some(st) = status {
            store.put(row(st, 2));
        }
        let r = ts.scan_live(&store, "luma.com", "/").await.unwrap();
        assert_eq!(names(&r.tools), ["luma.register"], "{status:?}");
        assert!(r.skipped.is_empty(), "{status:?}: {:?}", r.skipped);
    }
}

/// D-315: a retired site tool no longer shadows the `_generic` tool of the same name; a
/// row in another folder (`_generic/register` retired) does not hide the site tool.
#[tokio::test]
async fn retired_site_tool_lets_the_generic_one_load() {
    let (dir, ts) = luma();
    put(dir.path(), "_generic/register.js", REGISTER);
    let store = Store::default();
    store.put(row(ToolStatus::Retired, 1));
    let r = ts.scan_live(&store, "luma.com", "/").await.unwrap();
    assert_eq!(names(&r.tools), ["generic.register"]);
    assert_eq!(r.tools[0].domain, GENERIC);
    assert_eq!(r.skipped.len(), 1, "{:?}", r.skipped);
    assert_eq!(r.skipped[0].1, "retired (v1)");
    // Sync load: the site file still shadows the generic one.
    assert_eq!(names(&ts.load("luma.com", "/").unwrap()), ["luma.register"]);

    // The other way round: only the generic row is retired, the site tool loads.
    let store = Store::default();
    store.put(ToolHealth {
        domain: GENERIC.into(),
        ..row(ToolStatus::Retired, 4)
    });
    let r = ts.scan_live(&store, "luma.com", "/").await.unwrap();
    assert_eq!(names(&r.tools), ["luma.register"]);
}

/// D-315: a retired `_generic` row is skipped on every site and on `_generic` itself.
#[tokio::test]
async fn retired_generic_row_is_skipped() {
    let (dir, ts) = luma();
    let paginate = dir.path().join("_generic/paginate.js");
    put(
        dir.path(),
        "_generic/paginate.js",
        &REGISTER.replace("@name register", "@name paginate"),
    );
    let store = Store::default();
    store.put(ToolHealth {
        domain: GENERIC.into(),
        name: "paginate".into(),
        ..row(ToolStatus::Retired, 2)
    });
    for domain in ["luma.com", "example.org", GENERIC] {
        let r = ts.scan_live(&store, domain, "/").await.unwrap();
        assert!(
            !names(&r.tools).contains(&"generic.paginate"),
            "{domain}: {:?}",
            names(&r.tools)
        );
        assert!(
            r.skipped
                .contains(&(paginate.clone(), "retired (v2)".to_string())),
            "{domain}: {:?}",
            r.skipped
        );
    }
    assert_eq!(
        names(&ts.load_live(&store, "luma.com", "/").await.unwrap()),
        ["luma.register"]
    );
}

/// D-315: a memory error while reading `tools.health` is an error, never an unfiltered list.
#[tokio::test]
async fn load_live_fails_closed_on_a_memory_error() {
    let (_dir, ts) = luma();
    let store = Store::default();
    store.fail_get.store(true, Ordering::SeqCst);
    for domain in ["luma.com", GENERIC] {
        let r = ts.load_live(&store, domain, "/").await;
        assert!(matches!(r, Err(StoreError::Memory(_))), "{domain}: {r:?}");
        let r = ts.scan_live(&store, domain, "/").await;
        assert!(matches!(r, Err(StoreError::Memory(_))), "{domain}: {r:?}");
    }
}

/// D-315 read order: `scan_live` reads every tool file before any `tools.health` row. Retire
/// saves the row before it moves the file, so with files first a retire that commits before
/// the row read is always filtered, even when its archive then fails. The fake writes a new
/// tool file inside the row read: with rows first (the round 1 order) that file was read
/// after the rows and served, which is the window a retire-with-failed-archive could use.
#[tokio::test]
async fn scan_live_reads_files_before_rows() {
    let (dir, ts) = luma();
    let late = dir.path().join("luma.com/late.js");
    let late_src = REGISTER.replace("@name register", "@name late");
    for domain in ["luma.com", GENERIC] {
        let store = Store::default();
        let target = if domain == GENERIC {
            dir.path().join("_generic/late.js")
        } else {
            late.clone()
        };
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        *store.late_file.lock().unwrap() = Some((target.clone(), late_src.clone()));
        let r = ts.scan_live(&store, domain, "/").await.unwrap();
        assert!(store.late_file.lock().unwrap().is_none(), "{domain}");
        assert!(target.is_file(), "{domain}");
        assert!(
            !r.tools.iter().any(|t| t.header.name == "late"),
            "{domain}: a file written during the row read was served: {:?}",
            names(&r.tools)
        );
        assert!(r.skipped.is_empty(), "{domain}: {:?}", r.skipped);
        // The next call sees it: the file is a valid tool that fits the path.
        let r = ts.scan_live(&store, domain, "/").await.unwrap();
        assert!(
            r.tools.iter().any(|t| t.header.name == "late"),
            "{domain}: {:?}",
            names(&r.tools)
        );
        fs::remove_file(&target).unwrap();
    }
}
