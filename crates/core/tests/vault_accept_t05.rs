//! T-05 acceptance (docs/01 H-2, I-4, D-101, D-102, CLAUDE.md hard rule 2).
//!
//! Every planted value is the canary from `SB_VAULT_CANARY` (a unique, non-secret
//! string). No assertion message ever formats the canary or a resolved value, so a
//! failing test cannot itself leak it.
//!
//! Only `global_scrub_after_install_global` touches the process-wide scrub slot in this
//! binary; the panic test runs its child in a separate process.

use std::cell::Cell;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use harness_core::vault::{self, EntryKind, KeySource, Resolved, Vault, VaultError};

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| format!("sbcanary-t05-fallback-{}", std::process::id()))
}

fn canary_vault() -> Vault {
    Vault::in_memory([("test".to_owned(), canary(), EntryKind::Secret)])
}

/// A unique temp dir under the OS temp dir, removed on drop. Never the real vault path.
struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "sb-t05-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

// ---------------------------------------------------------------------------------------
// Trait-absence probes (autoref specialization; works on concrete types).

struct Probe<T: ?Sized>(PhantomData<T>);
trait NoClone {
    fn is_clone(&self) -> bool {
        false
    }
}
impl<T: ?Sized> NoClone for Probe<T> {}
impl<T: Clone> Probe<T> {
    #[allow(dead_code)]
    fn is_clone(&self) -> bool {
        true
    }
}
trait NoDisplay {
    fn is_display(&self) -> bool {
        false
    }
}
impl<T: ?Sized> NoDisplay for Probe<T> {}
impl<T: std::fmt::Display> Probe<T> {
    #[allow(dead_code)]
    fn is_display(&self) -> bool {
        true
    }
}
trait NoSerialize {
    fn is_serialize(&self) -> bool {
        false
    }
}
impl<T: ?Sized> NoSerialize for Probe<T> {}
impl<T: serde::Serialize> Probe<T> {
    #[allow(dead_code)]
    fn is_serialize(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------------------
// Row 1: @vault(test) resolves inside a sink and the sink sees the value.

#[test]
fn row1_vault_test_resolves_inside_sink() {
    let c = canary();
    let v = canary_vault();
    let calls = Cell::new(0);
    let saw_value = v
        .resolve_into("Authorization: Bearer @vault(test)", |r| {
            calls.set(calls.get() + 1);
            r.expose() == format!("Authorization: Bearer {c}")
        })
        .unwrap();
    assert!(saw_value, "sink did not see the resolved value");
    assert_eq!(calls.get(), 1, "sink must be called exactly once");

    // Two references and surrounding text, as in an insertText payload.
    let ok = v
        .resolve_into("[@vault(test)|@vault(test)]", |r| {
            r.expose() == format!("[{c}|{c}]")
        })
        .unwrap();
    assert!(ok, "multi-ref template resolved wrongly");
}

// ---------------------------------------------------------------------------------------
// Row 2: resolve_into never returns the value to the caller except as the sink payload.

#[test]
fn row2_resolve_into_returns_only_sink_payload() {
    let c = canary();
    let v = canary_vault();

    // The return value is exactly the sink's R (here a payload that is not the value).
    #[derive(Debug, PartialEq)]
    struct Sent {
        len: usize,
        header_name: &'static str,
    }
    let out = v
        .resolve_into("@vault(test)", |r| Sent {
            len: r.expose().len(),
            header_name: "x-api-key",
        })
        .unwrap();
    assert_eq!(
        out,
        Sent {
            len: c.len(),
            header_name: "x-api-key"
        }
    );
    assert!(!format!("{out:?}").contains(&c));

    // Error paths: sink never runs, and the Err carries no value.
    for bad in ["@vault(test) @vault(missing)", "@vault(test", "@vault()"] {
        let called = Cell::new(false);
        let e = v
            .resolve_into(bad, |_| called.set(true))
            .expect_err("bad template must fail");
        assert!(!called.get(), "sink ran on a failing template");
        assert!(!e.to_string().contains(&c), "error Display leaked");
        assert!(!format!("{e:?}").contains(&c), "error Debug leaked");
    }

    // Resolved has no Clone, Display or Serialize: the only route out is expose() inside
    // the sink. Vault has no Clone or Serialize.
    assert!(!Probe::<Resolved<'static>>(PhantomData).is_clone());
    assert!(!Probe::<Resolved<'static>>(PhantomData).is_display());
    assert!(!Probe::<Resolved<'static>>(PhantomData).is_serialize());
    assert!(!Probe::<Vault>(PhantomData).is_clone());
    assert!(!Probe::<Vault>(PhantomData).is_serialize());
    assert!(!Probe::<Vault>(PhantomData).is_display());

    // No other public accessor yields a value: names() is names and kinds only.
    let names = v.names();
    assert_eq!(names, vec![("test".to_owned(), EntryKind::Secret)]);
    assert!(!format!("{names:?}").contains(&c));
}

// ---------------------------------------------------------------------------------------
// Row 3: scrub() removes the planted canary in raw, JSON-escaped and percent-encoded forms.

#[test]
fn row3_scrub_raw_json_percent() {
    let base = canary();
    // A value with characters that JSON and percent encoding both change.
    let c = format!("{base} \"q\\/\u{e9}&=+");
    let v = Vault::in_memory([("test".to_owned(), c.clone(), EntryKind::Secret)]);

    let json_obj = serde_json::to_string(&serde_json::json!({ "v": c })).unwrap();
    let json_twice = serde_json::to_string(&json_obj).unwrap();
    let json_ascii: String = c
        .chars()
        .map(|ch| match ch {
            '"' => "\\\"".to_owned(),
            '\\' => "\\\\".to_owned(),
            ch if !ch.is_ascii() => format!("\\u{:04x}", ch as u32),
            ch => ch.to_string(),
        })
        .collect();
    let pct_all: String = c
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let pct_lower = pct_all
        .split('%')
        .enumerate()
        .map(|(i, s)| {
            if i == 0 {
                s.to_owned()
            } else {
                format!("%{}{}", s[..2].to_ascii_lowercase(), &s[2..])
            }
        })
        .collect::<Vec<_>>()
        .join("");
    // encodeURIComponent keeps - _ . ! ~ * ' ( )
    let pct_component: String = c
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    // form encoding: space as +
    let pct_form: String = c
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"*-._".contains(&b) {
                (b as char).to_string()
            } else if b == b' ' {
                "+".to_owned()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();

    let cases = [
        ("raw", format!("log: {c} end")),
        ("json", json_obj),
        ("json-twice", json_twice),
        ("json-ascii", format!("[\"{json_ascii}\"]")),
        ("pct-upper", format!("https://x.test/?a={pct_all}&b=1")),
        ("pct-lower", format!("?a={pct_lower}")),
        ("pct-component", format!("?a={pct_component}")),
        ("pct-form", format!("a={pct_form}&b=1")),
    ];
    for (form, s) in &cases {
        let out = v.scrub(s);
        assert!(!out.contains(&base), "canary survived scrub in {form} form");
        assert!(
            out.contains("@vault(test)"),
            "no @vault(test) literal in {form} form"
        );
    }

    // Plain canary (the docs/01 fixture shape) in all three forms too.
    let p = canary_vault();
    for s in [
        format!("x {base} y"),
        serde_json::to_string(&serde_json::json!({ "k": base })).unwrap(),
        format!("?q={}", base.replace('-', "%2D")),
    ] {
        let out = p.scrub(&s);
        assert!(!out.contains(&base), "plain canary survived scrub");
    }
    assert_eq!(v.scrub("nothing here"), "nothing here");
}

// ---------------------------------------------------------------------------------------
// Row 4: global scrub after install_global (the only in-process global touch here).

#[test]
fn row4_global_scrub_after_install_global() {
    let c = canary();
    let line = format!("tool result: {c}");
    assert_eq!(
        harness_core::scrub(&line),
        line,
        "before install, scrub is the identity"
    );

    let mut v = canary_vault();
    v.install_global();
    assert_eq!(harness_core::scrub(&line), "tool result: @vault(test)");
    assert_eq!(vault::scrub(&line), "tool result: @vault(test)");
    let json = serde_json::to_string(&serde_json::json!({ "r": c })).unwrap();
    assert!(!harness_core::scrub(&json).contains(&c));

    // set() on the installed vault keeps the global scrub current.
    let second = format!("{c}-second");
    v.set("second", &second, EntryKind::Personal).unwrap();
    assert_eq!(harness_core::scrub(&second), "@vault(second)");
    assert_eq!(harness_core::scrub(&line), "tool result: @vault(test)");

    // A later install of a different vault replaces the global one.
    let other = Vault::in_memory([("other".to_owned(), format!("{c}-o"), EntryKind::Secret)]);
    other.install_global();
    assert_eq!(harness_core::scrub(&format!("{c}-o")), "@vault(other)");
}

// ---------------------------------------------------------------------------------------
// Row 5: Debug of Vault / Resolved and every VaultError message contain no value.

#[test]
fn row5_debug_and_errors_contain_no_value() {
    let c = canary();
    let v = canary_vault();
    assert!(!format!("{v:?}").contains(&c), "Vault Debug leaked");
    assert!(!format!("{v:#?}").contains(&c), "Vault pretty Debug leaked");
    v.resolve_into("pre @vault(test) post", |r| {
        let d = format!("{r:?}");
        assert!(!d.contains(&c), "Resolved Debug leaked");
        assert!(d.contains("@vault(test)"));
        assert!(
            !format!("{r:#?}").contains(&c),
            "Resolved pretty Debug leaked"
        );
    })
    .unwrap();

    let mut errs: Vec<(&str, VaultError)> = Vec::new();
    // Unknown
    errs.push((
        "Unknown",
        v.resolve_into("@vault(test)@vault(nope)", |_| ())
            .unwrap_err(),
    ));
    // BadReference (template right after a value reference)
    errs.push((
        "BadReference",
        v.resolve_into("@vault(test) @vault(", |_| ()).unwrap_err(),
    ));
    // BadReference from set(): bad name, value is the canary
    let mut m = canary_vault();
    errs.push((
        "BadReference/set",
        m.set("bad name", &c, EntryKind::Secret).unwrap_err(),
    ));

    let dir = TempDir::new("errs");
    let path = dir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.0.join("vault.key"));
    let mut fv = Vault::open(&path, &key).unwrap();
    fv.set("test", &c, EntryKind::Secret).unwrap();

    // Decrypt: wrong identity
    let other = TempDir::new("errs-other");
    let other_key = KeySource::IdentityFile(other.0.join("k"));
    let mut seed = Vault::open(&other.0.join("v.age"), &other_key).unwrap();
    seed.set("x", "unrelated", EntryKind::Secret).unwrap();
    errs.push(("Decrypt", Vault::open(&path, &other_key).unwrap_err()));

    // Decrypt: valid age file whose plaintext is not the vault TOML shape and quotes the
    // canary (a parse error message could echo it).
    let bad_toml = dir.0.join("bad.toml.age");
    {
        let id_text = std::fs::read_to_string(dir.0.join("vault.key")).unwrap();
        let id_line = id_text
            .lines()
            .find(|l| l.starts_with("AGE-SECRET-KEY-"))
            .unwrap();
        let id: age::x25519::Identity = id_line.parse().unwrap();
        let body = format!("test = \"{c}\"\nbroken = [{c}\n");
        let ct = age::encrypt(&id.to_public(), body.as_bytes()).unwrap();
        std::fs::write(&bad_toml, ct).unwrap();
    }
    errs.push(("Decrypt/toml", Vault::open(&bad_toml, &key).unwrap_err()));

    // Key: file exists, identity missing
    let missing = KeySource::IdentityFile(dir.0.join("absent.key"));
    errs.push(("Key/missing", Vault::open(&path, &missing).unwrap_err()));
    // Key: identity file holds the canary instead of an age key
    let junk_key = dir.0.join("junk.key");
    std::fs::write(&junk_key, format!("AGE-SECRET-KEY-{c}\n")).unwrap();
    errs.push((
        "Key/junk",
        Vault::open(&path, &KeySource::IdentityFile(junk_key)).unwrap_err(),
    ));
    // Io: the vault path is a directory
    errs.push((
        "Io",
        Vault::open(&dir.0, &KeySource::IdentityFile(dir.0.join("vault.key"))).unwrap_err(),
    ));

    let seen: Vec<&str> = errs
        .iter()
        .map(|(_, e)| match e {
            VaultError::Unknown(_) => "Unknown",
            VaultError::BadReference(_) => "BadReference",
            VaultError::Key(_) => "Key",
            VaultError::Io(_) => "Io",
            VaultError::Decrypt => "Decrypt",
        })
        .collect();
    for want in ["Unknown", "BadReference", "Key", "Io", "Decrypt"] {
        assert!(seen.contains(&want), "variant {want} not exercised");
    }
    for (label, e) in &errs {
        assert!(!e.to_string().contains(&c), "{label}: Display leaked");
        assert!(!format!("{e:?}").contains(&c), "{label}: Debug leaked");
        let mut src = std::error::Error::source(e);
        while let Some(s) = src {
            assert!(!s.to_string().contains(&c), "{label}: source leaked");
            src = s.source();
        }
    }
}

// ---------------------------------------------------------------------------------------
// Row 6: age file round trip with a temp identity file (no keychain, no real vault path).

#[test]
fn row6_age_round_trip_temp_identity_file() {
    let c = canary();
    let dir = TempDir::new("age");
    let path = dir.0.join("vault.toml.age");
    let key_path = dir.0.join("keys").join("vault.key");
    let key = KeySource::IdentityFile(key_path.clone());

    let mut v = Vault::open(&path, &key).unwrap();
    assert!(v.names().is_empty(), "missing file opens empty");
    assert!(!key_path.exists(), "open must not create the identity");
    v.set("test", &c, EntryKind::Secret).unwrap();
    v.set("full_name", &format!("{c} \"q\""), EntryKind::Personal)
        .unwrap();
    assert!(path.exists() && key_path.exists());

    // Ciphertext is an age file and holds no plaintext value; identity holds none either.
    let ct = std::fs::read(&path).unwrap();
    assert!(ct.starts_with(b"age-encryption.org/v1"), "not an age file");
    assert!(!contains_bytes(&ct, c.as_bytes()), "value in ciphertext");
    let id = std::fs::read(&key_path).unwrap();
    assert!(!contains_bytes(&id, c.as_bytes()), "value in identity file");
    assert!(
        !std::fs::read_dir(&dir.0).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")),
        "temp file left behind"
    );

    // Re-open from disk with the same identity file.
    let v2 = Vault::open(&path, &key).unwrap();
    assert_eq!(
        v2.names(),
        vec![
            ("full_name".to_owned(), EntryKind::Personal),
            ("test".to_owned(), EntryKind::Secret),
        ]
    );
    assert!(v2
        .resolve_into("@vault(test)|@vault(full_name)", |r| r.expose()
            == format!("{c}|{c} \"q\""))
        .unwrap());
    assert_eq!(v2.scrub(&format!("<{c}>")), "<@vault(test)>");

    // Update round trip: replacing a value re-encrypts with the same identity.
    let mut v3 = v2;
    v3.set("test", &format!("{c}-v2"), EntryKind::Secret)
        .unwrap();
    let v4 = Vault::open(&path, &key).unwrap();
    assert!(v4
        .resolve_into("@vault(test)", |r| r.expose() == format!("{c}-v2"))
        .unwrap());
}

// ---------------------------------------------------------------------------------------
// Row 7: a panic whose message contains the canary prints only scrubbed text.
// The parent re-runs this test binary filtered to the child tests, with
// SB_T05_PANIC_CHILD set, and greps the child's stdout and stderr.

const CHILD_ENV: &str = "SB_T05_PANIC_CHILD";

#[test]
fn row7_child_panic_main_thread() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return; // only meaningful as the child of row7_panic_message_is_scrubbed
    }
    let c = canary();
    canary_vault().install_global();
    panic!("boom: login failed for {c}");
}

#[test]
fn row7_child_panic_unwrap_and_thread() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let c = canary();
    canary_vault().install_global();
    // A spawned thread's panic, formatted through Debug of an error type.
    let h = std::thread::spawn(move || {
        let r: Result<(), String> = std::hint::black_box(Err(format!("token {c} rejected")));
        r.unwrap();
    });
    let _ = h.join();
    // And a JSON-escaped spelling in the message.
    let j = serde_json::to_string(&serde_json::json!({ "v": canary() })).unwrap();
    panic!("json body {j}");
}

#[test]
fn row7_panic_message_is_scrubbed() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let c = canary();
    let exe = std::env::current_exe().unwrap();
    for child in [
        "row7_child_panic_main_thread",
        "row7_child_panic_unwrap_and_thread",
    ] {
        for bt in ["0", "1"] {
            let out = Command::new(&exe)
                .args([child, "--exact", "--nocapture", "--test-threads=1"])
                .env(CHILD_ENV, "1")
                .env("SB_VAULT_CANARY", &c)
                .env("RUST_BACKTRACE", bt)
                .output()
                .unwrap();
            assert!(
                !out.status.success(),
                "{child}: child should have failed by panicking"
            );
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                !contains_bytes(&out.stderr, c.as_bytes()),
                "{child} (RUST_BACKTRACE={bt}): canary in child stderr"
            );
            assert!(
                !contains_bytes(&out.stdout, c.as_bytes()),
                "{child} (RUST_BACKTRACE={bt}): canary in child stdout"
            );
            assert!(
                stderr.contains("@vault(test)") || stdout.contains("@vault(test)"),
                "{child} (RUST_BACKTRACE={bt}): scrubbed panic text not printed"
            );
            assert!(
                stderr.contains("panicked at"),
                "{child} (RUST_BACKTRACE={bt}): panic header missing"
            );
            if child == "row7_child_panic_unwrap_and_thread" {
                assert!(
                    stderr.matches("@vault(test)").count() >= 2,
                    "thread panic and json panic not both printed scrubbed"
                );
            }
        }
    }
}
