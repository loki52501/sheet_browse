//! T-15 C-4 `cli(cmd)` behind `cli-allow.toml`, through the H-6 wrapper. The allowlisted
//! program is the `cargo` that runs these tests (present on every machine that can run
//! them); its write and delete entries are subcommands cargo does not have, so even an
//! approved call cannot change anything.

mod common;

use common::{last_card_class, Env, Tmp};
use harness_core::class::Class;
use harness_core::gate::AskOutcome;
use harness_core::mcp::ToolError;
use harness_core::memory::LedgerMode;
use serde_json::json;
use srv_cli::Config;

fn allow_file(dir: &Tmp) -> std::path::PathBuf {
    let path = dir.0.join("cli-allow.toml");
    let toml = format!(
        "[programs.cargo]\nbin = '{}'\nread = [\"--version\"]\nwrite = [\"sb-no-such-write\"]\n\
         delete = [\"sb-no-such-delete\"]\ndeny_flags = [\"--config\", \"-Z\"]\n",
        env!("CARGO")
    );
    std::fs::write(&path, toml).unwrap();
    path
}

async fn env(dir: &Tmp, answers: Vec<AskOutcome>) -> Env {
    let cfg = Config {
        cli_allow_file: allow_file(dir),
        ..Config::default()
    };
    Env::new(srv_cli::tools_with(cfg), "cli_canary", answers).await
}

#[tokio::test]
async fn allowlisted_read_runs() {
    let dir = Tmp::new("allow");
    let env = env(&dir, vec![]).await;
    let out = env
        .call("cli", json!({"cmd": "cargo --version"}))
        .await
        .expect("cargo --version");
    assert!(out.text.contains(": exit 0\ncargo "), "{}", out.text);
    let e = env.last_tool_event();
    assert_eq!(e.class, Some(Class::Read));
    assert_eq!(e.mode, Some(LedgerMode::Allow));
    assert!(env.asker.cards().is_empty());
}

#[tokio::test]
async fn not_allowlisted_is_refused_and_nothing_runs() {
    let dir = Tmp::new("refuse");
    let env = env(&dir, vec![]).await;
    for cmd in [
        "rm -rf x",
        "cargo build",
        "cargo",
        "cargo --version --config=x",
        "cargo --version -Zunstable",
        "cargo --version | more",
        "cargo --version && cargo build",
        "./cargo --version",
    ] {
        let err = env.call("cli", json!({ "cmd": cmd })).await.expect_err(cmd);
        assert!(matches!(err, ToolError::Args(_)), "{cmd}: {err}");
    }
    assert!(env.asker.cards().is_empty(), "a refusal never asks");
    // Each refusal is one failed classify event; no tool ran.
    let events = env.events();
    assert_eq!(events.len(), 8);
    assert!(events.iter().all(|e| !e.ok && e.mode.is_none()));
}

#[tokio::test]
async fn missing_allowlist_allows_nothing() {
    let dir = Tmp::new("none");
    let cfg = Config {
        cli_allow_file: dir.0.join("absent.toml"),
        ..Config::default()
    };
    let env = Env::new(srv_cli::tools_with(cfg), "cli_canary", vec![]).await;
    let err = env
        .call("cli", json!({"cmd": "cargo --version"}))
        .await
        .expect_err("no allowlist");
    assert!(matches!(err, ToolError::Args(_)), "{err}");
    assert!(err.to_string().contains("allowed: none"), "{err}");
}

#[tokio::test]
async fn write_and_delete_entries_ask() {
    let dir = Tmp::new("ask");
    let env = env(
        &dir,
        vec![AskOutcome::AnsweredSkip, AskOutcome::AnsweredSkip],
    )
    .await;
    let out = env
        .call("cli", json!({"cmd": "cargo sb-no-such-write"}))
        .await
        .unwrap();
    assert!(out.text.starts_with("skipped:"), "{}", out.text);
    assert_eq!(last_card_class(&env.asker), Some(Class::Account));
    let out = env
        .call("cli", json!({"cmd": ["cargo", "sb-no-such-delete"]}))
        .await
        .unwrap();
    assert!(out.text.starts_with("skipped:"), "{}", out.text);
    assert_eq!(last_card_class(&env.asker), Some(Class::Delete));
    assert!(env.events().iter().all(|e| e.server == "core"));
}

#[tokio::test]
async fn approved_write_runs_and_a_nonzero_exit_is_an_error() {
    let dir = Tmp::new("run");
    let env = env(&dir, vec![AskOutcome::AnsweredProceed]).await;
    let err = env
        .call("cli", json!({"cmd": "cargo sb-no-such-write"}))
        .await
        .expect_err("cargo has no such subcommand");
    assert!(matches!(err, ToolError::Failed(_)), "{err}");
    assert!(err.to_string().contains(": exit "), "{err}");
    let e = env.last_tool_event();
    assert_eq!(e.class, Some(Class::Account));
    assert_eq!(e.mode, Some(LedgerMode::AnsweredProceed));
    assert!(!e.ok);
}
