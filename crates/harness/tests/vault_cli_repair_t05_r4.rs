//! T-05 round 4 acceptance (docs/01-HARNESS-CORE.md H-2, H-7; D-118). The ValueTooShort and
//! ValueInLiteral messages tell the user to give the entry another value with
//! `harness vault set <name>`. This checks that the command they name repairs a vault file
//! that `Vault::open` refuses. Written by the tester.
//!
//! This crate has no `age` dependency, so the broken vault file (with `test` = the canary
//! and a 2-character `st`) is written by the core test `r4_5_export_broken_vault_for_cli`:
//!
//! ```text
//! SB_VAULT_CANARY=<unique> SB_T05R4_EXPORT=<new dir> \
//!   cargo test -p harness-core --test vault_accept_t05_r4 r4_5_export_broken_vault_for_cli
//! SB_VAULT_CANARY=<same> SB_T05R4_EXPORT=<same dir> \
//!   cargo test -p harness --test vault_cli_repair_t05_r4 -- --include-ignored
//! ```
//!
//! The test deletes the directory when it ends. No assertion message formats a value.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use harness::cli::{execute, Cli, Ctx};
use harness::Env;
use harness_core::vault::{KeySource, Vault, VaultError};
use zeroize::Zeroizing;

struct RemoveOnDrop(PathBuf);
impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
#[ignore = "red-first T-05 round 5: harness vault set opens with Vault::open, which refuses the file the hint tells it to repair; call Vault::open_for_set in cli.rs vault_set (also needs SB_T05R4_EXPORT, see the header)"]
async fn vault_set_cli_repairs_a_short_entry_open_refuses() {
    let dir = PathBuf::from(
        std::env::var_os("SB_T05R4_EXPORT").expect("SB_T05R4_EXPORT is not set; see the header"),
    );
    let _cleanup = RemoveOnDrop(dir.clone());
    let canary = std::env::var("SB_VAULT_CANARY").expect("SB_VAULT_CANARY is not set");
    let path = dir.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.join("vault.key"));
    // The fixture is what the hint is about: open refuses it and names st.
    match Vault::open(&path, &key) {
        Err(VaultError::ValueTooShort { name, .. }) => assert_eq!(name, "st"),
        Err(other) => panic!("fixture: expected ValueTooShort, got {other}"),
        Ok(_) => panic!("fixture: open accepted the file"),
    }
    let env = Env {
        database_url: "postgres://unused".to_owned(),
        runs_root: dir.join("runs"),
        tools_root: dir.join("tools"),
        vault_path: path.clone(),
        vault_key: key.clone(),
        run_id: None,
        claude: dir.join("no-claude-here"),
    };
    let ctx = Ctx {
        env,
        store: Some(Arc::new(harness_core::testing::MemStore::new())),
        read_secret: Box::new(|_| Ok(Zeroizing::new("California".to_owned()))),
        launcher: None,
    };
    let cli = Cli::try_parse_from(["harness", "vault", "set", "st", "--personal"]).unwrap();
    let mut out = Vec::new();
    let result = execute(cli, &ctx, &mut out).await;
    let out = String::from_utf8(out).unwrap();
    let err_text = match &result {
        Ok(()) => String::new(),
        Err(e) => format!("{e} | {e:?}"),
    };
    assert!(
        !out.contains(&canary) && !err_text.contains(&canary),
        "canary in the CLI output or error"
    );
    // The error text names only the entry (checked canary-free above), so it is printed.
    assert!(
        result.is_ok(),
        "`harness vault set st` did not repair the file: {err_text}"
    );
    assert_eq!(out, "vault: st set (personal)\n");
    let ok = Vault::open(&path, &key).expect("open still refuses the repaired file");
    assert!(ok.scrub("from california") == "from @vault(st)");
    assert!(ok
        .resolve_into("@vault(test)", |r| r.expose() == canary)
        .unwrap());
}
