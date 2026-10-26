//! T-05c acceptance row 4 (docs/01-HARNESS-CORE.md H-2, H-7; D-84, D-118, D-124 (1)).
//! Written by the tester.
//!
//! `harness vault set <name>` repairs, end to end, a vault file `Vault::open` refuses (an
//! entry under the D-118 floor and an entry whose value sits inside the withheld marker),
//! one entry per command, and never installs anything as the process-wide scrubber; every
//! other command fails on the broken file before it prints anything, and installs the vault
//! once the file is repaired (positive control for the "nothing installed" check).
//!
//! One test function, because the installed scrubber is process-wide state. The planted
//! value is `SB_VAULT_CANARY` (or a fresh one when unset). No assertion message formats a
//! value. The temp directory is removed when the test ends.

use std::path::Path;
use std::sync::Arc;

use clap::Parser;
use harness::cli::{execute, Cli, Ctx};
use harness::Env;
use harness_core::vault::{scrub, EntryKind, KeySource, Vault, VaultError};
use zeroize::Zeroizing;

/// Inside `@vault(?) [output withheld: vault scrubber unavailable]` (D-118 (3b)).
const IN_MARKER: &str = "ber unavailab";

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| c.chars().count() >= 8)
        .unwrap_or_else(|| {
            format!(
                "sbcanary-t05c-cli-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )
        })
}

fn has_ci(hay: &str, needle: &str) -> bool {
    hay.to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

/// Re-encrypts the vault at `path` with exactly `entries`, as an older build or a hand
/// edit could have written it, with the identity `Vault::set` stored.
fn write_vault_file(path: &Path, key_file: &Path, entries: &[(&str, &str, &str)]) {
    let text = std::fs::read_to_string(key_file).unwrap();
    let id: age::x25519::Identity = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("AGE-SECRET-KEY-"))
        .unwrap()
        .parse()
        .unwrap();
    let mut t = toml::Table::new();
    for (name, value, kind) in entries {
        let mut e = toml::Table::new();
        e.insert("value".into(), toml::Value::String((*value).into()));
        e.insert("kind".into(), toml::Value::String((*kind).into()));
        t.insert((*name).into(), toml::Value::Table(e));
    }
    let ct = age::encrypt(&id.to_public(), toml::to_string(&t).unwrap().as_bytes()).unwrap();
    std::fs::write(path, ct).unwrap();
}

fn ctx(dir: &Path, path: &Path, key: &KeySource, secret: &'static str) -> Ctx {
    Ctx {
        env: Env {
            database_url: "postgres://unused".to_owned(),
            runs_root: dir.join("runs"),
            tools_root: dir.join("tools"),
            vault_path: path.to_path_buf(),
            vault_key: key.clone(),
            run_id: None,
            claude: dir.join("no-claude-here"),
        },
        store: Some(Arc::new(harness_core::testing::MemStore::new())),
        read_secret: Box::new(move |_| Ok(Zeroizing::new(secret.to_owned()))),
        launcher: None,
    }
}

/// Runs one CLI command; returns (ok, stdout, error text). Fails the test if the canary or
/// the planted in-marker value shows up in either.
async fn run(ctx: &Ctx, args: &[&str], canary: &str) -> (bool, String, String) {
    let cli = Cli::try_parse_from(args).unwrap();
    let mut out = Vec::new();
    let result = execute(cli, ctx, &mut out).await;
    let out = String::from_utf8(out).unwrap();
    let err = match &result {
        Ok(()) => String::new(),
        Err(e) => format!("{e} | {e:?}"),
    };
    for text in [&out, &err] {
        assert!(!has_ci(text, canary), "canary in CLI output or error");
        assert!(
            !has_ci(text, IN_MARKER),
            "planted value in CLI output or error"
        );
    }
    (result.is_ok(), out, err)
}

#[tokio::test]
async fn r4_vault_set_repairs_a_broken_file_and_installs_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let c = canary();
    let path = dir.join("vault.toml.age");
    let key_file = dir.join("vault.key");
    let key = KeySource::IdentityFile(key_file.clone());
    let mut v = Vault::open(&path, &key).unwrap();
    v.set("test", &c, EntryKind::Secret).unwrap();
    drop(v);
    write_vault_file(
        &path,
        &key_file,
        &[
            ("test", &c, "secret"),
            ("st", "CA", "personal"),
            ("mk", IN_MARKER, "personal"),
        ],
    );
    match Vault::open(&path, &key) {
        Err(VaultError::ValueTooShort { name, .. }) => assert_eq!(name, "st"),
        Err(_) => panic!("fixture: open refused for another reason"),
        Ok(_) => panic!("fixture: open accepted the broken file"),
    }
    assert!(
        scrub(&c) == c,
        "a scrubber was installed before any command"
    );

    // D-124 (1): any other command fails on the broken file before printing anything.
    let (ok, out, err) = run(
        &ctx(dir, &path, &key, "unused"),
        &["harness", "schedule", "list"],
        &c,
    )
    .await;
    assert!(!ok && out.is_empty(), "schedule list ran on a broken vault");
    assert!(
        err.contains("harness vault set st"),
        "no repair hint: {err}"
    );
    assert!(scrub(&c) == c, "a failed command installed a scrubber");

    // Repair st (short value).
    let (ok, out, err) = run(
        &ctx(dir, &path, &key, "California"),
        &["harness", "vault", "set", "st", "--personal"],
        &c,
    )
    .await;
    assert!(ok, "`harness vault set st` failed: {err}");
    assert_eq!(out, "vault: st set (personal)\n");
    assert!(scrub(&c) == c, "`vault set` installed a scrubber");
    assert!(
        scrub("from California") == "from California",
        "`vault set` installed a scrubber"
    );
    match Vault::open(&path, &key) {
        Err(VaultError::ValueInLiteral { name }) => assert_eq!(name, "mk"),
        Err(_) => panic!("after st: open refused for another reason"),
        Ok(_) => panic!("after st: open accepted a file with mk still in the marker"),
    }

    // Repair mk (value inside the withheld marker).
    let (ok, out, err) = run(
        &ctx(dir, &path, &key, "Mk-fixed-value"),
        &["harness", "vault", "set", "mk"],
        &c,
    )
    .await;
    assert!(ok, "`harness vault set mk` failed: {err}");
    assert_eq!(out, "vault: mk set (secret)\n");
    assert!(scrub(&c) == c, "`vault set` installed a scrubber");

    let fixed = Vault::open(&path, &key).expect("open still refuses the repaired file");
    let mut names = fixed.names();
    names.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        names,
        [
            ("mk".to_owned(), EntryKind::Secret),
            ("st".to_owned(), EntryKind::Personal),
            ("test".to_owned(), EntryKind::Secret),
        ]
    );
    assert!(
        fixed
            .resolve_into("@vault(test)", |r| r.expose() == c)
            .unwrap(),
        "the repair lost the untouched entry"
    );
    drop(fixed);

    // Positive control: another command now opens and installs the vault (D-124 (1)), so
    // the "nothing installed" checks above can see an installed scrubber.
    let (ok, _out, err) = run(
        &ctx(dir, &path, &key, "unused"),
        &["harness", "schedule", "list"],
        &c,
    )
    .await;
    assert!(ok, "schedule list failed on the repaired vault: {err}");
    assert_eq!(scrub(&c), "@vault(test)");
    assert_eq!(scrub("from California"), "from @vault(st)");
}
