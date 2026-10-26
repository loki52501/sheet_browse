//! T-04 acceptance, round 4: D-126 (rule_domain maps with UTS-46 through the idna crate)
//! and D-125 (MemStore's trigram cut-off is 0.5). Written by the tester, independent of the
//! coder's gate.rs unit tests.
//!
//! The oracle for "the key a browser host carries" is the WHATWG URL host parser as the
//! `url` crate (2.5.8, src/host.rs `Host::parse`) runs it for a special scheme: percent-
//! decode, `idna::domain_to_ascii_from_cow(.., AsciiDenyList::URL)`, refuse an empty
//! result, and serialise a host that ends in a number as IPv4. harness-core has no `url`
//! dependency, so [`url_host`] repeats those four steps with the same idna call. rule_domain
//! strips one trailing dot (a browser keeps it) and the gate canonicalises call.domain with
//! rule_domain too, so keys are compared against the oracle minus one trailing dot.
//!
//! `cargo test -p harness-core --test gate_accept_t04_r4`

use std::borrow::Cow;
use std::sync::Arc;

use harness_core::class::{Class, Mode};
use harness_core::gate::{rule_domain, Call, Decision, Gate};
use harness_core::memory::{Fact, GateRule, MemoryError, MemoryStore, Source, SourceKind, Why};
use harness_core::testing::MemStore;
use serde_json::json;

const STOP: GateRule = GateRule {
    mode: Mode::Stop,
    strict: false,
};

/// WHATWG host parsing for a special scheme (what a browser's URL host carries), as url
/// 2.5.8 `Host::parse` does it. `None` where the URL parser fails.
fn url_host(input: &str) -> Option<String> {
    if input.starts_with('[') {
        return None;
    }
    let decoded: Cow<'_, [u8]> = percent_encoding::percent_decode_str(input).into();
    let domain = idna::domain_to_ascii_from_cow(decoded, idna::AsciiDenyList::URL).ok()?;
    if domain.is_empty() {
        return None;
    }
    if ends_in_a_number(&domain) {
        return parse_ipv4(&domain).map(|a| {
            format!(
                "{}.{}.{}.{}",
                a >> 24,
                (a >> 16) & 0xff,
                (a >> 8) & 0xff,
                a & 0xff
            )
        });
    }
    Some(domain.into_owned())
}

fn ends_in_a_number(d: &str) -> bool {
    let mut parts: Vec<&str> = d.split('.').collect();
    if parts.last() == Some(&"") {
        if parts.len() == 1 {
            return false;
        }
        parts.pop();
    }
    let last = parts.last().copied().unwrap_or("");
    (!last.is_empty() && last.bytes().all(|b| b.is_ascii_digit())) || ipv4_number(last).is_some()
}

fn ipv4_number(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let (digits, radix) = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (h, 16)
    } else if s.len() > 1 && s.starts_with('0') {
        (&s[1..], 8)
    } else {
        (s, 10)
    };
    if digits.is_empty() {
        return Some(0);
    }
    if !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    // Saturate: anything over u32 fails the range check in parse_ipv4 anyway.
    Some(u64::from_str_radix(digits, radix).unwrap_or(u64::MAX))
}

fn parse_ipv4(d: &str) -> Option<u32> {
    let mut parts: Vec<&str> = d.split('.').collect();
    if parts.last() == Some(&"") && parts.len() > 1 {
        parts.pop();
    }
    if parts.len() > 4 {
        return None;
    }
    let nums: Vec<u64> = parts
        .iter()
        .map(|p| ipv4_number(p))
        .collect::<Option<_>>()?;
    let (last, head) = nums.split_last()?;
    if head.iter().any(|&n| n > 255) || *last >= 256u64.pow(5 - nums.len() as u32) {
        return None;
    }
    let mut a = *last;
    for (i, n) in head.iter().enumerate() {
        a += n * 256u64.pow(3 - i as u32);
    }
    Some(a as u32)
}

fn strip_dot(h: &str) -> &str {
    h.strip_suffix('.').unwrap_or(h)
}

fn esc(s: &str) -> String {
    s.escape_unicode().to_string()
}

/// Every entry b829add removed from the tester's refusal lists in gate_accept_t04_r3.rs
/// (r3_rule_domain_refuses_what_it_cannot_map_safely and
/// r3_gate_rule_set_stores_rule_key_and_gate_matches_idn_hosts) and in core-pg
/// accept_t03_r3.rs (r3_d113_d120_pg_gate_rule_set_stores_rule_key), with the A-label
/// `idna::domain_to_ascii` gives. core-pg's accept_t03_r4.rs holds the same table.
const REMOVED_REFUSALS: [(&str, &str); 10] = [
    ("bu\u{0308}cher.de", "xn--bcher-kva.de"), // decomposed: UTS-46 normalises to NFC
    ("ban\u{200b}k.com", "bank.com"),          // zero-width space: ignored
    ("ban\u{00ad}k.com", "bank.com"),          // soft hyphen: ignored
    ("ban\u{2060}k.com", "bank.com"),          // word joiner: ignored
    ("bank\u{2800}.com", "xn--bank-yj5b.com"), // braille blank: valid
    ("\u{fb01}le.com", "file.com"),            // ligature fi
    ("x\u{00b2}.com", "x2.com"),               // superscript two
    ("\u{1d41b}ank.com", "bank.com"),          // mathematical bold b
    ("\u{24d1}ank.com", "bank.com"),           // circled b
    ("\u{ff8a}\u{ff9f}.jp", "xn--odk.jp"),     // half-width katakana ha + semi-voiced mark
];

/// Inputs idna maps (D-126 and its why: long s, ij, modifier letters, final sigma, sharp s,
/// compatibility ideographs, jamo) plus full-width forms and IDNA dots.
const MAPPED: [&str; 16] = [
    "\u{17f}hop.example",
    "\u{133}ssel.nl",
    "b\u{1d2c}nk.example",
    "example.\u{391}\u{3a3}",
    "\u{392}\u{391}\u{3a3}.gr",
    "stra\u{1e9e}e.de",
    "\u{f900}.kr",
    "\u{3131}.kr",
    "BÜCHER.DE.",
    "\u{ff22}\u{ff21}\u{ff2e}\u{ff2b}\u{ff0e}\u{ff23}\u{ff2f}\u{ff2d}",
    "bank\u{3002}com",
    "bank\u{ff61}com",
    "\u{212a}elvin.com",
    "MÜNCHEN.de",
    "例え.テスト",
    "Pročprostěnemluvíčesky.cz",
];

fn all_mapped() -> impl Iterator<Item = &'static str> {
    REMOVED_REFUSALS.iter().map(|(i, _)| *i).chain(MAPPED)
}

#[test]
fn r4_removed_refusals_are_what_idna_maps() {
    let mut still_refused = Vec::new();
    for (input, want) in REMOVED_REFUSALS {
        let idna = idna::domain_to_ascii(input);
        let url = url_host(input);
        println!(
            "r4 removed {}: idna {:?}, url host {:?}, rule_domain {:?}",
            esc(input),
            idna,
            url,
            rule_domain(input)
        );
        match idna {
            Ok(a) => {
                assert_eq!(a, want, "{}: idna::domain_to_ascii", esc(input));
                assert_eq!(url.as_deref(), Some(want), "{}: URL host", esc(input));
                assert_eq!(rule_domain(input).unwrap(), want, "{}", esc(input));
            }
            Err(_) => still_refused.push(esc(input)),
        }
    }
    assert!(
        still_refused.is_empty(),
        "idna still refuses {still_refused:?}: restore them to the refusal lists"
    );
}

#[test]
fn r4_d126_mapped_inputs_store_the_key_a_browser_host_carries() {
    for input in all_mapped() {
        let host = url_host(input).unwrap_or_else(|| panic!("{}: URL host fails", esc(input)));
        let got = rule_domain(input).unwrap_or_else(|e| panic!("{}: {e}", esc(input)));
        println!("r4 d126 {} -> {got} (URL host {host})", esc(input));
        assert_eq!(got, strip_dot(&host), "{}", esc(input));
        // Idempotent: a stored key canonicalises to itself, and so does the host as a
        // browser reports it (trailing dot included).
        assert_eq!(rule_domain(&got).unwrap(), got);
        assert_eq!(rule_domain(&host).unwrap(), got);
    }
}

fn submit_on(domain: &str) -> Call {
    Call {
        run_id: None,
        server: "browser".into(),
        tool: "browser.submit".into(),
        class: Class::Submit,
        domain: Some(domain.to_string()),
        recipients: Vec::new(),
        hints: Vec::new(),
        args: json!({}),
        what: None,
        values: Vec::new(),
        strict: false,
    }
}

#[tokio::test]
async fn r4_d126_rule_written_in_any_spelling_stops_the_browser_host() {
    for mk in [MemStore::new, MemStore::empty] {
        for input in all_mapped() {
            let host = url_host(input).unwrap();
            let s = Arc::new(mk());
            let g = Gate::new(s.clone());
            s.gate_rule_set(input, "Submit", STOP, "").await.unwrap();
            assert_eq!(
                s.gate_rule(strip_dot(&host), "Submit").await.unwrap(),
                Some(STOP),
                "{}: lookup by the host key",
                esc(input)
            );
            assert!(
                matches!(g.check(&submit_on(&host)).await.unwrap(), Decision::Stop(_)),
                "{}: a Submit on the browser host {host} must stop",
                esc(input)
            );
            assert_eq!(
                g.check(&submit_on("other.example")).await.unwrap(),
                Decision::Allow
            );
        }
    }
}

/// Every scalar value placed inside a label: whenever rule_domain accepts, its key is the
/// URL host (minus one trailing dot). A refusal where the URL parser accepts is allowed
/// (the rule cannot be written, so it cannot be dead) and only counted.
#[test]
fn r4_d126_sweep_every_code_point_accepted_means_browser_host() {
    let mut wrong = Vec::new();
    let (mut accepted, mut stricter) = (0u32, Vec::new());
    for c in (0u32..=0x10ffff).filter_map(char::from_u32) {
        let input = format!("a{c}b.example");
        match rule_domain(&input) {
            Ok(got) => {
                accepted += 1;
                match url_host(&input) {
                    Some(h) if strip_dot(&h) == got => {}
                    other => wrong.push(format!("U+{:04X}: {got:?} vs URL {other:?}", c as u32)),
                }
            }
            Err(MemoryError::Invalid(_)) => {
                if url_host(&input).is_some() {
                    stricter.push(format!("U+{:04X}", c as u32));
                }
            }
            Err(e) => panic!("U+{:04X}: {e:?}", c as u32),
        }
    }
    println!(
        "r4 sweep: {accepted} accepted, {} refused though a URL host exists (first: {:?}), {} mismatches {:?}",
        stricter.len(),
        &stricter[..stricter.len().min(16)],
        wrong.len(),
        &wrong[..wrong.len().min(16)]
    );
    assert!(wrong.is_empty(), "keys no browser host carries: {wrong:?}");
}

/// What idna refuses stays refused (D-126) and nothing is stored.
#[tokio::test]
async fn r4_d126_what_idna_refuses_is_refused_and_not_stored() {
    let refused = [
        "ban\u{200d}k.com",  // ZWJ, CheckJoiners
        "bank\u{202e}.com",  // bidi override, disallowed
        "xn--zz.com",        // bad Punycode
        "bank\u{e0041}.com", // tag character
        "bank%2ecom",        // '%' is on the URL deny list (rule_domain does not decode)
        "bank.com:443",
        "user@bank.com",
        "bank.com/x",
        "a b.com",
        "",
    ];
    for d in refused {
        assert!(
            matches!(rule_domain(d), Err(MemoryError::Invalid(_))),
            "rule_domain({}) should be refused",
            esc(d)
        );
        let s = MemStore::empty();
        assert!(matches!(
            s.gate_rule_set(d, "Send", STOP, "").await,
            Err(MemoryError::Invalid(_))
        ));
        assert_eq!(s.gate_rule(d, "Send").await.unwrap_or(None), None);
    }
    // Where idna itself refuses, the URL parser refuses too: no browser host carries it.
    for d in [
        "ban\u{200d}k.com",
        "bank\u{202e}.com",
        "xn--zz.com",
        "bank\u{e0041}.com",
    ] {
        assert!(idna::domain_to_ascii(d).is_err(), "{}", esc(d));
        assert_eq!(url_host(d), None, "{}", esc(d));
    }
}

/// A host ending in a number is IPv4 to a browser, which serialises it as dotted decimal
/// (and refuses one that is not a valid IPv4 address). A rule written as `127.1` or
/// `0x7f.0.0.1` must store `127.0.0.1` (the key the browser host carries) or be refused;
/// any other stored key never matches (D-110 (1), D-126 "every stored key is what a
/// browser's URL host carries").
#[tokio::test]
#[ignore = "red-first T-04 round 5: rule_domain stores numeric hosts as typed (127.1, 0x7f.0.0.1, 0177.0.0.1, 2130706433, 192.168.0.010, 1.2.3.256), keys no browser host carries (D-126)"]
async fn r4_d126_numeric_hosts_store_what_the_browser_carries() {
    let mut wrong = Vec::new();
    for input in [
        "127.0.0.1",
        "192.168.0.8",
        "127.1",
        "0x7f.0.0.1",
        "0177.0.0.1",
        "2130706433",
        "192.168.0.010",
        "1.2.3.256",
        "1.2.3.4.5",
    ] {
        let host = url_host(input);
        match (rule_domain(input), &host) {
            (Err(MemoryError::Invalid(_)), _) => {}
            (Ok(k), Some(h)) if &k == h => {}
            other => wrong.push(format!("{input:?} -> {other:?}")),
        }
    }
    // Dotted-decimal hosts are what browsers carry and must stay writable.
    assert_eq!(rule_domain("127.0.0.1").unwrap(), "127.0.0.1");
    assert_eq!(rule_domain("192.168.0.8").unwrap(), "192.168.0.8");
    println!("r4 numeric hosts: {wrong:#?}");
    assert!(wrong.is_empty(), "dead numeric-host rules: {wrong:?}");
}

// ------------------------------------------------------------------ D-125 MemStore cut-off

fn src() -> Source {
    Source {
        kind: SourceKind::User,
        reference: "t04-r4".into(),
        run_id: None,
    }
}

/// `select word_similarity(q, 'Vasanth Raghavan')` on the compose Postgres 16 (pg_trgm),
/// as printed by core-pg accept_t03_r4 r4_d125_server_cutoff_is_0_5 in this round.
const SERVER_SCORES: [(&str, f32); 6] = [
    ("Vasnth", 0.5),
    ("Vasanh", 0.714_285_73),
    ("Raghvn", 0.571_428_6),
    ("Vsnth", 0.333_333_34),
    ("Vasnath", 0.375),
    ("Rghavan", 0.625),
];

/// D-125: MemStore's trigram tier admits word_similarity >= 0.5 and nothing under it, with
/// the server's scores.
#[tokio::test]
async fn r4_d125_memstore_cutoff_is_0_5() {
    let s = MemStore::empty();
    s.remember(Fact::new(
        "Vasanth Raghavan",
        "role",
        "staff engineer",
        src(),
    ))
    .await
    .unwrap();
    s.remember(Fact::new("reading list", "item", "rust book", src()))
        .await
        .unwrap();
    for (q, server) in SERVER_SCORES {
        let hits = s.search(q, 5).await.unwrap();
        let hit = hits.iter().find(|h| h.fact.subject == "Vasanth Raghavan");
        println!(
            "r4 mem {q:?}: server {server}, got {:?}",
            hit.map(|h| (h.score, h.why))
        );
        if server >= 0.5 {
            let h = hit.unwrap_or_else(|| panic!("{q:?} ({server}) must hit"));
            assert_eq!(h.why, Why::Trigram);
            assert!((h.score - server).abs() < 1e-6, "{q:?}: {}", h.score);
        } else {
            assert!(
                hit.is_none(),
                "{q:?} ({server}) is under 0.5 and must not hit"
            );
        }
    }
}
