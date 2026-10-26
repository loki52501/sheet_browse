//! T-05 round 4 acceptance (docs/01-HARNESS-CORE.md H-2; I-4; D-118 as implemented in round
//! 4; CLAUDE.md hard rule 2). Written by the tester, independent of the coder's tests.
//!
//! Round 4 adds VaultError::ValueInLiteral (a value lying, in some ASCII case, inside
//! `@vault(` or inside an entry's `@vault(name)`), refused by `set` and by `open`; a
//! fixed-point scrub across literals it inserts itself; `Vault::open_for_set` and the
//! same-name replacement on an `open` handle, so a file `open` refuses can be repaired; and
//! a repair hint (`harness vault set <name>`) in both error messages.
//!
//! Every planted value is derived from `SB_VAULT_CANARY` (a non-secret fallback when it is
//! unset). The canary is expected to use only `[A-Za-z0-9_.-]`, so it can also be planted
//! inside an entry name. No assertion message formats a value.
//!
//! `SB_VAULT_CANARY=<unique> cargo test -p harness-core --test vault_accept_t05_r4`

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use harness_core::vault::{vault_refs, EntryKind, KeySource, Vault, VaultError};
use serde_json::json;

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "SBcanary-r4-fallback-not-a-secret".to_owned())
}

/// True when `needle` occurs in `hay` in any ASCII case.
fn has_ci(hay: &str, needle: &str) -> bool {
    hay.to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "sb-t05r4-{tag}-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            chrono::Utc::now().timestamp_micros()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir_entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn identity(key_file: &Path) -> age::x25519::Identity {
    let text = std::fs::read_to_string(key_file).unwrap();
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap()
        .parse()
        .unwrap()
}

/// Writes the vault file with the `age` and `toml` crates, as a hand edit or an older
/// build would leave it (no D-118 checks).
fn write_vault_file(path: &Path, key_file: &Path, entries: &[(&str, &str, &str)]) {
    let id = identity(key_file);
    let mut t = toml::Table::new();
    for (name, value, kind) in entries {
        let mut e = toml::Table::new();
        e.insert("value".into(), toml::Value::String((*value).into()));
        e.insert("kind".into(), toml::Value::String((*kind).into()));
        t.insert((*name).into(), toml::Value::Table(e));
    }
    let text = toml::to_string(&t).unwrap();
    let ct = age::encrypt(&id.to_public(), text.as_bytes()).unwrap();
    std::fs::write(path, ct).unwrap();
}

/// A file-backed vault in a fresh temp dir holding `test` = the canary.
fn canary_vault(tag: &str) -> (TempDir, PathBuf, PathBuf, KeySource, Vault) {
    let dir = TempDir::new(tag);
    let path = dir.0.join("vault.toml.age");
    let key_file = dir.0.join("vault.key");
    let key = KeySource::IdentityFile(key_file.clone());
    let mut v = Vault::open(&path, &key).unwrap();
    v.set("test", &canary(), EntryKind::Secret).unwrap();
    (dir, path, key_file, key, v)
}

/// Expects `ValueInLiteral { name: want }`; checks the message names `want`, carries the
/// repair hint for `want`, and holds no canary. Returns (Display, Debug).
fn expect_in_literal(r: Result<(), VaultError>, want: &str, label: &str) -> (String, String) {
    match r {
        Err(e @ VaultError::ValueInLiteral { .. }) => {
            let (display, debug) = (e.to_string(), format!("{e:?}"));
            match &e {
                VaultError::ValueInLiteral { name } => {
                    assert!(name == want, "{label}: wrong entry named")
                }
                _ => unreachable!(),
            }
            assert!(
                display.contains(&format!("\"{want}\"")),
                "{label}: entry not in Display"
            );
            assert!(
                display.contains(&format!("`harness vault set {want}`")),
                "{label}: no repair hint"
            );
            assert!(
                !has_ci(&display, &canary()) && !has_ci(&debug, &canary()),
                "{label}: canary in the error"
            );
            assert!(std::error::Error::source(&e).is_none());
            (display, debug)
        }
        Err(other) => panic!("{label}: expected ValueInLiteral, got {other}"),
        Ok(()) => panic!("{label}: accepted"),
    }
}

// ---------------------------------------------------------------------------------------
// R4-1. set refuses a value that lies inside a literal (ValueInLiteral), before the file,
// the lock or the key is touched, naming only the entry, in every case the rule covers.

#[test]
fn r4_1_set_refuses_value_in_literal() {
    let c = canary();
    // A fresh path: refused before any vault file, identity or lock file is made.
    let fresh = TempDir::new("lit-fresh");
    let fpath = fresh.0.join("vault.toml.age");
    let fkey = KeySource::IdentityFile(fresh.0.join("vault.key"));
    let mut f = Vault::open(&fpath, &fkey).unwrap();
    expect_in_literal(f.set("pw", "vault", EntryKind::Secret), "pw", "fresh");
    expect_in_literal(
        f.set("zip_94110", "94110", EntryKind::Personal),
        "zip_94110",
        "fresh own literal",
    );
    assert!(
        dir_entries(&fresh.0).is_empty(),
        "a refused set created a vault file, an identity or a lock file"
    );
    assert!(f.names().is_empty(), "a refused set changed the vault");

    let (_dir, path, _kf, key, mut v) = canary_vault("lit-set");
    let before = std::fs::read(&path).unwrap();
    let name_c = format!("n_{c}");
    let cases: Vec<(&str, String, &str, &str)> = vec![
        ("pw", "Vault".into(), "pw", "inside @vault(, mixed case"),
        ("pw", "AULT(".into(), "pw", "inside @vault(, upper case"),
        ("pw", "@VAU".into(), "pw", "start of the opener"),
        ("pw", "(test)".into(), "pw", "inside the literal of test"),
        (
            "pw",
            "t(TEST".into(),
            "pw",
            "across the opener and the name test",
        ),
        ("pw", "test".into(), "pw", "the name of an entry"),
        (
            "zip_94110",
            "94110".into(),
            "zip_94110",
            "inside its own literal",
        ),
        (
            "kq7z",
            "\u{212a}q7z".into(),
            "kq7z",
            "KELVIN SIGN lower-cases to its own name",
        ),
        (
            &name_c,
            "unrelated-value".into(),
            "test",
            "a new name whose literal holds the canary",
        ),
    ];
    let mut pw_texts: Vec<(String, String)> = Vec::new();
    for (name, value, want, label) in &cases {
        let shown = expect_in_literal(v.set(name, value, EntryKind::Personal), want, label);
        if *name == "pw" {
            pw_texts.push(shown);
        }
    }
    // The message depends on the entry name alone, never on the value.
    assert!(
        pw_texts.windows(2).all(|w| w[0] == w[1]),
        "the ValueInLiteral text changes with the value"
    );
    assert!(
        std::fs::read(&path).unwrap() == before,
        "a refused set rewrote the file"
    );
    assert_eq!(v.names().len(), 1, "a refused set changed the vault");

    // The same rule on an in-memory vault.
    let mut m = Vault::in_memory([("test".to_owned(), c.clone(), EntryKind::Secret)]);
    expect_in_literal(m.set("pw", "vAuLt", EntryKind::Secret), "pw", "in_memory");
    expect_in_literal(
        m.set(&name_c, "unrelated-value", EntryKind::Secret),
        "test",
        "in_memory name holding the canary",
    );

    // Values that only touch or overlap a literal are accepted (they cannot hide in one).
    for (name, value) in [
        ("k", ")xyz1"),
        ("m", "vault(Z"),
        ("o", "@@vA"),
        ("e", "t)Qz"),
    ] {
        v.set(name, value, EntryKind::Personal).unwrap();
    }
    let re = Vault::open(&path, &key).unwrap();
    assert_eq!(re.names().len(), 5);
    let s = format!("a {c}xyz1 b");
    assert!(re.scrub(&s) == "a @vault(k) b", "touching value scrubbed");
}

// ---------------------------------------------------------------------------------------
// R4-2. open refuses a file holding a value inside a literal (written by hand with age and
// toml), naming only the entry; an `open` handle from before the edit refuses to set.

#[test]
fn r4_2_open_refuses_a_file_holding_a_value_in_literal() {
    let c = canary();
    let (_dir, path, kf, key, mut early) = canary_vault("lit-open");
    let name_c = format!("n_{c}");
    let cases: Vec<(&str, &str, &str, &str)> = vec![
        ("v", "ault", "v", "inside @vault("),
        ("t2", "(TEST)", "t2", "inside the literal of test"),
        (
            &name_c,
            "unrelated-value",
            "test",
            "a name holding the canary",
        ),
        ("zip_94110", "94110", "zip_94110", "inside its own literal"),
        ("k1zz", "\u{212a}1zz", "k1zz", "KELVIN SIGN"),
    ];
    for (name, value, want, label) in &cases {
        write_vault_file(
            &path,
            &kf,
            &[("test", &c, "secret"), (name, value, "personal")],
        );
        expect_in_literal(Vault::open(&path, &key).map(|_| ()), want, label);
        // A handle opened before the edit re-reads the file under its lock: refused, the
        // file is left byte for byte.
        let before = std::fs::read(&path).unwrap();
        expect_in_literal(
            early.set("other", &format!("{c}-other"), EntryKind::Secret),
            want,
            label,
        );
        assert!(std::fs::read(&path).unwrap() == before, "file rewritten");
        assert_eq!(early.names().len(), 1, "a refused set changed the vault");
    }
    // A short value is reported before a value inside a literal.
    write_vault_file(
        &path,
        &kf,
        &[
            ("test", &c, "secret"),
            ("st", "Q\u{2603}", "personal"),
            ("v", "ault", "personal"),
        ],
    );
    assert!(matches!(
        Vault::open(&path, &key),
        Err(VaultError::ValueTooShort { ref name, .. }) if name == "st"
    ));
    // The writer round-trips: the same file with good values opens.
    write_vault_file(
        &path,
        &kf,
        &[("test", &c, "secret"), ("v", "Zq7-v-value", "personal")],
    );
    let ok = Vault::open(&path, &key).unwrap();
    assert_eq!(ok.names().len(), 2);
    assert!(ok.scrub(&format!("[{c}]")) == "[@vault(test)]");
}

// ---------------------------------------------------------------------------------------
// R4-3. A short entry already in a file can be repaired through set: on an `open` handle
// by setting the same name (when it is the only broken entry), and on an `open_for_set`
// handle one entry at a time. The new value is still checked; the old value is gone.

#[test]
fn r4_3_short_entry_repair_through_set() {
    let c = canary();
    let short = "Q\u{2603}";

    // (a) An `open` handle replaces the one broken entry under its own name.
    let (_dir, path, kf, key, mut early) = canary_vault("repair-a");
    write_vault_file(
        &path,
        &kf,
        &[("test", &c, "secret"), ("st", short, "personal")],
    );
    match Vault::open(&path, &key) {
        Err(e @ VaultError::ValueTooShort { .. }) => {
            let shown = format!("{e} | {e:?}");
            assert!(e.to_string().contains("`harness vault set st`"), "no hint");
            assert!(!shown.contains(short) && !shown.contains('\u{2603}'));
            assert!(!has_ci(&shown, &c), "canary in the error");
        }
        Err(other) => panic!("expected ValueTooShort, got {other}"),
        Ok(_) => panic!("open accepted a short value"),
    }
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        early.set("other", &format!("{c}-other"), EntryKind::Secret),
        Err(VaultError::ValueTooShort { ref name, .. }) if name == "st"
    ));
    assert!(matches!(
        early.set("st", "Zq7", EntryKind::Personal),
        Err(VaultError::ValueTooShort { .. })
    ));
    assert!(std::fs::read(&path).unwrap() == before, "file rewritten");
    early.set("st", "California", EntryKind::Personal).unwrap();
    let ok = Vault::open(&path, &key).unwrap();
    let names: Vec<String> = ok.names().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["st", "test"]);
    assert!(ok.scrub("from CALIFORNIA") == "from @vault(st)");
    assert!(
        ok.scrub(short) == short,
        "the old short value is still scrubbed"
    );
    assert!(ok
        .resolve_into("@vault(test)", |r| r.expose() == c)
        .unwrap());
    let ct = std::fs::read(&path).unwrap();
    assert!(!contains_bytes(&ct, c.as_bytes()) && !contains_bytes(&ct, b"California"));

    // (b) `open_for_set` repairs a file with a short entry and an entry inside a literal.
    let (_dir2, path, kf, key, _v) = canary_vault("repair-b");
    write_vault_file(
        &path,
        &kf,
        &[
            ("test", &c, "secret"),
            ("st", short, "personal"),
            ("v", "ault", "personal"),
        ],
    );
    assert!(matches!(
        Vault::open(&path, &key),
        Err(VaultError::ValueTooShort { ref name, .. }) if name == "st"
    ));
    let mut fix = Vault::open_for_set(&path, &key).unwrap();
    // Another entry can be added while both are still broken; open keeps refusing.
    fix.set("other", &format!("{c}-other"), EntryKind::Secret)
        .unwrap();
    assert!(matches!(
        Vault::open(&path, &key),
        Err(VaultError::ValueTooShort { ref name, .. }) if name == "st"
    ));
    // The new value is still checked on the repair handle.
    assert!(matches!(
        fix.set("st", "Zq7", EntryKind::Personal),
        Err(VaultError::ValueTooShort { .. })
    ));
    expect_in_literal(
        fix.set("st", "Vault", EntryKind::Personal),
        "st",
        "repair st",
    );
    fix.set("st", "California", EntryKind::Personal).unwrap();
    expect_in_literal(Vault::open(&path, &key).map(|_| ()), "v", "next to fix");
    assert!(matches!(
        fix.set("v", "ab", EntryKind::Personal),
        Err(VaultError::ValueTooShort { .. })
    ));
    expect_in_literal(fix.set("v", "(TEST)", EntryKind::Personal), "v", "repair v");
    fix.set("v", "Zq7-v-value", EntryKind::Personal).unwrap();
    let ok = Vault::open(&path, &key).unwrap();
    let names: Vec<String> = ok.names().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["other", "st", "test", "v"]);
    let s = format!("{c} {c}-other california zq7-V-VALUE ault");
    assert!(
        ok.scrub(&s) == "@vault(test) @vault(other) @vault(st) @vault(v) ault",
        "repaired vault scrubs every value and no old one"
    );
}

// ---------------------------------------------------------------------------------------
// R4-4. The r3_6 fixed point with the real canary, file-backed and reopened: a literal scrub
// inserts cannot complete another value, in raw, upper-case, JSON and Debug text; a
// three-pass chain ends in one literal; a chain longer than the pass cap fails closed.

#[test]
fn r4_4_scrub_fixed_point_across_inserted_literals_real_canary() {
    let c = canary();
    let tail = "Zq7-tail-value";
    for (label, p, s) in [
        ("value ending in @", format!("{c}@"), format!("{c}{tail}")),
        (
            "value starting with )",
            format!("){c}"),
            format!("{tail}{c}"),
        ),
    ] {
        let dir = TempDir::new("fix");
        let path = dir.0.join("vault.toml.age");
        let key = KeySource::IdentityFile(dir.0.join("vault.key"));
        let mut w = Vault::open(&path, &key).unwrap();
        w.set("p", &p, EntryKind::Secret).unwrap();
        w.set("t", tail, EntryKind::Personal).unwrap();
        let v = Vault::open(&path, &key).unwrap();
        assert!(v.scrub(&s) == "@vault(p)", "{label}: not one literal");
        for (form, text) in [
            ("raw", s.clone()),
            ("upper", s.to_ascii_uppercase()),
            ("in text", format!("a {s} b")),
            ("json", json!({ "m": s }).to_string()),
            ("debug", format!("{s:?}")),
        ] {
            let once = v.scrub(&text);
            assert!(!has_ci(&once, &c), "{label} {form}: canary survived");
            assert!(!has_ci(&once, &p), "{label} {form}: a whole value survived");
            assert!(!has_ci(&once, tail), "{label} {form}: tail survived");
            assert!(once.contains("@vault(p)"), "{label} {form}: no literal");
            assert!(v.scrub(&once) == once, "{label} {form}: not a fixed point");
            assert!(
                vault_refs(&once).is_ok_and(|r| r.iter().all(|n| *n == "p" || *n == "t")),
                "{label} {form}: literals do not parse"
            );
        }
    }

    // Review round 3 (`)xyz1` after the literal of test) and a third pass (`)Qw9` after the
    // literal of k), file-backed.
    let (_dir, path, _kf, key, mut w) = canary_vault("chain");
    w.set("k", ")xyz1", EntryKind::Secret).unwrap();
    w.set("j", ")Qw9", EntryKind::Secret).unwrap();
    let v = Vault::open(&path, &key).unwrap();
    for (text, want) in [
        (format!("{c}xyz1"), "@vault(k)"),
        ("@vault(test)xyz1".to_owned(), "@vault(k)"),
        (format!("<{c}xyz1Qw9>"), "<@vault(j)>"),
        (
            format!("<{}XYZ1QW9>", c.to_ascii_uppercase()),
            "<@vault(j)>",
        ),
    ] {
        let once = v.scrub(&text);
        assert!(once == want, "chain: wrong output for {want}");
        assert!(v.scrub(&once) == once, "chain: not a fixed point");
    }

    // A chain of links `)Zz01`, `)Zz02`, ... each completed by the literal of the one
    // before. Six links (7 changing passes) end in one literal; ten links pass the cap of
    // 8 passes, and the output is withheld rather than left half scrubbed.
    for (links, withheld) in [(6usize, false), (10, true)] {
        let mut entries = vec![("test".to_owned(), c.clone(), EntryKind::Secret)];
        let mut text = c.clone();
        for i in 1..=links {
            entries.push((format!("l{i}"), format!(")Zz{i:02}"), EntryKind::Secret));
            text.push_str(&format!("Zz{i:02}"));
        }
        let v = Vault::in_memory(entries.clone());
        let once = v.scrub(&text);
        assert!(!has_ci(&once, &c), "chain {links}: canary survived");
        for (_, val, _) in &entries {
            assert!(!has_ci(&once, val), "chain {links}: a whole value survived");
        }
        if withheld {
            assert!(
                once.contains("output withheld") && vault_refs(&once).is_err(),
                "chain {links}: past the cap the output must be withheld"
            );
        } else {
            assert!(
                once == format!("@vault(l{links})"),
                "chain {links}: wrong output"
            );
            assert!(v.scrub(&once) == once, "chain {links}: not a fixed point");
        }
    }
}

// ---------------------------------------------------------------------------------------
// Helper for crates/harness/tests/vault_cli_repair_t05_r4.rs (not a check): when
// SB_T05R4_EXPORT names a directory, writes there a vault file holding `test` = the canary
// and a short `st`, plus its identity file. The harness crate has no `age` dependency to
// write such a file itself.

#[test]
fn r4_5_export_broken_vault_for_cli() {
    let Some(dir) = std::env::var_os("SB_T05R4_EXPORT") else {
        return;
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("vault.toml.age");
    let key_file = dir.join("vault.key");
    let key = KeySource::IdentityFile(key_file.clone());
    let mut v = Vault::open(&path, &key).unwrap();
    v.set("test", &canary(), EntryKind::Secret).unwrap();
    write_vault_file(
        &path,
        &key_file,
        &[
            ("test", &canary(), "secret"),
            ("st", "Q\u{2603}", "personal"),
        ],
    );
    assert!(matches!(
        Vault::open(&path, &key),
        Err(VaultError::ValueTooShort { ref name, .. }) if name == "st"
    ));
}
