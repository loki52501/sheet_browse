//! T-04 acceptance, round 3: the D-120 / B-190 / B-191 rows (docs/01 H-1.1..H-1.4, I-3) and
//! the MemStore parity items of bae6c45 (D-113, D-115).
//!
//! Every gate row goes through `Gate::check` with `harness_core::testing::MemStore`; rules
//! are written through the `MemoryStore::gate_rule_set` trait method, timeouts are played
//! out through `ScriptedAsker` with no scripted answer. The `testing` feature comes from the
//! self dev-dependency (D-111): `cargo test -p harness-core --test gate_accept_t04_r3`.

use std::sync::Arc;

use harness_core::class::{Class, Mode};
use harness_core::gate::{
    clean_card, rule_class_key, rule_domain, rule_hint, rule_key, AskCard, AskOutcome, Asker, Call,
    Decision, Gate, OnTimeout,
};
use harness_core::memory::{Fact, GateRule, MemoryError, MemoryStore, Source, SourceKind, Why};
use harness_core::testing::{MemStore, ScriptedAsker};
use serde_json::json;

const ASK: GateRule = GateRule {
    mode: Mode::Ask,
    strict: false,
};
const ALLOW: GateRule = GateRule {
    mode: Mode::Allow,
    strict: false,
};
const STOP: GateRule = GateRule {
    mode: Mode::Stop,
    strict: false,
};

fn call(class: Class, domain: &str) -> Call {
    Call {
        run_id: None,
        server: "echo".into(),
        tool: "echo.act".into(),
        class,
        domain: Some(domain.to_string()),
        recipients: Vec::new(),
        hints: Vec::new(),
        args: json!({}),
        what: None,
        values: Vec::new(),
        strict: false,
    }
}

fn send(to: &[&str]) -> Call {
    let mut c = call(Class::Send, "mail.example");
    c.tool = "echo.send".into();
    c.recipients = to.iter().map(|s| s.to_string()).collect();
    c.args = json!({"body": "hello"});
    c
}

fn gate(store: MemStore) -> (Arc<MemStore>, Gate) {
    let store = Arc::new(store);
    (store.clone(), Gate::new(store))
}

async fn check(g: &Gate, c: &Call) -> Decision {
    g.check(c).await.expect("gate check")
}

fn card(d: Decision) -> AskCard {
    match d {
        Decision::Ask(c) => c,
        other => panic!("expected Ask, got {other:?}"),
    }
}

async fn timeout(c: &AskCard) -> AskOutcome {
    ScriptedAsker::new().ask(c).await
}

fn stores() -> [fn() -> MemStore; 2] {
    [MemStore::new, MemStore::empty]
}

/// No control, escape, bidi, zero-width, soft hyphen or braille-blank character survives.
fn assert_clean(s: &str, what: &str) {
    for c in s.chars() {
        let bad = c.is_control()
            || matches!(
                c,
                '\u{200b}'..='\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}'
                    | '\u{2800}'
                    | '\u{feff}'
                    | '\u{00ad}'
            );
        assert!(!bad, "{what}: {c:?} left in {s:?}");
    }
}

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "sbcanary-t04r3-default".to_string())
}

// ------------------------------------------------------------------ D-120 (1) rule_domain

#[test]
fn r3_rule_domain_maps_to_the_a_label() {
    let ok = [
        (" Bank.COM. ", "bank.com"),
        ("BÜCHER.DE", "xn--bcher-kva.de"),
        ("bücher.de", "xn--bcher-kva.de"),
        ("München.DE", "xn--mnchen-3ya.de"),
        ("例え.テスト", "xn--r8jz45g.xn--zckzah"),
        // RFC 3492 7.1 samples (L), (M), (O), (D), with the basic code points lowercased.
        ("3年B組金八先生.jp", "xn--3b-ww4c5e180e575a65lsy2b.jp"),
        (
            "安室奈美恵-with-SUPER-MONKEYS.jp",
            "xn---with-super-monkeys-pc58ag80a8qai00g7n9n.jp",
        ),
        ("ひとつ屋根の下2.jp", "xn--2-u9tlzr9756bt3uc0v.jp"),
        (
            "Pročprostěnemluvíčesky.cz",
            "xn--proprostnemluvesky-uyb24dma41a.cz",
        ),
        // UTS-46 full stops and full-width ASCII.
        ("bank\u{3002}com", "bank.com"),
        ("bank\u{ff0e}com", "bank.com"),
        ("bank\u{ff61}com", "bank.com"),
        (
            "\u{ff22}\u{ff21}\u{ff2e}\u{ff2b}.\u{ff23}\u{ff2f}\u{ff2d}",
            "bank.com",
        ),
        ("bücher\u{3002}DE\u{3002}", "xn--bcher-kva.de"),
        ("*", "*"),
        (" * ", "*"),
        ("my_host-1.example", "my_host-1.example"),
    ];
    for (input, want) in ok {
        assert_eq!(
            rule_domain(input).unwrap_or_else(|e| panic!("{input:?}: {e}")),
            want,
            "{input:?}"
        );
    }
    // Idempotent on its own output: a stored key canonicalises to itself.
    for (_, want) in ok {
        assert_eq!(rule_domain(want).unwrap(), want);
    }
}

#[test]
fn r3_rule_domain_refuses_what_it_cannot_map_safely() {
    let label63 = "a".repeat(63);
    let label64 = "a".repeat(64);
    let long_name = format!("{0}.{0}.{0}.{1}", label63, "a".repeat(61)); // 253
    let too_long = format!("{long_name}a"); // 254
    assert_eq!(
        rule_domain(&format!("{label63}.com")).unwrap(),
        format!("{label63}.com")
    );
    assert_eq!(rule_domain(&long_name).unwrap().len(), 253);
    // A non-ASCII label whose Punycode form is over 63 characters.
    let long_idn = format!("{}\u{fc}.de", "a".repeat(60)); // xn-- + 60 + "-" + digits > 63
    let refused = [
        "",
        "   ",
        ".",
        "..",
        ".bank.com",
        "bank..com",
        "bank.com..",
        "bank.com:443",
        "*.bank.com",
        "bank com",
        "user@bank.com",
        "bank.com/x",
        // D-126: decomposed, zero-width space, soft hyphen, word joiner, braille blank, fi,
        // superscript two, math bold, circled and half-width forms now map (UTS-46).
        "ban\u{200d}k.com", // zero-width joiner
        "bank\u{202e}.com", // bidi override
    ];
    for d in refused {
        match rule_domain(d) {
            Err(MemoryError::Invalid(_)) => {}
            other => panic!("rule_domain({d:?}) should be Invalid, got {other:?}"),
        }
    }
    // KELVIN SIGN lowercases to ASCII k, which is what UTS-46 maps it to: a refusal and
    // "kelvin.com" are both acceptable, any other key is not.
    match rule_domain("\u{212a}elvin.com") {
        Err(MemoryError::Invalid(_)) => {}
        Ok(k) => assert_eq!(k, "kelvin.com"),
        Err(e) => panic!("{e:?}"),
    }
    for d in [
        label64.clone(),
        format!("{label64}.com"),
        too_long,
        long_idn,
    ] {
        assert!(
            matches!(rule_domain(&d), Err(MemoryError::Invalid(_))),
            "len {} should be refused",
            d.len()
        );
    }
}

/// B-190: "everything UTS-46 would map further is refused ... switching to the idna crate
/// later only turns some refusals into mappings; no stored key changes". So a domain the
/// subset accepts must already be the A-label UTS-46 (non-transitional, what URL hosts
/// carry) gives for it.
#[test]
fn r3_b190_an_accepted_domain_is_its_uts46_a_label() {
    let mut wrong = Vec::new();
    // (input, what UTS-46 maps it to before Punycode)
    let cases = [
        // U+017F LATIN SMALL LETTER LONG S is mapped to "s".
        ("\u{17f}hop.example", "shop.example"),
        // U+0133 LATIN SMALL LIGATURE IJ is mapped to "ij".
        ("\u{133}ssel.nl", "ijssel.nl"),
        // U+1D2C MODIFIER LETTER CAPITAL A is mapped to "a".
        ("b\u{1d2c}nk.example", "bank.example"),
        // U+03A3 GREEK CAPITAL SIGMA is mapped to U+03C3 wherever it stands; a label-final
        // capital sigma must not become the final form U+03C2.
        ("example.\u{391}\u{3a3}", "example.\u{3b1}\u{3c3}"),
    ];
    for (input, uts46) in cases {
        let want = rule_domain(uts46).expect("the mapped form is accepted");
        match rule_domain(input) {
            Err(MemoryError::Invalid(_)) => {}
            Ok(got) if got == want => {}
            other => wrong.push(format!("{input:?} -> {other:?}, URL host is {want:?}")),
        }
    }
    println!("r3 b190 mismatches: {wrong:#?}");
    assert!(
        wrong.is_empty(),
        "accepted rule domains that no URL host carries: {wrong:?}"
    );
}

// ------------------------------------------------------------------ D-120 (1) rule_hint

#[test]
fn r3_rule_hint_is_ascii_only() {
    assert_eq!(rule_hint(" Job_Application ").unwrap(), "job_application");
    assert_eq!(rule_hint("News.Letter-2").unwrap(), "news.letter-2");
    for h in [
        "",
        "  ",
        "\u{212a}elvin",            // KELVIN SIGN, would fold to k
        "\u{ff4a}\u{ff4f}\u{ff42}", // full-width "job"
        "job application",
        "job:app",
        "j\u{f6}b",
        "job\u{200b}",
        "job\u{0301}",
        "job/app",
        "job\u{1b}[0m",
    ] {
        assert!(
            matches!(rule_hint(h), Err(MemoryError::Invalid(_))),
            "rule_hint({h:?}) should be Invalid"
        );
    }
    assert_eq!(rule_class_key(" submit : Wire ").unwrap(), "Submit:wire");
    assert!(rule_class_key("Submit:\u{212a}elvin").is_err());
    assert!(rule_class_key("Submit:\u{ff4a}ob").is_err());
}

#[tokio::test]
async fn r3_gate_rule_set_stores_rule_key_and_gate_matches_idn_hosts() {
    for mk in stores() {
        let (s, g) = gate(mk());
        // Every backend stores rule_key's output (D-113).
        assert_eq!(
            rule_key("BÜCHER.DE", "send", &ASK).unwrap(),
            ("xn--bcher-kva.de".to_string(), "Send".to_string())
        );
        s.gate_rule_set("BÜCHER.DE", "send", ASK, "").await.unwrap();
        assert_eq!(
            s.gate_rule("xn--bcher-kva.de", "Send").await.unwrap(),
            Some(ASK)
        );
        // MemStore::gate_rule canonicalises its lookup like PgStore.
        assert_eq!(s.gate_rule("Bücher.de.", "SEND").await.unwrap(), Some(ASK));
        s.gate_rule_set("bank\u{3002}com", "Submit", STOP, "")
            .await
            .unwrap();
        assert_eq!(s.gate_rule("bank.com", "Submit").await.unwrap(), Some(STOP));
        // Refused rules are not stored.
        for (d, c) in [
            // D-126: decomposed and ligature domains now map (UTS-46); these two it refuses.
            ("bank\u{202e}.com", "Send"),
            ("ban\u{200d}k.com", "Send"),
            ("bank.com", "Submit:\u{212a}elvin"),
            ("bank.com", "Submit:\u{ff4a}ob"),
        ] {
            assert!(
                matches!(
                    s.gate_rule_set(d, c, ASK, "").await,
                    Err(MemoryError::Invalid(_))
                ),
                "gate_rule_set({d:?}, {c:?}) should be refused"
            );
        }
        // The gate canonicalises call.domain the same way: the A-label host a URL carries,
        // the U-label and the ideographic full stop all hit the rule.
        s.touch_contact("known@x.com", "email", "").await.unwrap();
        for host in ["xn--bcher-kva.de", "BÜCHER.de", "bücher\u{3002}de"] {
            let mut c = send(&["known@x.com"]);
            c.domain = Some(host.into());
            let k = card(check(&g, &c).await);
            assert_eq!(
                k.on_timeout,
                OnTimeout::Skip,
                "{host}: user ask rule on a known Send"
            );
        }
        let mut other = send(&["known@x.com"]);
        other.domain = Some("buecher.de".into());
        assert_eq!(check(&g, &other).await, Decision::Allow);
        let mut sub = call(Class::Submit, "BANK\u{ff0e}COM");
        assert!(matches!(check(&g, &sub).await, Decision::Stop(_)));
        sub.domain = Some("bank.org".into());
        assert_eq!(check(&g, &sub).await, Decision::Allow);
    }
}

// ------------------------------------------------------------------ D-120 (2) cards

#[tokio::test]
async fn r3_card_components_are_cleaned_one_by_one() {
    let (_s, g) = gate(MemStore::new());
    // An OSC opened in one recipient and closed (BEL) in the next must not eat the second.
    let mut c = send(&["one@x.com\u{1b}]0;title", "two@y.com\u{7}"]);
    c.args = json!({
        "subject": "hi\u{1b}]8;;",
        "body": "visible-body\u{7}",
        "k\u{2800}ey": "v\u{2800}al"
    });
    let k = card(check(&g, &c).await);
    println!("r3 card: {k:?}");
    assert!(
        k.what.contains("two@y.com"),
        "what lost a recipient: {}",
        k.what
    );
    assert!(
        k.what.contains("visible-body"),
        "what lost an arg: {}",
        k.what
    );
    assert_clean(&k.what, "what");
    for (key, v) in &k.values {
        assert_clean(key, "value key");
        assert_clean(v, "value");
    }
    assert!(k.values[0].1.contains("two@y.com"), "{:?}", k.values[0]);
    // A recipient not shown as written cannot send on silence.
    assert_eq!(k.on_timeout, OnTimeout::Skip);

    // U+2800 is invisible on the card.
    let mut w = call(Class::Pay, "shop.example");
    w.what = Some("pay\u{2800}\u{2800}100\u{2800}EUR".into());
    w.values = vec![("amount\u{2800}".into(), "\u{2800}100".into())];
    let k = card(check(&g, &w).await);
    assert_eq!(k.what, "pay 100 EUR");
    assert_eq!(k.values, vec![("amount".to_string(), "100".to_string())]);
}

#[tokio::test]
async fn r3_stop_reason_cleans_the_tool_name() {
    let (s, g) = gate(MemStore::new());
    let tool = format!(
        "evil\u{1b}[2K\u{1b}]0;x\u{7}tool\u{202e}\r\n{}",
        "t".repeat(400)
    );
    let mut c = call(Class::Challenge, "x.example");
    c.tool = tool.clone();
    c.args = json!({"element": "#31\u{1b}[31m"});
    match check(&g, &c).await {
        Decision::Stop(r) => {
            println!("r3 stop: {r:?}");
            assert_clean(&r.why, "why");
            assert!(r.why.contains("evil"));
            assert!(!r.why.contains("[2K"));
            assert!(r.why.chars().count() < 300, "tool name not capped");
            assert_clean(r.element.as_deref().unwrap_or(""), "element");
        }
        other => panic!("{other:?}"),
    }
    s.gate_rule_set("x.example", "Delete", STOP, "")
        .await
        .unwrap();
    let mut d = call(Class::Delete, "x.example");
    d.tool = tool;
    match check(&g, &d).await {
        Decision::Stop(r) => assert_clean(&r.why, "rule stop why"),
        other => panic!("{other:?}"),
    }
}

// ------------------------------------------------------------------ D-120 (3) Send row

fn row(k: &AskCard) -> &(String, String) {
    let r = &k.values[0];
    assert!(
        r.0.starts_with("new recipients ("),
        "first value is not the row: {r:?}"
    );
    r
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

#[tokio::test]
async fn r3_send_blank_recipients_are_not_recipients() {
    for mk in stores() {
        let (s, g) = gate(mk());
        s.touch_contact("friend@example.com", "email", "")
            .await
            .unwrap();
        // Only blanks: a Send with no recipients asks and skips; the row says none.
        let k = card(check(&g, &send(&["  ", "", "\t", "\u{3000}"])).await);
        assert_eq!(k.on_timeout, OnTimeout::Skip);
        assert_eq!(row(&k), &pair("new recipients (0)", "none"));
        assert_eq!(timeout(&k).await, AskOutcome::TimeoutSkip);
        // A known contact plus blanks: every real recipient is known, so Allow.
        assert_eq!(
            check(&g, &send(&["friend@example.com", "   ", ""])).await,
            Decision::Allow
        );
    }
}

#[tokio::test]
async fn r3_send_new_recipients_deduped_by_contact_key() {
    for mk in stores() {
        let (s, g) = gate(mk());
        s.touch_contact("friend@example.com", "email", "")
            .await
            .unwrap();
        let to = [
            "New@X.com",
            "new@x.com",
            " NEW@x.com ",
            "friend@example.com",
        ];
        let k = card(check(&g, &send(&to)).await);
        assert_eq!(row(&k), &pair("new recipients (1)", "New@X.com"));
        assert_eq!(k.on_timeout, OnTimeout::Proceed);
        assert_eq!(timeout(&k).await, AskOutcome::TimeoutProceed);
    }
}

#[tokio::test]
async fn r3_send_row_is_first_and_only_the_gate_writes_it() {
    let (s, g) = gate(MemStore::new());
    s.touch_contact("friend@example.com", "email", "")
        .await
        .unwrap();
    // Classifier values that try to forge the row are dropped, in any spelling.
    let mut c = send(&["new@x.com"]);
    c.values = vec![
        pair("new recipients (0)", "none"),
        pair("NEW RECIPIENTS", "none"),
        pair("\u{200b}new recipients (0)", "none"),
        pair("new\u{a0}recipients", "none"),
        pair("subject", "Hi"),
    ];
    let k = card(check(&g, &c).await);
    println!("r3 forged row card: {:?}", k.values);
    assert_eq!(row(&k), &pair("new recipients (1)", "new@x.com"));
    assert_eq!(k.values.len(), 2, "{:?}", k.values);
    assert_eq!(k.values[1], pair("subject", "Hi"));
    // An arg under the key is dropped too.
    let mut a = send(&["new@x.com"]);
    a.args = json!({"new recipients": "none", "body": "b"});
    let k = card(check(&g, &a).await);
    let rows = k
        .values
        .iter()
        .filter(|(k, _)| k.to_lowercase().starts_with("new recipients"))
        .count();
    assert_eq!(rows, 1, "{:?}", k.values);
    // A user ask rule on Send to known contacts: the row says none, and it skips.
    s.gate_rule_set("mail.example", "Send", ASK, "")
        .await
        .unwrap();
    let k = card(check(&g, &send(&["friend@example.com"])).await);
    assert_eq!(row(&k), &pair("new recipients (0)", "none"));
    assert_eq!(k.on_timeout, OnTimeout::Skip);
    // Non-Send cards carry no row.
    let k = card(check(&g, &call(Class::Pay, "shop.example")).await);
    assert!(k
        .values
        .iter()
        .all(|(k, _)| !k.starts_with("new recipients")));
}

#[tokio::test]
async fn r3_send_row_limits_and_proceed_only_when_complete() {
    let (_s, g) = gate(MemStore::new());
    // Nine recipients overflow the 200-char what line but all fit the row: proceeds.
    let nine: Vec<String> = (0..9)
        .map(|i| format!("person.number.{i}@long-domain-name.example"))
        .collect();
    let refs: Vec<&str> = nine.iter().map(String::as_str).collect();
    let k = card(check(&g, &send(&refs)).await);
    assert_eq!(k.values[0].0, "new recipients (9)");
    for r in &nine {
        assert!(
            k.values[0].1.contains(r.as_str()),
            "{r} missing from the row"
        );
    }
    assert_eq!(k.on_timeout, OnTimeout::Proceed);

    // Exactly 32 short recipients: complete, proceeds.
    let n32: Vec<String> = (0..32).map(|i| format!("u{i}@x.com")).collect();
    let refs: Vec<&str> = n32.iter().map(String::as_str).collect();
    let k = card(check(&g, &send(&refs)).await);
    assert_eq!(k.values[0].0, "new recipients (32)");
    assert!(!k.values[0].1.contains("more)"));
    assert_eq!(k.on_timeout, OnTimeout::Proceed);

    // 33: the 33rd is counted, never silently dropped, and silence skips.
    let n33: Vec<String> = (0..33).map(|i| format!("u{i}@x.com")).collect();
    let refs: Vec<&str> = n33.iter().map(String::as_str).collect();
    let k = card(check(&g, &send(&refs)).await);
    assert_eq!(k.values[0].0, "new recipients (33)");
    assert!(
        k.values[0].1.ends_with("\u{2026}(+1 more)"),
        "{}",
        k.values[0].1
    );
    assert_eq!(k.on_timeout, OnTimeout::Skip);
    assert_eq!(timeout(&k).await, AskOutcome::TimeoutSkip);

    // The 2000-character budget: 20 recipients of 110 characters.
    let wide: Vec<String> = (0..20)
        .map(|i| format!("{}{i:02}@example.com", "w".repeat(110 - 14)))
        .collect();
    assert!(wide.iter().all(|w| w.chars().count() == 110));
    let refs: Vec<&str> = wide.iter().map(String::as_str).collect();
    let k = card(check(&g, &send(&refs)).await);
    println!("r3 wide row: {} chars", k.values[0].1.chars().count());
    assert!(k.values[0].1.chars().count() <= 2000 + 20);
    assert!(k.values[0].1.contains("more)"));
    assert_eq!(k.on_timeout, OnTimeout::Skip);

    // One recipient of exactly 120 characters is shown in full; 130 is shortened.
    let r120 = format!("{}@example.com", "a".repeat(120 - 12));
    assert_eq!(r120.chars().count(), 120);
    let k = card(check(&g, &send(&[r120.as_str()])).await);
    assert_eq!(k.values[0].1, r120);
    assert_eq!(k.on_timeout, OnTimeout::Proceed);
    let r130 = format!("{}@example.com", "b".repeat(130 - 12));
    let k = card(check(&g, &send(&[r130.as_str()])).await);
    println!("r3 long recipient: {}", k.values[0].1);
    assert!(
        k.values[0].1.contains("\u{2026}(+10 chars)\u{2026}"),
        "{}",
        k.values[0].1
    );
    assert!(k.values[0].1.ends_with("@example.com"));
    assert!(k.values[0].1.starts_with("bbbb"));
    assert_eq!(k.on_timeout, OnTimeout::Skip);
}

#[tokio::test]
async fn r3_send_proceeds_only_when_every_new_recipient_is_shown_as_written() {
    let (_s, g) = gate(MemStore::new());
    // Surrounding whitespace is not part of the address: still "as written".
    let k = card(check(&g, &send(&["  new@x.com  "])).await);
    assert_eq!(k.values[0].1, "new@x.com");
    assert_eq!(k.on_timeout, OnTimeout::Proceed);
    for bad in [
        "new\u{200b}@x.com",
        "new@x\u{2800}.com",
        "new@x.com\u{202e}moc.live",
        "new  @x.com",
        "new@x.com\u{1b}[8m",
        "new@x.com\r\nbcc@evil.example",
        "\u{1b}[2K",
    ] {
        let k = card(check(&g, &send(&["ok@y.com", bad])).await);
        assert_eq!(k.on_timeout, OnTimeout::Skip, "{bad:?}: {:?}", k.values[0]);
        assert_eq!(timeout(&k).await, AskOutcome::TimeoutSkip);
        assert_clean(&k.values[0].1, "row");
    }
}

// ------------------------------------------------------------------ D-120 (4) from_hint

#[tokio::test]
async fn r3_losing_lower_class_hint_keeps_the_class_level_timeout() {
    for mk in stores() {
        let (s, g) = gate(mk());
        s.gate_rule_set("*", "Read:newsletter", ALLOW, "")
            .await
            .unwrap();
        // Account and Send(new) hinted newsletter: the class level decided, so they proceed.
        let mut acct = call(Class::Account, "news.example");
        acct.hints = vec!["newsletter".into()];
        let k = card(check(&g, &acct).await);
        assert_eq!(k.on_timeout, OnTimeout::Proceed);
        assert_eq!(timeout(&k).await, AskOutcome::TimeoutProceed);
        let mut sn = send(&["new@x.com"]);
        sn.hints = vec!["newsletter".into()];
        let k = card(check(&g, &sn).await);
        assert_eq!(k.on_timeout, OnTimeout::Proceed);
        // Pay hinted newsletter is still an ask that skips (D-113).
        let mut pay = call(Class::Pay, "news.example");
        pay.hints = vec!["newsletter".into()];
        assert_eq!(card(check(&g, &pay).await).on_timeout, OnTimeout::Skip);
        // A lower-class hint ask that ties the class level decided too: skip.
        s.gate_rule_set("*", "Read:promo", ASK, "").await.unwrap();
        let mut tie = call(Class::Account, "news.example");
        tie.hints = vec!["promo".into()];
        assert_eq!(card(check(&g, &tie).await).on_timeout, OnTimeout::Skip);
        // A same-class hint ask decides: skip.
        s.gate_rule_set("*", "Account:signup", ASK, "")
            .await
            .unwrap();
        let mut same = call(Class::Account, "news.example");
        same.hints = vec!["signup".into()];
        assert_eq!(card(check(&g, &same).await).on_timeout, OnTimeout::Skip);
        // Both hints together: the tying hint counts, so skip.
        let mut both = call(Class::Account, "news.example");
        both.hints = vec!["newsletter".into(), "promo".into()];
        assert_eq!(card(check(&g, &both).await).on_timeout, OnTimeout::Skip);
    }
}

// ------------------------------------------------------------------ D-70 clean_card public

#[test]
fn r3_clean_card_is_public_and_cleans_everything() {
    let mut values: Vec<(String, String)> = (0..40)
        .map(|i| (format!("k{i}\u{1b}[1m"), format!("v{i}\u{202e}\u{2800}")))
        .collect();
    values.insert(0, pair("New Recipients (0)", "none"));
    values.insert(1, pair("card", "@vault(card)"));
    let dirty = AskCard {
        class: Class::Send,
        what: format!("send\u{1b}]0;x\u{7} to a@b.c\r\n{}", "w".repeat(500)),
        site: Some("site\u{1b}[31m.example\u{200b}".into()),
        values,
        timeout: std::time::Duration::from_secs(5),
        on_timeout: OnTimeout::Proceed,
    };
    let c = clean_card(dirty);
    println!("r3 clean_card what: {}", c.what);
    assert_eq!(c.class, Class::Send);
    assert_eq!(c.on_timeout, OnTimeout::Proceed);
    assert_eq!(c.timeout, std::time::Duration::from_secs(5));
    assert_clean(&c.what, "what");
    assert!(c.what.chars().count() <= 200);
    assert!(c.what.starts_with("send to a@b.c"), "{}", c.what);
    assert_eq!(c.site.as_deref(), Some("site .example"));
    assert!(c
        .values
        .iter()
        .all(|(k, _)| !k.to_lowercase().starts_with("new recipients")));
    assert_eq!(c.values[0], pair("card", "@vault(card)"));
    assert!(c.values.len() <= 33, "{}", c.values.len());
    assert_eq!(c.values.last().unwrap().0, "...");
    for (k, v) in &c.values {
        assert_clean(k, "key");
        assert_clean(v, "value");
    }
    let ser = serde_json::to_string(&c).unwrap();
    assert!(!ser.contains(&canary()));
}

// ------------------------------------------------------------------ MemStore parity

fn src() -> Source {
    Source {
        kind: SourceKind::User,
        reference: "t04r3".into(),
        run_id: None,
    }
}

fn brief(hits: &[harness_core::memory::Hit]) -> Vec<(String, f32, Why)> {
    hits.iter()
        .map(|h| (h.fact.subject.clone(), h.score, h.why))
        .collect()
}

#[tokio::test]
async fn r3_memstore_search_follows_d115() {
    let s = MemStore::empty();
    let mut exact = Fact::new("Foo-Bar Inc", "domain", "foobar.example", src());
    exact.seen_at = chrono::Utc::now() - chrono::Duration::days(30);
    s.remember(exact).await.unwrap();
    s.remember(Fact::new(
        "weekly notes",
        "met",
        "lunch with Foo-Bar Inc people",
        src(),
    ))
    .await
    .unwrap();
    s.remember(Fact::new(
        "Vasanth Raghavan",
        "role",
        "staff engineer",
        src(),
    ))
    .await
    .unwrap();
    s.remember(Fact::new(
        "reading list",
        "next",
        "raghavan vasanth biography",
        src(),
    ))
    .await
    .unwrap();

    let hits = s.search("foo bar, INC.", 5).await.unwrap();
    println!("r3 mem search foo: {:?}", brief(&hits));
    assert_eq!(hits[0].fact.subject, "Foo-Bar Inc");
    assert_eq!(hits[0].score, 1.0);
    assert_eq!(hits[0].why, Why::Exact);

    let hits = s.search("Foo-Bar Inc", 5).await.unwrap();
    assert_eq!(
        hits[0].fact.subject, "Foo-Bar Inc",
        "equality beats a newer mention"
    );
    assert_eq!(hits[0].score, 1.0);
    assert_eq!(hits[1].fact.subject, "weekly notes");
    assert!((hits[1].score - 0.9).abs() < 1e-6);

    // A typo inside a longer subject that clears the 0.6 word_similarity bar.
    let hits = s.search("Vasanh", 5).await.unwrap();
    assert_eq!(hits[0].fact.subject, "Vasanth Raghavan");
    assert_eq!(hits[0].why, Why::Trigram);
    assert!(hits[0].score >= 0.6);

    // Ordered by score, tier only breaking ties: a 1.0 trigram hit beats a 0.9 substring.
    let hits = s.search("Raghavan Vasanth", 5).await.unwrap();
    println!("r3 mem search rv: {:?}", brief(&hits));
    assert_eq!(hits[0].fact.subject, "Vasanth Raghavan");
    assert_eq!(hits[0].why, Why::Trigram);
    assert!(hits[0].score > 0.9);
    assert_eq!(hits[1].fact.subject, "reading list");
}

/// D-115's own example: "a typo inside a longer subject ('Vasnth' in 'Vasanth Raghavan')
/// must still hit". pg_trgm's word_similarity('Vasnth', 'Vasanth Raghavan') is 0.5, under
/// the default 0.6 threshold both stores use, so neither finds it.
#[tokio::test]
async fn r3_d115_vasnth_typo_inside_longer_subject_hits() {
    let s = MemStore::empty();
    s.remember(Fact::new(
        "Vasanth Raghavan",
        "role",
        "staff engineer",
        src(),
    ))
    .await
    .unwrap();
    let hits = s.search("Vasnth", 5).await.unwrap();
    println!("r3 mem search Vasnth: {:?}", brief(&hits));
    assert_eq!(
        hits.first().map(|h| h.fact.subject.as_str()),
        Some("Vasanth Raghavan"),
        "D-115: 'Vasnth' must hit 'Vasanth Raghavan'"
    );
}

async fn interval(s: &MemStore, id: harness_core::memory::ReviewId) -> f32 {
    s.due_reviews(chrono::Utc::now() + chrono::Duration::days(400), 100)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.id == id)
        .expect("review is due within 400 days")
        .interval_days
}

#[tokio::test]
async fn r3_memstore_sm2_rounds_up() {
    let s = MemStore::empty();
    let id = s.review_add("q", "a").await.unwrap();
    let mut seen = Vec::new();
    for g in [5u8, 5, 5, 5] {
        s.grade_review(id, g).await.unwrap();
        seen.push(interval(&s, id).await);
    }
    println!("r3 mem sm2: {seen:?}");
    // 1, 6, ceil(6 * 2.7) = 17, ceil(17 * 2.8) = 48.
    assert_eq!(seen, vec![1.0, 6.0, 17.0, 48.0]);
    // An exact product is not bumped: grade 4 keeps ease 2.5, 6 * 2.5 = 15.
    let id = s.review_add("q2", "a").await.unwrap();
    for g in [4u8, 4, 4] {
        s.grade_review(id, g).await.unwrap();
    }
    assert_eq!(interval(&s, id).await, 15.0);
}

/// bae6c45: MemStore truncates timestamps toward zero from 2000-01-01 like sqlx, the same
/// values core-pg's accept_t03_r3::r3_timestamps_truncate_toward_zero_from_2000_like_memstore
/// reads back from Postgres.
#[tokio::test]
async fn r3_memstore_timestamps_truncate_toward_zero_from_2000() {
    use chrono::{Duration, TimeZone, Utc};
    let s = MemStore::empty();
    let pre = Utc.with_ymd_and_hms(1999, 12, 31, 23, 59, 59).unwrap()
        + Duration::nanoseconds(999_999_500);
    let post = Utc.with_ymd_and_hms(2026, 10, 25, 12, 0, 0).unwrap() + Duration::nanoseconds(1_900);
    for (subject, at, want) in [
        (
            "pre2000 stamp",
            pre,
            Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap(),
        ),
        (
            "post2000 stamp",
            post,
            Utc.with_ymd_and_hms(2026, 10, 25, 12, 0, 0).unwrap() + Duration::microseconds(1),
        ),
    ] {
        let mut f = Fact::new(subject, "k", "v", src());
        f.seen_at = at;
        s.remember(f).await.unwrap();
        let hit = s.search(subject, 1).await.unwrap().remove(0);
        println!("r3 mem ts {at:?} -> {:?}", hit.fact.seen_at);
        assert_eq!(hit.fact.seen_at, want, "{subject}");
    }
}

#[test]
#[should_panic(expected = "blank person")]
fn r3_memstore_add_contact_refuses_blank_person() {
    MemStore::empty().add_contact("   ");
}

#[tokio::test]
async fn r3_memstore_touch_contact_refuses_blank_person() {
    let s = MemStore::empty();
    assert!(matches!(
        s.touch_contact(" \t ", "email", "").await,
        Err(MemoryError::Invalid(_))
    ));
}
