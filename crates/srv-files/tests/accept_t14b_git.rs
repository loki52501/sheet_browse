//! T-14b item 5 (D-547): hooks and fsmonitor already in a repository and a filter in an
//! existing global git config do not run under Read-class git. A planted
//! `post-index-change` hook, a repository `core.fsmonitor` and a global clean filter each
//! write a marker file; `git status` and `git diff` through the `shell` tool (dispatch,
//! gate, ledger) must leave no marker, and the same `git status` run directly by bash
//! must write them (so the plants are live and the test can fail).
//!
//! Its own test binary: it points HOME at a fake home for the whole process, which no
//! other test may see.

use harness_core::memory::{ts_now, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::Vault;
use harness_core::{dispatch, Class, Harness, ToolRegistry};
use serde_json::json;
use srv_files::shell::Runner;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

fn git(git: &Path, dir: &Path, args: &[&str]) {
    let out = Command::new(git)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// Forward slashes, no verbatim prefix: a path sh and git config both read.
fn sh_path(p: &Path) -> String {
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

fn touch_change(p: &Path, line: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
    writeln!(f, "{line}").unwrap();
}

#[tokio::test]
async fn read_class_git_runs_no_hook_fsmonitor_or_global_filter() {
    let runner = Runner::discover();
    let Runner::Bash(bash) = runner.clone() else {
        eprintln!("skipped: no bash on this machine");
        return;
    };
    let Some(git_exe) = runner.git_program() else {
        eprintln!("skipped: no git on this machine");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let root_path = PathBuf::from(srv_files::paths::plain(&root_path));
    let home = root_path.join("home");
    let marks = root_path.join("marks");
    let base = root_path.join("base");
    for d in [&home, &marks, &base] {
        std::fs::create_dir_all(d).unwrap();
    }
    // The whole process (bash, git, the seal's config read) sees the fake home.
    std::env::set_var("HOME", &home);

    // A repository at the base folder with a tracked .txt file under a filter attribute.
    git(&git_exe, &base, &["init", "-q"]);
    std::fs::write(base.join(".gitignore"), "runs/\n").unwrap();
    std::fs::write(base.join(".gitattributes"), "*.txt filter=mark\n").unwrap();
    std::fs::write(base.join("a.txt"), "one\n").unwrap();
    git(&git_exe, &base, &["add", "."]);
    git(&git_exe, &base, &["commit", "-q", "-m", "init"]);

    // Plants, each writing its own marker (the value is quoted: `;` starts a comment).
    let m = sh_path(&marks);
    std::fs::write(
        home.join(".gitconfig"),
        format!("[filter \"mark\"]\n\tclean = \"sh -c 'echo g >> {m}/global-filter; cat'\"\n"),
    )
    .unwrap();
    let cfg = base.join(".git").join("config");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str(&format!(
        "[core]\n\tfsmonitor = sh -c 'echo f >> {m}/repo-fsmonitor'\n"
    ));
    std::fs::write(&cfg, text).unwrap();
    std::fs::write(
        base.join(".git").join("hooks").join("post-index-change"),
        format!("#!/bin/sh\necho h >> {m}/hook\n"),
    )
    .unwrap();

    // The harness, with this base folder holding runs/.
    let runs_root = base.join("runs");
    let run = RunFolder::create(&runs_root, "t14b", "sealed git").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "sealed git".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    let asker = Arc::new(ScriptedAsker::new());
    let harness = Harness {
        store: store.clone(),
        vault: Arc::new(Vault::in_memory([])),
        asker: asker.clone(),
        runs_root,
        run: Some(run.id.clone()),
    };
    let registry = ToolRegistry::new(srv_files::shell::tools_with(runner.clone()));

    touch_change(&base.join("a.txt"), "two");
    for cmd in ["git status --short", "git diff --stat", "git status"] {
        let out = dispatch("files", &registry, &harness, "shell", json!({"cmd": cmd}))
            .await
            .unwrap_or_else(|e| panic!("{cmd}: {e}"));
        assert!(out.text.starts_with("exit: 0"), "{cmd}: {}", out.text);
        assert!(out.text.contains("a.txt"), "{cmd}: {}", out.text);
        assert!(markers(&marks).is_empty(), "{cmd}: {:?}", markers(&marks));
    }
    assert!(asker.cards().is_empty(), "Read-class git asks nothing");
    let rows = store.ledger();
    assert!(rows
        .iter()
        .filter(|(_, e)| e.server == "files")
        .all(|(_, e)| e.class == Some(Class::Read)));

    // The same command run by bash as configured: every plant is live.
    touch_change(&base.join("a.txt"), "three");
    let out = Command::new(&bash)
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "git status --short && git diff --stat",
        ])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let live = markers(&marks);
    for want in ["global-filter", "repo-fsmonitor", "hook"] {
        assert!(live.iter().any(|m| m == want), "{want} not live: {live:?}");
    }
}
