//! T-14b item 1 (D-543), tester-1: a write to `<home>/bin` is code-class even when the
//! bash PATH probe fails. This process points HOME at a fake home in the Git Bash
//! spelling (`/f/...`, not the profile folder) and SB_BASH at a bash that does not exist,
//! so the real `paths::path_dirs()` is built from a failed probe. Its own test binary:
//! HOME and SB_BASH are process-wide.

use harness_core::memory::{ts_now, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::Vault;
use harness_core::{Class, Harness, ToolCtx, ToolRegistry};
use serde_json::json;
use srv_files::paths::Roots;
use srv_files::shell::classify::classify;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn sh(p: &Path) -> String {
    srv_files::paths::plain(p).replace('\\', "/")
}

/// `C:/x/y` as Git Bash spells it: `/c/x/y`.
fn msys(p: &Path) -> String {
    let s = sh(p);
    let mut c = s.chars();
    match (c.next(), c.next()) {
        (Some(d), Some(':')) => format!("/{}{}", d.to_ascii_lowercase(), &s[2..]),
        _ => s,
    }
}

#[tokio::test]
async fn item1_home_bin_is_code_class_when_the_bash_probe_fails() {
    let t = tempfile::tempdir().unwrap();
    let root = PathBuf::from(srv_files::paths::plain(
        &std::fs::canonicalize(t.path()).unwrap(),
    ));
    let home = root.join("fakehome");
    // bin/ is not created yet: D-543 canonicalises the existing prefix.
    std::fs::create_dir_all(home.join("notbin")).unwrap();
    std::env::set_var("HOME", msys(&home));
    std::env::set_var(
        "SB_BASH",
        root.join("no-such-git").join("bin").join("bash.exe"),
    );

    // The probe fails (no bash there), so only the guessed Git folders come back.
    let probed = srv_files::shell::bash_path().expect("SB_BASH gives a Bash runner");
    let homes = srv_files::paths::home_dirs();
    assert!(
        homes.iter().any(|h| srv_files::safeio::same_path(h, &home)),
        "HOME {} not in home_dirs {homes:?}",
        msys(&home)
    );
    let probed_has_home_bin = probed
        .dirs
        .iter()
        .any(|d| srv_files::safeio::same_path(d, &home.join("bin")));
    assert!(
        !probed_has_home_bin,
        "the probe was meant to fail: {probed:?}"
    );

    let base = root.join("base");
    let runs_root = base.join("runs");
    std::fs::create_dir_all(&runs_root).unwrap();
    let run = RunFolder::create(&runs_root, "t14b", "home bin").unwrap();
    std::fs::create_dir_all(runs_root.join("x")).unwrap();
    std::fs::write(runs_root.join("x").join("a.txt"), "a").unwrap();
    let roots = Roots::from_run_dir(&run.dir).unwrap();

    // Shell writers, Windows, MSYS, ~ and case spellings.
    let hb = sh(&home.join("bin"));
    let mb = msys(&home.join("bin"));
    let mut bad = Vec::new();
    for line in [
        format!("cp runs/x/a.txt '{hb}/jq'"),
        format!("echo x > '{hb}/jq.exe'"),
        format!("echo x > '{mb}/git'"),
        format!("cp runs/x/a.txt '{}/JQ'", sh(&home.join("BIN"))),
        "cp runs/x/a.txt ~/bin/jq".to_string(),
        "echo x > ~/bin/ls".to_string(),
        "mkdir -p ~/bin && cp runs/x/a.txt ~/bin/cat".to_string(),
    ] {
        let v = classify(&roots, &roots.base, &line, false);
        if v.class != Class::Delete {
            bad.push(format!("{line:?} is {:?} ({})", v.class, v.why()));
        }
    }

    // fs.write and fs.move through the handler's classify (nothing is written).
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "home bin".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    let harness = Harness {
        store,
        vault: Arc::new(Vault::in_memory([])),
        asker: Arc::new(ScriptedAsker::new()),
        runs_root,
        run: Some(run.id.clone()),
    };
    let registry = ToolRegistry::new(srv_files::fs::tools());
    let classify_tool = |tool: &str, args: serde_json::Value| {
        let spec = registry.get(tool).unwrap();
        let ctx = ToolCtx::new(
            run.id.clone(),
            run.dir.clone(),
            spec.class,
            &harness,
            registry.clone(),
        );
        async move {
            let c = spec.handler.classify(&ctx, &args).await.unwrap();
            c.class.map_or(spec.class, |k| k.max(spec.class))
        }
    };
    for (tool, args) in [
        (
            "fs.write",
            json!({"path": format!("{hb}/jq"), "content": "x"}),
        ),
        (
            "fs.write",
            json!({"path": format!("{hb}/jq.exe"), "content": "x", "append": true}),
        ),
        (
            "fs.move",
            json!({"from": "runs/x/a.txt", "to": format!("{hb}/jq")}),
        ),
    ] {
        let k = classify_tool(tool, args.clone()).await;
        if k != Class::Delete {
            bad.push(format!("{tool} {args} is {k:?}"));
        }
    }
    // Control: a sibling folder of bin is an ordinary outside write (Fill), so the check
    // can fail.
    let k = classify_tool(
        "fs.write",
        json!({"path": format!("{}/jq", sh(&home.join("notbin"))), "content": "x"}),
    )
    .await;
    assert_eq!(k, Class::Fill, "control");
    // Now that bin/ exists (created after path_dirs was computed), still Delete.
    std::fs::create_dir_all(home.join("bin")).unwrap();
    let k = classify_tool(
        "fs.write",
        json!({"path": format!("{hb}/jq"), "content": "x"}),
    )
    .await;
    if k != Class::Delete {
        bad.push(format!("fs.write {hb}/jq after mkdir is {k:?}"));
    }
    assert!(bad.is_empty(), "want Delete:\n{}", bad.join("\n"));
}
