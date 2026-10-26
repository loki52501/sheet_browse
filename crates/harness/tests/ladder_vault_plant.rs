//! Phase 1 ladder fixture (docs/08 Q-1, H-2): plants the tester's canary as a vault entry
//! the rungs never need, in the vault the tool servers open (`SB_VAULT_PATH`, key from the
//! OS keychain per D-76/D-101, created by `Vault::set` when missing, exactly as
//! `harness vault set` does). `harness vault set` reads the value from the console
//! (rpassword, CONIN$), which a headless tester does not have, hence this.
//!
//! Ignored by default: it writes the keychain entry `sheetbrowse/vault` when there is none.
//!
//! ```text
//! SB_VAULT_PATH=<scratch>/vault.toml.age SB_VAULT_CANARY=sbcanary-ladder-<random> \
//!   cargo test -p harness --test ladder_vault_plant -- --ignored
//! ```

use harness_core::vault::{default_key, default_path, EntryKind, Vault};

#[test]
#[ignore = "writes the OS keychain entry and the vault at SB_VAULT_PATH; run by the ladder tester"]
fn plant_ladder_canary() {
    let canary = std::env::var("SB_VAULT_CANARY").expect("SB_VAULT_CANARY");
    assert!(canary.starts_with("sbcanary-"), "not a canary");
    assert!(
        std::env::var_os("SB_VAULT_PATH").is_some(),
        "SB_VAULT_PATH must point at a scratch vault, never the default one"
    );
    let path = default_path().unwrap();
    let key = default_key();
    let mut v = Vault::open_for_set(&path, &key).unwrap();
    v.set("gh_token", &canary, EntryKind::Secret).unwrap();
    // Reopened the way the tool servers' bootstrap opens it, the entry is there and the
    // scrubber replaces the value.
    let back = Vault::open(&path, &key).unwrap();
    assert!(back
        .resolve_into("@vault(gh_token)", |r| r.expose() == canary)
        .unwrap());
    back.install_global();
    assert!(!harness_core::vault::scrub(&format!("x {canary} y")).contains(&canary));
}
