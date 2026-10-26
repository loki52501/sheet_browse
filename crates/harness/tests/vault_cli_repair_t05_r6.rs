//! T-05 round 6 acceptance through the CLI (docs/01-HARNESS-CORE.md H-2, H-7; D-118 (3b)
//! as amended in round 6). A vault file holding a value inside a withheld marker is refused
//! by `Vault::open` with ValueInLiteral, whose message names `harness vault set <name>`;
//! that command repairs it, and refuses to store a marker word itself. Written by the
//! tester; the broken file is written with `age` and `toml` (dev-dependencies).
//!
//! The canary is `SB_VAULT_CANARY`, or a fresh one when unset. No assertion message
//! formats a value.

use std::path::Path;
use std::sync::Arc;

use clap::Parser;
use harness::cli::{execute, Cli, Ctx};
use harness::Env;
use harness_core::vault::{EntryKind, KeySource, Vault, VaultError};
use zeroize::Zeroizing;

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| c.chars().count() >= 8)
        .unwrap_or_else(|| {
            format!(
                "sbcanary-t05r6-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )
        })
}

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
    let ct = age::encrypt(&id.to_public(), toml::to_string(&t).unwrap().as_bytes()).unwrap();
    std::fs::write(path, ct).unwrap();
}

fn ctx(dir: &Path, path: &Path, key: &KeySource, value: &'static str) -> Ctx {
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
        read_secret: Box::new(move |_| Ok(Zeroizing::new(value.to_owned()))),
        launcher: None,
    }
}

async fn vault_set(ctx: &Ctx, name: &str) -> (Result<(), String>, String) {
    let cli = Cli::try_parse_from(["harness", "vault", "set", name, "--personal"]).unwrap();
    let mut out = Vec::new();
    let r = execute(cli, ctx, &mut out).await;
    (
        r.map_err(|e| format!("{e} | {e:?}")),
        String::from_utf8(out).unwrap(),
    )
}

#[tokio::test]
async fn vault_set_cli_repairs_a_marker_word_entry_and_refuses_new_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
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
        &[("test", &c, "secret"), ("m", "Not Settle", "personal")],
    );
    match Vault::open(&path, &key) {
        Err(e @ VaultError::ValueInLiteral { .. }) => {
            assert!(e.to_string().contains("`harness vault set m`"), "no hint");
            assert!(!format!("{e} {e:?}").contains(&c));
        }
        Err(other) => panic!("fixture: expected ValueInLiteral, got {other}"),
        Ok(_) => panic!("fixture: open accepted the file"),
    }

    // A new entry whose value is a marker word is refused through the CLI too.
    let before = std::fs::read(&path).unwrap();
    let (r, out) = vault_set(&ctx(&dir, &path, &key, "output withheld"), "m2").await;
    let err = r.expect_err("a marker word was stored");
    assert!(
        err.contains("ValueInLiteral") && err.contains("\"m2\""),
        "{err}"
    );
    assert!(
        !err.contains(&c) && !out.contains(&c),
        "canary in CLI output"
    );
    assert!(std::fs::read(&path).unwrap() == before, "file rewritten");

    // The command the hint names repairs the broken entry.
    let (r, out) = vault_set(&ctx(&dir, &path, &key, "Fresno-94"), "m").await;
    assert!(!out.contains(&c), "canary in CLI output");
    let r_text = r.as_ref().err().cloned().unwrap_or_default();
    assert!(!r_text.contains(&c), "canary in CLI error");
    assert!(r.is_ok(), "`harness vault set m` did not repair: {r_text}");
    assert_eq!(out, "vault: m set (personal)\n");
    let ok = Vault::open(&path, &key).expect("open still refuses the repaired file");
    assert!(ok.scrub("from FRESNO-94, not settle") == "from @vault(m), not settle");
    assert!(ok
        .resolve_into("@vault(test)", |r| r.expose() == c)
        .unwrap());
}
