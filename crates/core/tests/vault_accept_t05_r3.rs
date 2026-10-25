//! T-05 round 3 acceptance (docs/01-HARNESS-CORE.md H-2 and the step-1 vault fixture; I-4;
//! D-102 as amended by D-118; CLAUDE.md hard rule 2). Written by the tester, independent of
//! the coder's tests: every expected spelling is built here by hand or from std/serde_json.
//!
//! Covers the D-118 behaviour the earlier files do not: the value floor through the public
//! API (refused before any file, lock or identity is touched; the error names only the key;
//! characters, not bytes; a hand-made vault file holding a short value), empty values,
//! scrub output as a parseable fixed point, a reflected secret wrapped in an unknown
//! `@vault(...)`, the round-3 spellings, the panic hook's hand-off to the previous hook,
//! and the docs/01 fixture end to end with a file-backed vault.
//!
//! Every planted value is derived from `SB_VAULT_CANARY` (a non-secret fallback when it is
//! unset). No assertion message formats a value.
//!
//! `SB_VAULT_CANARY=<unique> cargo test -p harness-core --test vault_accept_t05_r3`

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_core::class::Class;
use harness_core::ledger::read_jsonl;
use harness_core::mcp::{
    dispatch, Classified, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolRegistry,
    ToolSpec,
};
use harness_core::memory::{ts_now, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::{vault_refs, EntryKind, KeySource, Vault, VaultError, MIN_VALUE_CHARS};
use serde_json::{json, Value};

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "SBcanary-r3-fallback-not-a-secret".to_owned())
}

fn mem(entries: &[(&str, &str)]) -> Vault {
    Vault::in_memory(
        entries
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned(), EntryKind::Secret)),
    )
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
            "sb-t05r3-{tag}-{}-{}-{}",
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

/// Every file under `dir` (contents and names) is free of `needle` in any ASCII case;
/// returns the file count.
fn assert_clean_tree(dir: &Path, needle: &str, label: &str) -> usize {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, &mut files);
    let lower = needle.to_ascii_lowercase();
    for f in &files {
        let bytes = std::fs::read(f).unwrap().to_ascii_lowercase();
        assert!(
            !contains_bytes(&bytes, lower.as_bytes()),
            "{label}: a vault value leaked into {}",
            f.display()
        );
        assert!(
            !has_ci(&f.to_string_lossy(), needle),
            "{label}: a vault value leaked into a file name"
        );
    }
    files.len()
}

/// A JSON string body (no quotes) in the style of an encoder that writes every non-ASCII
/// char (and every char in `extra`) as `\uXXXX` UTF-16 units.
fn json_ascii(v: &str, extra: &[char], upper: bool) -> String {
    let mut out = String::new();
    for c in v.chars() {
        if extra.contains(&c) || !c.is_ascii() || (c as u32) < 0x20 {
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                if upper {
                    out.push_str(&format!("\\u{u:04X}"));
                } else {
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
        } else if c == '"' {
            out.push_str("\\\"");
        } else if c == '\\' {
            out.push_str("\\\\");
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// R3-1. set refuses a short value before touching the file, the lock or the key.

#[test]
fn r3_1_set_refuses_short_value_before_touching_anything() {
    let dir = TempDir::new("floor-fresh");
    let path = dir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.0.join("vault.key"));
    let mut v = Vault::open(&path, &key).unwrap();
    for short in ["Q", "Zq", "Zq7", "   ", "\u{e9}\u{65e5}\u{672c}"] {
        match v.set("mi", short, EntryKind::Personal) {
            Err(VaultError::ValueTooShort { name, min }) => {
                assert_eq!(name, "mi");
                assert_eq!(min, MIN_VALUE_CHARS);
            }
            Err(other) => panic!("expected ValueTooShort, got {other}"),
            Ok(()) => panic!("a value under the floor was accepted"),
        }
    }
    assert_eq!(MIN_VALUE_CHARS, 4, "D-118 fixes the floor at 4");
    assert!(
        dir_entries(&dir.0).is_empty(),
        "a refused set created a vault file, an identity or a lock file: {:?}",
        dir_entries(&dir.0)
    );
    assert!(v.names().is_empty(), "a refused set changed the vault");
    // At the floor it is accepted, in ASCII and in multi-byte characters.
    v.set("pin", "Zq7k", EntryKind::Personal).unwrap();
    v.set("kanji", "\u{e9}\u{65e5}\u{672c}x", EntryKind::Personal)
        .unwrap();
    let re = Vault::open(&path, &key).unwrap();
    let names: Vec<String> = re.names().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["kanji", "pin"]);
    assert_eq!(re.scrub("a zQ7K b"), "a @vault(pin) b");
    // A short value is refused on an existing vault too, and the file is not rewritten.
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        v.set("pin", "Zq7", EntryKind::Personal),
        Err(VaultError::ValueTooShort { .. })
    ));
    assert!(std::fs::read(&path).unwrap() == before, "file rewritten");
    assert_eq!(v.scrub("zq7k"), "@vault(pin)", "old value must stay");
}

// ---------------------------------------------------------------------------------------
// R3-2. ValueTooShort names only the key.

#[test]
#[ignore = "red-first T-05 round 4: ValueTooShort text gained the repair hint; tester updates the fixed string"]
fn r3_2_value_too_short_error_names_only_the_key() {
    let c = canary();
    let mut v = mem(&[("test", &c)]);
    // Three distinctive characters that occur nowhere in the error text by chance.
    let short = "\u{2603}Xj";
    let err = v.set("state_code", short, EntryKind::Personal).unwrap_err();
    let shown = format!("{err} | {err:?} | {err:#?}");
    assert!(shown.contains("state_code"), "the key is named");
    assert!(shown.contains('4'), "the floor is named");
    assert!(!shown.contains(short), "the value leaked into the error");
    assert!(!shown.contains('\u{2603}'), "part of the value leaked");
    assert!(!shown.contains(&c), "another value leaked into the error");
    assert!(std::error::Error::source(&err).is_none());
    // A short value whose bytes spell part of the canary is not echoed either.
    let prefix: String = c.chars().take(3).collect();
    let err = v.set("x", &prefix, EntryKind::Personal).unwrap_err();
    let shown = format!("{err}");
    assert!(
        shown
            == "vault entry \"x\" is shorter than 4 characters; a value that short cannot be scrubbed without destroying ordinary text",
        "error text is fixed and names only the key"
    );
}

// ---------------------------------------------------------------------------------------
// R3-3. open refuses a vault file holding a short value (written here with age + toml, as a
// hand edit or an older build would leave it); an empty value opens fine.

fn identity(key_file: &Path) -> age::x25519::Identity {
    let text = std::fs::read_to_string(key_file).unwrap();
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap()
        .parse()
        .unwrap()
}

fn write_vault_file(path: &Path, id: &age::x25519::Identity, entries: &[(&str, &str, &str)]) {
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

#[test]
fn r3_3_open_refuses_a_file_holding_a_short_value() {
    let c = canary();
    let dir = TempDir::new("floor-file");
    let path = dir.0.join("vault.toml.age");
    let key_file = dir.0.join("vault.key");
    let key = KeySource::IdentityFile(key_file.clone());
    let mut early = Vault::open(&path, &key).unwrap();
    early.set("test", &c, EntryKind::Secret).unwrap();
    let id = identity(&key_file);

    // The writer itself round-trips: a file with only good values opens.
    write_vault_file(
        &path,
        &id,
        &[("test", &c, "secret"), ("blank", "", "personal")],
    );
    let ok = Vault::open(&path, &key).unwrap();
    assert_eq!(ok.names().len(), 2);
    assert_eq!(ok.scrub(&format!("[{c}]")), "[@vault(test)]");
    assert_eq!(
        ok.scrub("plain text"),
        "plain text",
        "empty value scrubs nothing"
    );

    let short = "\u{2603}Xj";
    write_vault_file(
        &path,
        &id,
        &[("test", &c, "secret"), ("state", short, "personal")],
    );
    match Vault::open(&path, &key) {
        Err(e @ VaultError::ValueTooShort { .. }) => {
            let shown = format!("{e} {e:?}");
            assert!(shown.contains("\"state\""), "the key is named");
            assert!(
                !shown.contains(short) && !shown.contains(&c),
                "a value leaked"
            );
            match e {
                VaultError::ValueTooShort { name, min } => {
                    assert_eq!(name, "state");
                    assert_eq!(min, MIN_VALUE_CHARS);
                }
                _ => unreachable!(),
            }
        }
        Err(other) => panic!("expected ValueTooShort, got {other}"),
        Ok(_) => panic!("open accepted a file holding a short value"),
    }
    // A vault opened before the edit re-reads the file under its lock in set: refused,
    // and the file is left byte for byte.
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        early.set("other", &format!("{c}-other"), EntryKind::Secret),
        Err(VaultError::ValueTooShort { ref name, .. }) if name == "state"
    ));
    assert!(std::fs::read(&path).unwrap() == before, "file rewritten");
    assert_eq!(early.names().len(), 1, "a refused set changed the vault");
}

// ---------------------------------------------------------------------------------------
// R3-4. Empty values are allowed and scrub nothing.

#[test]
fn r3_4_empty_value_allowed_and_scrubs_nothing() {
    let c = canary();
    let dir = TempDir::new("empty");
    let path = dir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.0.join("vault.key"));
    let mut v = Vault::open(&path, &key).unwrap();
    v.set("blank", "", EntryKind::Personal).unwrap();
    let text = "\"\" {} %20 @vault(x) a b c";
    assert_eq!(v.scrub(text), text, "an empty value scrubbed something");
    v.set("test", &c, EntryKind::Secret).unwrap();
    let re = Vault::open(&path, &key).unwrap();
    assert_eq!(re.names().len(), 2);
    let got = re
        .resolve_into("[@vault(blank)][@vault(test)]", |r| {
            r.expose() == format!("[][{c}]")
        })
        .unwrap();
    assert!(got, "empty and canary resolve inside the sink");
    assert!(
        re.scrub(&format!("{text} {c}")) == format!("{text} @vault(test)"),
        "scrub with an empty entry present"
    );
    // The empty entry's literal is left alone.
    assert_eq!(re.scrub("@vault(blank)"), "@vault(blank)");
}

// ---------------------------------------------------------------------------------------
// R3-5. Scrub output is a fixed point and keeps known literals parseable (D-118 (2)), over a
// corpus built from awkward pieces, with values at the floor and values that spell parts
// of `@vault(name)`.

#[test]
#[ignore = "red-first T-05 round 4: values vault/ault/test are now refused as ValueInLiteral; tester updates the corpus"]
fn r3_5_scrub_output_is_a_parseable_fixed_point() {
    let c = canary();
    let dir = TempDir::new("fixpoint");
    let path = dir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.0.join("vault.key"));
    let mut v = Vault::open(&path, &key).unwrap();
    v.set("test", &c, EntryKind::Secret).unwrap();
    v.set("pin", "7Kq2", EntryKind::Personal).unwrap();
    v.set("v", "vault", EntryKind::Personal).unwrap();
    v.set("a", "ault", EntryKind::Personal).unwrap();
    v.set("t", "test", EntryKind::Personal).unwrap();
    let known = ["a", "pin", "t", "test", "v"];
    let json_c = serde_json::to_string(&c).unwrap();
    let pieces: Vec<String> = vec![
        "hello".into(),
        " ".into(),
        c.clone(),
        c.to_ascii_uppercase(),
        json_c,
        "7kQ2".into(),
        "@vault(test)".into(),
        "@vault(pin)".into(),
        "@vault(v)".into(),
        "@vault(a)".into(),
        "@vault(t)".into(),
        "Vault".into(),
        "ault".into(),
        "TEST".into(),
        "@".into(),
        "(".into(),
        ")".into(),
        "q=".into(),
        "%2D".into(),
        "{\"k\":".into(),
    ];
    // Deterministic pseudo-random strings of 1 to 8 pieces (xorshift64).
    let mut seed: u64 = 0x5eed_7005;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut bad_fixed = 0usize;
    let mut bad_parse = 0usize;
    let mut leaks = 0usize;
    let mut first_bad: Option<usize> = None;
    for i in 0..4000 {
        let n = 1 + (next() % 8) as usize;
        let s: String = (0..n)
            .map(|_| pieces[(next() % pieces.len() as u64) as usize].as_str())
            .collect();
        let once = v.scrub(&s);
        if has_ci(&once, &c) {
            leaks += 1;
        }
        if v.scrub(&once) != once {
            bad_fixed += 1;
            first_bad.get_or_insert(i);
        }
        // Input whose every literal is a known one must give output that still parses.
        let input_known = vault_refs(&s).is_ok_and(|r| r.iter().all(|n| known.contains(n)));
        if input_known && vault_refs(&once).is_err() {
            bad_parse += 1;
            first_bad.get_or_insert(i);
        }
    }
    assert_eq!(leaks, 0, "canary survived scrub");
    assert_eq!(
        bad_fixed, 0,
        "scrub(scrub(s)) != scrub(s) (first at {first_bad:?})"
    );
    assert_eq!(
        bad_parse, 0,
        "known literals no longer parse (first at {first_bad:?})"
    );
    // Explicit cases.
    for (s, want) in [
        (
            "@vault(test) @vault(pin) @vault(v) @vault(a)",
            "@vault(test) @vault(pin) @vault(v) @vault(a)",
        ),
        ("Vault test ault", "@vault(v) @vault(t) @vault(a)"),
        ("@vault(zzz)", "@vault(zzz)"),
    ] {
        let once = v.scrub(s);
        assert_eq!(once, want);
        assert_eq!(v.scrub(&once), once);
    }
}

// ---------------------------------------------------------------------------------------
// R3-6. D-118 (2) claims scrub(scrub(s)) == scrub(s) for every s, and rule 2 says no value
// leaves. Here a replacement literal completes a value: `<canary>@` (a password that ends
// in `@`) followed by another entry's value, or `)<canary>` preceded by one. The first pass
// sees neither value whole; its output holds the first value in full, and a second pass
// changes it.

#[test]
fn r3_6_replacement_literal_does_not_complete_another_value() {
    let c = canary();
    let tail = "Zq7-tail-value";
    let mut bad: Vec<String> = Vec::new();
    for (label, p, s) in [
        ("value ending in @", format!("{c}@"), format!("{c}{tail}")),
        (
            "value starting with )",
            format!("){c}"),
            format!("{tail}{c}"),
        ),
    ] {
        let v = mem(&[("p", &p), ("t", tail)]);
        let once = v.scrub(&s);
        if has_ci(&once, &p) {
            bad.push(format!("{label}: scrub output holds a whole vault value"));
        }
        let twice = v.scrub(&once);
        if twice != once {
            bad.push(format!("{label}: scrub is not idempotent"));
        }
        if vault_refs(&once).is_ok() && vault_refs(&twice).is_err() {
            bad.push(format!("{label}: second scrub breaks the literal"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("; "));
}

// ---------------------------------------------------------------------------------------
// R3-7. A reflected secret wrapped in an unknown or malformed @vault(...) is still scrubbed,
// and a value that only overlaps a known literal is still scrubbed.

#[test]
fn r3_7_reflected_secret_in_unknown_literal_is_scrubbed() {
    let c = canary();
    let v = mem(&[("test", &c), ("pin", "7Kq2")]);
    let upper = c.to_ascii_uppercase();
    let pct: String = c
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let cases = [
        format!("@vault({c})"),
        format!("@vault({upper})"),
        format!("@vault({c}"),
        format!("@vault(x{c})"),
        format!("@vault(test{c})"),
        format!("@vault(pin{c})"),
        format!("@vault({c}test)"),
        format!("@vault({pct})"),
        format!("@vault(@vault({c}))"),
        format!("@vault(test){c}"),
        format!("{c}@vault(test)"),
        json!({"a": format!("@vault({c})")}).to_string(),
        format!("{:?}", format!("@vault({c})")),
    ];
    for (i, s) in cases.iter().enumerate() {
        let once = v.scrub(s);
        assert!(!has_ci(&once, &c), "case {i}: reflected secret survived");
        assert!(once.contains("@vault(test)"), "case {i}: no literal");
        assert!(v.scrub(&once) == once, "case {i}: not a fixed point");
    }
    // The fixed `@vault(` of an unknown opener is kept intact.
    assert_eq!(v.scrub(&format!("@vault({c})")), "@vault(@vault(test))");
    // A value that overlaps a known literal (but is not inside it) cannot hide behind it.
    let k = format!("{c}@vault");
    let w = mem(&[("k", &k), ("pin", "7Kq2")]);
    let once = w.scrub(&format!("{c}@vault(pin)"));
    assert!(!has_ci(&once, &c), "overlapping value survived");
    assert!(once.starts_with("@vault(k)"));
}

// ---------------------------------------------------------------------------------------
// R3-8. Round-3 spellings (D-118 (1)) of one value: escape_debug, escape_default, JSON with
// `\/`, ASCII-only JSON with a surrogate pair, Go/Rails HTML-safe JSON, System.Text.Json,
// JSON in JSON, Debug in JSON, JSON in Debug, Debug in Debug. File-backed vault.

#[test]
fn r3_8_round3_spellings() {
    let c = canary();
    let d = format!("{c}/A&B<x>'q'+1 `\u{e9}\u{1f600}\u{2028}\"\u{1}");
    let dir = TempDir::new("spellings");
    let path = dir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.0.join("vault.key"));
    let mut v = Vault::open(&path, &key).unwrap();
    v.set("d", &d, EntryKind::Secret).unwrap();
    let v = Vault::open(&path, &key).unwrap();

    let json = serde_json::to_string(&d).unwrap();
    let mut go = String::new();
    for ch in json.trim_matches('"').chars() {
        match ch {
            '<' => go.push_str("\\u003c"),
            '>' => go.push_str("\\u003e"),
            '&' => go.push_str("\\u0026"),
            '\u{2028}' => go.push_str("\\u2028"),
            ch => go.push(ch),
        }
    }
    let dbg = format!("{d:?}");
    let cases: Vec<(&str, String)> = vec![
        ("escape_debug", d.escape_debug().to_string()),
        ("escape_default", d.escape_default().to_string()),
        ("json \\/", json.replace('/', "\\/")),
        ("json ascii lower", json_ascii(&d, &[], false)),
        ("json ascii upper", json_ascii(&d, &[], true)),
        ("go html-safe", go),
        (
            "system.text.json",
            json_ascii(&d, &['<', '>', '&', '\'', '+', '`', '"'], true),
        ),
        ("json in json", serde_json::to_string(&json).unwrap()),
        ("debug in json", serde_json::to_string(&dbg).unwrap()),
        ("json in debug", format!("{json:?}")),
        ("debug in debug", format!("{dbg:?}")),
    ];
    for (label, s) in cases {
        let text = format!("{{\"msg\":\"x {s} y\"}}");
        let out = v.scrub(&text);
        assert!(!has_ci(&out, &c), "{label}: canary survived");
        assert!(out.contains("@vault(d)"), "{label}: no literal");
        assert!(v.scrub(&out) == out, "{label}: not a fixed point");
    }
}

// ---------------------------------------------------------------------------------------
// R3-9. Panic hook (D-118 (5)): with a hook installed before install_global, a clean
// message goes to that previous hook, a dirty one is printed scrubbed in the default
// hook's format and never reaches the previous hook.

#[test]
fn r3_9_child_previous_hook() {
    if std::env::var_os("SB_T05R3_HOOK_CHILD").is_none() {
        return;
    }
    std::panic::set_hook(Box::new(|info| {
        let p = info.payload();
        let msg = p
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string>".to_owned());
        let _ = writeln!(std::io::stderr().lock(), "PREV-HOOK-SAW: {msg}");
    }));
    let c = canary();
    mem(&[("test", &c)]).install_global();
    let _ = std::panic::catch_unwind(|| panic!("clean message zebra"));
    let _ = std::panic::catch_unwind(|| panic!("dirty login for {} failed", canary()));
    let _ = std::panic::catch_unwind(|| std::panic::panic_any(42u32));
    let _ = std::thread::Builder::new()
        .name("r3-worker".into())
        .spawn(move || panic!("thread token {c} rejected"))
        .unwrap()
        .join();
    let _ = writeln!(std::io::stderr().lock(), "CHILD-DONE");
}

#[test]
fn r3_9_panic_hook_hands_clean_messages_to_previous_hook() {
    if std::env::var_os("SB_T05R3_HOOK_CHILD").is_some() {
        return;
    }
    let c = canary();
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe)
        .args(["r3_9_child_previous_hook", "--exact", "--nocapture"])
        .env("SB_T05R3_HOOK_CHILD", "1")
        .env("SB_VAULT_CANARY", &c)
        .env("RUST_BACKTRACE", "0")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "child failed");
    assert!(!has_ci(&stdout, &c), "canary in child stdout");
    assert!(!has_ci(&stderr, &c), "canary in child stderr");
    assert!(stderr.contains("CHILD-DONE"), "child did not finish");
    assert!(
        stderr.contains("PREV-HOOK-SAW: clean message zebra"),
        "clean message did not reach the previous hook"
    );
    assert!(
        stderr.contains("PREV-HOOK-SAW: <non-string>"),
        "non-string payload did not reach the previous hook"
    );
    assert!(
        !stderr.contains("PREV-HOOK-SAW: dirty") && !stderr.contains("PREV-HOOK-SAW: thread"),
        "a dirty message reached the previous hook"
    );
    let lines: Vec<&str> = stderr.lines().collect();
    for msg in [
        "dirty login for @vault(test) failed",
        "thread token @vault(test) rejected",
    ] {
        let i = lines
            .iter()
            .position(|l| *l == msg)
            .unwrap_or_else(|| panic!("scrubbed message missing: {msg}"));
        assert!(i > 0, "no header line before {msg}");
        let head = lines[i - 1];
        assert!(
            head.starts_with("thread '")
                && head.contains("' panicked at ")
                && head.contains("vault_accept_t05_r3.rs:")
                && head.ends_with(':'),
            "header not in the default hook's format: {head}"
        );
    }
    assert!(
        stderr.contains("thread 'r3-worker' panicked at "),
        "thread name missing from the header"
    );
}

// ---------------------------------------------------------------------------------------
// R3-10. docs/01 step-1 vault fixture, file-backed vault: `@vault(test)` and a value at the
// floor resolve inside the fake tool's sink through the H-6 wrapper; neither value appears
// in the returned output, the ledger table (MemStore, the DB stand-in), ledger.jsonl or
// any file under the runs root, and the fail-closed ledger (D-117) writes every row.

#[derive(Default)]
struct Wire(Mutex<Vec<String>>);

struct EchoSend(Arc<Wire>);

#[async_trait]
impl ToolHandler for EchoSend {
    async fn classify(&self, _ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let to = args.get("to").and_then(Value::as_str).unwrap_or("");
        Ok(Classified {
            class: None,
            domain: Some("mail.example".into()),
            recipients: vec![to.to_owned()],
            hints: Vec::new(),
            what: Some(format!("send a message to {to}")),
            values: vec![("to".into(), to.to_owned())],
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let to = args.get("to").and_then(Value::as_str).unwrap_or("");
        let body = args.get("body").and_then(Value::as_str).unwrap_or("");
        // A careless tool echoes the resolved body back; the wrapper must scrub it.
        let echoed = ctx.resolve_into(body, |r| {
            self.0 .0.lock().unwrap().push(r.expose().to_owned());
            r.expose().to_owned()
        })?;
        Ok(ToolOutput {
            text: format!("sent to {to}: {echoed}"),
            summary: Some(format!("sent {echoed}")),
            consequences: json!({"to": to, "body": echoed, "dbg": format!("{echoed:?}")}),
            files: Vec::new(),
            stopped: None,
        })
    }
}

#[tokio::test]
async fn r3_10_docs01_vault_fixture_file_backed() {
    const FRIEND: &str = "friend@example.com";
    let c = canary();
    let pin = "7Kq2";
    let root = TempDir::new("runs");
    let vdir = TempDir::new("vault");
    let path = vdir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(vdir.0.join("vault.key"));
    {
        let mut v = Vault::open(&path, &key).unwrap();
        v.set("test", &c, EntryKind::Secret).unwrap();
        v.set("pin", pin, EntryKind::Personal).unwrap();
    }
    let vault = Arc::new(Vault::open(&path, &key).unwrap());
    vault.install_global();

    let run = RunFolder::create(&root.0, "accept-t05-r3", "acceptance t05 r3").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "acceptance t05 r3".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    store.add_contact(FRIEND);
    let harness = Harness {
        store: store.clone(),
        vault: vault.clone(),
        asker: Arc::new(ScriptedAsker::new()),
        runs_root: root.0.clone(),
        run: Some(run.id.clone()),
    };
    let wire = Arc::new(Wire::default());
    let reg = ToolRegistry::new(vec![ToolSpec {
        name: "echo.send".into(),
        description: "echo.send(to, body): fake send for acceptance".into(),
        class: Class::Send,
        args: json!({"type": "object"}),
        handler: Arc::new(EchoSend(wire.clone())),
    }]);

    let out = dispatch(
        "echo",
        &reg,
        &harness,
        "echo.send",
        json!({"to": FRIEND, "body": "code @vault(test) pin @vault(pin) end"}),
    )
    .await
    .unwrap();
    {
        let w = wire.0.lock().unwrap();
        assert_eq!(w.len(), 1, "sink called once");
        assert!(
            w[0] == format!("code {c} pin {pin} end"),
            "sink saw both values"
        );
    }
    // A careless caller passes the raw values as arguments too.
    let out2 = dispatch(
        "echo",
        &reg,
        &harness,
        "echo.send",
        json!({"to": FRIEND, "body": format!("{c} {}", pin.to_lowercase())}),
    )
    .await
    .unwrap();
    for (i, o) in [&out, &out2].into_iter().enumerate() {
        let s = serde_json::to_string(o).unwrap();
        assert!(
            !has_ci(&s, &c) && !has_ci(&s, pin),
            "output {i} leaked a value"
        );
        assert!(
            s.contains("@vault(test)") && s.contains("@vault(pin)"),
            "output {i} has no literal"
        );
    }

    let mut rows = store.ledger();
    rows.sort_by_key(|(id, _)| *id);
    assert!(rows.len() >= 2, "ledger rows written: {}", rows.len());
    for (_, ev) in &rows {
        assert!(ev.ok, "a ledger row failed");
        let s = serde_json::to_string(ev).unwrap();
        assert!(!has_ci(&s, &c), "ledger row leaked the canary");
        for field in [
            ev.args.to_string(),
            ev.result_summary.clone(),
            ev.consequences.to_string(),
            ev.why.clone().unwrap_or_default(),
        ] {
            assert!(!has_ci(&field, pin), "ledger data field leaked the pin");
        }
    }
    let jsonl = std::fs::read_to_string(run.ledger_jsonl()).unwrap();
    assert!(!has_ci(&jsonl, &c), "ledger.jsonl leaked the canary");
    let mut lines = read_jsonl(&run.ledger_jsonl()).unwrap();
    lines.sort_by_key(|(id, _)| *id);
    assert!(
        lines == rows,
        "ledger.jsonl must match the table row for row"
    );
    let n = assert_clean_tree(&root.0, &c, "runs root");
    assert!(n >= 2, "runs tree has prompt.md and ledger.jsonl ({n})");
    // The vault file itself holds neither value in clear.
    let ct = std::fs::read(&path).unwrap();
    assert!(!contains_bytes(&ct, c.as_bytes()) && !contains_bytes(&ct, pin.as_bytes()));
}
