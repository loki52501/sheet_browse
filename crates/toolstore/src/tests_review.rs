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
    let r = ts.record(&store, res("register", miss())).await;
    assert!(r.is_err(), "{r:?}");
    let h = store.row("luma.com", "register").unwrap();
    assert_eq!(h.status, ToolStatus::Retired);
    assert!(ts.try_tool_path("luma.com", "register").unwrap().is_file());
    // Recoverable: the next sweep archives the retired file in place.
    fs::remove_file(dir.path().join("_archive")).unwrap();
    let done = ts.sweep(&store, Utc::now(), &|_| 0).await.unwrap();
    assert!(
        matches!(&done[..], [SweepAction::Archived { name, .. }] if name == "register"),
        "{done:?}"
    );
    assert!(!ts.try_tool_path("luma.com", "register").unwrap().exists());
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

/// D-312: a re-forge whose new @name is not the name it was called with is refused and
/// writes nothing, even when the old file (hand-edited) already carries that other @name,
/// so check_repair's old-vs-new rename check cannot catch it.
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
    assert_eq!(
        store.row("luma.com", "register").unwrap(),
        row(ToolStatus::NeedsRepair, 1)
    );
    assert_eq!(store.ledger_len(), 0);
}
