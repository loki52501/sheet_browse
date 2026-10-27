//! T-14b acceptance (tester-1): items 2, 3, 4, 6, 7 and 8 of agent-memory/tasks.yaml T-14b
//! (D-544, D-545, D-546, D-548, D-549), plus adversarial probes per item. Items 1 and 5
//! set HOME for their process and live in `accept_t14b_home.rs` and `accept_t14b_seal.rs`.
//!
//! Classifier probes call `srv_files::shell::classify::classify` (pure, nothing runs) and
//! the `fs.*` handlers' `classify` through a real `ToolCtx`, so no probe touches a real
//! Git install or a system folder. Item 6 drives `fs.write` / `fs.delete` through
//! `harness_core::dispatch` while another thread swaps a folder for a real junction.

use harness_core::memory::{ts_now, LedgerEvent, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::Vault;
use harness_core::{dispatch, Class, Harness, ToolCtx, ToolRegistry};
use serde_json::{json, Value};
use srv_files::paths::Roots;
use srv_files::shell::classify::classify;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

struct Fx {
    _t: tempfile::TempDir,
    root: PathBuf,
    roots: Roots,
    run: RunFolder,
    store: Arc<MemStore>,
    asker: Arc<ScriptedAsker>,
    harness: Harness,
    registry: ToolRegistry,
}

/// `<tmp>/<base_rel>/runs/<id>`; `base_rel` empty puts runs/ at the temp root.
async fn fx_at(base_rel: &str) -> Fx {
    let t = tempfile::tempdir().unwrap();
    let root = PathBuf::from(srv_files::paths::plain(
        &std::fs::canonicalize(t.path()).unwrap(),
    ));
    let base = if base_rel.is_empty() {
        root.clone()
    } else {
        root.join(base_rel)
    };
    let runs_root = base.join("runs");
    std::fs::create_dir_all(&runs_root).unwrap();
    let run = RunFolder::create(&runs_root, "t14b", "t14b acceptance").unwrap();
    std::fs::create_dir_all(runs_root.join("x")).unwrap();
    std::fs::write(runs_root.join("x").join("a.txt"), "a").unwrap();
    std::fs::write(base.join("file.txt"), "keep").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "t14b acceptance".into(),
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
        runs_root: runs_root.clone(),
        run: Some(run.id.clone()),
    };
    let mut tools = srv_files::fs::tools();
    tools.extend(srv_files::shell::tools());
    let registry = ToolRegistry::new(tools);
    let roots = Roots::from_run_dir(&run.dir).unwrap();
    Fx {
        _t: t,
        root,
        roots,
        run,
        store,
        asker,
        harness,
        registry,
    }
}

async fn fx() -> Fx {
    fx_at("").await
}

impl Fx {
    fn base(&self) -> &Path {
        &self.roots.base
    }

    fn class(&self, cmd: &str) -> (Class, String) {
        let v = classify(&self.roots, &self.roots.base, cmd, false);
        (v.class, v.why())
    }

    /// The class the gate would see for a tool call, from the handler's own `classify`
    /// (nothing is executed).
    async fn tool_class(&self, tool: &str, args: Value) -> Class {
        self.try_tool_class(tool, args.clone())
            .await
            .unwrap_or_else(|e| panic!("{tool} {args}: {e}"))
    }

    /// [`Fx::tool_class`]; `Err` when the handler refuses the arguments outright.
    async fn try_tool_class(&self, tool: &str, args: Value) -> Result<Class, String> {
        let spec = self
            .registry
            .get(tool)
            .unwrap_or_else(|| panic!("no tool {tool}"));
        let ctx = ToolCtx::new(
            self.run.id.clone(),
            self.run.dir.clone(),
            spec.class,
            &self.harness,
            self.registry.clone(),
        );
        let c = spec
            .handler
            .classify(&ctx, &args)
            .await
            .map_err(|e| e.to_string())?;
        Ok(c.class.map_or(spec.class, |k| k.max(spec.class)))
    }

    fn tool_events(&self) -> Vec<LedgerEvent> {
        let mut rows = self.store.ledger();
        rows.sort_by_key(|(id, _)| *id);
        rows.into_iter()
            .map(|(_, e)| e)
            .filter(|e| e.server == "files")
            .collect()
    }
}

/// Every line in `cmds` whose class fails `ok`, with its class and reason.
fn wrong(f: &Fx, cmds: &[String], ok: impl Fn(Class) -> bool) -> Vec<String> {
    let print = std::env::var_os("T14B_PRINT").is_some();
    cmds.iter()
        .filter_map(|c| {
            let (k, why) = f.class(c);
            if print {
                eprintln!("{k:?}\t{c}\t{why}");
            }
            (!ok(k)).then(|| format!("{c:?} is {k:?} ({why})"))
        })
        .collect()
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn git_bin() -> Option<PathBuf> {
    srv_files::shell::Runner::discover().git_program()
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let g = git_bin().expect("git");
    Command::new(g)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap()
}

/// A real repository at `dir` with one commit.
fn real_repo(dir: &Path, gitignore: &str) {
    assert!(git(dir, &["init", "-q"]).status.success());
    std::fs::write(dir.join(".gitignore"), gitignore).unwrap();
    std::fs::write(dir.join("src.txt"), "src\n").unwrap();
    assert!(git(dir, &["add", "src.txt", ".gitignore"]).status.success());
    assert!(git(dir, &["commit", "-q", "-m", "init"]).status.success());
}

fn sh(p: &Path) -> String {
    srv_files::paths::plain(p).replace('\\', "/")
}

// ---- item 2 (D-544): Git's system config folders ----

/// Writes below `<msys root>/etc`, `<msys root>/mingw64/etc` and `%ProgramData%\Git` are
/// Delete for the shell (every writer, Windows and MSYS spellings) and for fs.write
/// (append included). Classified only: no system file is touched.
#[tokio::test]
async fn item2_writes_under_git_system_config_are_delete() {
    let f = fx().await;
    let dirs = srv_files::shell::git_config_dirs();
    assert!(!dirs.is_empty(), "no git config folders found");
    let mut lines: Vec<String> = Vec::new();
    for d in &dirs {
        let gc = sh(&d.join("gitconfig"));
        let ga = sh(&d.join("gitattributes"));
        let prof = sh(&d.join("profile.d").join("zz.sh"));
        let cfg = sh(&d.join("config"));
        let upper = gc.to_uppercase();
        lines.extend([
            format!("echo x >> '{gc}'"),
            format!("echo x > '{gc}'"),
            format!("printf '[core]\\n' >> '{cfg}'"),
            format!("tee -a '{gc}' < runs/x/a.txt"),
            format!("cat runs/x/a.txt >> '{ga}'"),
            format!("cp runs/x/a.txt '{prof}'"),
            format!("sed -i s/a/b/ '{gc}'"),
            format!("echo x >> '{upper}'"),
            format!("mkdir -p '{}'", sh(&d.join("newdir"))),
        ]);
    }
    // Bash's own spellings of the MSYS root.
    lines.extend(s(&[
        "echo x >> /etc/gitconfig",
        "echo '* filter=x' >> /etc/gitattributes",
        "echo x >> /mingw64/etc/gitconfig",
        "cp runs/x/a.txt /etc/profile.d/zz.sh",
        "echo x >> /c/ProgramData/Git/config",
        "echo x >> '/c/Program Files/Git/etc/gitconfig'",
    ]));
    let bad = wrong(&f, &lines, |k| k == Class::Delete);
    assert!(bad.is_empty(), "want Delete:\n{}", bad.join("\n"));

    // fs.write and fs.mkdir through the handler's classify (append to etc/gitconfig too).
    let mut tool_bad = Vec::new();
    for d in &dirs {
        for (tool, args) in [
            (
                "fs.write",
                json!({"path": sh(&d.join("gitconfig")), "content": "[core]\n", "append": true}),
            ),
            (
                "fs.write",
                json!({"path": sh(&d.join("gitattributes")), "content": "* filter=x\n"}),
            ),
            (
                "fs.write",
                json!({"path": sh(&d.join("config")), "content": "x", "append": true}),
            ),
            ("fs.mkdir", json!({"path": sh(&d.join("profile.d2"))})),
        ] {
            let k = f.tool_class(tool, args.clone()).await;
            if k != Class::Delete {
                tool_bad.push(format!("{tool} {args} is {k:?}"));
            }
        }
    }
    assert!(tool_bad.is_empty(), "want Delete:\n{}", tool_bad.join("\n"));
}

/// The same rule for an MSYS root the test controls, reached through a junction and with
/// odd spellings (case, trailing dot), through `paths::code_path` with
/// `git_config_dirs_of`.
#[tokio::test]
async fn item2_adversarial_config_tree_through_junction_and_spellings() {
    let f = fx().await;
    let git_root = f.root.join("FakeGit");
    for d in ["etc", "mingw64/etc", "usr/bin"] {
        std::fs::create_dir_all(git_root.join(d)).unwrap();
    }
    std::fs::write(git_root.join("etc").join("gitconfig"), "[core]\n").unwrap();
    let trees = srv_files::shell::git_config_dirs_of(std::slice::from_ref(&git_root));
    let dirs = srv_files::paths::PathDirs::from_entries(&[], None, false).with_trees(&trees);
    let j = f.root.join("j");
    if !junction(&git_root, &j) {
        eprintln!("skipped: cannot create a junction here");
        return;
    }
    let mut bad = Vec::new();
    for raw in [
        sh(&git_root.join("etc").join("gitconfig")),
        sh(&git_root.join("ETC").join("GitConfig")),
        format!("{}.", sh(&git_root.join("etc").join("gitconfig"))),
        sh(&git_root.join("mingw64").join("etc").join("gitconfig")),
        sh(&git_root.join("etc").join("profile.d").join("new.sh")),
        sh(&j.join("etc").join("gitconfig")),
        sh(&j.join("mingw64").join("etc").join("new")),
    ] {
        let p = srv_files::paths::resolve(f.base(), &raw).unwrap();
        if !srv_files::paths::code_path(&f.roots, &p, &[], &dirs) {
            bad.push(raw);
        }
    }
    assert!(bad.is_empty(), "not code paths: {bad:?}");
    // Control: a sibling folder of etc is not a tree (the check can fail).
    let p = srv_files::paths::resolve(f.base(), &sh(&git_root.join("share").join("x"))).unwrap();
    assert!(!srv_files::paths::code_path(&f.roots, &p, &[], &dirs));
}

// ---- item 3 (D-546 (1)): in-place updates of a repository outside runs/ ----

#[tokio::test]
async fn item3_in_place_repo_updates_outside_runs_are_not_read() {
    let f = fx().await;
    if git_bin().is_none() {
        eprintln!("skipped: no git");
        return;
    }
    real_repo(f.base(), "runs/\n");
    let lines = s(&[
        "git pull",
        "git pull --rebase",
        "git pull --ff-only origin main",
        "git -C . pull",
        "git --no-pager pull",
        "cd . && git pull",
        "git checkout main",
        "git checkout -b feat",
        "git checkout -- src.txt",
        "git checkout .",
        "git checkout HEAD~1 -- .",
        "git switch main",
        "git switch -c feat2",
        "git stash pop",
        "git stash apply",
        "git stash apply stash@{1}",
        "git stash branch b1",
        "git merge feat",
        "git merge --ff-only origin/main",
        "git rebase main",
        "git rebase --onto main feat",
        "git cherry-pick abc123",
        "git revert abc123",
        "git reset --hard",
        "git reset --hard origin/main",
        "git restore .",
        "git restore --source=HEAD~1 src.txt",
        "git am runs/x/a.txt",
        "git apply runs/x/a.txt",
        "git read-tree -u -m HEAD",
        "git checkout-index -a -f",
        "git clean -fd",
        "git sparse-checkout set src",
        "git bisect start",
        "git bisect good",
        "git worktree add ../w",
        "git submodule update --init",
    ]);
    let bad = wrong(&f, &lines, |k| k > Class::Read);
    assert!(bad.is_empty(), "want above Read:\n{}", bad.join("\n"));
    // Base folder inside a repository rooted above it (git finds the parent's .git).
    let g = fx_at("proj/base").await;
    real_repo(&g.root.join("proj"), "target/\n");
    let lines = s(&[
        "git pull",
        "git checkout main",
        "git stash pop",
        "git merge feat",
        "git reset --hard",
    ]);
    let bad = wrong(&g, &lines, |k| k > Class::Read);
    assert!(
        bad.is_empty(),
        "parent repo, want above Read:\n{}",
        bad.join("\n")
    );
}

// ---- item 4 (D-546 (2)): stash and commit -a reaching runs/ ----

#[tokio::test]
async fn item4_stash_and_commit_all_with_unignored_runs_are_delete() {
    let f = fx().await;
    if git_bin().is_none() {
        eprintln!("skipped: no git");
        return;
    }
    real_repo(f.base(), "target/\n");
    // A planted repository in runs/ too.
    let planted = f.roots.runs.join("x").join("r");
    std::fs::create_dir_all(&planted).unwrap();
    assert!(git(&planted, &["init", "-q"]).status.success());
    std::fs::write(f.base().join("list.txt"), "runs/x/a.txt\n").unwrap();
    let lines = s(&[
        "git stash -u",
        "git stash -a",
        "git stash",
        "git stash push",
        "git stash push -u -q",
        "git stash push -m wip -u",
        "git stash -ku",
        "git stash -uk",
        "git stash --keep-index -u",
        "git stash push --include-untracked",
        "git stash push --all",
        "git stash save -u wip",
        "git -C . stash -u",
        "git --no-pager stash -u",
        "git -c core.quotepath=off stash -u",
        "cd . && git stash -u",
        "git commit -a -m x",
        "git commit -am x",
        "git commit -qam x",
        "git commit -vam x",
        "git commit -a --amend --no-edit",
        "git commit --amend -a --no-edit",
        "git commit --all -m x",
        "git commit -m x -- runs",
        "git commit -m x ':/'",
        "git commit -m x '*'",
        "git commit -m x -i runs/x/a.txt",
        "git commit -m x --pathspec-from-file=list.txt",
        "git commit -m x --pathspec-from-file list.txt",
        "git stash push --pathspec-from-file=list.txt",
        "git stash push -u --pathspec-from-file=list.txt",
        "git add --pathspec-from-file=list.txt",
        "git commit -m x --pathspec-from-file=- < list.txt",
        "git add -A",
        "git add .",
        "git add --all",
        "git add -u",
        "git add :/",
        "git add runs",
        "git -C runs/x/r stash -u",
        "git -C runs/x/r commit -a -m x",
    ]);
    let bad = wrong(&f, &lines, |k| k == Class::Delete);
    assert!(bad.is_empty(), "want Delete:\n{}", bad.join("\n"));

    // Base inside a repository rooted above it, runs/ not ignored there.
    let g = fx_at("proj/base").await;
    real_repo(&g.root.join("proj"), "target/\n");
    let lines = s(&[
        "git stash -u",
        "git stash",
        "git commit -a -m x",
        "git add -A",
    ]);
    let bad = wrong(&g, &lines, |k| k == Class::Delete);
    assert!(
        bad.is_empty(),
        "parent repo, want Delete:\n{}",
        bad.join("\n")
    );
}

/// Whenever git itself says runs/x/a.txt is not ignored, `git stash -u` and
/// `git commit -a` must be Delete, whatever the root .gitignore looks like (negations,
/// comments, anchors); `stash -a` and `add -f runs` always are.
#[tokio::test]
async fn item4_adversarial_gitignore_spellings_match_git() {
    if git_bin().is_none() {
        eprintln!("skipped: no git");
        return;
    }
    let cases = [
        "runs/\n",
        "/runs/\n",
        "runs\n",
        "runs/**\n",
        "runs/\n!runs/\n",
        "runs/*\n!runs/x/\n",
        "# runs/\n",
        "\\#runs/\n",
        "runs/ \n",
        "Runs/\n",
        "runs/\n!/runs\n",
        "*\n!*/\n!src.txt\n!.gitignore\n",
    ];
    let mut bad = Vec::new();
    for gi in cases {
        let f = fx().await;
        real_repo(f.base(), gi);
        let ci = git(f.base(), &["check-ignore", "-q", "runs/x/a.txt"]);
        let ignored = ci.status.code() == Some(0);
        for (cmd, must_delete) in [
            ("git stash -u", !ignored),
            ("git stash", !ignored),
            ("git commit -a -m x", !ignored),
            ("git stash -a", true),
            ("git add -f runs", true),
            ("git add -f runs/x/a.txt", true),
        ] {
            let (k, why) = f.class(cmd);
            if std::env::var_os("T14B_PRINT").is_some() {
                eprintln!("{gi:?} ignored={ignored} {cmd}: {k:?} {why}");
            }
            if must_delete && k != Class::Delete {
                bad.push(format!(
                    ".gitignore {gi:?} (git: ignored={ignored}): {cmd} is {k:?} ({why})"
                ));
            }
        }
    }
    assert!(bad.is_empty(), "want Delete:\n{}", bad.join("\n"));
}

// ---- item 5, classifier half (D-547): programs named on the line or in env ----

/// A Read-looking git line that names a program itself (flags, -c, env assignments,
/// GIT_CONFIG_* that would beat the seal) or targets another repository must not run as
/// sealed Read. Each must be Delete.
#[tokio::test]
async fn item5_adversarial_programs_named_on_the_line_are_delete() {
    let f = fx().await;
    if git_bin().is_none() {
        eprintln!("skipped: no git");
        return;
    }
    real_repo(f.base(), "runs/\n");
    // A repository outside the base whose config names a filter driver.
    let other = f.root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    real_repo(&other, "");
    assert!(
        git(&other, &["config", "filter.m.clean", "sh -c 'echo x > m'"])
            .status
            .success()
    );
    let o = sh(&other);
    let planted = f.roots.runs.join("x").join("r");
    std::fs::create_dir_all(&planted).unwrap();
    assert!(git(&planted, &["init", "-q"]).status.success());
    let lines: Vec<String> = [
        "git grep -Oevil src",
        "git grep --open-files-in-pager=evil src",
        "git -c core.pager=evil grep -O src",
        "git -c core.fsmonitor=evil status",
        "git -c diff.external=evil diff",
        "git -c alias.zz='!evil' zz",
        "git --config-env=core.pager=HOME grep -O src",
        "git --exec-path=runs/x status",
        "git ls-remote --upload-pack=evil .",
        "git archive --remote=. --exec=evil HEAD",
        "GIT_PAGER=evil git grep -O src",
        "GIT_EXTERNAL_DIFF=evil git diff",
        "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.fsmonitor GIT_CONFIG_VALUE_0=evil git status",
        "GIT_CONFIG_GLOBAL=runs/x/a.txt git status",
        "GIT_DIR=runs/x/r/.git git status",
        "env GIT_EXTERNAL_DIFF=evil git diff",
        "export GIT_EXTERNAL_DIFF=evil; git diff",
        "export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.fsmonitor GIT_CONFIG_VALUE_0=evil; git status",
        "git --git-dir=runs/x/r/.git status",
        "git --work-tree=. --git-dir=runs/x/r/.git status",
        "git difftool -y",
        "git difftool -y -x evil",
        "git help -w log",
        "git bisect run evil",
        "git interpret-trailers --trailer x runs/x/a.txt",
        "git log --ext-diff -p",
        "git diff --ext-diff",
        "git config --edit",
        "git rebase -i HEAD~1 --exec evil",
    ]
    .iter()
    .map(|x| x.to_string())
    .chain([
        format!("git -C '{o}' status"),
        format!("cd '{o}' && git status"),
        format!("git --git-dir='{o}/.git' --work-tree='{o}' diff"),
        format!("GIT_DIR='{o}/.git' git status"),
    ])
    .collect();
    let bad = wrong(&f, &lines, |k| k == Class::Delete);
    assert!(bad.is_empty(), "want Delete:\n{}", bad.join("\n"));
}

// ---- item 7 (D-545, reverted by D-550): dotfiles in runs/ ----

/// D-550 reverts D-545 (review-T-14b Blocking): a dotfile in runs/ is code-class again.
/// Through fs.write it asks and, skipped, is not written; the shell writes are Delete
/// for every writer; git's names and code folders in runs/ stay Delete, with odd
/// spellings (case, trailing dot, the ::$DATA stream) too; a plain name stays scratch.
#[tokio::test]
async fn item7_dotfiles_in_runs_ask_and_git_names_stay_delete() {
    let f = fx().await;
    let id = f.run.id.to_string();
    // A plain name in runs/ still writes without an ask, end to end.
    let out = dispatch(
        "files",
        &f.registry,
        &f.harness,
        "fs.write",
        json!({"path": format!("runs/{id}/files/something"), "content": "x"}),
    )
    .await
    .unwrap();
    assert!(out.text.starts_with("wrote"), "{}", out.text);
    assert!(f.asker.cards().is_empty(), "{:?}", f.asker.cards());
    // A dotfile asks (the asker times out to skip) and is not written.
    let dotted = [
        (format!("runs/{id}/files/.something"), "x"),
        (format!("runs/{id}/files/.env"), "A=1"),
        ("runs/x/.npmrc".to_string(), "registry=x"),
        ("runs/x/.netrc".to_string(), "machine x"),
        ("runs/x/.envrc".to_string(), "echo x"),
        ("runs/x/.bashrc".to_string(), "echo x"),
        ("runs/x/.yarnrc".to_string(), "yarn-path \"e.js\""),
        ("runs/x/.yarnrc.yml".to_string(), "yarnPath: e.js"),
        ("runs/x/.pytest.ini".to_string(), "[pytest]"),
    ];
    for (p, c) in &dotted {
        let before = f.asker.cards().len();
        let out = dispatch(
            "files",
            &f.registry,
            &f.harness,
            "fs.write",
            json!({"path": p, "content": c}),
        )
        .await
        .unwrap();
        assert!(!out.text.starts_with("wrote"), "{p}: {}", out.text);
        assert_eq!(f.asker.cards().len(), before + 1, "{p}: no ask");
        assert!(!f.base().join(p).exists(), "{p} was written");
    }
    // The gate saw each dotfile write as Delete, and no tool call above Fill ran.
    let deletes = f
        .store
        .ledger()
        .iter()
        .filter(|(_, e)| e.tool == "fs.write" && e.class == Some(Class::Delete))
        .count();
    assert!(
        deletes >= dotted.len(),
        "{deletes} Delete ledger rows for {} dotfile writes",
        dotted.len()
    );
    assert!(f
        .tool_events()
        .iter()
        .all(|e| e.class.is_some_and(|k| k <= Class::Fill)));
    if matches!(
        srv_files::shell::Runner::discover(),
        srv_files::shell::Runner::Bash(_)
    ) {
        let before = f.asker.cards().len();
        let _ = dispatch(
            "files",
            &f.registry,
            &f.harness,
            "shell",
            json!({"cmd": format!("echo x > runs/{id}/files/.dotted")}),
        )
        .await
        .unwrap();
        assert_eq!(f.asker.cards().len(), before + 1, "shell dotfile: no ask");
        assert!(!f.run.dir.join("files").join(".dotted").exists());
    }

    // Still Delete in runs/: fs.write classify and the shell.
    let strict = [
        format!("runs/{id}/files/.git/hooks/pre-commit"),
        "runs/x/r/.git/hooks/post-index-change".into(),
        "runs/x/.gitattributes".into(),
        "runs/x/.curlrc".into(),
        "runs/x/_curlrc".into(),
        "runs/x/.wgetrc".into(),
        "runs/x/.gitmodules".into(),
        "runs/x/.gitconfig".into(),
        "runs/x/.git".into(),
        "runs/x/.vscode/tasks.json".into(),
        "runs/x/.cargo/config.toml".into(),
        "runs/x/.config/git/config".into(),
        // Spellings Windows maps to the same file.
        "runs/x/.GITATTRIBUTES".into(),
        "runs/x/.gitattributes.".into(),
        "runs/x/.gitattributes::$DATA".into(),
        "runs/x/.CurlRc".into(),
        "runs/x/.GIT/hooks/pre-commit".into(),
        "runs/x/.git./hooks/pre-commit".into(),
    ];
    let mut bad = Vec::new();
    for p in &strict {
        // Refused outright (bad arguments) is as safe as Delete.
        match f
            .try_tool_class("fs.write", json!({"path": p, "content": "x"}))
            .await
        {
            Ok(Class::Delete) | Err(_) => {}
            Ok(k) => bad.push(format!("fs.write {p} is {k:?}")),
        }
    }
    let lines: Vec<String> = strict.iter().map(|p| format!("echo x > '{p}'")).collect();
    bad.extend(wrong(&f, &lines, |k| k == Class::Delete));
    // The shell writes of a dotfile in runs/ are Delete too, any writer (D-550).
    let dot_lines = s(&[
        "echo x > runs/x/.npmrc",
        "echo x > runs/x/.netrc",
        "echo x > runs/x/.envrc",
        "echo 'yarn-path \"e.js\"' > runs/x/.yarnrc",
        "cp runs/x/a.txt runs/x/.yarnrc.yml",
        "touch runs/x/.lfsconfig",
        "echo x > runs/x/.mailmap",
        "echo x > runs/x/.tool-versions",
        "mv runs/x/a.txt runs/x/.pytest.ini",
    ]);
    bad.extend(wrong(&f, &dot_lines, |k| k == Class::Delete));
    // A plain name there is still scratch.
    bad.extend(wrong(
        &f,
        &s(&["echo x > runs/x/pip.conf", "cp runs/x/a.txt runs/x/b.txt"]),
        |k| k == Class::Read,
    ));
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

/// A dotfile written in runs/ without an ask must not be read as configuration by a
/// program the shell runs below Delete from that folder: npm, pip, yarn, node, make, hg,
/// direnv, git-lfs, gh and git in runs/ must all be Delete there (they read .npmrc,
/// pip.conf, .yarnrc.yml, Makefile, .hg/hgrc, .envrc, .lfsconfig ... from the cwd).
#[tokio::test]
async fn item7_adversarial_tools_that_read_cwd_dotfiles_are_delete_in_runs() {
    let f = fx().await;
    for (name, body) in [
        (".npmrc", "script-shell=evil\n"),
        (".yarnrc.yml", "yarnPath: evil.js\n"),
        ("pip.conf", "[global]\nindex-url=http://x\n"),
        (".envrc", "echo x > m\n"),
        (".lfsconfig", "[lfs]\nurl=x\n"),
        ("Makefile", "x:\n\techo x\n"),
    ] {
        std::fs::write(f.roots.runs.join("x").join(name), body).unwrap();
    }
    // Programs that read a dotfile from the cwd before anything else must be Delete in
    // runs/: yarn's launcher hands every command, --version included, to the yarnPath of
    // a .yarnrc.yml (yarn-path of a .yarnrc) in the cwd or above.
    let lines = s(&[
        "cd runs/x && yarn --version",
        "cd runs/x && yarn",
        "cd runs/x && yarn -v",
        "cd runs/x && yarn info lodash",
        "cd runs/x && npm view lodash",
        "cd runs/x && npm ls",
        "cd runs/x && npm config list",
        "cd runs/x && npm -v",
        "cd runs/x && npx -y cowsay hi",
        "cd runs/x && corepack --version",
        "cd runs/x && pip config list",
        "cd runs/x && pip download x",
        "cd runs/x && node -v",
        "cd runs/x && make -n",
        "cd runs/x && hg status",
        "cd runs/x && direnv allow",
        "cd runs/x && git lfs ls-files",
        "cd runs/x && git status",
        "git -C runs/x status",
        "cd runs/x && bash -c ls",
        "BASH_ENV=runs/x/.envrc bash -c ls",
        "cd runs/x && sh -c ls",
    ]);
    // Printed only (T14B_PRINT): --version of tools that read no cwd dotfile first.
    let _ = wrong(
        &f,
        &s(&[
            "cd runs/x && pnpm --version",
            "cd runs/x && npm --version",
            "cd runs/x && bun --version",
            "cd runs/x && deno --version",
            "cd runs/x && cargo --version",
            "cd runs/x && uv --version",
            "cd runs/x && pip --version",
            "cd runs/x && git --version",
            "cd runs/x && jq --version",
        ]),
        |_| true,
    );
    let bad = wrong(&f, &lines, |k| k == Class::Delete);
    assert!(
        bad.is_empty(),
        "want Delete:
{}",
        bad.join(
            "
"
        )
    );
}

// ---- item 8 (D-548): python -c ----

#[tokio::test]
async fn item8_python_c_stays_delete_with_variants() {
    let f = fx().await;
    std::fs::write(f.roots.runs.join("x").join("a.json"), "{}").unwrap();
    std::fs::write(f.roots.runs.join("x").join("s.py"), "print(1)").unwrap();
    let lines = s(&[
        "python3 -c 'print(1+1)'",
        "python -c \"print(len('abc'))\"",
        "python3 -I -S -c 'print(2)'",
        "python3 -Ic 'print(2)'",
        "python -E -c 1",
        "python -cprint(1)",
        "py -c 1",
        "py -3 -c 1",
        "python3.12 -c 1",
        "pythonw -c 1",
        "/usr/bin/python3 -c 1",
        "env python3 -c 1",
        "command python3 -c 1",
        "exec python3 -c 1",
        "echo 1 | xargs python3 -c",
        "python3 -c 1 > runs/x/o.txt",
        "PYTHONSTARTUP=runs/x/s.py python3 -c 1",
        "python3 -m json.tool runs/x/a.json",
        "python3 runs/x/s.py",
        "python3 < runs/x/s.py",
        "echo 'print(1)' | python3",
        "python3 - <<< 'print(1)'",
        "cd runs/x && python3 -c 'import json'",
    ]);
    let bad = wrong(&f, &lines, |k| k == Class::Delete);
    assert!(bad.is_empty(), "want Delete:\n{}", bad.join("\n"));
}

// ---- item 6 (D-549): a folder swapped for a real junction between check and write ----

fn junction(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    {
        Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link.to_string_lossy().replace('/', "\\"))
            .arg(target.to_string_lossy().replace('/', "\\"))
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

fn is_link(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// Swaps `d` between a fresh real folder and a junction (`dlink`) until `stop`; returns
/// how many times `d` was a junction. Real folders it moves away are kept as `old<N>`.
fn toggle(d: PathBuf, dlink: PathBuf, stop: Arc<AtomicBool>, seed_file: bool) -> u64 {
    let mut n = 0u64;
    let mut swaps = 0u64;
    let parent = d.parent().unwrap().to_path_buf();
    while !stop.load(Ordering::Relaxed) {
        n += 1;
        // d -> junction: move the real folder away, the junction in.
        if std::fs::symlink_metadata(&d).is_ok() && !is_link(&d) {
            let _ = std::fs::rename(&d, parent.join(format!("old{n}")));
        }
        if !is_link(&d) && is_link(&dlink) && std::fs::rename(&dlink, &d).is_ok() {
            swaps += 1;
        }
        for _ in 0..200 {
            std::hint::spin_loop();
        }
        // d -> real: move the junction back, make a fresh folder.
        if is_link(&d) {
            let _ = std::fs::rename(&d, &dlink);
        }
        if std::fs::symlink_metadata(&d).is_err() && std::fs::create_dir(&d).is_ok() && seed_file {
            let _ = std::fs::write(d.join("f.txt"), "scratch");
        }
        for _ in 0..200 {
            std::hint::spin_loop();
        }
    }
    swaps
}

/// The protected folder holds only f.txt = keep.
fn untouched(precious: &Path) -> Result<(), String> {
    let names: Vec<String> = std::fs::read_dir(precious)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let body = std::fs::read_to_string(precious.join("f.txt")).unwrap_or_default();
    if names == ["f.txt"] && body == "keep" {
        Ok(())
    } else {
        Err(format!("protected folder now {names:?}, f.txt = {body:?}"))
    }
}

/// fs.write and fs.delete of runs/x/d/f.txt, over and over, while another thread swaps
/// runs/x/d for a real junction to `.vscode` (a code folder: writing there is Delete and
/// the asker skips every ask). Nothing may be written or deleted in `.vscode`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item6_swapped_junction_race_never_writes_through() {
    let f = fx().await;
    let precious = f.base().join(".vscode");
    std::fs::create_dir_all(&precious).unwrap();
    std::fs::write(precious.join("f.txt"), "keep").unwrap();
    let d = f.roots.runs.join("x").join("d");
    let dlink = f.roots.runs.join("x").join("dlink");
    std::fs::create_dir_all(&d).unwrap();
    if !junction(&precious, &dlink) {
        eprintln!("skipped: cannot create a junction here");
        return;
    }
    let iters: usize = std::env::var("T14B_RACE_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let mut report = Vec::new();
    for (tool, seed) in [("fs.write", false), ("fs.delete", true)] {
        let stop = Arc::new(AtomicBool::new(false));
        let th = {
            let (d, dlink, stop) = (d.clone(), dlink.clone(), stop.clone());
            std::thread::spawn(move || toggle(d, dlink, stop, seed))
        };
        let (mut wrote, mut refused, mut skipped, mut other) = (0, 0, 0, 0);
        for i in 0..iters {
            let args = if tool == "fs.write" {
                json!({"path": "runs/x/d/f.txt", "content": format!("scratch {i}")})
            } else {
                json!({"path": "runs/x/d/f.txt"})
            };
            match dispatch("files", &f.registry, &f.harness, tool, args).await {
                Ok(o) if o.text.starts_with("wrote") || o.text.starts_with("deleted") => wrote += 1,
                Ok(o) if o.text.starts_with("skipped") => skipped += 1,
                Ok(o) if o.text.contains("not done") => refused += 1,
                Err(e) if e.to_string().contains("not done") => refused += 1,
                _ => other += 1,
            }
            if let Err(e) = untouched(&precious) {
                stop.store(true, Ordering::Relaxed);
                let _ = th.join();
                panic!("{tool} call {i} reached the junction target: {e}");
            }
        }
        stop.store(true, Ordering::Relaxed);
        let swaps = th.join().unwrap();
        report.push(format!(
            "{tool}: {iters} calls, done {wrote}, refused by the handle check {refused}, asked+skipped {skipped}, other errors {other}, junction swaps {swaps}"
        ));
        // Back to a known state: d a real folder, dlink the junction.
        if is_link(&d) {
            std::fs::rename(&d, &dlink).unwrap();
        }
        if std::fs::symlink_metadata(&d).is_err() {
            std::fs::create_dir(&d).unwrap();
        }
    }
    eprintln!("{}", report.join("\n"));
    untouched(&precious).unwrap();
}

/// The public safeio entry points against a real junction swapped in after the path was
/// resolved: an ancestor two levels up swapped, and the file's own folder swapped. Writes,
/// appends, new files, deletes and the entry checks all refuse; nothing lands outside.
#[test]
fn item6_safeio_refuses_through_real_junctions_at_any_depth() {
    let t = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(t.path()).unwrap();
    let precious = base.join("precious");
    std::fs::create_dir_all(precious.join("d")).unwrap();
    std::fs::write(precious.join("f.txt"), "keep").unwrap();
    std::fs::write(precious.join("d").join("f.txt"), "keep").unwrap();
    for (swap_rel, file_rel) in [("runs/x/d", "runs/x/d/f.txt"), ("runs/x", "runs/x/d/f.txt")] {
        let swap = base.join(swap_rel);
        let _ = std::fs::remove_dir_all(base.join("runs"));
        std::fs::create_dir_all(base.join("runs/x/d")).unwrap();
        std::fs::write(base.join("runs/x/d/f.txt"), "scratch").unwrap();
        let planned = srv_files::paths::resolve(&base, file_rel).unwrap();
        let new = planned.with_file_name("new.txt");
        std::fs::remove_dir_all(&swap).unwrap();
        if !junction(&precious, &swap) {
            eprintln!("skipped: cannot create a junction here");
            return;
        }
        let outside = if swap_rel == "runs/x" {
            precious.join("d").join("f.txt")
        } else {
            precious.join("f.txt")
        };
        assert!(
            srv_files::safeio::write(&planned, b"x").is_err(),
            "{swap_rel}"
        );
        assert!(srv_files::safeio::open_write(&planned, true).is_err());
        assert!(srv_files::safeio::write(&new, b"x").is_err());
        assert!(srv_files::safeio::remove_file(&planned).is_err());
        assert!(srv_files::safeio::check_entry(&planned).is_err());
        assert!(srv_files::safeio::check_parent(&planned).is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
        assert!(!outside.with_file_name("new.txt").exists());
        std::fs::remove_dir(&swap).unwrap();
    }
}

/// Outside D-545 (not a dotfile), found while probing item 7: rustup's proxy reads
/// rust-toolchain.toml from the cwd, and a `path` toolchain makes `cargo --version`
/// (Read) run `<path>/bin/cargo.exe`. The toolchain file is written through fs.write
/// without an ask; the program is placed in runs/ by the test (a scratch write the shell
/// also allows, e.g. printf of its bytes). It must not run without an ask.
#[tokio::test]
async fn item7_adversarial_rust_toolchain_file_in_cwd_does_not_run_a_program() {
    let f = fx().await;
    let tools_ok = matches!(
        srv_files::shell::Runner::discover(),
        srv_files::shell::Runner::Bash(_)
    ) && Command::new("rustc").arg("--version").output().is_ok()
        && Command::new("rustup").arg("--version").output().is_ok();
    if !tools_ok {
        eprintln!("skipped: needs bash, rustc and rustup");
        return;
    }
    // cargo test runs under rustup with RUSTUP_TOOLCHAIN set, which beats the file; the
    // harness's MCP server is not started by cargo, so the shell it runs has none.
    std::env::remove_var("RUSTUP_TOOLCHAIN");
    let marker = f.root.join("toolchain-marker");
    let src = f.root.join("m.rs");
    std::fs::write(
        &src,
        format!(
            "fn main() {{ std::fs::write({:?}, \"ran\").unwrap(); println!(\"not-cargo\"); }}\n",
            sh(&marker)
        ),
    )
    .unwrap();
    let exe = f.root.join("m.exe");
    let built = Command::new("rustc")
        .arg("-o")
        .arg(&exe)
        .arg(&src)
        .current_dir(&f.root)
        .output()
        .unwrap();
    if !built.status.success() {
        eprintln!("skipped: rustc could not build the probe: {built:?}");
        return;
    }
    let bin = f.roots.runs.join("x").join("tc").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let name = if cfg!(windows) { "cargo.exe" } else { "cargo" };
    std::fs::copy(&exe, bin.join(name)).unwrap();
    let tc = sh(&f.roots.runs.join("x").join("tc"));
    let out = dispatch(
        "files",
        &f.registry,
        &f.harness,
        "fs.write",
        json!({"path": "runs/x/rust-toolchain.toml", "content": format!("[toolchain]\npath = \"{tc}\"\n")}),
    )
    .await
    .unwrap();
    assert!(out.text.starts_with("wrote"), "{}", out.text);
    let out = dispatch(
        "files",
        &f.registry,
        &f.harness,
        "shell",
        json!({"cmd": "cd runs/x && cargo --version", "timeout": 60}),
    )
    .await
    .unwrap();
    let cards = f.asker.cards().len();
    eprintln!(
        "cargo --version in runs/x: {}",
        out.text.lines().take(3).collect::<Vec<_>>().join(" | ")
    );
    assert!(
        !marker.exists(),
        "cargo --version in runs/x ran the planted program with {cards} asks: {}",
        out.text.lines().take(3).collect::<Vec<_>>().join(" | ")
    );
}

/// Item 6 adversarial (review-T-14b should-fix 6): a call whose target folder does not
/// exist yet (fs.mkdir runs/x/d/new/a, fs.write runs/x/d/sub/f.txt) while runs/x/d is
/// swapped for a real junction to `.vscode`. `check_parent` skips a missing parent and
/// fs.write runs create_dir_all before its handle check, so the race may create folders
/// in the junction target. Nothing may appear in `.vscode`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item6_adversarial_new_subfolder_race_never_creates_outside() {
    let f = fx().await;
    let precious = f.base().join(".vscode");
    std::fs::create_dir_all(&precious).unwrap();
    std::fs::write(precious.join("f.txt"), "keep").unwrap();
    let d = f.roots.runs.join("x").join("d");
    let dlink = f.roots.runs.join("x").join("dlink");
    std::fs::create_dir_all(&d).unwrap();
    if !junction(&precious, &dlink) {
        eprintln!("skipped: cannot create a junction here");
        return;
    }
    let iters: usize = std::env::var("T14B_RACE_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let mut leaks = Vec::new();
    for tool in ["fs.mkdir", "fs.write"] {
        let stop = Arc::new(AtomicBool::new(false));
        let th = {
            let (d, dlink, stop) = (d.clone(), dlink.clone(), stop.clone());
            std::thread::spawn(move || toggle(d, dlink, stop, false))
        };
        for i in 0..iters {
            let args = if tool == "fs.mkdir" {
                json!({"path": format!("runs/x/d/new{i}/a")})
            } else {
                json!({"path": format!("runs/x/d/sub{i}/f.txt"), "content": "x"})
            };
            let _ = dispatch("files", &f.registry, &f.harness, tool, args).await;
            if let Err(e) = untouched(&precious) {
                leaks.push(format!("{tool} call {i}: {e}"));
                break;
            }
        }
        stop.store(true, Ordering::Relaxed);
        let _ = th.join();
        if is_link(&d) {
            std::fs::rename(&d, &dlink).unwrap();
        }
        if std::fs::symlink_metadata(&d).is_err() {
            std::fs::create_dir(&d).unwrap();
        }
        // Clean the target for the next tool (only what a leak left; f.txt stays).
        for e in std::fs::read_dir(&precious).unwrap().flatten() {
            if e.file_name() != "f.txt" {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    assert!(leaks.is_empty(), "\n{}", leaks.join("\n"));
}
