//! Review T-14b round 3 (D-555, D-556): a bare repository planted outside runs/ with Fill
//! writes (its config and hooks sit at plain names the classifier's repository scan never
//! follows) runs none of its drivers or hooks through the `shell` tool, on sealed lines
//! (every git Read) and on lines with a git above Read alike. A control runs the same
//! lines in bash as configured, which must fire every plant (so the test can fail).

use harness_core::memory::{ts_now, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::Vault;
use harness_core::{dispatch, Harness, ToolRegistry};
use serde_json::json;
use srv_files::shell::Runner;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

fn sh(p: &Path) -> String {
    srv_files::paths::plain(p).replace('\\', "/")
}

fn markers(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn clear(dir: &Path) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        std::fs::remove_file(e.path()).unwrap();
    }
}

/// A bare repository at `dir` with `extra` appended to its config and `attrs` as its
/// info/attributes, plus two loose files to diff.
fn bare(git: &Path, dir: &Path, extra: &str, attrs: &str) {
    let st = Command::new(git)
        .args(["init", "--bare", "-q"])
        .arg(dir)
        .env_remove("GIT_DIR")
        .status()
        .unwrap();
    assert!(st.success());
    let cfg = dir.join("config");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str(extra);
    std::fs::write(&cfg, text).unwrap();
    std::fs::create_dir_all(dir.join("info")).unwrap();
    std::fs::write(dir.join("info").join("attributes"), attrs).unwrap();
    std::fs::write(dir.join("a"), "one\n").unwrap();
    std::fs::write(dir.join("b"), "two\n").unwrap();
}

/// The `git init` line goes last: it makes `planted/.git`, which later lines would find first.
const LINES: &[&str] = &[
    "cd planted && git diff --no-index a b && git add nothing-here",
    "cd planted && git diff --no-index a b",
    "cd planted && git log -p -1",
    "cd planted && git gc -q",
    "cd worktree_repo && git add -A",
    "cd planted && git diff --no-index a b; git init -q",
];

#[tokio::test]
async fn a_planted_bare_repository_runs_nothing_on_any_line() {
    let runner = Runner::discover();
    let Runner::Bash(bash) = runner.clone() else {
        eprintln!("skipped: no bash on this machine");
        return;
    };
    let Some(g) = runner.git_program() else {
        eprintln!("skipped: no git on this machine");
        return;
    };
    let t = tempfile::tempdir().unwrap();
    let top = std::path::PathBuf::from(srv_files::paths::plain(
        &std::fs::canonicalize(t.path()).unwrap(),
    ));
    // Each phase gets its own planted folder: a `git init` line changes what later lines
    // find, in either phase.
    let root = top.join("tool");
    let control = top.join("control");
    let marks = top.join("marks");
    std::fs::create_dir_all(&marks).unwrap();
    plant(&g, &root, &marks);
    plant(&g, &control, &marks);
    run_lines(&runner, &bash, &root, &control, &marks).await;
}

fn plant(g: &Path, root: &Path, marks: &Path) {
    let m = sh(marks);
    let planted = root.join("planted");
    bare(
        g,
        &planted,
        &format!("[diff \"x\"]\n\tcommand = \"echo x >> '{m}/diff' #\"\n"),
        "* diff=x\n",
    );
    std::fs::write(
        planted.join("hooks").join("reference-transaction"),
        format!("#!/bin/sh\necho x >> '{m}/hook'\n"),
    )
    .unwrap();
    let worktree_repo = root.join("worktree_repo");
    bare(
        g,
        &worktree_repo,
        &format!(
            "[core]\n\tbare = false\n\tworktree = .\n\
             [filter \"x\"]\n\tclean = \"sh -c 'echo x >> {m}/filter; cat'\"\n"
        ),
        "* filter=x\n",
    );
}

async fn run_lines(runner: &Runner, bash: &Path, root: &Path, control: &Path, marks: &Path) {
    let runs_root = root.join("runs");
    let run = RunFolder::create(&runs_root, "t14b-bare", "bare").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "bare".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    let harness = Harness {
        store: store.clone(),
        vault: Arc::new(Vault::in_memory([])),
        asker: Arc::new(ScriptedAsker::new()),
        runs_root,
        run: Some(run.id.clone()),
    };
    let registry = ToolRegistry::new(srv_files::shell::tools_with(runner.clone()));
    let mut leaks = Vec::new();
    for line in LINES {
        clear(marks);
        let out = dispatch(
            "files",
            &registry,
            &harness,
            "shell",
            json!({"cmd": line, "timeout": 30}),
        )
        .await;
        let now = markers(marks);
        if !now.is_empty() {
            let text = match out {
                Ok(o) => o.text,
                Err(e) => format!("error: {e}"),
            };
            leaks.push(format!(
                "{line:?} ran {now:?}: {}",
                text.lines().next().unwrap_or("")
            ));
        }
    }

    // Control: the same lines in bash fire every plant (system and global config off so
    // this machine's own settings, e.g. a global core.hooksPath, cannot mask a plant).
    clear(marks);
    let empty = control.join("empty-global");
    std::fs::write(&empty, "").unwrap();
    let all = LINES
        .iter()
        .map(|c| format!("( {c} ) >/dev/null 2>&1 </dev/null"))
        .collect::<Vec<_>>()
        .join("; ");
    Command::new(bash)
        .args(["--noprofile", "--norc", "-c", &all])
        .current_dir(control)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &empty)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "safe.bareRepository")
        .env("GIT_CONFIG_VALUE_0", "all")
        .output()
        .unwrap();
    let live = markers(marks);
    assert_eq!(
        live,
        ["diff", "filter", "hook"],
        "every plant is live in the control"
    );
    assert!(leaks.is_empty(), "planted programs ran: {leaks:#?}");
}
