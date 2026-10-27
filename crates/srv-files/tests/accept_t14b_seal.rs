//! T-14b item 5 (D-547), tester-1 adversarial: settings that make git run a program,
//! planted in an existing global config (~/.gitconfig, an included file, the XDG config
//! and attributes) and in a repository's own config, never run under Read-class git
//! through the `shell` tool. Each plant writes its own marker file; a battery of
//! Read-looking git lines runs through dispatch (gate, asker that skips every ask,
//! ledger), then the same battery runs in bash as configured, which must write the
//! markers (so every plant is live and the test can fail). The vault canary is planted
//! too and must not leave through text, ledger rows or ledger.jsonl.
//!
//! Its own test binary: HOME points at a fake home for the whole process.

use harness_core::memory::{ts_now, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::{EntryKind, Vault};
use harness_core::{dispatch, Harness, ToolRegistry};
use serde_json::json;
use srv_files::shell::Runner;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "canary-t14b-not-a-secret".to_owned())
}

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

/// A repository with a tracked a.txt (one commit), runs/ ignored, and a change.
fn repo(g: &Path, dir: &Path, attrs: &str) {
    std::fs::create_dir_all(dir).unwrap();
    git(g, dir, &["init", "-q"]);
    std::fs::write(dir.join(".gitignore"), "runs/\n").unwrap();
    std::fs::write(dir.join(".gitattributes"), attrs).unwrap();
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(g, dir, &["add", "."]);
    git(g, dir, &["commit", "-q", "-m", "init"]);
    std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(dir.join("msg.txt"), "subject\n").unwrap();
}

/// Read-looking lines that would start a planted program somewhere.
const BATTERY: &[&str] = &[
    "git status --short",
    "git status",
    "git diff --stat",
    "git diff",
    "git log -p -1",
    "git show HEAD",
    "git blame a.txt",
    "git cat-file --textconv HEAD:a.txt",
    "git grep -O one",
    "git grep --open-files-in-pager one",
    "git archive --format=tgz HEAD | wc -c",
    "git archive --format=evil HEAD | wc -c",
    "git st",
    "git st2",
    "git interpret-trailers --trailer sign < msg.txt",
    "git ls-remote ssh://localhost:1/x",
    "git ls-remote 'ext::sh ../ext.sh'",
    "git var GIT_EDITOR",
    "git log --show-signature -1",
];

struct Case {
    name: &'static str,
    base: PathBuf,
}

#[tokio::test]
async fn item5_planted_programs_never_run_under_read_git() {
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
    let root = PathBuf::from(srv_files::paths::plain(
        &std::fs::canonicalize(t.path()).unwrap(),
    ));
    let home = root.join("home");
    let marks = root.join("marks");
    for d in [&home, &marks, &home.join(".config").join("git")] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::env::set_var("HOME", &home);
    let m = sh(&marks);

    // Global plants: ~/.gitconfig, a file it includes, the XDG config and attributes.
    std::fs::write(
        home.join(".gitconfig"),
        format!(
            "[include]\n\tpath = {inc}\n\
             [core]\n\tpager = \"sh -c 'echo x >> {m}/global-pager; cat'\"\n\
             \teditor = \"sh -c 'echo x >> {m}/global-editor'\"\n\
             \tsshCommand = \"sh -c 'echo x >> {m}/global-ssh'\"\n\
             [diff]\n\texternal = \"sh -c 'echo x >> {m}/global-extdiff'\"\n\
             [filter \"mark\"]\n\tclean = \"sh -c 'echo x >> {m}/global-filter; cat'\"\n\
             [alias]\n\tst = \"!echo x >> {m}/global-alias\"\n\
             [tar \"tgz\"]\n\tcommand = \"sh -c 'echo x >> {m}/global-tar; cat'\"\n\
             [trailer \"sign\"]\n\tkey = Signed-off-by\n\tcommand = \"echo x >> {m}/global-trailer\"\n\
             [gpg]\n\tprogram = \"sh -c 'echo x >> {m}/global-gpg'\"\n\
             [log]\n\tshowSignature = true\n\
             [protocol \"ext\"]\n\tallow = always\n",
            inc = sh(&home.join("inc.cfg"))
        ),
    )
    .unwrap();
    std::fs::write(
        home.join("inc.cfg"),
        format!("[core]\n\tfsmonitor = \"sh -c 'echo x >> {m}/global-include-fsmonitor'\"\n"),
    )
    .unwrap();
    std::fs::write(
        home.join(".config").join("git").join("config"),
        format!(
            "[diff \"xdg\"]\n\ttextconv = \"sh -c 'echo x >> {m}/xdg-textconv; cat \\\"$0\\\"'\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        home.join(".config").join("git").join("attributes"),
        "*.txt diff=xdg\n",
    )
    .unwrap();

    // The ext:: transport runs this (protocol.ext.allow = always in both configs).
    std::fs::write(root.join("ext.sh"), format!("echo x >> {m}/ext-proto\n")).unwrap();

    // Case A: a clean repository under the global plants.
    let a = root.join("a");
    repo(&g, &a, "*.txt filter=mark\n");
    // Case B: a repository whose own config names programs the seal does not list as
    // unsealable (pager, tar filter, alias, trailer command, sshCommand, editor).
    let b = root.join("b");
    repo(&g, &b, "");
    let cfg = b.join(".git").join("config");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str(&format!(
        "[core]\n\tpager = \"sh -c 'echo x >> {m}/repo-pager; cat'\"\n\
         \tsshCommand = \"sh -c 'echo x >> {m}/repo-ssh'\"\n\
         \teditor = \"sh -c 'echo x >> {m}/repo-editor'\"\n\
         \tfsmonitor = \"sh -c 'echo x >> {m}/repo-fsmonitor'\"\n\
         [protocol \"ext\"]\n\tallow = always\n\
         [tar \"evil\"]\n\tcommand = \"sh -c 'echo x >> {m}/repo-tar; cat'\"\n\
         [alias]\n\tst2 = \"!echo x >> {m}/repo-alias\"\n\
         [trailer \"sign\"]\n\tkey = Signed-off-by\n\tcmd = \"echo x >> {m}/repo-trailer\"\n"
    ));
    std::fs::write(&cfg, text).unwrap();
    std::fs::write(
        b.join(".git").join("hooks").join("post-index-change"),
        format!("#!/bin/sh\necho x >> {m}/repo-hook\n"),
    )
    .unwrap();

    let cases = [
        Case {
            name: "global plants",
            base: a,
        },
        Case {
            name: "repo plants",
            base: b,
        },
    ];
    let secret = canary();
    let mut leaks = Vec::new();
    let mut live_all = Vec::new();
    for case in &cases {
        let runs_root = case.base.join("runs");
        let run = RunFolder::create(&runs_root, "t14b", "sealed git").unwrap();
        std::fs::create_dir_all(runs_root.join("x")).unwrap();
        std::fs::write(runs_root.join("x").join("secret.txt"), &secret).unwrap();
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
        let vault = Vault::in_memory([("t14b".to_owned(), secret.clone(), EntryKind::Secret)]);
        vault.install_global();
        let harness = Harness {
            store: store.clone(),
            vault: Arc::new(vault),
            asker: Arc::new(ScriptedAsker::new()),
            runs_root,
            run: Some(run.id.clone()),
        };
        let registry = ToolRegistry::new(srv_files::shell::tools_with(runner.clone()));
        clear(&marks);
        let mut texts = Vec::new();
        for cmd in BATTERY
            .iter()
            .map(|c| c.to_string())
            .chain(["cat runs/x/secret.txt && git status --short".to_string()])
        {
            let out = dispatch(
                "files",
                &registry,
                &harness,
                "shell",
                json!({"cmd": cmd, "timeout": 30}),
            )
            .await;
            let text = match out {
                Ok(o) => o.text,
                Err(e) => format!("error: {e}"),
            };
            if std::env::var_os("T14B_PRINT").is_some() {
                eprintln!(
                    "[{}] {cmd}: {}",
                    case.name,
                    text.lines().next().unwrap_or("")
                );
            }
            let mut rows = store.ledger();
            rows.sort_by_key(|(id, _)| *id);
            let class = rows
                .iter()
                .rev()
                .find(|(_, e)| e.server == "files" || e.server == "core")
                .map(|(_, e)| format!("{} {:?} {:?}", e.server, e.class, e.mode));
            let now = markers(&marks);
            if !now.is_empty() {
                leaks.push(format!(
                    "[{}] {cmd:?} ran {now:?} (last ledger row: {class:?})",
                    case.name
                ));
                clear(&marks);
            }
            texts.push(text);
        }
        // Vault canary: counts only, never printed.
        let mut hits = texts.iter().filter(|t| t.contains(&secret)).count();
        hits += store
            .ledger()
            .iter()
            .filter(|(_, e)| serde_json::to_string(e).unwrap().contains(&secret))
            .count();
        let jsonl = std::fs::read_to_string(run.ledger_jsonl()).unwrap_or_default();
        hits += jsonl.matches(&secret).count();
        assert_eq!(hits, 0, "[{}] canary hits in text/ledger/jsonl", case.name);
        assert!(
            texts.last().is_some_and(|t| t.contains("@vault(t14b)")),
            "[{}] positive control: the canary was read and scrubbed",
            case.name
        );

        // Control: the same battery in bash as configured writes the markers.
        clear(&marks);
        let line = BATTERY
            .iter()
            .map(|c| format!("( {c} ) >/dev/null 2>&1 </dev/null"))
            .collect::<Vec<_>>()
            .join("; ");
        let line = line.replace(
            "( git interpret-trailers --trailer sign < msg.txt ) >/dev/null 2>&1 </dev/null",
            "( git interpret-trailers --trailer sign < msg.txt ) >/dev/null 2>&1",
        );
        let out = Command::new(&bash)
            .args(["--noprofile", "--norc", "-c", &line])
            .current_dir(&case.base)
            .env("HOME", &home)
            .output()
            .unwrap();
        let live = markers(&marks);
        eprintln!(
            "[{}] control exit {:?}, live plants: {live:?}",
            case.name,
            out.status.code()
        );
        live_all.extend(live);
        clear(&marks);
    }
    // Every plant must be live in the control (else the probe proves nothing).
    for want in [
        "global-pager",
        "global-extdiff",
        "global-filter",
        "global-alias",
        "global-tar",
        "global-trailer",
        "global-include-fsmonitor",
        "global-ssh",
        "xdg-textconv",
        "repo-pager",
        "repo-tar",
        "repo-alias",
        "repo-trailer",
        "repo-ssh",
        "repo-fsmonitor",
        "repo-hook",
        "ext-proto",
    ] {
        assert!(
            live_all.iter().any(|l| l == want),
            "plant {want} not live in the control: {live_all:?}"
        );
    }
    assert!(
        leaks.is_empty(),
        "planted programs ran:\n{}",
        leaks.join("\n")
    );
}
