//! T-05d acceptance (docs/01-HARNESS-CORE.md H-2; D-77, B-151; CLAUDE.md rule 2). Written by
//! the tester, black box, independent of the coder's unit tests in `vault.rs`.
//!
//! Rows:
//! 1. `Basic <base64 of user:value>` is scrubbed for all 4 encoders (standard and url,
//!    padded and unpadded), any user length, in-memory, file-backed and the global scrub.
//! 2. A JWT (base64url unpadded) whose payload carries the value is scrubbed, header and
//!    signature untouched, also when the value needs JSON escapes.
//! 3. The value at all 3 byte alignments in both alphabets, padded and unpadded, with any
//!    tail after it: only the edge characters that carry neighbouring bits survive. When
//!    the value ends the blob (no neighbour after it), nothing after the literal survives:
//!    neither the last, zero-filled character nor the `=` padding.
//! 4. A trimmed base64 pattern under 8 characters is never added: base64 of a 4 or 5
//!    character value is never touched, a 7-character core is not a pattern while an
//!    8-character one is, and every base64 redaction removes at least 8 characters.
//! 5. The D-118 `ValueInLiteral` rule covers the base64 spellings: `set` refuses a value
//!    whose base64url core (any alignment, any ASCII case) lies inside another entry's
//!    `@vault(name)`, and a name whose literal holds an existing value's base64 core, on
//!    in-memory and file-backed (`VaultEditor`) vaults, leaving the file unchanged; a value
//!    whose base64 is under the 8-character floor is not refused for it.
//! 6. End to end: a tool resolves `@vault(test)` at its sink and echoes the value
//!    base64-encoded (Basic header, JWT, data URL) in its text, summary, consequences, an
//!    error and a stop; the client also passes an encoded value as an argument. Through
//!    `mcp::dispatch` (returned output, ledger table, jsonl mirror, runs/) and through
//!    `mcp::serve` in a child process (real stdout and stderr), no spelling of the value is
//!    left: neither the raw value nor any base64 core of it at any alignment.
//!
//! The planted value is `SB_VAULT_CANARY` (or a fresh one when unset). No assertion message
//! formats a value. Temp directories are removed at the end of each test unless
//! `SB_T05D_KEEP` names a directory to keep them under (for an external grep).
//!
//! `SB_VAULT_CANARY=<unique> cargo test -p harness-core --test vault_accept_t05d -- --nocapture`

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::engine::GeneralPurpose;
use base64::Engine as _;
use harness_core::class::Class;
use harness_core::gate::StopReason;
use harness_core::mcp::{
    dispatch, scrub_value, serve, Classified, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput,
    ToolRegistry, ToolSpec,
};
use harness_core::memory::{ts_now, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::{EntryKind, KeySource, Vault, VaultError};
use serde_json::{json, Value};

/// Standard, standard unpadded, url, url unpadded.
const ENGINES: [&GeneralPurpose; 4] = [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD];
const CAP: Duration = Duration::from_secs(120);

/// One value per process (a fresh one when `SB_VAULT_CANARY` is unset), so every helper
/// and the serve child (which gets it explicitly) plant the same value.
fn canary() -> String {
    static C: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        std::env::var("SB_VAULT_CANARY")
            .ok()
            .filter(|c| c.chars().count() >= 8)
            .unwrap_or_else(|| {
                format!(
                    "sbcanary-t05d-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                )
            })
    })
    .clone()
}

fn lit(name: &str) -> String {
    format!("@vault({name})")
}

fn vault_of(entries: &[(&str, &str)]) -> Vault {
    Vault::in_memory(
        entries
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned(), EntryKind::Secret)),
    )
}

/// The base64 characters that carry only bits of `v`, at each of the 3 byte alignments, in
/// both alphabets: what must never survive. Computed with the `base64` crate, not with the
/// implementation's encoder.
fn b64_cores(v: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for k in 0..3 {
        let mut blob = vec![0u8; k];
        blob.extend_from_slice(v);
        let lead = (8 * k).div_ceil(6);
        let end = 8 * (k + v.len()) / 6;
        for e in [&STANDARD_NO_PAD, &URL_SAFE_NO_PAD] {
            let enc = e.encode(&blob);
            if end > lead {
                out.push(enc[lead..end].to_owned());
            }
        }
    }
    out
}

/// True when `hay` holds `value` (any ASCII case) or any base64 core of it.
fn leaks(hay: &str, value: &str) -> bool {
    hay.to_ascii_lowercase()
        .contains(&value.to_ascii_lowercase())
        || b64_cores(value.as_bytes())
            .iter()
            .any(|c| hay.contains(c.as_str()))
}

/// `out` is `before`, then `enc` with exactly one `@vault(name)` in place of everything
/// but its edge characters, then `after`. The value is the `n` bytes at byte `off` of the
/// blob `enc` encodes: at most `ceil(8 off / 6)` characters survive before the literal and
/// none of the characters `[.. floor(8 (off + n) / 6))` survive after it. Prints no value.
#[allow(clippy::too_many_arguments)]
fn assert_edges(
    what: &str,
    out: &str,
    before: &str,
    enc: &str,
    after: &str,
    name: &str,
    off: usize,
    n: usize,
) {
    assert!(
        out.len() >= before.len() + after.len() && out.starts_with(before) && out.ends_with(after),
        "{what}: the text around the blob changed (off {off})"
    );
    let mid = &out[before.len()..out.len() - after.len()];
    let l = lit(name);
    assert!(
        mid.matches(l.as_str()).count() == 1,
        "{what}: not exactly one {l} in the scrubbed blob (off {off})"
    );
    let i = mid.find(l.as_str()).unwrap();
    let (left, right) = (&mid[..i], &mid[i + l.len()..]);
    assert!(
        enc.starts_with(left) && enc.ends_with(right),
        "{what}: the kept edges are not the blob's own (off {off})"
    );
    assert!(
        left.len() <= (8 * off).div_ceil(6),
        "{what}: {} characters kept before the literal (off {off})",
        left.len()
    );
    assert!(
        enc.len() - right.len() >= 8 * (off + n) / 6,
        "{what}: {} characters kept after the literal (off {off}, n {n})",
        right.len()
    );
}

/// A temp directory, removed on drop unless `SB_T05D_KEEP` is set.
struct TempDir(PathBuf, bool);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let keep = std::env::var_os("SB_T05D_KEEP").map(PathBuf::from);
        let parent = keep.clone().unwrap_or_else(std::env::temp_dir);
        let p = parent.join(format!(
            "sb-t05d-accept-{tag}-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p, keep.is_some())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if !self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// Every file under `dir`, recursively, with its bytes read lossily as text.
fn all_files(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            all_files(&p, out);
        } else {
            let b = std::fs::read(&p).unwrap();
            out.push((p, String::from_utf8_lossy(&b).into_owned()));
        }
    }
}

// =====================================================================================
// Row 1: HTTP Basic
// =====================================================================================

#[test]
fn r1_basic_auth_is_scrubbed_for_all_four_encoders() {
    let c = canary();
    let v = vault_of(&[("pw", &c)]);
    // User lengths 0..=5 plus a 2-byte character: every alignment of the value.
    for user in ["", "a", "ab", "abc", "user", "admin", "\u{fc}ser"] {
        let creds = format!("{user}:{c}");
        let off = user.len() + 1;
        for (i, engine) in ENGINES.iter().enumerate() {
            let enc = engine.encode(&creds);
            for before in ["Authorization: Basic ", "authorization: basic "] {
                let out = v.scrub(&format!("{before}{enc}"));
                assert!(!leaks(&out, &c), "Basic header leaks (encoder {i})");
                assert_edges("Basic", &out, before, &enc, "", "pw", off, c.len());
            }
            // Inside a JSON header map, through the scrub_value path.
            let headers = json!({"Authorization": format!("Basic {enc}"), "Accept": "*/*"});
            let out = scrub_value(&v, &headers).to_string();
            assert!(!leaks(&out, &c), "Basic header leaks in JSON (encoder {i})");
            assert!(out.contains("@vault(pw)") && out.contains("*/*"));
        }
    }
    // The value as the user name, and as both parts.
    for creds in [format!("{c}:not-a-secret"), format!("{c}:{c}")] {
        for (i, engine) in ENGINES.iter().enumerate() {
            let enc = engine.encode(&creds);
            let out = v.scrub(&format!("Basic {enc}"));
            assert!(!leaks(&out, &c), "Basic user leaks (encoder {i})");
            assert!(out.starts_with("Basic ") && out.contains("@vault(pw)"));
        }
    }
}

#[test]
fn r1_basic_auth_is_scrubbed_by_a_file_backed_vault_after_open_and_set() {
    let c = canary();
    let dir = TempDir::new("r1file");
    let path = dir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.0.join("vault.key"));
    let mut ed = Vault::open_for_set(&path, &key).unwrap();
    ed.set("pw", &c, EntryKind::Secret).unwrap();
    ed.save().unwrap();
    let mut v = Vault::open(&path, &key).unwrap();
    let second = format!("{c}-second");
    for (name, value, user) in [("pw", c.clone(), "user"), ("pw2", second.clone(), "ab")] {
        if name == "pw2" {
            // The automaton is rebuilt by `set` on an open vault.
            v.set("pw2", &second, EntryKind::Personal).unwrap();
        }
        for (i, engine) in ENGINES.iter().enumerate() {
            let enc = engine.encode(format!("{user}:{value}"));
            let out = v.scrub(&format!("Basic {enc}"));
            assert!(
                !leaks(&out, &value),
                "file vault: Basic leaks (encoder {i})"
            );
            assert_edges(
                "file",
                &out,
                "Basic ",
                &enc,
                "",
                name,
                user.len() + 1,
                value.len(),
            );
        }
    }
}

#[test]
fn r1_basic_auth_is_scrubbed_by_the_global_scrub() {
    let c = canary();
    let v = vault_of(&[("pw", &c)]);
    v.install_global();
    for (i, engine) in ENGINES.iter().enumerate() {
        let enc = engine.encode(format!("user:{c}"));
        let out = harness_core::vault::scrub(&format!("log: sent Basic {enc}"));
        assert!(!leaks(&out, &c), "global scrub leaks Basic (encoder {i})");
        assert!(out.contains("@vault(pw)"));
    }
}

// =====================================================================================
// Row 2: JWT
// =====================================================================================

#[test]
fn r2_jwt_payload_carrying_the_value_is_scrubbed() {
    let c = canary();
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
    let sig = URL_SAFE_NO_PAD.encode([0xfb_u8, 0xff, 0x3e, 0x01, 0x80, 0x7f, 0xfe, 0x00].repeat(4));
    // The second value needs JSON escapes: only its escaped form is in the payload.
    for value in [c.clone(), format!("{c}\"\\/\u{e9}")] {
        let v = vault_of(&[("pw", &value)]);
        let quoted = serde_json::to_string(&value).unwrap();
        let inner = &quoted[1..quoted.len() - 1];
        let mut aligns = [false; 3];
        // `sub` lengths 4..=6 put the value at every alignment; last = the value is the
        // last claim, so it ends just before `"}`.
        for sub in ["user", "users", "user01"] {
            for last in [false, true] {
                let payload = if last {
                    format!(r#"{{"sub":"{sub}","iat":1700000000,"pw":{quoted}}}"#)
                } else {
                    format!(r#"{{"sub":"{sub}","pw":{quoted},"iat":1700000000}}"#)
                };
                let off = payload.find(inner).unwrap();
                aligns[off % 3] = true;
                let enc = URL_SAFE_NO_PAD.encode(&payload);
                let before = format!("Authorization: Bearer {header}.");
                let after = format!(".{sig}");
                let out = v.scrub(&format!("{before}{enc}{after}"));
                assert!(!leaks(&out, &value), "JWT leaks the value");
                assert!(
                    !b64_cores(inner.as_bytes())
                        .iter()
                        .any(|k| out.contains(k.as_str())),
                    "JWT leaks the JSON form"
                );
                assert_edges("JWT", &out, &before, &enc, &after, "pw", off, inner.len());
                // The same token as a JSON field, through scrub_value.
                let body = json!({"token": format!("{header}.{enc}.{sig}")});
                let out = scrub_value(&v, &body).to_string();
                assert!(!leaks(&out, &value), "JWT leaks via scrub_value");
                assert!(out.contains(&format!("{header}.")) && out.contains(&format!(".{sig}")));
            }
        }
        assert!(aligns == [true; 3], "not every alignment exercised");
    }
}

// =====================================================================================
// Row 3: alignments, alphabets, padding
// =====================================================================================

#[test]
fn r3_every_alignment_both_alphabets_padded_and_unpadded() {
    let c = canary();
    let prefix = [0xa5_u8, 0xff, 0x00, 0x7e, 0xfb, 0x3f];
    let suffix = [0xff_u8, 0xfe, 0x01];
    let mut alphabets_differ = false;
    let mut padded_seen = false;
    let mut aligns = [false; 3];
    // `~~~???` and `\u{ff}\u{fe}>>>` put characters 62 and 63 (where the alphabets differ)
    // into the value's own encoding.
    for value in [
        c.clone(),
        format!("{c}~~~???"),
        format!("\u{ff}\u{fe}>>>{c}"),
    ] {
        let v = vault_of(&[("c", &value)]);
        for off in 0..prefix.len() {
            aligns[off % 3] = true;
            for tail in 0..=suffix.len() {
                let mut blob = prefix[..off].to_vec();
                blob.extend_from_slice(value.as_bytes());
                blob.extend_from_slice(&suffix[..tail]);
                let encs = ENGINES.map(|e| e.encode(&blob));
                alphabets_differ |= encs[0] != encs[2];
                padded_seen |= encs[0] != encs[1];
                for (i, enc) in encs.iter().enumerate() {
                    for (before, after) in [
                        ("x ", " y"),
                        ("data:application/octet-stream;base64,", ""),
                        ("\"", "\""),
                    ] {
                        let out = v.scrub(&format!("{before}{enc}{after}"));
                        assert!(
                            !leaks(&out, &value),
                            "aligned blob leaks (off {off}, tail {tail}, encoder {i})"
                        );
                        assert_edges("aligned", &out, before, enc, after, "c", off, value.len());
                    }
                }
            }
        }
    }
    assert!(alphabets_differ, "no blob exercised the url alphabet");
    assert!(padded_seen, "no blob exercised padding");
    assert!(aligns == [true; 3]);
}

#[test]
fn r3_value_ending_the_blob_leaves_no_trailing_character_or_padding() {
    let c = canary();
    let prefix = [0xa5_u8, 0xff, 0x00, 0x7e, 0xfb, 0x3f];
    let mut padded_seen = false;
    let mut partial_seen = false;
    for value in [c.clone(), format!("{c}~"), format!("{c}~?")] {
        let v = vault_of(&[("c", &value)]);
        for off in 0..prefix.len() {
            let mut blob = prefix[..off].to_vec();
            blob.extend_from_slice(value.as_bytes());
            partial_seen |= !blob.len().is_multiple_of(3);
            for (i, e) in ENGINES.iter().enumerate() {
                let enc = e.encode(&blob);
                padded_seen |= enc.ends_with('=');
                for (before, after) in [("x ", " y"), ("data:text/plain;base64,", "")] {
                    let out = v.scrub(&format!("{before}{enc}{after}"));
                    assert!(
                        !leaks(&out, &value),
                        "blob end leaks (off {off}, encoder {i})"
                    );
                    assert_edges("end", &out, before, &enc, after, "c", off, value.len());
                    assert!(
                        out.ends_with(&format!("{}{after}", lit("c"))),
                        "a character or padding after the value survived (off {off}, encoder {i})"
                    );
                }
            }
        }
    }
    assert!(
        padded_seen && partial_seen,
        "no blob exercised a partial last group"
    );
}

// =====================================================================================
// Row 4: the 8-character floor
// =====================================================================================

/// Blobs holding `value` at byte offsets 0..6 with tails of 0..=2 bytes, in all 4 encoders.
fn blobs(value: &str, suffix: &[u8]) -> Vec<(usize, usize, String)> {
    let prefix = [0xa5_u8, 0xff, 0x00, 0x7e, 0xfb, 0x3f];
    let mut out = Vec::new();
    for off in 0..prefix.len() {
        for tail in 0..=suffix.len() {
            let mut blob = prefix[..off].to_vec();
            blob.extend_from_slice(value.as_bytes());
            blob.extend_from_slice(&suffix[..tail]);
            for e in ENGINES {
                out.push((off, tail, e.encode(&blob)));
            }
        }
    }
    out
}

#[test]
fn r4_base64_of_values_under_the_floor_is_never_touched() {
    // 4 and 5 characters: no alignment yields 8 base64 characters of value bits.
    for value in ["q7Zk", "Q7zk9", "w#9!", "z0~?>"] {
        let v = vault_of(&[("s", value)]);
        for (off, tail, enc) in blobs(value, &[0xff, 0x00]) {
            let text = format!("x {enc} y");
            assert!(
                !text
                    .to_ascii_lowercase()
                    .contains(&value.to_ascii_lowercase()),
                "fixture holds the value raw"
            );
            assert!(
                v.scrub(&text) == text,
                "base64 of a {}-character value was scrubbed (off {off}, tail {tail})",
                value.len()
            );
        }
    }
}

#[test]
fn r4_a_seven_character_core_is_not_a_pattern_and_eight_is() {
    // 6 bytes: alignment 0 has an 8-character core; alignments 1 and 2 have 7-character
    // cores (only their 8-character tails, the value ending the blob, are patterns).
    let value = "k3Rv9Q";
    let v = vault_of(&[("s", value)]);
    let mid_tail = [0xff_u8, 0xff];
    for (pre, scrubbed_mid) in [
        (&[][..], true),
        (&[0xa5_u8][..], false),
        (&[0xa5_u8, 0x5a][..], false),
        (&[0x11_u8, 0x22, 0x33][..], true),
        (&[0x11_u8, 0x22, 0x33, 0xa5][..], false),
    ] {
        let off = pre.len();
        // Mid-blob: the character after the core carries 0xff bits.
        let mut blob = pre.to_vec();
        blob.extend_from_slice(value.as_bytes());
        blob.extend_from_slice(&mid_tail);
        for (i, e) in ENGINES.iter().enumerate() {
            let enc = e.encode(&blob);
            let text = format!("x {enc} y");
            let out = v.scrub(&text);
            if scrubbed_mid {
                assert!(
                    !leaks(&out, value),
                    "8-character core not scrubbed (off {off}, encoder {i})"
                );
                assert_edges("core8", &out, "x ", &enc, " y", "s", off, value.len());
            } else {
                assert!(
                    out == text,
                    "a 7-character core was scrubbed (off {off}, encoder {i})"
                );
            }
        }
        // Ending the blob: every alignment has an 8-character tail, so it is scrubbed.
        let mut end = pre.to_vec();
        end.extend_from_slice(value.as_bytes());
        for (i, e) in ENGINES.iter().enumerate() {
            let enc = e.encode(&end);
            let out = v.scrub(&format!("x {enc} y"));
            assert!(
                !leaks(&out, value),
                "value ending the blob not scrubbed (off {off}, encoder {i})"
            );
            assert_edges("tail8", &out, "x ", &enc, " y", "s", off, value.len());
        }
    }
}

#[test]
fn r4_every_base64_redaction_removes_at_least_eight_characters() {
    for value in [
        "q7Zk",
        "Q7zk9",
        "k3Rv9Q",
        "m4Xp2Lw",
        "a1b2c3d4",
        "zz00YY11x",
    ] {
        let v = vault_of(&[("s", value)]);
        for (off, tail, enc) in blobs(value, &[0xff, 0x00]) {
            let text = format!("x {enc} y");
            let out = v.scrub(&text);
            if out == text {
                continue;
            }
            assert!(out.starts_with("x ") && out.ends_with(" y"));
            let mid = &out[2..out.len() - 2];
            let l = lit("s");
            let i = mid
                .find(l.as_str())
                .expect("a changed blob holds the literal");
            let (left, right) = (&mid[..i], &mid[i + l.len()..]);
            assert!(enc.starts_with(left) && enc.ends_with(right));
            let removed = &enc[left.len()..enc.len() - right.len()];
            assert!(
                removed.trim_end_matches('=').len() >= 8,
                "a redaction of {} base64 characters (len {}, off {off}, tail {tail})",
                removed.trim_end_matches('=').len(),
                value.len()
            );
        }
    }
}

// =====================================================================================
// Row 5: D-118 ValueInLiteral over the base64 spellings
// =====================================================================================

/// The base64url cores of `v` at alignments 0, 1, 2 that are at least 8 characters (all
/// valid entry names: `[A-Za-z0-9_-]`), plus one with its ASCII case flipped.
fn b64url_names(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    for k in 0..3 {
        let mut blob = vec![0u8; k];
        blob.extend_from_slice(v.as_bytes());
        let lead = (8 * k).div_ceil(6);
        let end = 8 * (k + v.len()) / 6;
        let enc = URL_SAFE_NO_PAD.encode(&blob);
        if end >= lead + 8 {
            out.push(enc[lead..end].to_owned());
        }
    }
    let flipped: String = out[0]
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    out.push(flipped);
    out
}

/// Asserts `r` is `ValueInLiteral` naming `want`; prints names only, never a value.
fn refused_for(r: Result<(), VaultError>, want: &str, what: &str) {
    match r {
        Err(VaultError::ValueInLiteral { name }) => {
            assert!(name == want, "{what}: ValueInLiteral named the wrong entry")
        }
        Err(_) => panic!("{what}: refused with another error than ValueInLiteral"),
        Ok(()) => panic!("{what}: not refused"),
    }
}

#[test]
fn r5_value_in_literal_covers_base64_spellings() {
    let c = canary();
    let names = b64url_names(&c);
    assert!(names.len() >= 4, "every alignment yields a name");
    for (i, n) in names.iter().enumerate() {
        // The literal is already there: setting the value is refused, naming the value.
        let mut v = vault_of(&[(n.as_str(), "unrelated-entry-value")]);
        refused_for(
            v.set("pw", &c, EntryKind::Secret),
            "pw",
            &format!("value, name {i}"),
        );
        assert!(
            v.names().iter().all(|(k, _)| k != "pw"),
            "a refused set kept the entry"
        );
        // The value is already there: the name whose literal hides it is refused, naming
        // the value's entry.
        let mut v = vault_of(&[("pw", &c)]);
        refused_for(
            v.set(n, "unrelated-entry-value", EntryKind::Secret),
            "pw",
            &format!("name {i}"),
        );
        assert!(v.names().len() == 1, "a refused set added the entry");
    }
    // Under the floor: base64 of 4 and 5 character values is under 8 characters, so a
    // name equal to it hides nothing scrub would match, and both orders are accepted.
    for value in ["q7Zk", "Q7zk9"] {
        let n = URL_SAFE_NO_PAD.encode(value);
        assert!(n.len() < 8);
        let mut v = vault_of(&[(n.as_str(), "unrelated-entry-value")]);
        assert!(
            v.set("s", value, EntryKind::Secret).is_ok(),
            "a {}-character value refused for its sub-floor base64",
            value.len()
        );
        let mut v = vault_of(&[("s", value)]);
        assert!(
            v.set(&n, "unrelated-entry-value", EntryKind::Secret)
                .is_ok(),
            "a name equal to the sub-floor base64 of a value refused"
        );
    }
}

#[test]
fn r5_vault_editor_refuses_base64_in_literal_and_leaves_the_file() {
    let c = canary();
    let n = b64url_names(&c).remove(1);
    let dir = TempDir::new("r5file");
    let path = dir.0.join("vault.toml.age");
    let key = KeySource::IdentityFile(dir.0.join("vault.key"));
    let mut ed = Vault::open_for_set(&path, &key).unwrap();
    ed.set(&n, "unrelated-entry-value", EntryKind::Secret)
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    refused_for(ed.set("pw", &c, EntryKind::Secret), "pw", "editor value");
    assert!(
        std::fs::read(&path).unwrap() == before,
        "a refused set rewrote the file"
    );
    ed.save().unwrap();
    let v = Vault::open(&path, &key).unwrap();
    assert!(
        v.names().iter().all(|(k, _)| k != "pw"),
        "the refused entry is on disk"
    );
    // The other order, on a fresh file holding the value.
    let path2 = dir.0.join("vault2.toml.age");
    let mut ed = Vault::open_for_set(&path2, &key).unwrap();
    ed.set("pw", &c, EntryKind::Secret).unwrap();
    let before = std::fs::read(&path2).unwrap();
    refused_for(
        ed.set(&n, "unrelated-entry-value", EntryKind::Secret),
        "pw",
        "editor name",
    );
    assert!(
        std::fs::read(&path2).unwrap() == before,
        "a refused set rewrote the file"
    );
}

// =====================================================================================
// Row 6: end to end through the MCP wrapper
// =====================================================================================

/// What the echo tools do with the resolved value.
#[derive(Clone, Copy)]
enum Echo {
    /// Returns the encodings in text, summary and consequences.
    Ok,
    /// Fails with the encodings in the error message.
    Fail,
    /// Returns them with a stop whose why and element carry them.
    Stop,
}

struct EchoB64(Echo);

/// Every encoded form of `value` a buggy tool could echo: Basic in 4 encoders, a JWT
/// with the value in its payload, a data URL.
fn encoded_forms(value: &str) -> Vec<String> {
    let mut out: Vec<String> = ENGINES
        .iter()
        .map(|e| format!("Basic {}", e.encode(format!("user:{value}"))))
        .collect();
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(json!({"sub": "user", "pw": value}).to_string());
    out.push(format!("Bearer {header}.{payload}.c2lnbmF0dXJl"));
    out.push(format!("data:text/plain;base64,{}", STANDARD.encode(value)));
    out
}

#[async_trait]
impl ToolHandler for EchoB64 {
    async fn classify(&self, _ctx: &ToolCtx, _args: &Value) -> Result<Classified, ToolError> {
        Ok(Classified::default())
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let secret = args
            .get("secret")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Args("secret is required".into()))?;
        // The sink builds the headers from the resolved value: a derived payload, which a
        // buggy tool then echoes back.
        let forms = ctx.resolve_into(secret, |r| encoded_forms(r.expose()))?;
        let text = forms.join("\n");
        match self.0 {
            Echo::Fail => Err(ToolError::Failed(format!("upstream rejected {}", forms[1]))),
            Echo::Ok | Echo::Stop => Ok(ToolOutput {
                text,
                summary: Some(format!("sent {}", forms[0])),
                consequences: json!({ "headers": forms, "auth": { forms[2].clone(): forms[3] } }),
                files: Vec::new(),
                stopped: matches!(self.0, Echo::Stop).then(|| StopReason {
                    class: Class::Read,
                    why: format!("challenge after {}", forms[4]),
                    element: Some(forms[5].clone()),
                }),
            }),
        }
    }
}

fn tools() -> Vec<ToolSpec> {
    [
        ("echo.b64", Echo::Ok),
        ("echo.b64fail", Echo::Fail),
        ("echo.b64stop", Echo::Stop),
    ]
    .into_iter()
    .map(|(name, mode)| ToolSpec {
        name: name.into(),
        description: format!("{name}: echoes base64 of a vault value (acceptance T-05d)"),
        class: Class::Read,
        args: json!({"type": "object", "properties": {"secret": {"type": "string"}}}),
        handler: Arc::new(EchoB64(mode)),
    })
    .collect()
}

/// The arguments a client sends: the vault literal, plus encoded copies of the value
/// (as if the model had read one somewhere), as values and as an object key.
fn client_args(c: &str) -> Value {
    let forms = encoded_forms(c);
    json!({
        "secret": "user:@vault(test)",
        "seen": forms,
        "nested": { forms[3].clone(): [forms[4].clone()] },
    })
}

async fn harness_in(root: &Path) -> (Harness, RunFolder, Arc<MemStore>) {
    let run = RunFolder::create(root, "t05d", "acceptance t05d").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "acceptance t05d".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    let harness = Harness {
        store: store.clone(),
        vault: Arc::new(Vault::in_memory([(
            "test".to_owned(),
            canary(),
            EntryKind::Secret,
        )])),
        asker: Arc::new(ScriptedAsker::new()),
        runs_root: root.to_path_buf(),
        run: Some(run.id.clone()),
    };
    (harness, run, store)
}

#[tokio::test]
async fn r6_dispatch_leaves_no_encoded_value_in_output_ledger_jsonl_or_runs() {
    let c = canary();
    let root = TempDir::new("r6dispatch");
    let (harness, run, store) = harness_in(&root.0).await;
    let reg = ToolRegistry::new(tools());
    let mut returned = Vec::new();
    for name in ["echo.b64", "echo.b64fail", "echo.b64stop"] {
        let r = tokio::time::timeout(CAP, dispatch("echo", &reg, &harness, name, client_args(&c)))
            .await
            .expect("dispatch within the cap");
        match r {
            Ok(out) => {
                assert!(name != "echo.b64fail", "the failing tool succeeded");
                returned.push(serde_json::to_string(&out).unwrap());
            }
            Err(e) => {
                assert!(name == "echo.b64fail", "{name} failed");
                returned.push(format!("{e} | {e:?}"));
            }
        }
    }
    // The tools really ran and resolved the value: the literal is in what they returned.
    for (i, r) in returned.iter().enumerate() {
        assert!(!leaks(r, &c), "returned value {i} leaks");
        assert!(
            r.contains("@vault(test)"),
            "returned value {i} has no literal"
        );
    }
    let table = store.ledger();
    assert!(table.len() >= 4, "too few ledger events: {}", table.len());
    let table_text =
        serde_json::to_string(&table.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>()).unwrap();
    assert!(!leaks(&table_text, &c), "the ledger table leaks");
    assert!(table_text.contains("@vault(test)"));
    let jsonl = std::fs::read_to_string(run.ledger_jsonl()).unwrap();
    assert!(!leaks(&jsonl, &c), "ledger.jsonl leaks");
    assert!(jsonl.contains("@vault(test)"));
    assert!(
        jsonl.lines().count() == table.len(),
        "jsonl mirror is not one line per event"
    );
    let mut files = Vec::new();
    all_files(&root.0, &mut files);
    assert!(!files.is_empty());
    for (p, text) in &files {
        assert!(
            !leaks(text, &c),
            "a file under runs/ leaks: {}",
            p.file_name().unwrap().to_string_lossy()
        );
    }
}

const CHILD: &str = "SB_T05D_SERVE_CHILD";
const CHILD_ROOT: &str = "SB_T05D_SERVE_ROOT";

/// Child of `r6_serve_stdout_stderr_ledger_and_runs_hold_no_encoded_value`: serves the
/// echo tools on the real stdin and stdout, then writes the ledger table into the runs
/// root for the parent to grep.
#[tokio::test]
#[ignore = "child process of r6_serve_stdout_stderr_ledger_and_runs_hold_no_encoded_value"]
async fn r6_serve_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else {
        return;
    };
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let root = PathBuf::from(root);
    let (harness, _run, store) = harness_in(&root).await;
    serve("echo", tools(), harness).await.unwrap();
    let table: Vec<_> = store.ledger().into_iter().map(|(_, e)| e).collect();
    std::fs::write(
        root.join("ledger_table.json"),
        serde_json::to_string(&table).unwrap(),
    )
    .unwrap();
}

#[test]
fn r6_serve_stdout_stderr_ledger_and_runs_hold_no_encoded_value() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let c = canary();
    let root = TempDir::new("r6serve");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "r6_serve_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env(CHILD_ROOT, &root.0)
        .env("SB_VAULT_CANARY", &c)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        let mut send = |v: Value| {
            let mut s = v.to_string();
            s.push('\n');
            stdin.write_all(s.as_bytes()).unwrap();
        };
        send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}));
        send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        for (id, name) in [(3, "echo.b64"), (4, "echo.b64fail"), (5, "echo.b64stop")] {
            send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": name, "arguments": client_args(&c)}}));
        }
        // stdin closes here: serve answers what is in flight and returns.
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let out = rx
        .recv_timeout(CAP)
        .expect("the serve child exits within the cap")
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!leaks(&stdout, &c), "child stdout leaks");
    assert!(!leaks(&stderr, &c), "child stderr leaks");
    assert!(out.status.success(), "child failed ({:?})", out.status);
    assert!(stdout.contains("1 passed"), "the child test ran");
    // Each call was answered over stdout with the scrubbed echo.
    let replies: Vec<Value> = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("jsonrpc").is_some())
        .collect();
    for id in [3, 4, 5] {
        let r = replies
            .iter()
            .find(|v| v["id"] == json!(id))
            .expect("a reply for every call");
        let text = r["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(text.contains("@vault(test)"), "reply {id} has no literal");
    }
    let mut files = Vec::new();
    all_files(&root.0, &mut files);
    let table = files
        .iter()
        .find(|(p, _)| p.file_name().unwrap() == "ledger_table.json")
        .expect("the child wrote the ledger table");
    assert!(
        table.1.contains("@vault(test)"),
        "ledger table has no event"
    );
    assert!(
        files
            .iter()
            .any(|(p, t)| p.file_name().unwrap() == "ledger.jsonl" && t.contains("@vault(test)")),
        "no ledger.jsonl with an event"
    );
    for (p, text) in &files {
        assert!(
            !leaks(text, &c),
            "a file under the runs root leaks: {}",
            p.file_name().unwrap().to_string_lossy()
        );
    }
}
