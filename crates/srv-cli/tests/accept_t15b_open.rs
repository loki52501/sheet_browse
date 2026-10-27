//! T-15b acceptance: adversarial cases that FAIL on 03dd5d5 (recorded red in
//! bench/results/accept-T-15b.md). They are the review-T-15b Blocking finding (a flag before
//! the gh group) and should-fix 1 (an alias swapped for one of the same class between
//! classify and call). Kept in their own binary so the rest of srv-cli stays readable; each
//! turns green when the server does what D-591/D-592 say.

mod common;

use common::{fixtures, last_card_class, Env, Tmp};
use harness_core::class::Class;
use harness_core::gate::AskOutcome;
use harness_core::mcp::ToolError;
use serde_json::json;
use srv_cli::Config;
use std::path::Path;

fn fake(script: &str, arg: &Path) -> Vec<String> {
    let arg = arg.display().to_string();
    if cfg!(windows) {
        vec![
            fixtures()
                .join(format!("{script}.cmd"))
                .display()
                .to_string(),
            arg,
        ]
    } else {
        vec![
            "sh".into(),
            fixtures()
                .join(format!("{script}.sh"))
                .display()
                .to_string(),
            arg,
        ]
    }
}

async fn gh_env(dir: &Tmp, script: &str, aliases: &str, answers: Vec<AskOutcome>) -> Env {
    let log = dir.0.join("gh.log");
    std::fs::write(&log, "").unwrap();
    std::fs::write(dir.0.join("gh.log.aliases"), aliases).unwrap();
    let cfg = Config {
        gh: fake(script, &log),
        ..Config::default()
    };
    Env::new(srv_cli::tools_with(cfg), "gh_canary", answers).await
}

/// Item 2: gh (cobra) strips a flag before the group and still expands the alias, so
/// `gh --hostname github.com tok` runs `auth token`. It must be refused like `gh tok`.
#[tokio::test]
async fn item2_open_leading_flag_does_not_skip_the_alias_or_auth_refusal() {
    let dir = Tmp::new("open-lead-alias");
    let env = gh_env(
        &dir,
        "fake-gh-swap",
        "tok: auth token\n",
        vec![AskOutcome::AnsweredProceed; 4],
    )
    .await;
    for cmd in [
        "--hostname github.com tok",
        "--hostname github.com auth token",
        "--hostname=github.com auth token",
    ] {
        let r = env.call("gh", json!({ "args": cmd })).await;
        assert!(
            matches!(r, Err(ToolError::Args(_))),
            "gh {cmd} was not refused: {:?}",
            r.as_ref().map(|o| &o.text)
        );
    }
    assert!(
        !dir.0.join("gh.log.ran").exists(),
        "gh ran a call that reaches auth token"
    );
}

/// Item 3: a flag before the group is handed to the subcommand by gh, so these run deletes
/// (and the repo host is the flag's). The class must be Delete, and another host refused.
#[tokio::test]
async fn item3_open_leading_flag_does_not_lower_a_delete() {
    let dir = Tmp::new("open-lead-del");
    let env = gh_env(&dir, "fake-gh", "", vec![AskOutcome::AnsweredSkip; 4]).await;
    for cmd in [
        "--repo=o/r run delete 5",
        "--yes=true repo delete o/r",
        "-R o/r run delete 5",
    ] {
        let r = env.call("gh", json!({ "args": cmd })).await;
        assert!(r.is_ok(), "gh {cmd}: {r:?}");
        assert_eq!(last_card_class(&env.asker), Some(Class::Delete), "gh {cmd}");
    }
    let r = env
        .call("gh", json!({"args": "--repo=evil.example/o/r pr view 5"}))
        .await;
    assert!(
        matches!(r, Err(ToolError::Args(_))),
        "another host was not refused: {:?}",
        r.as_ref().map(|o| &o.text)
    );
}

/// D-591 ("what the gate saw is what runs"): the alias is swapped for another read between
/// classify and call. The call must refuse, not run a command nobody classified.
#[tokio::test]
async fn item2_open_equal_class_alias_swap_is_refused() {
    let dir = Tmp::new("open-swap-eq");
    std::fs::write(dir.0.join("gh.log.aliases2"), "x: issue list\n").unwrap();
    let env = gh_env(&dir, "fake-gh-swap", "x: pr view\n", vec![]).await;
    let r = env.call("gh", json!({"args": "x 5"})).await;
    let ran = std::fs::read_to_string(dir.0.join("gh.log.ran")).unwrap_or_default();
    assert!(
        r.is_err() && ran.is_empty(),
        "classified `pr view 5`, ran {ran:?}: {:?}",
        r.as_ref().map(|o| &o.text)
    );
}
