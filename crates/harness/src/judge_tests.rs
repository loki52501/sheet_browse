//! T-28 (d), D-153: reflect judges recipe runs from each run's `browseros.jsonl`.
//!
//! Through `ReflectLauncher::reflect` on `MemStore` (no Postgres, no model, no session):
//! three failing windows retire the recipe and archive its file (and the commit carries both
//! paths); an ok window records Ok; a second pass judges nothing new; a running run and the
//! lines after the next `tools.run` are not counted; a pinned recipe gets a proposal.

use super::*;
use crate::reflect::ReflectLauncher;
use async_trait::async_trait;
use chrono::{Duration, TimeZone};
use harness_core::memory::{Run, ToolHealth};
use harness_core::run::RunFolder;
use harness_core::sched::{Clock, LaunchError, LaunchExit, LaunchRequest, Launcher};
use harness_core::testing::MemStore;
use std::process::Command;
use std::sync::Arc;

struct Fixed(Ts);

#[async_trait]
impl Clock for Fixed {
    fn now(&self) -> Ts {
        self.0
    }
    async fn sleep_until(&self, _t: Ts) {}
}

struct Nop;

#[async_trait]
impl Launcher for Nop {
    async fn launch(&self, _req: LaunchRequest<'_>) -> Result<LaunchExit, LaunchError> {
        Ok(LaunchExit::OK)
    }
}

fn t(h: u32, m: u32) -> Ts {
    Utc.with_ymd_and_hms(2026, 10, 27, h, m, 0).unwrap()
}

const FOLDER: &str = "mail.google.com";

/// A forge `tools.run` event of the inbox recipe at `ts`.
fn run_event(ts: Ts, ok: bool) -> LedgerEvent {
    LedgerEvent {
        run_id: None,
        ts,
        server: "forge".into(),
        tool: "tools.run".into(),
        class: None,
        mode: Some(harness_core::memory::LedgerMode::Allow),
        args: json!({"domain": FOLDER, "name": "inbox", "args": {}}),
        result_summary: "recipe mail.google.com/inbox: 3 steps".into(),
        consequences: json!({
            "folder": FOLDER,
            "name": "inbox",
            "tools": ["navigate", "wait", "grep"],
            "expects": null,
        }),
        duration_ms: 1,
        ok,
        why: None,
    }
}

/// One browseros.jsonl line `secs` after `at`, in the hook's format (local offset).
fn bline(at: Ts, secs: i64, tool: &str, ok: bool) -> String {
    let ts = (at + Duration::seconds(secs))
        .with_timezone(&chrono::FixedOffset::east_opt(3600).unwrap())
        .format("%Y-%m-%dT%H:%M:%S%z")
        .to_string();
    json!({"run_id": "x", "ts": ts, "server": "browseros", "tool": tool, "class": null,
           "mode": "external", "args": {}, "result_summary": format!("{tool} done"),
           "ok": ok, "why": null})
    .to_string()
}

/// A run folder at `at` with these ledger events and browseros lines.
fn run(runs: &Path, at: Ts, events: &[LedgerEvent], lines: &[String]) -> RunFolder {
    let f = RunFolder::create_at(runs, "work", "work", at).unwrap();
    let mut text = String::new();
    for (i, e) in events.iter().enumerate() {
        let mut v = serde_json::to_value(e).unwrap();
        v["id"] = json!(i + 1);
        v["run_id"] = json!(f.id.0);
        text.push_str(&v.to_string());
        text.push('\n');
    }
    std::fs::write(f.ledger_jsonl(), text).unwrap();
    std::fs::write(f.browseros_jsonl(), lines.join("\n") + "\n").unwrap();
    f
}

struct Fx {
    tmp: tempfile::TempDir,
    store: Arc<MemStore>,
    tools: PathBuf,
    runs: PathBuf,
}

fn fixture(site_yaml: Option<&str>) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tmp.path().join("tools");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/mail.google.com");
    std::fs::create_dir_all(tools.join(FOLDER)).unwrap();
    std::fs::copy(
        repo.join("inbox.recipe.yaml"),
        tools.join(FOLDER).join("inbox.recipe.yaml"),
    )
    .unwrap();
    if let Some(y) = site_yaml {
        std::fs::write(tools.join(FOLDER).join("_site.yaml"), y).unwrap();
    }
    let runs = tmp.path().join("runs");
    Fx {
        tmp,
        store: Arc::new(MemStore::new()),
        tools,
        runs,
    }
}

impl Fx {
    async fn reflect(&self, at: Ts, commit: bool) -> crate::reflect::Reflected {
        let launcher = ReflectLauncher {
            store: self.store.clone(),
            tools_root: self.tools.clone(),
            proposals_file: None,
            clock: Arc::new(Fixed(at)),
            commit,
            inner: Arc::new(Nop),
            readers: Arc::new(Nop),
        };
        let me = RunFolder::create_at(&self.runs, "reflect", "reflect", at).unwrap();
        launcher
            .reflect(&LaunchRequest {
                run: &me,
                runs_root: &self.runs,
                prompt: "reflect",
                schedule: None,
            })
            .await
            .unwrap()
    }

    fn judge_events(&self) -> Vec<LedgerEvent> {
        self.store
            .ledger()
            .into_iter()
            .map(|(_, e)| e)
            .filter(|e| e.server == CORE_SERVER && e.tool == JUDGE_TOOL)
            .collect()
    }

    /// The `runs` row of `f` with `status` (the judge reads it, D-153).
    async fn status(&self, f: &RunFolder, status: RunStatus) {
        self.store
            .run_start(Run {
                id: f.id.clone(),
                started: t(0, 0),
                ended: None,
                prompt: "work".into(),
                status,
                folder: f.dir.to_string_lossy().into_owned(),
                parent_run: None,
                strict: false,
            })
            .await
            .unwrap();
    }

    /// A finished run (row `done`) with these events and lines.
    async fn done(&self, at: Ts, events: &[LedgerEvent], lines: &[String]) -> RunFolder {
        let f = run(&self.runs, at, events, lines);
        self.status(&f, RunStatus::Done).await;
        f
    }

    async fn row(&self) -> Option<ToolHealth> {
        ToolStore::new(&self.tools)
            .health(&*self.store, FOLDER, "inbox")
            .await
            .unwrap()
    }
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// A failing window: navigate and wait ok, the grep failed (and an ok grep only after the
/// next tools.run, which belongs to that window).
fn failing(at: Ts) -> Vec<String> {
    vec![
        bline(at, -5, "grep", true),
        bline(at, 0, "navigate", true),
        bline(at, 2, "wait", true),
        bline(at, 3, "grep", false),
    ]
}

#[tokio::test]
async fn three_failing_windows_retire_and_archive_and_a_second_pass_judges_nothing() {
    let fx = fixture(None);
    let git_ok = git(&fx.tools, &["init", "-q"]).is_some()
        && git(&fx.tools, &["config", "user.email", "t@example.invalid"]).is_some()
        && git(&fx.tools, &["config", "user.name", "judge test"]).is_some()
        && git(&fx.tools, &["config", "commit.gpgsign", "false"]).is_some()
        && git(&fx.tools, &["add", "-A"]).is_some()
        && git(&fx.tools, &["commit", "-q", "-m", "seed (T-28)"]).is_some();
    // Run 1: two tools.run events; the first window fails (its ok grep comes after the
    // second tools.run, at the same second), the second window fails too.
    let a = t(1, 0);
    let b = a + Duration::seconds(10);
    let mut lines = failing(a);
    lines.push(bline(b, 0, "grep", true));
    lines.extend(failing(b + Duration::seconds(1)).into_iter().skip(1));
    fx.done(a, &[run_event(a, true), run_event(b, true)], &lines)
        .await;
    // Run 2: a failed tools.run (not judged) and one failing window.
    let c = t(2, 0);
    let events = [
        run_event(c - Duration::seconds(30), false),
        run_event(c, true),
    ];
    fx.done(c, &events, &failing(c)).await;
    // Run 3 is still running: not judged.
    let d = t(3, 0);
    let running = run(&fx.runs, d, &[run_event(d, true)], &failing(d));
    fx.status(&running, RunStatus::Running).await;
    // Run 4 has no runs row: its state is unknown, never judged (fail closed).
    let e = t(3, 30);
    let unknown = run(&fx.runs, e, &[run_event(e, true)], &failing(e));

    let done = fx.reflect(t(4, 0), git_ok).await;
    let j = &done.judged;
    assert_eq!(j.judged(), 3, "{:?}", j.verdicts);
    assert_eq!(j.failed(), 3);
    assert!(j
        .verdicts
        .iter()
        .all(|v| v.run != running.id && v.run != unknown.id));
    let why = |i: usize| match &j.verdicts[i].outcome {
        Some(RunOutcome::Failed { why }) => why.clone(),
        o => panic!("{o:?}"),
    };
    assert!(why(0).contains("step 3 (grep)"), "{}", why(0));
    assert!(j.verdicts[2].retired);
    assert_eq!(j.verdicts[2].status, Some(ToolStatus::Retired));
    let file = fx.tools.join(FOLDER).join("inbox.recipe.yaml");
    let archived = fx
        .tools
        .join("_archive/mail.google.com/inbox.1.recipe.yaml");
    assert!(!file.exists());
    assert!(archived.is_file());
    assert_eq!(
        j.moved,
        vec![
            PathBuf::from("_archive/mail.google.com/inbox.1.recipe.yaml"),
            PathBuf::from("mail.google.com/inbox.recipe.yaml"),
        ]
    );
    let row = fx.row().await.unwrap();
    assert_eq!((row.status, row.fails), (ToolStatus::Retired, 3));
    let marks = fx.judge_events();
    assert_eq!(marks.len(), 3);
    assert_eq!(marks[0].args["at"], json!(at_key(a)));
    assert_eq!(marks[0].consequences["verdict"], json!("failed"));
    assert!(fx
        .store
        .ledger()
        .iter()
        .any(|(_, e)| e.tool == "tools.retire"));
    if git_ok {
        let c = done.commit.expect("committed");
        let files = git(
            &fx.tools,
            &["show", "--name-only", "--no-renames", "--format=%s", &c],
        )
        .unwrap();
        assert!(
            files.contains("_archive/mail.google.com/inbox.1.recipe.yaml"),
            "{files}"
        );
        assert!(
            files.contains("mail.google.com/inbox.recipe.yaml"),
            "{files}"
        );
    }

    // A second pass: every event is marked (the first pass's ledger.jsonl), nothing new.
    let again = fx.reflect(t(5, 0), false).await;
    assert_eq!(again.judged.verdicts, vec![], "idempotent");
    assert_eq!(fx.judge_events().len(), 3);
    // Once run 3 ends it is judged, as skipped: the recipe is retired.
    fx.store
        .run_end(&running.id, RunStatus::Done, t(5, 30))
        .await
        .unwrap();
    let third = fx.reflect(t(6, 0), false).await;
    assert_eq!(third.judged.verdicts.len(), 1);
    assert_eq!(third.judged.verdicts[0].outcome, None);
    assert_eq!(third.judged.verdicts[0].note, "recipe retired");
    assert_eq!(fx.row().await.unwrap().fails, 3, "nothing recorded for it");
    assert_eq!(
        third.judged.verdicts[0].run, running.id,
        "run 4 has no row still"
    );
    let _ = &fx.tmp;
}

#[tokio::test]
async fn an_ok_window_records_ok() {
    let fx = fixture(None);
    let a = t(1, 0);
    let mut lines = failing(a);
    lines.push(bline(a, 4, "grep", true));
    fx.done(a, &[run_event(a, true)], &lines).await;
    let done = fx.reflect(t(2, 0), false).await;
    assert_eq!(done.judged.verdicts.len(), 1);
    assert_eq!(done.judged.verdicts[0].outcome, Some(RunOutcome::Ok));
    let row = fx.row().await.unwrap();
    assert_eq!(
        (row.status, row.runs, row.fails),
        (ToolStatus::Ephemeral, 1, 0)
    );
    assert!(fx.tools.join(FOLDER).join("inbox.recipe.yaml").is_file());
    assert_eq!(fx.judge_events()[0].consequences["verdict"], json!("ok"));
}

#[tokio::test]
async fn a_pinned_recipe_is_proposed_not_retired() {
    let fx = fixture(Some("pinned: [inbox]\n"));
    for h in 1..=3 {
        let a = t(h, 0);
        fx.done(a, &[run_event(a, true)], &failing(a)).await;
    }
    let done = fx.reflect(t(5, 0), false).await;
    assert_eq!(done.judged.failed(), 3);
    assert!(fx.tools.join(FOLDER).join("inbox.recipe.yaml").is_file());
    assert_eq!(fx.row().await.unwrap().status, ToolStatus::Ephemeral);
    let ids: Vec<&str> = done.proposals.iter().map(|p| p.id.as_str()).collect();
    assert!(
        ids.contains(&"t7-retire-mail.google.com-inbox-v1"),
        "{ids:?}"
    );
}

#[test]
fn verdict_rules() {
    let at = t(1, 0);
    let l = |tool: &str, ok: bool, summary: &str| BrowserLine {
        ts: at,
        tool: tool.into(),
        ok,
        summary: summary.into(),
    };
    let tools: Vec<String> = ["navigate", "act", "wait"].map(String::from).to_vec();
    let w = [
        l("navigate", true, ""),
        l("snapshot", true, ""),
        l("act", true, ""),
        l("wait", true, "Message sent"),
    ];
    let refs: Vec<&BrowserLine> = w.iter().collect();
    assert_eq!(verdict(&tools, Some("Message sent"), &refs), RunOutcome::Ok);
    assert!(matches!(
        verdict(&tools, Some("You're In"), &refs),
        RunOutcome::Failed { why } if why.contains("expects")
    ));
    // Out of order is a failure.
    let rev: Vec<&BrowserLine> = w.iter().rev().collect();
    assert!(matches!(
        verdict(&tools, None, &rev),
        RunOutcome::Failed { .. }
    ));
    assert!(
        matches!(verdict(&tools, None, &[]), RunOutcome::Failed { why } if why.contains("no browseros"))
    );
    assert_eq!(
        parse_hook_ts("2026-10-27T02:00:05+0100"),
        Some(Utc.with_ymd_and_hms(2026, 10, 27, 1, 0, 5).unwrap())
    );
}
