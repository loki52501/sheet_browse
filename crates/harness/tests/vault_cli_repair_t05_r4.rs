//! T-05 round 4 acceptance (docs/01-HARNESS-CORE.md H-2, H-7; D-118). The ValueTooShort and
//! ValueInLiteral messages tell the user to give the entry another value with
//! `harness vault set <name>`. This checks that the command they name repairs a vault file
//! that `Vault::open` refuses. Written by the tester.
//!
//! The broken vault file (with `test` = the canary and a 2-character `st`) is written by
//! the test itself with `age` (a dev-dependency), as the core test
//! `r4_5_export_broken_vault_for_cli` writes it: `Vault::set` for `test`, then the file
//! re-encrypted with `st` added. The canary is `SB_VAULT_CANARY`, or a fresh one when unset.
//!
//! The test deletes the directory when it ends. No assertion message formats a value.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use harness::cli::{execute, Cli, Ctx};
use harness::Env;
use harness_core::vault::{EntryKind, KeySource, Vault, VaultError};
use zeroize::Zeroizing;

struct RemoveOnDrop(PathBuf);
impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| c.chars().count() >= 8)
        .unwrap_or_else(|| {
            format!(
                "sbcanary-t05r4-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )
        })
}

/// Re-encrypts the vault at `path` with exactly `entries` (name, value, kind), as an older
/// build or a hand edit could have written it, with the identity `Vault::set` stored.
fn write_vault_file(path: &Path, key_file: &Path, entries: &[(&str, &str, &str)]) {
    let text = std::fs::read_to_string(key_file).unwrap();
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap();
    let id: age::x25519::Identity = line.parse().unwrap();
    let mut t = toml::Table::new();
    for (name, value, kind) in entries {
        let mut e = toml::Table::new();
        e.insert("value".into(), toml::Value::String((*value).into()));
        e.insert("kind".into(), toml::Value::String((*kind).into()));
        t.insert((*name).into(), toml::Value::Table(e));
    }
    let toml_text = toml::to_string(&t).unwrap();
    let ct = age::encrypt(&id.to_public(), toml_text.as_bytes()).unwrap();
    std::fs::write(path, ct).unwrap();
}

#[tokio::test]
async fn vault_set_cli_repairs_a_short_entry_open_refuses() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let _cleanup = RemoveOnDrop(dir.clone());
    let canary = canary();
    let path = dir.join("vault.toml.age");
    let key_file = dir.join("vault.key");
    let key = KeySource::IdentityFile(key_file.clone());
    let mut v = Vault::open(&path, &key).unwrap();
    v.set("test", &canary, EntryKind::Secret).unwrap();
    drop(v);
    write_vault_file(
        &path,
        &key_file,
        &[("test", &canary, "secret"), ("st", "Q\u{2603}", "personal")],
    );
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
