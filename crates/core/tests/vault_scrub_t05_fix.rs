//! T-05 fix round (docs/01 H-2, D-102, CLAUDE.md hard rule 2): regression tests for the
//! scrub bypasses and `set` races found in review of 49cce85.
//!
//! Every planted value is built from the canary (`SB_VAULT_CANARY`, or a non-secret
//! fallback). No assertion message formats a value: assertions compare whole scrubbed
//! strings with `assert!(a == b, "label")`, so a failure never prints the canary.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use harness_core::vault::{EntryKind, KeySource, Vault};

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "canary-7f3a-not-a-secret".to_owned())
}

fn vault(entries: &[(&str, &str)]) -> Vault {
    Vault::in_memory(
        entries
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned(), EntryKind::Personal)),
    )
}

/// Scrubs `text` and checks the result is exactly `want`, without printing either.
fn check(v: &Vault, text: &str, want: &str, label: &str) {
    let out = v.scrub(text);
    assert!(out == want, "{label}: scrub output differs from expected");
}

/// Percent-encodes `v` byte-wise: every non-ASCII byte, and every ASCII byte for which
/// `encode` is true. `plus` writes space as `+`; `lower` uses lower-case hex.
fn pct(v: &str, encode: &dyn Fn(u8) -> bool, plus: bool, lower: bool) -> String {
    let mut out = String::new();
    for b in v.bytes() {
        if plus && b == b' ' {
            out.push('+');
        } else if !b.is_ascii() || encode(b) {
            if lower {
                out.push_str(&format!("%{b:02x}"));
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(b as char);
        }
    }
    out
}

fn c0(b: u8) -> bool {
    b < 0x20 || b == 0x7f
}

// URL standard percent-encode sets (https://url.spec.whatwg.org/#percent-encoded-bytes),
// written out from the spec text, independent of the implementation.
fn fragment_set(b: u8) -> bool {
    c0(b) || b" \"<>`".contains(&b)
}
fn query_set(b: u8) -> bool {
    c0(b) || b" \"#<>".contains(&b)
}
fn special_query_set(b: u8) -> bool {
    query_set(b) || b == b'\''
}
fn path_set(b: u8) -> bool {
    query_set(b) || b"?^`{}".contains(&b)
}
fn userinfo_set(b: u8) -> bool {
    path_set(b) || b"/:;=@[\\]^|".contains(&b)
}
/// encodeURI keeps A-Z a-z 0-9 ; , / ? : @ & = + $ - _ . ! ~ * ' ( ) #
fn encode_uri_set(b: u8) -> bool {
    !(b.is_ascii_alphanumeric() || b";,/?:@&=+$-_.!~*'()#".contains(&b))
}
/// encodeURIComponent keeps A-Z a-z 0-9 - _ . ! ~ * ' ( )
fn component_set(b: u8) -> bool {
    !(b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b))
}
/// application/x-www-form-urlencoded keeps A-Z a-z 0-9 * - . _
fn form_set(b: u8) -> bool {
    !(b.is_ascii_alphanumeric() || b"*-._".contains(&b))
}

// ---------------------------------------------------------------------------------------
// 1. Browser (WHATWG URL) serialization and encodeURI.

#[test]
fn url_whatwg_sets_and_encode_uri() {
    // The review's case, written out by hand: space encoded, comma raw.
    let v = vault(&[("addr", "1 canary-7f3a St, Apt 2")]);
    check(
        &v,
        "https://x.test/maps?q=1%20canary-7f3a%20St,%20Apt%202&z=3",
        "https://x.test/maps?q=@vault(addr)&z=3",
        "literal WHATWG query",
    );
    check(
        &v,
        "https://x.test/maps?q=1%20canary-7f3a%20st,%20apt%202&z=3",
        "https://x.test/maps?q=@vault(addr)&z=3",
        "literal WHATWG query, lower-cased text",
    );

    let c = canary();
    let addr = format!("1 {c} St, Apt 2");
    // Every ASCII punctuation the sets disagree on, plus a non-ASCII letter.
    let rich = format!("{c} a,b;c/d?e:f@g&h=i+j$k#l'm\"n<o>p`q{{r}}s^t|u[v]w\\x !~*() \u{e9}");
    let v = vault(&[("addr", &addr), ("rich", &rich)]);
    let sets: [(&str, &dyn Fn(u8) -> bool); 6] = [
        ("fragment", &fragment_set),
        ("query", &query_set),
        ("special-query", &special_query_set),
        ("path", &path_set),
        ("userinfo", &userinfo_set),
        ("encodeURI", &encode_uri_set),
    ];
    for (label, set) in sets {
        for lower in [false, true] {
            for (name, value) in [("addr", &addr), ("rich", &rich)] {
                let enc = pct(value, set, false, lower);
                check(
                    &v,
                    &format!("https://x.test/p?{name}={enc}#end"),
                    &format!("https://x.test/p?{name}=@vault({name})#end"),
                    &format!("{label} set, lower hex {lower}, entry {name}"),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// 2. Case-folded echoes (normalised emails, CSS text-transform in innerText).

#[test]
fn case_folded_echoes() {
    let c = canary();
    let email = format!("Mx.{c}@Example.Test");
    let name = format!("{c} \u{c9}mile \u{c6}r\u{f8} \u{3a3}\u{39f}\u{3a3}");
    let v = vault(&[("email", &email), ("name", &name)]);
    for (label, text) in [
        ("ascii lower", email.to_lowercase()),
        ("ascii upper", email.to_uppercase()),
        ("non-ascii lower", name.to_lowercase()),
        ("non-ascii upper", name.to_uppercase()),
    ] {
        let entry = if label.starts_with("ascii") {
            "email"
        } else {
            "name"
        };
        check(
            &v,
            &format!("<td>{text}</td>"),
            &format!("<td>@vault({entry})</td>"),
            label,
        );
    }
    // A lower-cased non-ASCII value that then went through encodeURIComponent.
    let enc = pct(&name.to_lowercase(), &component_set, false, false);
    check(
        &v,
        &format!("?n={enc}"),
        "?n=@vault(name)",
        "lower-cased then percent-encoded",
    );
}

// ---------------------------------------------------------------------------------------
// 3. Double percent-encoding (a URL inside a continue= parameter).

#[test]
fn double_percent_encoding() {
    let v = vault(&[("p", "canary-7f3a x/y")]);
    check(
        &v,
        "/login?continue=canary-7f3a%2520x%252Fy",
        "/login?continue=@vault(p)",
        "literal %25XX upper",
    );
    check(
        &v,
        "/login?continue=canary-7f3a%2520x%252fy",
        "/login?continue=@vault(p)",
        "literal %25XX lower",
    );

    let c = canary();
    let value = format!("{c} 1, x/y&z");
    let v = vault(&[("p", &value)]);
    let combos: [(&str, String); 4] = [
        (
            "component in component",
            pct(
                &pct(&value, &component_set, false, false),
                &component_set,
                false,
                false,
            ),
        ),
        (
            "query in component",
            pct(
                &pct(&value, &query_set, false, false),
                &component_set,
                false,
                false,
            ),
        ),
        (
            "form in form",
            pct(&pct(&value, &form_set, true, false), &form_set, true, false),
        ),
        (
            "component in component, lower hex",
            pct(
                &pct(&value, &component_set, false, true),
                &component_set,
                false,
                true,
            ),
        ),
    ];
    for (label, enc) in combos {
        check(
            &v,
            &format!("https://x.test/a?continue={enc}&b=1"),
            "https://x.test/a?continue=@vault(p)&b=1",
            label,
        );
    }
}

// ---------------------------------------------------------------------------------------
// 4. Rust Debug spellings (assert_eq!/unwrap panics, tracing `?field`).

#[test]
fn rust_debug_spellings() {
    let c = canary();
    let value = format!("{c}\u{1}\u{200b}e\u{301}\t\"'\\ \u{e9}\u{7f}");
    let v = vault(&[("d", &value)]);
    check(
        &v,
        &format!("left: {value:?}"),
        "left: \"@vault(d)\"",
        "{:?} of str",
    );
    check(
        &v,
        &format!(
            "called `Result::unwrap()` on an `Err` value: {:?}",
            Err::<(), _>(&value)
        ),
        "called `Result::unwrap()` on an `Err` value: Err(\"@vault(d)\")",
        "unwrap message",
    );
    check(
        &v,
        &format!("field={}", value.escape_debug()),
        "field=@vault(d)",
        "escape_debug",
    );
    check(
        &v,
        &format!("field={}", value.escape_default()),
        "field=@vault(d)",
        "escape_default",
    );
}

// ---------------------------------------------------------------------------------------
// 5. Overlapping entries: the union of the match spans is redacted.

#[test]
fn overlapping_entries_redact_union() {
    let v = vault(&[("head", "canary-7f3a"), ("tail", "7f3a-tail")]);
    check(&v, "canary-7f3a-tail", "@vault(head)", "partial overlap");
    check(
        &v,
        "x canary-7f3a-tail y canary-7f3a z 7f3a-tail",
        "x @vault(head) y @vault(head) z @vault(tail)",
        "overlap among separate matches",
    );
    // Adjacent spans merge too; the leftmost match names the span.
    let v = vault(&[("a", "canary-7f3a"), ("b", "-tail9")]);
    check(&v, "[canary-7f3a-tail9]", "[@vault(a)]", "adjacent spans");
    // Longest wins at the same start (the old leftmost-longest behaviour).
    let v = vault(&[("short", "canary-7f3a"), ("long", "canary-7f3a-long")]);
    check(&v, "<canary-7f3a-long>", "<@vault(long)>", "same start");
    check(&v, "<canary-7f3a>", "<@vault(short)>", "short alone");
    // Chain of three overlaps.
    let v = vault(&[("x", "canary-7f"), ("y", "7f3a-xy"), ("z", "xy-end")]);
    check(&v, "(canary-7f3a-xy-end)", "(@vault(x))", "chain");
}

// ---------------------------------------------------------------------------------------
// 6. Concurrent `vault set` keeps one identity and every entry.

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "sb-t05fix-{tag}-{}-{}",
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

#[test]
fn concurrent_sets_keep_identity_and_entries() {
    const THREADS: usize = 4;
    let c = canary();
    for round in 0..6 {
        let dir = TempDir::new("race");
        let path = dir.0.join("vault.toml.age");
        let key = KeySource::IdentityFile(dir.0.join("keys").join("vault.key"));
        // Odd rounds start from an existing vault; even rounds race the first set.
        if round % 2 == 1 {
            Vault::open(&path, &key)
                .unwrap()
                .set("base", &format!("{c}-base"), EntryKind::Secret)
                .unwrap();
        }
        let barrier = Arc::new(Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|i| {
                let (path, key, barrier, c) =
                    (path.clone(), key.clone(), barrier.clone(), c.clone());
                std::thread::spawn(move || {
                    // Every thread opens before any thread sets, so none sees the others.
                    let mut v = Vault::open(&path, &key).map_err(|_| "open failed")?;
                    barrier.wait();
                    v.set(&format!("t{i}"), &format!("{c}-{i}"), EntryKind::Secret)
                        .map_err(|_| "set failed")
                })
            })
            .collect();
        for h in handles {
            let r = h.join().expect("thread panicked");
            assert!(r.is_ok(), "round {round}: a concurrent set failed");
        }
        let v = Vault::open(&path, &key);
        assert!(
            v.is_ok(),
            "round {round}: vault unreadable after concurrent sets"
        );
        let v = v.unwrap();
        let mut want: Vec<String> = (0..THREADS).map(|i| format!("t{i}")).collect();
        if round % 2 == 1 {
            want.insert(0, "base".to_owned());
        }
        let got: Vec<String> = v.names().into_iter().map(|(n, _)| n).collect();
        assert_eq!(got, want, "round {round}: entries dropped");
        for i in 0..THREADS {
            let ok = v
                .resolve_into(&format!("@vault(t{i})"), |r| {
                    r.expose() == format!("{c}-{i}")
                })
                .unwrap();
            assert!(ok, "round {round}: entry t{i} has the wrong value");
        }
    }
}

// ---------------------------------------------------------------------------------------
// 7. The panic hook survives a broken stderr (no double panic, no abort).

const CHILD_ENV: &str = "SB_T05FIX_PANIC_CHILD";

#[test]
fn child_dirty_panic_with_broken_stderr() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return; // only meaningful as the child of panic_hook_survives_broken_stderr
    }
    let c = canary();
    Vault::in_memory([("test".to_owned(), c.clone(), EntryKind::Secret)]).install_global();
    panic!("boom: login failed for {c}");
}

#[test]
fn panic_hook_survives_broken_stderr() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let (reader, writer) = std::io::pipe().unwrap();
    // No reader anywhere: every write to the child's stderr fails (EPIPE, ERROR_NO_DATA).
    drop(reader);
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "child_dirty_panic_with_broken_stderr",
        "--exact",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(CHILD_ENV, "1")
    .env("SB_VAULT_CANARY", canary())
    .env("RUST_BACKTRACE", "0")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(writer);
    let status = cmd.status().unwrap();
    // 101 is libtest's "a test failed": the panic unwound normally. An abort from a
    // double panic is a signal on unix and 0xC0000409 on Windows.
    assert_eq!(
        status.code(),
        Some(101),
        "child aborted instead of unwinding"
    );
}

// ---------------------------------------------------------------------------------------
// 8. The hand-written TOML serialization round-trips awkward names and values.

#[test]
fn file_round_trip_awkward_names_and_values() {
    let c = canary();
    let dir = TempDir::new("toml");
    let path = dir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.0.join("vault.key"));
    let values = [
        (
            "a.b",
            format!("{c} \"q\" \\ \t\n\r\u{8}\u{c}\u{1}\u{7f} \u{e9}\u{200b}\u{1f600}"),
        ),
        ("x-y_z.1", format!("{c}'''\"\"\"")),
        ("plain", c.clone()),
    ];
    let mut v = Vault::open(&path, &key).unwrap();
    for (n, val) in &values {
        v.set(n, val, EntryKind::Personal).unwrap();
    }
    let v2 = Vault::open(&path, &key).unwrap();
    let mut want: Vec<(String, EntryKind)> = values
        .iter()
        .map(|(n, _)| ((*n).to_owned(), EntryKind::Personal))
        .collect();
    want.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(v2.names(), want);
    for (n, val) in &values {
        let ok = v2
            .resolve_into(&format!("@vault({n})"), |r| r.expose() == val.as_str())
            .unwrap();
        assert!(ok, "entry {n} did not round-trip");
    }
}

// ---------------------------------------------------------------------------------------
// 9. Round 3 (D-118): the value floor, and scrub output that stays parseable.

#[test]
fn set_refuses_a_short_value_and_leaves_the_file_alone() {
    use harness_core::vault::{vault_refs, VaultError, MIN_VALUE_CHARS};
    let c = canary();
    let dir = TempDir::new("floor");
    let path = dir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.0.join("vault.key"));
    let mut v = Vault::open(&path, &key).unwrap();
    v.set("test", &c, EntryKind::Secret).unwrap();
    let before = std::fs::read(&path).unwrap();
    let short: String = c.chars().take(MIN_VALUE_CHARS - 1).collect();
    assert!(matches!(
        v.set("st", &short, EntryKind::Personal),
        Err(VaultError::ValueTooShort { ref name, min }) if name == "st" && min == MIN_VALUE_CHARS
    ));
    assert!(
        std::fs::read(&path).unwrap() == before,
        "a refused set rewrote the file"
    );
    let names: Vec<String> = Vault::open(&path, &key)
        .unwrap()
        .names()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(names, ["test"]);
    // Scrub output of the file-backed vault is a fixed point and parses.
    let once = v.scrub(&format!("q={c}&r=@vault(test)"));
    check(&v, &once, "q=@vault(test)&r=@vault(test)", "fixed point");
    assert_eq!(vault_refs(&once).unwrap(), ["test", "test"]);
}

#[test]
fn short_in_memory_values_leave_literals_and_text_parseable() {
    let c = canary();
    // in_memory is exempt from the floor; a middle initial and a state code.
    let v = vault(&[("mi", "Q"), ("st", "ca"), ("test", &c)]);
    check(
        &v,
        "type @vault(test) then @vault(st) then @vault(mi)",
        "type @vault(test) then @vault(st) then @vault(mi)",
        "literals of known names untouched",
    );
    let once = v.scrub(&format!("Quit: {c} in CA"));
    assert!(!once.contains(&c), "value survived");
    assert!(v.scrub(&once) == once, "scrub is not idempotent");
    assert!(
        harness_core::vault::vault_refs(&once).is_ok(),
        "scrub output has a malformed ref"
    );
}

// ---------------------------------------------------------------------------------------
// 10. Round 4 (D-118): values inside literals are refused, scrub is a real fixed point,
// and a file `open` refuses can be repaired with `open_for_set` + `VaultEditor::set`.

/// True when `needle` occurs in `hay` in any ASCII case.
fn has_ci(hay: &str, needle: &str) -> bool {
    hay.to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

/// A file-backed vault holding `test` = the canary, in a fresh temp dir.
fn canary_file_vault(tag: &str) -> (TempDir, PathBuf, KeySource, Vault) {
    let dir = TempDir::new(tag);
    let path = dir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.0.join("vault.key"));
    let mut v = Vault::open(&path, &key).unwrap();
    v.set("test", &canary(), EntryKind::Secret).unwrap();
    (dir, path, key, v)
}

#[test]
fn canary_wrapped_in_literals_is_scrubbed_to_a_fixed_point() {
    let c = canary();
    let (_dir, _path, _key, v) = canary_file_vault("wrap");
    for (label, s) in [
        ("inside an unknown literal", format!("@vault({c})")),
        ("appended to a known name", format!("@vault(test{c})")),
        ("right after a known literal", format!("@vault(test){c}")),
        ("right before a known literal", format!("{c}@vault(test)")),
        ("upper-case opener", format!("@VAULT({c})")),
        ("unclosed opener", format!("@vault({c}")),
        (
            "between two literals",
            format!("x@vault(test)@vault({c})@vault(test)y"),
        ),
        ("closed after a literal", format!("@vault(test){c})")),
        (
            "upper-cased value",
            format!("@vault({})", c.to_ascii_uppercase()),
        ),
    ] {
        let once = v.scrub(&s);
        assert!(!has_ci(&once, &c), "{label}: canary survived");
        assert!(v.scrub(&once) == once, "{label}: not a fixed point");
    }
}

#[test]
fn literal_completing_a_value_is_scrubbed_review_round_3() {
    use harness_core::vault::vault_refs;
    let c = canary();
    let (_dir, path, key, mut v) = canary_file_vault("xyz1");
    v.set("k", ")xyz1", EntryKind::Secret).unwrap();
    let once = v.scrub(&format!("{c}xyz1"));
    check(&v, &once, "@vault(k)", "first pass");
    check(&v, "@vault(test)xyz1", "@vault(k)", "half scrubbed input");
    assert_eq!(vault_refs(&once).unwrap(), ["k"]);
    // The same through a reopened file.
    let re = Vault::open(&path, &key).unwrap();
    check(&re, &format!("a {c}xyz1 b"), "a @vault(k) b", "reopened");
}

#[test]
fn scrub_fixed_point_property_over_tricky_values() {
    use harness_core::vault::vault_refs;
    let c = canary();
    // Built through set, so every value meets the D-118 rules.
    let mut v = Vault::in_memory(std::iter::empty());
    let values: Vec<(&str, String)> = vec![
        ("test", c.clone()),
        ("k", ")xyz1".to_owned()),
        ("p", format!("{c}@")),
        ("q", format!("){c}")),
        ("o", "@@x(".to_owned()),
        ("m", "vault(x".to_owned()),
        ("t", "Zq7-tail".to_owned()),
    ];
    for (n, val) in &values {
        v.set(n, val, EntryKind::Secret).unwrap();
    }
    let known = ["k", "m", "o", "p", "q", "t", "test"];
    let pieces: Vec<String> = vec![
        c.clone(),
        c.to_ascii_uppercase(),
        ")".into(),
        "(".into(),
        "@".into(),
        "@vault(".into(),
        "@vault(test)".into(),
        "@vault(k)".into(),
        "@vault(t)".into(),
        "xyz1".into(),
        "x".into(),
        "Zq7-tail".into(),
        "vault".into(),
        " ".into(),
        "\"".into(),
    ];
    let mut seed: u64 = 0x7005_f1c5;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (mut leaks, mut not_fixed, mut unparsed) = (0usize, 0usize, 0usize);
    for _ in 0..3000 {
        let n = 1 + (next() % 8) as usize;
        let s: String = (0..n)
            .map(|_| pieces[(next() % pieces.len() as u64) as usize].as_str())
            .collect();
        let once = v.scrub(&s);
        if values.iter().any(|(_, val)| has_ci(&once, val)) {
            leaks += 1;
        }
        if v.scrub(&once) != once {
            not_fixed += 1;
        }
        let input_known = vault_refs(&s).is_ok_and(|r| r.iter().all(|n| known.contains(n)));
        if input_known && vault_refs(&once).is_err() {
            unparsed += 1;
        }
    }
    assert_eq!(leaks, 0, "a whole value survived scrub");
    assert_eq!(not_fixed, 0, "scrub(scrub(s)) != scrub(s)");
    assert_eq!(unparsed, 0, "known literals no longer parse");
}

#[test]
fn set_refuses_values_inside_literals_and_leaves_the_file_alone() {
    use harness_core::vault::VaultError;
    let c = canary();
    let (_dir, path, _key, mut v) = canary_file_vault("in-literal");
    let before = std::fs::read(&path).unwrap();
    let cf: String = c.chars().filter(|ch| ch.is_ascii_alphanumeric()).collect();
    let cf = if cf.len() >= 4 {
        cf
    } else {
        format!("{cf}Zq7k")
    };
    for (label, name, value, want) in [
        ("inside @vault(", "pw", "Vault", "pw"),
        ("inside its own literal", "zip_94110", "94110", "zip_94110"),
        ("inside the literal of test", "pw", "(TEST)", "pw"),
    ] {
        match v.set(name, value, EntryKind::Personal) {
            Err(VaultError::ValueInLiteral { name: got }) => assert_eq!(got, want, "{label}"),
            Err(other) => panic!("{label}: expected ValueInLiteral, got {other}"),
            Ok(()) => panic!("{label}: set accepted it"),
        }
    }
    assert!(
        std::fs::read(&path).unwrap() == before,
        "a refused set rewrote the file"
    );
    // A new name whose literal would hold the canary-derived value of an existing entry.
    v.set("zip", &cf, EntryKind::Personal).unwrap();
    let after_zip = std::fs::read(&path).unwrap();
    match v.set(&format!("k_{cf}"), "unrelated-value", EntryKind::Secret) {
        Err(e @ VaultError::ValueInLiteral { .. }) => {
            let shown = format!("{e} {e:?}");
            assert!(shown.contains("\"zip\""), "the value's entry is named");
            assert!(
                !has_ci(&shown, &cf) && !shown.contains(&c),
                "a value leaked"
            );
        }
        Err(other) => panic!("expected ValueInLiteral, got {other}"),
        Ok(()) => panic!("set accepted a name holding a value"),
    }
    assert!(std::fs::read(&path).unwrap() == after_zip, "file rewritten");
    assert_eq!(v.names().len(), 2);
}

#[test]
fn open_for_set_repairs_a_pre_floor_file() {
    use harness_core::vault::VaultError;
    let c = canary();
    let (dir, path, key, _v) = canary_file_vault("repair");
    // An older build wrote a short value and one inside `@vault(`.
    let id_text = std::fs::read_to_string(dir.0.join("vault.key")).unwrap();
    let id: age::x25519::Identity = id_text
        .lines()
        .find(|l| l.starts_with("AGE-SECRET-KEY-"))
        .unwrap()
        .parse()
        .unwrap();
    let body = format!(
        "test = {{ value = {}, kind = \"secret\" }}\nst = {{ value = \"CA\", kind = \"personal\" }}\nv = {{ value = \"ault\", kind = \"personal\" }}\n",
        serde_json::to_string(&c).unwrap()
    );
    let ct = age::encrypt(&id.to_public(), body.as_bytes()).unwrap();
    std::fs::write(&path, ct).unwrap();

    let err = Vault::open(&path, &key).unwrap_err();
    assert!(matches!(err, VaultError::ValueTooShort { ref name, .. } if name == "st"));
    assert!(
        err.to_string().contains("harness vault set st"),
        "no repair hint"
    );
    // The editor (D-84) sets one entry while the other is still broken.
    let mut fix: harness_core::vault::VaultEditor = Vault::open_for_set(&path, &key).unwrap();
    fix.set("st", "California", EntryKind::Personal).unwrap();
    let err = Vault::open(&path, &key).unwrap_err();
    assert!(matches!(err, VaultError::ValueInLiteral { ref name } if name == "v"));
    assert!(
        err.to_string().contains("harness vault set v"),
        "no repair hint"
    );
    fix.set("v", "Zq7-v-value", EntryKind::Personal).unwrap();
    fix.save().unwrap();
    let ok = Vault::open(&path, &key).unwrap();
    assert_eq!(ok.names().len(), 3);
    let kept = ok
        .resolve_into("@vault(test)", |r| r.expose() == c.as_str())
        .unwrap();
    assert!(kept, "the repair lost an entry");
}
