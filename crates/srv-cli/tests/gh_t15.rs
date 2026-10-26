//! T-15 C-1 `gh(args)` through the H-6 wrapper (`dispatch`), offline: a fake gh
//! (tests/fixtures/fake-gh.cmd or fake-gh.sh) prints the log file the test chose for
//! `run view <id> --log` and echoes its arguments otherwise.
//! docs/04 acceptance: `gh("run view 123 --log")` returns the first failing step when given a
//! fixture log. D-540 classification: reads run, writes ask (Account), DELETE asks (Delete),
//! unknown commands fail closed to Account, `gh auth` is refused.

mod common;

use common::{assert_clean, canary, fixtures, last_card_class, Env, Tmp};
use harness_core::class::Class;
use harness_core::gate::AskOutcome;
use harness_core::mcp::ToolError;
use harness_core::memory::LedgerMode;
use serde_json::json;
use srv_cli::Config;
use std::path::Path;

/// The command vector that runs the fake gh with `log` as the log it prints.
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

async fn env_with_log(log: &Path, answers: Vec<AskOutcome>) -> Env {
    let cfg = Config {
        gh: fake_gh(log),
        ..Config::default()
    };
    Env::new(srv_cli::tools_with(cfg), "gh_canary", answers).await
}

#[tokio::test]
async fn acceptance_run_view_log_returns_first_failing_step() {
    let log = fixtures().join("gh-run-123.log");
    let env = env_with_log(&log, vec![]).await;
    let out = env
        .call("gh", json!({"args": "run view 123 --log"}))
        .await
        .expect("gh run view --log");
    let t = &out.text;
    assert!(
        t.starts_with("first failing step: job \"test\", step \"Run cargo test\""),
        "{t}"
    );
    assert!(t.contains("\n  test parse::unicode ... FAILED\n"), "{t}");
    assert!(t.contains("panicked at src/parse.rs:42:9"), "{t}");
    assert!(
        t.contains("[error] Process completed with exit code 101."),
        "{t}"
    );
    // Only the first failing step: not the later failing step, not the passing job.
    assert!(!t.contains("No files were found"), "{t}");
    assert!(!t.contains("Syncing repository"), "{t}");
    assert!(!t.contains("2024-05-01T"), "timestamps stripped: {t}");
    assert!(
        t.ends_with("full log: files/gh-run-123.log (28 lines)"),
        "{t}"
    );
    // Short: the step, not the log.
    assert!(t.lines().count() <= 12, "{t}");

    // The full log is saved under the run's files/.
    let saved = env.run.files().join("gh-run-123.log");
    let fixture = std::fs::read_to_string(&log).unwrap();
    let got = std::fs::read_to_string(&saved).expect("saved log");
    assert_eq!(got.lines().count(), fixture.lines().count());
    assert_eq!(out.files, vec![saved]);

    let e = env.last_tool_event();
    assert_eq!(e.class, Some(Class::Read));
    assert_eq!(e.mode, Some(LedgerMode::Allow));
    assert!(e.ok);
    assert!(env.asker.cards().is_empty(), "a read never asks");
}

#[tokio::test]
async fn saved_log_and_returned_step_are_scrubbed() {
    let dir = Tmp::new("log");
    let fixture = std::fs::read_to_string(fixtures().join("gh-run-123.log")).unwrap();
    // Plant the canary in a failing line of the failing step (input, not a fixture file).
    let planted = fixture.replace(
        "error: test failed",
        &format!("error: token {} was rejected; test failed", canary()),
    );
    assert!(planted.contains(&canary()));
    let log = dir.0.join("planted.log");
    std::fs::write(&log, planted).unwrap();

    let env = env_with_log(&log, vec![]).await;
    let out = env
        .call("gh", json!({"args": ["run", "view", "123", "--log"]}))
        .await
        .expect("gh run view --log");
    assert_clean(&out.text, "returned text");
    assert!(out
        .text
        .contains("error: token @vault(gh_canary) was rejected"));
    let saved = std::fs::read_to_string(env.run.files().join("gh-run-123.log")).unwrap();
    assert_clean(&saved, "saved log file");
    assert!(saved.contains("@vault(gh_canary)"));
    assert_clean(&env.everything_written(), "ledger or run folder");
}

#[tokio::test]
async fn reads_run_without_asking() {
    let env = env_with_log(&fixtures().join("gh-run-123.log"), vec![]).await;
    let out = env
        .call("gh", json!({"args": "pr view 5"}))
        .await
        .expect("pr view");
    assert!(out.text.contains("fake gh: pr view 5"), "{}", out.text);
    assert_eq!(env.last_tool_event().class, Some(Class::Read));

    // `--web` opens a browser: Navigate, still allowed.
    let out = env
        .call("gh", json!({"args": "gh pr view 5 --web"}))
        .await
        .expect("pr view --web");
    assert!(
        out.text.contains("fake gh: pr view 5 --web"),
        "{}",
        out.text
    );
    assert_eq!(env.last_tool_event().class, Some(Class::Navigate));
    assert!(env.asker.cards().is_empty());
}

#[tokio::test]
async fn writes_reach_the_gate_as_account_or_delete() {
    let cases: &[(&str, Class)] = &[
        ("api -X POST repos/o/r/issues -f title=x", Class::Account),
        ("api -X GET repos/o/r -f a=b", Class::Account),
        ("api -X DELETE repos/o/r/issues/1", Class::Delete),
        ("pr merge 5", Class::Account),
        ("release create v1", Class::Account),
        ("repo delete o/r --yes", Class::Delete),
        ("frobnicate now", Class::Account),
    ];
    let env = env_with_log(
        &fixtures().join("gh-run-123.log"),
        vec![AskOutcome::AnsweredSkip; cases.len()],
    )
    .await;
    for (i, (cmd, want)) in cases.iter().enumerate() {
        let out = env
            .call("gh", json!({ "args": cmd }))
            .await
            .expect("skipped call");
        assert!(
            out.text.starts_with("skipped: you said no"),
            "{cmd}: {}",
            out.text
        );
        let cards = env.asker.cards();
        assert_eq!(cards.len(), i + 1, "{cmd}: one card per call");
        assert_eq!(last_card_class(&env.asker), Some(*want), "{cmd}");
        assert!(
            cards[i].what.starts_with("run gh "),
            "{cmd}: {}",
            cards[i].what
        );
        assert_eq!(cards[i].site.as_deref(), Some("github.com"));
    }
    // A skipped call never ran gh: no tool event, only the ask outcomes.
    assert!(env.events().iter().all(|e| e.server == "core"));
}

#[tokio::test]
async fn approved_write_runs_and_is_logged_as_account() {
    let env = env_with_log(
        &fixtures().join("gh-run-123.log"),
        vec![AskOutcome::AnsweredProceed],
    )
    .await;
    let out = env
        .call("gh", json!({"args": "pr merge 5 --squash"}))
        .await
        .expect("approved merge");
    assert!(
        out.text.contains("fake gh: pr merge 5 --squash"),
        "{}",
        out.text
    );
    let e = env.last_tool_event();
    assert_eq!(e.class, Some(Class::Account));
    assert_eq!(e.mode, Some(LedgerMode::AnsweredProceed));
}

#[tokio::test]
async fn auth_is_refused_and_nothing_runs() {
    let env = env_with_log(&fixtures().join("gh-run-123.log"), vec![]).await;
    for cmd in ["auth token", "auth status"] {
        let err = env
            .call("gh", json!({ "args": cmd }))
            .await
            .expect_err("gh auth must be refused");
        assert!(matches!(err, ToolError::Args(_)), "{cmd}: {err}");
    }
    assert!(env.asker.cards().is_empty());
    // Shell syntax is refused before anything runs.
    let err = env
        .call("gh", json!({"args": "run list | head"}))
        .await
        .expect_err("pipe refused");
    assert!(matches!(err, ToolError::Args(_)), "{err}");
    assert!(env.events().iter().all(|e| !e.ok && e.mode.is_none()));
}

#[tokio::test]
async fn failing_gh_is_an_error_with_its_stderr() {
    let dir = Tmp::new("missing");
    let env = env_with_log(&dir.0.join("no-such.log"), vec![]).await;
    let err = env
        .call("gh", json!({"args": "run view 999 --log"}))
        .await
        .expect_err("missing log");
    assert!(matches!(err, ToolError::Failed(_)), "{err}");
    assert!(err.to_string().contains("exit 1"), "{err}");
    assert!(!env.last_tool_event().ok);
}
