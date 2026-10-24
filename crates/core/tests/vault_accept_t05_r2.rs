//! T-05 round 2 acceptance (docs/01-HARNESS-CORE.md H-2, I-4, D-102, CLAUDE.md hard
//! rule 2). Written by the tester, independent of the coder's `vault_scrub_t05_fix.rs`:
//! every expected spelling is built here by hand or from std/serde_json, never from the
//! implementation.
//!
//! Every planted value is derived from `SB_VAULT_CANARY` (a non-secret fallback when it
//! is unset). No assertion message formats a value: comparisons use `assert!(a == b,
//! "label")`, so a failure never prints the canary.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use harness_core::vault::{EntryKind, KeySource, Vault};

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "SBcanary-r2-fallback-not-a-secret".to_owned())
}

fn vault(entries: &[(&str, &str)]) -> Vault {
    Vault::in_memory(
        entries
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned(), EntryKind::Personal)),
    )
}

/// The canary, in any ASCII case, must not be in `s`.
fn assert_no_canary(s: &str, label: &str) {
    let c = canary().to_ascii_lowercase();
    assert!(
        !s.to_ascii_lowercase().contains(&c),
        "{label}: canary survived scrub"
    );
}

/// Scrubs `text`, checks no canary remains, and that the output is exactly `want`.
fn check(v: &Vault, text: &str, want: &str, label: &str) {
    let out = v.scrub(text);
    assert_no_canary(&out, label);
    assert!(out == want, "{label}: scrub output differs from expected");
}

/// Byte-wise percent-encoding: non-ASCII bytes always, ASCII bytes where `enc` is true.
fn pct(v: &str, enc: &dyn Fn(u8) -> bool, plus: bool, lower: bool) -> String {
    let mut out = String::new();
    for b in v.bytes() {
        if plus && b == b' ' {
            out.push('+');
        } else if !b.is_ascii() || enc(b) {
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
/// WHATWG query percent-encode set.
fn query_set(b: u8) -> bool {
    c0(b) || b" \"#<>".contains(&b)
}
/// WHATWG path percent-encode set.
fn path_set(b: u8) -> bool {
    query_set(b) || b"?^`{}".contains(&b)
}
/// encodeURIComponent keeps A-Z a-z 0-9 - _ . ! ~ * ' ( )
fn component_set(b: u8) -> bool {
    !(b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b))
}
/// URLSearchParams / application/x-www-form-urlencoded keeps A-Z a-z 0-9 * - . _
fn form_set(b: u8) -> bool {
    !(b.is_ascii_alphanumeric() || b"*-._".contains(&b))
}

// ---------------------------------------------------------------------------------------
// R2-1. WHATWG query/path spelling of '1 <canary> St, Apt 2'.

#[test]
fn r2_1_whatwg_query_and_path_address() {
    let c = canary();
    let addr = format!("1 {c} St, Apt 2");
    let v = vault(&[("addr", &addr)]);
    // Written by hand: what `new URL(...)` serializes (space -> %20, comma kept).
    let q = format!("1%20{c}%20St,%20Apt%202");
    check(
        &v,
        &format!("https://maps.test/search?q={q}&hl=en"),
        "https://maps.test/search?q=@vault(addr)&hl=en",
        "WHATWG query, literal",
    );
    check(
        &v,
        &format!("https://maps.test/place/{q}/data=!3m1"),
        "https://maps.test/place/@vault(addr)/data=!3m1",
        "WHATWG path, literal",
    );
    check(
        &v,
        &format!("https://maps.test/#{q}"),
        "https://maps.test/#@vault(addr)",
        "WHATWG fragment, literal",
    );
    check(
        &v,
        &format!("https://maps.test/search?q=1+{c}+St%2C+Apt+2&hl=en"),
        "https://maps.test/search?q=@vault(addr)&hl=en",
        "URLSearchParams form, literal",
    );
    check(
        &v,
        &format!("https://maps.test/search?q=1%20{c}%20St%2c%20Apt%202"),
        "https://maps.test/search?q=@vault(addr)",
        "encodeURIComponent, lower hex, literal",
    );
    // The same from the encode-set functions, both hex cases.
    for (label, set) in [
        ("query set", &query_set as &dyn Fn(u8) -> bool),
        ("path set", &path_set),
        ("component set", &component_set),
    ] {
        for lower in [false, true] {
            let enc = pct(&addr, set, false, lower);
            check(
                &v,
                &format!("GET /p/{enc}?a={enc}#x"),
                "GET /p/@vault(addr)?a=@vault(addr)#x",
                &format!("{label}, lower {lower}"),
            );
        }
    }
    // A browser URL logged inside a JSON ledger arg.
    let url = format!("https://maps.test/search?q={q}");
    let line = serde_json::json!({"tool":"web.open","url": url}).to_string();
    let want =
        serde_json::json!({"tool":"web.open","url":"https://maps.test/search?q=@vault(addr)"})
            .to_string();
    check(&v, &line, &want, "WHATWG query inside JSON");
}

// ---------------------------------------------------------------------------------------
// R2-2. Lowercase and uppercase echoes.

#[test]
fn r2_2_lower_and_upper_echoes() {
    let c = canary();
    let email = format!("Mx.{c}@Example.Test");
    let addr = format!("1 {c} St, Apt 2");
    let name = format!("{c} Zo\u{eb} \u{c5}ngstr\u{f6}m");
    let v = Vault::in_memory([
        ("test".to_owned(), c.clone(), EntryKind::Secret),
        ("email".to_owned(), email.clone(), EntryKind::Personal),
        ("addr".to_owned(), addr.clone(), EntryKind::Personal),
        ("name".to_owned(), name.clone(), EntryKind::Personal),
    ]);
    let alternating: String = c
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            if i % 2 == 0 {
                ch.to_ascii_uppercase()
            } else {
                ch.to_ascii_lowercase()
            }
        })
        .collect();
    let cases: Vec<(String, String, &str)> = vec![
        (
            format!("token={}", c.to_lowercase()),
            "token=@vault(test)".into(),
            "canary lower",
        ),
        (
            format!("TOKEN={}", c.to_uppercase()),
            "TOKEN=@vault(test)".into(),
            "canary upper",
        ),
        (
            format!("t={alternating}"),
            "t=@vault(test)".into(),
            "canary alternating case",
        ),
        (
            format!("<td>{}</td>", email.to_lowercase()),
            "<td>@vault(email)</td>".into(),
            "email lower",
        ),
        (
            format!("<td>{}</td>", email.to_uppercase()),
            "<td>@vault(email)</td>".into(),
            "email upper",
        ),
        (
            serde_json::json!({"to": email.to_lowercase()}).to_string(),
            serde_json::json!({"to": "@vault(email)"}).to_string(),
            "email lower in JSON",
        ),
        (
            format!("?q={}", pct(&addr.to_uppercase(), &query_set, false, false)),
            "?q=@vault(addr)".into(),
            "address upper, WHATWG query",
        ),
        (
            format!(
                "?q={}",
                pct(&addr.to_lowercase(), &component_set, false, true)
            ),
            "?q=@vault(addr)".into(),
            "address lower, encodeURIComponent lower hex",
        ),
        (
            format!("<b>{}</b>", name.to_lowercase()),
            "<b>@vault(name)</b>".into(),
            "non-ASCII name lower",
        ),
        (
            format!("<b>{}</b>", name.to_uppercase()),
            "<b>@vault(name)</b>".into(),
            "non-ASCII name upper",
        ),
    ];
    for (text, want, label) in cases {
        check(&v, &text, &want, label);
    }
}

// ---------------------------------------------------------------------------------------
// R2-3. Double-percent-encoded value.

#[test]
fn r2_3_double_percent_encoded() {
    let c = canary();
    let addr = format!("1 {c} St, Apt 2");
    let path_val = format!("{c}/x y&z");
    let v = vault(&[("addr", &addr), ("p", &path_val)]);
    // By hand: encodeURIComponent of the whole browser URL (a continue= parameter).
    check(
        &v,
        &format!(
            "/signin?continue=https%3A%2F%2Fm.test%2Fsearch%3Fq%3D1%2520{c}%2520St%2C%2520Apt%25202&hl=en"
        ),
        "/signin?continue=https%3A%2F%2Fm.test%2Fsearch%3Fq%3D@vault(addr)&hl=en",
        "continue= URL, literal",
    );
    let combos: Vec<(&str, String, &str)> = vec![
        (
            "component of query",
            pct(
                &pct(&addr, &query_set, false, false),
                &component_set,
                false,
                false,
            ),
            "addr",
        ),
        (
            "component of component",
            pct(
                &pct(&addr, &component_set, false, false),
                &component_set,
                false,
                false,
            ),
            "addr",
        ),
        (
            "component of component, lower hex",
            pct(
                &pct(&addr, &component_set, false, true),
                &component_set,
                false,
                true,
            ),
            "addr",
        ),
        (
            "form of form",
            pct(&pct(&addr, &form_set, true, false), &form_set, true, false),
            "addr",
        ),
        (
            "component of path, value with /",
            pct(
                &pct(&path_val, &path_set, false, false),
                &component_set,
                false,
                false,
            ),
            "p",
        ),
        (
            "component of component, value with /",
            pct(
                &pct(&path_val, &component_set, false, false),
                &component_set,
                false,
                false,
            ),
            "p",
        ),
    ];
    for (label, enc, name) in combos {
        check(
            &v,
            &format!("https://x.test/a?continue={enc}&b=1"),
            &format!("https://x.test/a?continue=@vault({name})&b=1"),
            label,
        );
    }
}

// ---------------------------------------------------------------------------------------
// R2-4. {:?} spelling with a control char.

#[derive(Debug)]
#[allow(dead_code)] // fields are read only through Debug
struct Login<'a> {
    user: &'a str,
    pass: &'a str,
}

#[test]
fn r2_4_debug_spelling_with_control_char() {
    let c = canary();
    let d = format!("{c}\u{1}\u{7}\0\u{1b}[0m\u{8}x\r\n");
    let v = Vault::in_memory([
        ("d".to_owned(), d.clone(), EntryKind::Secret),
        ("u".to_owned(), format!("{c}-user"), EntryKind::Personal),
    ]);
    check(&v, &format!("{d:?}"), "\"@vault(d)\"", "{:?} of str");
    check(
        &v,
        &format!("{:?}", Some(d.as_str())),
        "Some(\"@vault(d)\")",
        "{:?} of Option",
    );
    check(
        &v,
        &format!("{:?}", Err::<(), String>(d.clone())),
        "Err(\"@vault(d)\")",
        "{:?} of Err(String)",
    );
    let user = format!("{c}-user");
    let login = Login {
        user: &user,
        pass: &d,
    };
    check(
        &v,
        &format!("{login:?}"),
        "Login { user: \"@vault(u)\", pass: \"@vault(d)\" }",
        "{:?} of struct",
    );
    check(
        &v,
        &format!("{login:#?}"),
        "Login {\n    user: \"@vault(u)\",\n    pass: \"@vault(d)\",\n}",
        "{:#?} of struct",
    );
    check(
        &v,
        &format!("assertion `left == right` failed\n  left: {d:?}\n right: \"x\""),
        "assertion `left == right` failed\n  left: \"@vault(d)\"\n right: \"x\"",
        "assert_eq! message",
    );
    check(
        &v,
        &format!("field={}", d.escape_debug()),
        "field=@vault(d)",
        "escape_debug",
    );
    check(
        &v,
        &format!("field={}", d.escape_default()),
        "field=@vault(d)",
        "escape_default",
    );
}

// R2-4b/4c (probes beyond the listed row): a {:?} spelling with a control char that then
// went into JSON (a serialized ledger/jsonl line scrubbed as a whole) or into {:?} again
// (`Result<_, String>::unwrap()` where the String already holds a {:?}).
#[test]
#[ignore = "red-first T-05 round 2"]
fn r2_4b_debug_control_char_nested_in_json() {
    let c = canary();
    let d = format!("{c}\u{1}x");
    let v = vault(&[("d", &d)]);
    let inner = format!("bad header {d:?}");
    check(
        &v,
        &serde_json::json!({"error": inner}).to_string(),
        &serde_json::json!({"error": "bad header \"@vault(d)\""}).to_string(),
        "{:?} inside JSON",
    );
}

#[test]
#[ignore = "red-first T-05 round 2"]
fn r2_4c_debug_control_char_nested_in_debug() {
    let c = canary();
    let d = format!("{c}\u{1}x");
    let v = vault(&[("d", &d)]);
    let inner = format!("bad header {d:?}");
    check(
        &v,
        &format!("called `Result::unwrap()` on an `Err` value: {inner:?}"),
        "called `Result::unwrap()` on an `Err` value: \"bad header \\\"@vault(d)\\\"\"",
        "{:?} inside {:?}",
    );
}

// ---------------------------------------------------------------------------------------
// R2-5. Adjacent and overlapping values leave no tail.

#[test]
fn r2_5_adjacent_overlapping_values_leave_no_tail() {
    let c = canary();
    let a = format!("{c}-K9");
    let v = vault(&[("a", &a), ("b", "K9-tail")]);
    check(&v, &format!("[{c}-K9-tail]"), "[@vault(a)]", "overlap");
    check(
        &v,
        &format!("[{}]", format!("{c}-K9-tail").to_uppercase()),
        "[@vault(a)]",
        "overlap, upper-cased",
    );
    check(
        &v,
        &format!("{c}-K9-tail {c}-K9 K9-tail {c}-K9-tail"),
        "@vault(a) @vault(a) @vault(b) @vault(a)",
        "overlap, repeated",
    );
    let out = v.scrub(&format!("<{c}-K9-tail>"));
    assert!(
        !out.contains("tail") && !out.contains("K9"),
        "overlap tail left"
    );

    // Adjacent, no shared byte.
    let v = vault(&[("x", &c), ("y", "Zq7-next")]);
    check(&v, &format!("({c}Zq7-next)"), "(@vault(x))", "adjacent");
    check(
        &v,
        &format!("(Zq7-next{c})"),
        "(@vault(y))",
        "adjacent, reversed",
    );

    // Overlap in an encoded spelling: "<c> AB" and "AB cd" meet in "%20AB%20".
    let a2 = format!("{c} AB");
    let v = vault(&[("a2", &a2), ("b2", "AB cd")]);
    check(
        &v,
        &format!("q={c}%20AB%20cd&"),
        "q=@vault(a2)&",
        "overlap, percent",
    );
    check(
        &v,
        &serde_json::json!({"q": format!("{c} AB cd")}).to_string(),
        &serde_json::json!({"q": "@vault(a2)"}).to_string(),
        "overlap, JSON",
    );

    // Right value much longer than the overlap, and a contained value.
    let v = vault(&[("l", &format!("{c}-K")), ("r", "-K9-a-much-longer-tail")]);
    check(
        &v,
        &format!("{c}-K9-a-much-longer-tail!"),
        "@vault(l)!",
        "long right overlap",
    );
    let v = vault(&[("whole", &format!("{c}-K9-tail")), ("mid", "K9")]);
    check(
        &v,
        &format!("{c}-K9-tail K9"),
        "@vault(whole) @vault(mid)",
        "contained",
    );
}

// ---------------------------------------------------------------------------------------
// R2-6. Two concurrent first-time sets keep one identity and both entries.

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "sb-t05r2-{tag}-{}-{}",
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

/// Number of age identities in the identity file.
fn identity_count(key: &Path) -> usize {
    let text = std::fs::read_to_string(key).unwrap_or_default();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .count()
}

/// Every file under `dir` (recursively): none holds the canary, none is a `.tmp`.
fn assert_dir_clean(dir: &Path, label: &str) {
    let c = canary();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            assert!(!name.ends_with(".tmp"), "{label}: temp file left: {name}");
            let bytes = std::fs::read(&p).unwrap();
            let hit = bytes.windows(c.len()).any(|w| w == c.as_bytes());
            assert!(!hit, "{label}: canary in plaintext file {name}");
        }
    }
}

fn check_two_entries(path: &Path, key: &Path, names: [&str; 2], round: usize, label: &str) {
    let c = canary();
    assert_eq!(
        identity_count(key),
        1,
        "{label} round {round}: identity file must hold exactly one identity"
    );
    let v = Vault::open(path, &KeySource::IdentityFile(key.to_path_buf()));
    assert!(v.is_ok(), "{label} round {round}: vault unreadable");
    let v = v.unwrap();
    let got: Vec<String> = v.names().into_iter().map(|(n, _)| n).collect();
    let want: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
    assert!(got == want, "{label} round {round}: entries dropped");
    for n in names {
        let ok = v
            .resolve_into(&format!("@vault({n})"), |r| {
                r.expose() == format!("{c}-{n}")
            })
            .unwrap();
        assert!(ok, "{label} round {round}: entry {n} has the wrong value");
    }
}

#[test]
fn r2_6_two_concurrent_first_time_sets_threads() {
    let c = canary();
    for round in 0..12 {
        let dir = TempDir::new("thr");
        let path = dir.0.join("vault.toml.age");
        let key = dir.0.join("key").join("vault.key");
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|n| {
                let (path, key, barrier, c) =
                    (path.clone(), key.clone(), barrier.clone(), c.clone());
                std::thread::spawn(move || {
                    // Both open the missing vault before either sets: two first-time sets.
                    let mut v = Vault::open(&path, &KeySource::IdentityFile(key))
                        .map_err(|_| "open failed")?;
                    barrier.wait();
                    v.set(n, &format!("{c}-{n}"), EntryKind::Secret)
                        .map_err(|_| "set failed")
                })
            })
            .collect();
        for h in handles {
            let r = h.join().expect("set thread panicked");
            assert!(r.is_ok(), "round {round}: a concurrent first set failed");
        }
        check_two_entries(&path, &key, ["a", "b"], round, "threads");
        assert_dir_clean(&dir.0.join("key"), "threads key dir");
        // The vault dir holds the ciphertext and the lock file only.
        for e in std::fs::read_dir(&dir.0).unwrap() {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            assert!(!name.ends_with(".tmp"), "round {round}: temp file left");
        }
    }
}

const CHILD_ENV: &str = "SB_T05R2_CHILD";

/// Child for the process race: opens the vault, prints READY, waits for a line on stdin
/// (the parent releases both children at once), then sets one entry.
#[test]
fn r2_child_set_process() {
    let Ok(mode) = std::env::var(CHILD_ENV) else {
        return; // helper, only does work as a child process
    };
    if mode != "set" {
        return;
    }
    let c = canary();
    let path = PathBuf::from(std::env::var("SB_T05R2_PATH").unwrap());
    let key = PathBuf::from(std::env::var("SB_T05R2_KEY").unwrap());
    let name = std::env::var("SB_T05R2_NAME").unwrap();
    let mut v = Vault::open(&path, &KeySource::IdentityFile(key)).expect("child open");
    {
        let mut out = std::io::stdout().lock();
        writeln!(out, "SB_T05R2_READY").unwrap();
        out.flush().unwrap();
    }
    let mut go = String::new();
    std::io::stdin().read_line(&mut go).unwrap();
    v.set(&name, &format!("{c}-{name}"), EntryKind::Secret)
        .expect("child set");
}

#[test]
fn r2_6b_two_concurrent_first_time_sets_processes() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let exe = std::env::current_exe().unwrap();
    for round in 0..4 {
        let dir = TempDir::new("proc");
        let path = dir.0.join("vault.toml.age");
        let key = dir.0.join("key").join("vault.key");
        let mut children: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|n| {
                Command::new(&exe)
                    .args(["r2_child_set_process", "--exact", "--nocapture"])
                    .env(CHILD_ENV, "set")
                    .env("SB_VAULT_CANARY", canary())
                    .env("SB_T05R2_PATH", &path)
                    .env("SB_T05R2_KEY", &key)
                    .env("SB_T05R2_NAME", n)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        // Barrier: both children have opened before either is released.
        let mut readers = Vec::new();
        for ch in &mut children {
            let mut r = BufReader::new(ch.stdout.take().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                let n = r.read_line(&mut line).unwrap();
                assert!(n > 0, "round {round}: child exited before READY");
                if line.trim() == "SB_T05R2_READY" {
                    break;
                }
            }
            readers.push(r);
        }
        for ch in &mut children {
            let mut stdin = ch.stdin.take().unwrap();
            stdin.write_all(b"go\n").unwrap();
        }
        for (ch, mut r) in children.into_iter().zip(readers) {
            let mut rest = String::new();
            std::io::Read::read_to_string(&mut r, &mut rest).unwrap();
            let out = ch.wait_with_output().unwrap();
            let err = String::from_utf8_lossy(&out.stderr).into_owned();
            assert_no_canary(&rest, "child stdout");
            assert_no_canary(&err, "child stderr");
            assert!(out.status.success(), "round {round}: child set failed");
        }
        check_two_entries(&path, &key, ["a", "b"], round, "processes");
        assert_dir_clean(&dir.0.join("key"), "processes key dir");
    }
}

// R2-6c (probe beyond the listed row): an open that races a set never fails or sees a
// torn file (handoff limit (e) claims rename atomicity makes a shared lock unnecessary).
#[test]
fn r2_6c_open_during_set_never_fails() {
    let c = canary();
    let dir = TempDir::new("rd");
    let path = dir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.0.join("vault.key"));
    Vault::open(&path, &key)
        .unwrap()
        .set("e0", &format!("{c}-0"), EntryKind::Secret)
        .unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let failures = Arc::new(AtomicUsize::new(0));
    let opens = Arc::new(AtomicUsize::new(0));
    let readers: Vec<_> = (0..2)
        .map(|_| {
            let (path, key, done, failures, opens) = (
                path.clone(),
                key.clone(),
                done.clone(),
                failures.clone(),
                opens.clone(),
            );
            std::thread::spawn(move || {
                while !done.load(Ordering::Acquire) {
                    match Vault::open(&path, &key) {
                        Ok(v) if !v.names().is_empty() => {}
                        _ => {
                            failures.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    opens.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();
    let mut w = Vault::open(&path, &key).unwrap();
    for i in 1..=25 {
        w.set(&format!("e{i}"), &format!("{c}-{i}"), EntryKind::Secret)
            .unwrap();
    }
    done.store(true, Ordering::Release);
    for r in readers {
        r.join().unwrap();
    }
    assert!(opens.load(Ordering::Relaxed) > 0, "readers never ran");
    assert_eq!(
        failures.load(Ordering::Relaxed),
        0,
        "an open racing a set failed"
    );
    assert_eq!(Vault::open(&path, &key).unwrap().names().len(), 26);
}

// ---------------------------------------------------------------------------------------
// R2-7. A panic with a broken stderr does not abort.

#[test]
fn r2_child_panics() {
    let Ok(mode) = std::env::var(CHILD_ENV) else {
        return; // helper, only does work as a child process
    };
    let c = canary();
    Vault::in_memory([("test".to_owned(), c.clone(), EntryKind::Secret)]).install_global();
    match mode.as_str() {
        "panic-main" => panic!("boom: login failed for {c}"),
        "panic-thread" => {
            let c2 = c.clone();
            let r = std::thread::spawn(move || panic!("thread boom {c2}")).join();
            assert!(r.is_err(), "thread did not panic");
            // Two dirty panics in a row, then return normally: exit 0.
            let r = std::thread::spawn(move || {
                std::hint::black_box(Err::<(), String>(format!("token {c} rejected"))).unwrap()
            })
            .join();
            assert!(r.is_err(), "second thread did not panic");
        }
        "panic-clean" => panic!("clean panic, no vault value"),
        "panic-nested-debug" => {
            // A value with a control char, {:?}-formatted into an error String that is
            // then unwrapped: the panic message holds the value Debug-escaped twice.
            let d = format!("{c}\u{1}x");
            Vault::in_memory([("d".to_owned(), d.clone(), EntryKind::Secret)]).install_global();
            std::hint::black_box(Err::<(), String>(format!("bad header {d:?}"))).unwrap();
        }
        _ => {}
    }
}

#[allow(clippy::type_complexity)]
fn run_panic_child(mode: &str, broken: bool, backtrace: &str) -> (Option<i32>, String, String) {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "r2_child_panics",
        "--exact",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(CHILD_ENV, mode)
    .env("SB_VAULT_CANARY", canary())
    .env("RUST_BACKTRACE", backtrace)
    .stdin(Stdio::null());
    if broken {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader); // no reader anywhere: every write to the child's stderr fails
                      // stdout stays usable: libtest itself exits 101 when its own stdout is broken,
                      // which would hide the result.
        cmd.stdout(Stdio::null()).stderr(writer);
        let status = cmd.status().unwrap();
        drop(cmd);
        (status.code(), String::new(), String::new())
    } else {
        let out = cmd.output().unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

#[test]
fn r2_7_panic_with_broken_stderr_does_not_abort() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    for bt in ["0", "1"] {
        // libtest exits 101 when the test panicked and unwound; an abort is a signal on
        // unix and 0xC0000409 (STATUS_STACK_BUFFER_OVERRUN) on Windows.
        let (code, _, _) = run_panic_child("panic-main", true, bt);
        assert_eq!(code, Some(101), "dirty main panic, broken stderr, bt {bt}");
        let (code, _, _) = run_panic_child("panic-thread", true, bt);
        assert_eq!(code, Some(0), "dirty thread panics, broken stderr, bt {bt}");
        let (code, _, _) = run_panic_child("panic-clean", true, bt);
        assert_eq!(code, Some(101), "clean panic, broken stderr, bt {bt}");
    }
    // Sanity: the same children with a working stderr print only scrubbed text.
    let (code, out, err) = run_panic_child("panic-main", false, "0");
    assert_eq!(code, Some(101));
    assert_no_canary(&out, "panic-main stdout");
    assert_no_canary(&err, "panic-main stderr");
    assert!(
        err.contains("boom: login failed for @vault(test)"),
        "scrubbed panic message missing"
    );
    let (code, out, err) = run_panic_child("panic-thread", false, "1");
    assert_eq!(code, Some(0));
    assert_no_canary(&out, "panic-thread stdout");
    assert_no_canary(&err, "panic-thread stderr");
    assert!(
        err.contains("thread boom @vault(test)"),
        "thread message missing"
    );
    assert!(
        err.contains("token @vault(test) rejected"),
        "unwrap message missing"
    );
}

// R2-4d (probe, end to end for R2-4c): the panic hook prints the nested-{:?} unwrap
// message with the value in it.
#[test]
#[ignore = "red-first T-05 round 2"]
fn r2_4d_panic_hook_nested_debug_unwrap_is_scrubbed() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let (code, out, err) = run_panic_child("panic-nested-debug", false, "0");
    assert_eq!(code, Some(101), "child did not panic normally");
    assert_no_canary(&out, "nested-debug panic stdout");
    assert_no_canary(&err, "nested-debug panic stderr");
}
