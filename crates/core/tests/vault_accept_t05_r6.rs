//! T-05 rounds 5 and 6 acceptance (docs/01-HARNESS-CORE.md H-2; I-4; D-118 as amended in
//! rounds 5 and 6; CLAUDE.md hard rule 2). Written by the tester, independent of the
//! coder's vault.rs unit tests.
//!
//! Round 5 (5cec6ef): a scrub that hits the 8-pass cap withholds the whole output as
//! `@vault(?) [output withheld: scrub did not settle]` (not the failed-build marker); a
//! well-formed reference to an unknown name holding a value scrubs to
//! `@vault(xx@vault(n)yy)`; set on an `open` handle checks every entry of the re-read
//! file, set on an `open_for_set` or `in_memory` handle only the entry being set.
//! Round 6 (3f6fea5): D-118 (3b) also refuses a value any of whose spellings lies, in some
//! ASCII case, inside either withheld marker, on set and on open (ValueInLiteral).
//!
//! Every planted value is derived from `SB_VAULT_CANARY` (a non-secret fallback when it is
//! unset). No assertion message formats a value.
//!
//! `SB_VAULT_CANARY=<unique> cargo test -p harness-core --test vault_accept_t05_r6`

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use harness_core::vault::{vault_refs, EntryKind, KeySource, Vault, VaultError};

const UNSETTLED: &str = "@vault(?) [output withheld: scrub did not settle]";
const UNAVAILABLE: &str = "@vault(?) [output withheld: vault scrubber unavailable]";

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "SBcanary-r6-fallback-not-a-secret".to_owned())
}

fn has_ci(hay: &str, needle: &str) -> bool {
    hay.to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "sb-t05r6-{tag}-{}-{}-{}",
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

fn identity(key_file: &Path) -> age::x25519::Identity {
    let text = std::fs::read_to_string(key_file).unwrap();
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap()
        .parse()
        .unwrap()
}

/// Writes the vault file with `age` and `toml`, as a hand edit or an older build would.
fn write_vault_file(path: &Path, key_file: &Path, entries: &[(&str, &str, &str)]) {
    let id = identity(key_file);
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

/// Expects `ValueInLiteral { name: want }` with the repair hint and no canary.
fn expect_in_literal<T>(r: Result<T, VaultError>, want: &str, label: &str) {
    match r {
        Err(e @ VaultError::ValueInLiteral { .. }) => {
            let (display, debug) = (e.to_string(), format!("{e:?}"));
            if let VaultError::ValueInLiteral { name } = &e {
                assert!(name == want, "{label}: wrong entry named");
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
        }
        Err(other) => panic!("{label}: expected ValueInLiteral, got {other}"),
        Ok(_) => panic!("{label}: accepted"),
    }
}

fn names(v: &Vault) -> Vec<String> {
    v.names().into_iter().map(|(n, _)| n).collect()
}

/// Fail closed: the marker, no value in it, refused by vault_refs and resolve_into.
fn assert_withheld(v: &Vault, out: &str, values: &[&str], label: &str) {
    assert!(out == UNSETTLED, "{label}: not the unsettled marker");
    for val in values {
        assert!(!has_ci(out, val), "{label}: a value in the withheld output");
    }
    assert!(
        matches!(vault_refs(out), Err(VaultError::BadReference(0))),
        "{label}: vault_refs"
    );
    let mut sunk = false;
    assert!(
        matches!(
            v.resolve_into(out, |_| sunk = true),
            Err(VaultError::BadReference(0))
        ),
        "{label}: resolve_into"
    );
    assert!(!sunk, "{label}: sink called");
    assert!(v.scrub(out) == out, "{label}: marker is not a fixed point");
}

// ---------------------------------------------------------------------------------------
// R6-1 (round 5). Past the 8-pass cap the whole output is withheld with the unsettled
// marker, file-backed with the real canary; up to the cap it settles into one literal.

#[test]
fn r6_1_pass_cap_withholds_with_the_unsettled_marker() {
    let c = canary();
    let k = ")xyz1";
    let (_dir, path, _kf, key, mut v) = canary_vault("cap");
    v.set("k", k, EntryKind::Personal).unwrap();
    let v = Vault::open(&path, &key).unwrap();
    // Each `xyz1` after the canary costs one more pass: `)xyz1` overlaps the `)` of the
    // literal written before it and is widened over it.
    let text = |n: usize| format!("{c}{}", "xyz1".repeat(n));
    assert!(v.scrub(&text(0)) == "@vault(test)");
    for n in 1..=6 {
        let out = v.scrub(&text(n));
        assert!(out == "@vault(k)", "{n} repeats settle within the cap");
        assert!(v.scrub(&out) == out);
        assert!(v.scrub(&text(n).to_ascii_uppercase()) == "@vault(k)");
    }
    for n in [7usize, 8, 9, 40] {
        let out = v.scrub(&text(n));
        assert_withheld(&v, &out, &[&c, k], &format!("{n} repeats"));
        // The whole output is withheld, not only the unsettled span.
        let wrapped = v.scrub(&format!("before {} after", text(n)));
        assert_withheld(&v, &wrapped, &[&c, k, "before", "after"], "wrapped");
        assert!(
            out != UNAVAILABLE,
            "the failed-build marker is not used here"
        );
    }
    // The process-wide scrub (logs, ledger, panics) follows the same rule.
    v.install_global();
    let out = harness_core::vault::scrub(&text(7));
    assert!(out == UNSETTLED, "global scrub past the cap");
    assert!(harness_core::vault::scrub(&text(6)) == "@vault(k)");
}

// ---------------------------------------------------------------------------------------
// R6-2 (round 5, D-118 (2) amended). The name inside a well-formed reference to an unknown
// entry is scrubbed: `@vault(xx<v>yy)` becomes `@vault(xx@vault(test)yy)`, idempotent,
// value-free, and BadReference for vault_refs.

#[test]
fn r6_2_unknown_reference_holding_a_value_is_scrubbed() {
    let c = canary();
    let (_dir, path, _kf, key, _v) = canary_vault("unknown-ref");
    let v = Vault::open(&path, &key).unwrap();
    // The canary uses only [A-Za-z0-9_.-], so `xx<canary>yy` is a well-formed name.
    for (input, want) in [
        (
            format!("@vault(xx{c}yy)"),
            "@vault(xx@vault(test)yy)".to_owned(),
        ),
        (
            format!("see @vault(xx{}yy) now", c.to_ascii_uppercase()),
            "see @vault(xx@vault(test)yy) now".to_owned(),
        ),
        (format!("@vault({c})"), "@vault(@vault(test))".to_owned()),
    ] {
        let out = v.scrub(&input);
        assert!(out == want, "unknown-reference scrub");
        assert!(!has_ci(&out, &c), "canary left");
        assert!(v.scrub(&out) == out, "not idempotent");
        assert!(matches!(vault_refs(&out), Err(VaultError::BadReference(_))));
    }
    // A known literal is still left alone.
    assert!(v.scrub("@vault(test)") == "@vault(test)");
}

// ---------------------------------------------------------------------------------------
// R6-3 (round 6). set refuses a value inside either withheld marker on every kind of
// handle, before the file changes; near words are accepted and both markers stay fixed
// points.

const MARKER_WORDS: [&str; 16] = [
    "withheld",
    "OUTPUT WITHHELD",
    "put withh",
    "Scrubber Unavailable",
    "did not settle",
    "settle]",
    "[output",
    // the tester's own pieces of the two markers
    "vault scrubber",
    "SCRUB DID",
    "?) [out",
    "eld: s",
    "eld: v",
    "not s",
    "able]",
    "@vault(?) [output withheld: scrub did not settle]",
    "?) [output withheld: vault scrubber unavailable]",
];

const NEAR_WORDS: [&str; 6] = [
    "settled",
    "outputs",
    "withholds",
    "unavailable!",
    "scrubbers",
    "[outpost",
];

#[test]
fn r6_3_set_refuses_values_inside_the_withheld_markers() {
    let c = canary();
    let (_dir, path, _kf, key, mut v) = canary_vault("marker-set");
    let before = std::fs::read(&path).unwrap();
    for (i, w) in MARKER_WORDS.iter().enumerate() {
        expect_in_literal(
            v.set("m", w, EntryKind::Personal),
            "m",
            &format!("open handle, word {i}"),
        );
    }
    assert!(std::fs::read(&path).unwrap() == before, "file rewritten");
    assert_eq!(names(&v), ["test"]);
    // open_for_set checks the entry being set; in_memory too.
    let mut fix = Vault::open_for_set(&path, &key).unwrap();
    let mut mem = Vault::in_memory([("test".to_owned(), c.clone(), EntryKind::Secret)]);
    for (i, w) in MARKER_WORDS.iter().enumerate() {
        expect_in_literal(
            fix.set("m", w, EntryKind::Personal),
            "m",
            &format!("open_for_set, word {i}"),
        );
        expect_in_literal(
            mem.set("m", w, EntryKind::Personal),
            "m",
            &format!("in_memory, word {i}"),
        );
    }
    assert!(std::fs::read(&path).unwrap() == before, "file rewritten");
    // Near words are accepted, and with them in the vault both markers scrub to
    // themselves; the unsettled output stays a value-free fixed point.
    for (i, w) in NEAR_WORDS.iter().enumerate() {
        v.set(&format!("n{i}"), w, EntryKind::Personal).unwrap();
    }
    v.set("k", ")xyz1", EntryKind::Personal).unwrap();
    let v = Vault::open(&path, &key).unwrap();
    assert_eq!(names(&v).len(), NEAR_WORDS.len() + 2);
    assert!(v.scrub(UNSETTLED) == UNSETTLED);
    assert!(v.scrub(UNAVAILABLE) == UNAVAILABLE);
    let out = v.scrub(&format!("{c}{}", "xyz1".repeat(9)));
    let mut values: Vec<&str> = NEAR_WORDS.to_vec();
    values.push(&c);
    assert_withheld(&v, &out, &values, "near words present");
    assert!(v.scrub("all settled, outputs ready") == "all @vault(n0), @vault(n1) ready");
}

// ---------------------------------------------------------------------------------------
// R6-4 (rounds 5 and 6). open refuses a file holding a marker word, naming the entry;
// an `open` handle checks every entry of the re-read file; `open_for_set` checks only the
// entry being set and repairs the file one entry at a time.

#[test]
fn r6_4_open_refuses_marker_words_and_repair_paths() {
    let c = canary();
    for (i, w) in ["Not Settle", "WITHHELD:", "[Output Withheld", "bber unav"]
        .iter()
        .enumerate()
    {
        let (_dir, path, kf, key, _v) = canary_vault(&format!("marker-open-{i}"));
        write_vault_file(&path, &kf, &[("test", &c, "secret"), ("m", w, "personal")]);
        expect_in_literal(Vault::open(&path, &key), "m", &format!("open, word {i}"));
    }

    // An `open` handle from before a hand edit: set of another entry re-reads the file,
    // checks every entry, names `m` and writes nothing.
    let (_dir, path, kf, key, mut early) = canary_vault("marker-reread");
    write_vault_file(
        &path,
        &kf,
        &[("test", &c, "secret"), ("m", "did not settle", "personal")],
    );
    let before = std::fs::read(&path).unwrap();
    expect_in_literal(
        early.set("other", &format!("{c}-other"), EntryKind::Secret),
        "m",
        "open handle re-read",
    );
    assert!(std::fs::read(&path).unwrap() == before, "file rewritten");

    // open_for_set adds another entry while `m` is broken, refuses a marker word for the
    // entry it sets, then repairs `m`.
    let mut fix = Vault::open_for_set(&path, &key).unwrap();
    fix.set("other", &format!("{c}-other"), EntryKind::Secret)
        .unwrap();
    expect_in_literal(Vault::open(&path, &key), "m", "still broken");
    expect_in_literal(
        fix.set("m", "scrub did", EntryKind::Personal),
        "m",
        "repair with a marker word",
    );
    fix.set("m", "Fresno-94", EntryKind::Personal).unwrap();
    let ok = Vault::open(&path, &key).unwrap();
    assert_eq!(names(&ok), ["m", "other", "test"]);
    let s = format!("{c} {c}-other FRESNO-94 did not settle");
    assert!(
        ok.scrub(&s) == "@vault(test) @vault(other) @vault(m) did not settle",
        "repaired vault scrubs every value and no old one"
    );
    let ct = std::fs::read(&path).unwrap();
    assert!(!ct.windows(c.len()).any(|w| w == c.as_bytes()));
}
