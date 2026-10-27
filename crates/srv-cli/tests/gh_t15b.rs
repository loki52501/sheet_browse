//! T-15b items 2 and 3 for C-1 `gh(args)`, through the H-6 wrapper (`dispatch`) with the
//! fake gh (tests/fixtures/fake-gh.*): `alias list` prints `<log>.aliases`, anything else
//! echoes its arguments, so the output shows what really ran.
//!
//! D-591: an alias is classified and run as its expansion (auth refused, delete asks
//! Delete, read runs at once), and `alias set` is at the class of what the alias would run.
//! D-592: a flag's value is not a subcommand or `-w`, and no write loses its class.

mod common;

use common::{fixtures, last_card_class, Env, Tmp};
use harness_core::class::Class;
use harness_core::gate::AskOutcome;
use harness_core::mcp::ToolError;
use serde_json::json;
use srv_cli::Config;
use std::path::Path;

fn fake_gh(log: &Path) -> Vec<String> {
    let log = log.display().to_string();
    if cfg!(windows) {
        vec![fixtures().join("fake-gh.cmd").display().to_string(), log]
    } else {
        vec![
            "sh".into(),
            fixtures().join("fake-gh.sh").display().to_string(),
            log,
        ]
    }
}

/// An env whose fake gh lists `aliases` (gh's `gh alias list` format).
async fn env_with_aliases(dir: &Tmp, aliases: &str, answers: Vec<AskOutcome>) -> Env {
    let log = dir.0.join("gh.log");
    std::fs::write(&log, "").unwrap();
    std::fs::write(dir.0.join("gh.log.aliases"), aliases).unwrap();
    let cfg = Config {
        gh: fake_gh(&log),
        ..Config::default()
    };
    Env::new(srv_cli::tools_with(cfg), "gh_canary", answers).await
}

const ALIASES: &str = "co: pr checkout\ntok: auth token\nrmr: repo delete\nv: pr view\n\
sh1: '!gh issue list | cat'\n";

#[tokio::test]
async fn an_alias_runs_as_its_expansion_at_its_class() {
    let dir = Tmp::new("alias-run");
    let env = env_with_aliases(
        &dir,
        ALIASES,
        vec![AskOutcome::AnsweredSkip, AskOutcome::AnsweredProceed],
    )
    .await;

    // An alias of a read runs at once, as the read.
    let out = env
        .call("gh", json!({"args": "v 5"}))
        .await
        .expect("alias of pr view");
    assert!(out.text.contains("fake gh: pr view 5"), "{}", out.text);
    assert_eq!(env.last_tool_event().class, Some(Class::Read));
    assert!(env.asker.cards().is_empty(), "a read alias never asks");

    // An alias of `auth token` is refused like `auth token`, and nothing runs.
    let err = env
        .call("gh", json!({"args": "tok"}))
        .await
        .expect_err("alias of gh auth");
    assert!(
        matches!(&err, ToolError::Args(m) if m.contains("gh auth")),
        "{err}"
    );
    let err = env
        .call("gh", json!({"args": "sh1"}))
        .await
        .expect_err("shell alias");
    assert!(
        matches!(&err, ToolError::Args(m) if m.contains("shell")),
        "{err}"
    );
    assert!(env.asker.cards().is_empty());

    // An alias of a delete asks as Delete; skipped, nothing runs; approved, the expansion
    // runs (not the alias name).
    for want_ran in [false, true] {
        let out = env
            .call("gh", json!({"args": "rmr o/r --yes"}))
            .await
            .expect("alias of repo delete");
        assert_eq!(last_card_class(&env.asker), Some(Class::Delete));
        assert!(env.asker.cards().last().unwrap().what.contains("alias rmr"));
        assert_eq!(
            out.text.contains("fake gh: repo delete o/r --yes"),
            want_ran,
            "{}",
            out.text
        );
    }
    assert_eq!(env.last_tool_event().class, Some(Class::Delete));
    assert_eq!(env.asker.cards().len(), 2);
}

#[tokio::test]
async fn alias_set_is_at_the_class_of_its_expansion() {
    let dir = Tmp::new("alias-set");
    let env = env_with_aliases(&dir, ALIASES, vec![AskOutcome::AnsweredSkip; 3]).await;
    for cmd in [
        "alias set tok2 'auth token'",
        "alias set t3 tok",
        "alias set c2 'config get oauth_token'",
        "alias set --shell s 'echo hi'",
    ] {
        let err = env
            .call("gh", json!({ "args": cmd }))
            .await
            .expect_err("refused alias set");
        assert!(matches!(err, ToolError::Args(_)), "{cmd}: {err}");
    }
    assert!(
        env.asker.cards().is_empty(),
        "a refused alias set never asks"
    );
    let cases: &[(&str, Class)] = &[
        ("alias set rm2 'repo delete'", Class::Delete),
        ("alias set rm3 rmr", Class::Delete),
        ("alias set pv 'pr view'", Class::Account),
    ];
    for (i, (cmd, want)) in cases.iter().enumerate() {
        let out = env
            .call("gh", json!({ "args": cmd }))
            .await
            .expect("skipped alias set");
        assert!(out.text.starts_with("skipped: you said no"), "{cmd}");
        assert_eq!(env.asker.cards().len(), i + 1, "{cmd}");
        assert_eq!(last_card_class(&env.asker), Some(*want), "{cmd}");
    }
    // Nothing ran: refusals are failed events, skips only ask outcomes.
    assert!(env.events().iter().all(|e| !e.ok || e.server == "core"));
}

#[tokio::test]
async fn flag_values_are_not_subcommands_and_writes_keep_their_class() {
    let dir = Tmp::new("flags");
    let env = env_with_aliases(&dir, "", vec![AskOutcome::AnsweredSkip; 2]).await;
    // False positives fixed: -R's value and run list's -w (--workflow) run as reads.
    for cmd in ["pr view 5 -Rw/x", "run list -w ci.yml", "pr -R o/r view 5"] {
        env.call("gh", json!({ "args": cmd }))
            .await
            .unwrap_or_else(|e| panic!("{cmd}: {e}"));
        assert_eq!(env.last_tool_event().class, Some(Class::Read), "{cmd}");
    }
    assert!(env.asker.cards().is_empty());
    // True positives next to them: a delete after -R, and -w after a bool flag.
    env.call("gh", json!({"args": "run -R o/r delete 5"}))
        .await
        .expect("skipped");
    assert_eq!(last_card_class(&env.asker), Some(Class::Delete));
    let out = env
        .call("gh", json!({"args": "pr view 5 -cw"}))
        .await
        .expect("navigate");
    assert!(out.text.contains("fake gh: pr view 5 -cw"), "{}", out.text);
    assert_eq!(env.last_tool_event().class, Some(Class::Navigate));
    // -R inside a cluster names a host: refused.
    let err = env
        .call("gh", json!({"args": "pr view -cR127.0.0.1:9/o/r 5"}))
        .await
        .expect_err("other host");
    assert!(matches!(err, ToolError::Args(_)), "{err}");
}

/// review-T-15b Blocking (D-594): a flag before the group is handed to the command by gh,
/// so it is classified (and run) behind the group: a delete asks Delete, the auth refusal,
/// the alias lookup and the host check still apply, and any other flag first is refused.
#[tokio::test]
async fn a_flag_before_the_group_keeps_every_check() {
    let dir = Tmp::new("lead-flags");
    let env = env_with_aliases(&dir, ALIASES, vec![AskOutcome::AnsweredProceed; 2]).await;
    for cmd in [
        "--hostname github.com auth token",
        "--hostname=github.com auth token",
        "--hostname github.com tok",
        "--repo=other.host/o/r pr view 5",
        "--bogus run delete 5",
        "-y repo delete o/r",
    ] {
        let err = env.call("gh", json!({ "args": cmd })).await.expect_err(cmd);
        assert!(matches!(err, ToolError::Args(_)), "gh {cmd}: {err}");
    }
    assert!(env.asker.cards().is_empty(), "a refusal never asks");
    // A delete asks as Delete, and what runs is the command with the flag behind the group.
    // (These run the fake gh, and cmd.exe splits its arguments at `=`: two-word forms.)
    let out = env
        .call("gh", json!({"args": "-R o/r run delete 5"}))
        .await
        .expect("approved delete");
    assert_eq!(last_card_class(&env.asker), Some(Class::Delete));
    assert!(
        out.text.contains("fake gh: run -R o/r delete 5"),
        "{}",
        out.text
    );
    // A read with --repo first runs at once as the read.
    let out = env
        .call("gh", json!({"args": "--repo o/r pr view 5"}))
        .await
        .expect("read");
    assert!(
        out.text.contains("fake gh: pr --repo o/r view 5"),
        "{}",
        out.text
    );
    assert_eq!(env.last_tool_event().class, Some(Class::Read));
    assert_eq!(env.asker.cards().len(), 1);
}

/// review-T-15b should-fix 1 (D-595): an alias swapped between classify and call for the
/// same command on another configured host (same class, another gate domain) is refused,
/// and nothing but `gh alias list` is spawned (fake-gh-swap logs every other call).
#[tokio::test]
async fn a_plan_that_changed_after_classify_does_not_run() {
    let dir = Tmp::new("swap-host");
    let log = dir.0.join("gh.log");
    std::fs::write(&log, "").unwrap();
    std::fs::write(dir.0.join("gh.log.aliases"), "x: pr view\n").unwrap();
    std::fs::write(
        dir.0.join("gh.log.aliases2"),
        "x: pr view -R ghe.corp/o/r\n",
    )
    .unwrap();
    let swap = if cfg!(windows) {
        vec![
            fixtures().join("fake-gh-swap.cmd").display().to_string(),
            log.display().to_string(),
        ]
    } else {
        vec![
            "sh".into(),
            fixtures().join("fake-gh-swap.sh").display().to_string(),
            log.display().to_string(),
        ]
    };
    let cfg = Config {
        gh: swap,
        gh_hosts: vec!["ghe.corp".into()],
        ..Config::default()
    };
    let env = Env::new(srv_cli::tools_with(cfg), "gh_canary", vec![]).await;
    let err = env
        .call("gh", json!({"args": "x 5"}))
        .await
        .expect_err("the plan changed");
    assert!(
        matches!(&err, ToolError::Failed(m) if m.contains("changed after it was classified")),
        "{err}"
    );
    assert!(
        !dir.0.join("gh.log.ran").exists(),
        "gh ran the swapped alias"
    );
    // Planned the same way twice (the list no longer changes): it runs.
    let out = env
        .call("gh", json!({"args": "x 5"}))
        .await
        .expect("same plan");
    assert!(
        out.text.contains("fake gh: pr view -R ghe.corp/o/r 5"),
        "{}",
        out.text
    );
}

/// review-T-15b should-fix 4 (D-597): `alias import` would store aliases nobody checked, so
/// it is refused before any card; `alias set` (checked) still asks.
#[tokio::test]
async fn alias_import_is_refused() {
    let dir = Tmp::new("alias-import");
    let env = env_with_aliases(&dir, ALIASES, vec![AskOutcome::AnsweredSkip]).await;
    for cmd in [
        "alias import x.yml",
        "alias import -",
        "alias --bogus import x.yml",
    ] {
        let err = env.call("gh", json!({ "args": cmd })).await.expect_err(cmd);
        assert!(
            matches!(&err, ToolError::Args(m) if m.contains("alias set")),
            "gh {cmd}: {err}"
        );
    }
    assert!(env.asker.cards().is_empty(), "a refusal never asks");
    env.call("gh", json!({"args": "alias set pv 'pr view'"}))
        .await
        .expect("skipped");
    assert_eq!(last_card_class(&env.asker), Some(Class::Account));
    assert!(env.events().iter().all(|e| !e.ok || e.server == "core"));
}

/// review-T-15b should-fix 3 (D-591 fails closed): when `gh alias list` fails, a call that
/// may name an alias is refused at classify with no card and nothing run, whatever class
/// the word would have (fixture fake-gh-page: `<page>.aliasfail` makes `alias list` exit 1;
/// any other call prints the page, so a run would show as an ok event).
#[tokio::test]
async fn a_failing_alias_list_refuses_the_call() {
    let dir = Tmp::new("alias-list-fails");
    let page = dir.0.join("page.txt");
    std::fs::write(&page, "it ran\n").unwrap();
    std::fs::write(dir.0.join("page.txt.aliasfail"), "").unwrap();
    let script = if cfg!(windows) {
        vec![
            fixtures().join("fake-gh-page.cmd").display().to_string(),
            page.display().to_string(),
        ]
    } else {
        vec![
            "sh".into(),
            fixtures().join("fake-gh-page.sh").display().to_string(),
            page.display().to_string(),
        ]
    };
    let cfg = Config {
        gh: script,
        ..Config::default()
    };
    let env = Env::new(
        srv_cli::tools_with(cfg),
        "gh_canary",
        vec![AskOutcome::AnsweredProceed; 4],
    )
    .await;
    for cmd in [
        "x 5",
        "pr merge 5",
        "rmr o/r --yes",
        "--repo=o/r pr merge 5",
    ] {
        let err = env.call("gh", json!({ "args": cmd })).await.expect_err(cmd);
        assert!(
            matches!(&err, ToolError::Failed(m)
                if m.contains("cannot tell whether this names a gh alias")),
            "gh {cmd}: {err}"
        );
    }
    assert!(env.asker.cards().is_empty(), "nothing was asked");
    assert!(
        env.events().iter().all(|e| !e.ok || e.server == "core"),
        "an event shows a run"
    );
    // The listing works again: the same call asks and runs.
    std::fs::remove_file(dir.0.join("page.txt.aliasfail")).unwrap();
    let out = env
        .call("gh", json!({"args": "pr merge 5"}))
        .await
        .expect("runs");
    assert!(out.text.contains("it ran"), "{}", out.text);
    assert_eq!(env.asker.cards().len(), 1);
}
