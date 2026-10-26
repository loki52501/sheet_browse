//! T-15 acceptance (docs/04 C-1): the read set the spec names (`run list`, `run view`,
//! `pr view`, `api` GET) runs as Read through `dispatch` without a card, and every write the
//! spec names (`api -X POST|PUT|DELETE`, `pr merge`, `release ...`) reaches the gate as
//! Account or higher, with the card seen by ScriptedAsker. Complements gh_t15.rs, which does
//! not cover `run list`, `api` without `-X`, `api -X PUT`, `--method=DELETE` or a read-looking
//! `release list`.

mod common;

use common::{fixtures, last_card_class, Env};
use harness_core::class::Class;
use harness_core::gate::AskOutcome;
use harness_core::memory::LedgerMode;
use serde_json::json;
use srv_cli::Config;

fn fake_gh() -> Vec<String> {
    let log = fixtures().join("gh-run-123.log").display().to_string();
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

async fn env(answers: Vec<AskOutcome>) -> Env {
    let cfg = Config {
        gh: fake_gh(),
        ..Config::default()
    };
    Env::new(srv_cli::tools_with(cfg), "gh_canary", answers).await
}

#[tokio::test]
async fn c1_spec_reads_are_read_and_run_without_a_card() {
    let env = env(vec![]).await;
    for cmd in [
        "run list",
        "run list --limit 5",
        "run view 123",
        "pr view 5",
        "api repos/o/r",
        "api -X GET repos/o/r",
        "api --method=HEAD repos/o/r",
    ] {
        let out = env
            .call("gh", json!({ "args": cmd }))
            .await
            .unwrap_or_else(|e| panic!("{cmd}: {e}"));
        assert!(out.text.contains("fake gh: "), "{cmd}: {}", out.text);
        let e = env.last_tool_event();
        assert_eq!(e.class, Some(Class::Read), "{cmd}");
        assert_eq!(e.mode, Some(LedgerMode::Allow), "{cmd}");
        assert!(e.ok, "{cmd}");
    }
    assert!(env.asker.cards().is_empty(), "a read never asks");
}

#[tokio::test]
async fn c1_spec_writes_reach_the_gate_as_account_or_higher() {
    let cases: &[(&str, Class)] = &[
        ("api -X POST repos/o/r/issues", Class::Account),
        ("api -X PUT repos/o/r/topics", Class::Account),
        ("api --method=PUT repos/o/r/topics", Class::Account),
        ("api -X DELETE repos/o/r/issues/1", Class::Delete),
        ("api --method=DELETE repos/o/r/issues/1", Class::Delete),
        ("pr merge 5", Class::Account),
        ("release create v1", Class::Account),
        ("release list", Class::Account),
        ("release upload v1 a.zip", Class::Account),
        ("no-such-subcommand x", Class::Account),
    ];
    let env = env(vec![AskOutcome::AnsweredSkip; cases.len()]).await;
    for (i, (cmd, want)) in cases.iter().enumerate() {
        let out = env
            .call("gh", json!({ "args": cmd }))
            .await
            .unwrap_or_else(|e| panic!("{cmd}: {e}"));
        assert!(out.text.starts_with("skipped:"), "{cmd}: {}", out.text);
        assert_eq!(env.asker.cards().len(), i + 1, "{cmd}: one card per call");
        let got = last_card_class(&env.asker).expect("card");
        assert_eq!(got, *want, "{cmd}");
        assert!(got >= Class::Account, "{cmd}: below Account");
    }
    // Nothing ran: only the core ask outcomes are in the ledger.
    assert!(env.events().iter().all(|e| e.server == "core"));
}
