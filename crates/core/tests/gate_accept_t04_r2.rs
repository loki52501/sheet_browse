//! T-04 acceptance, round 2: the D-110 / D-113 fix-round rows (docs/01 H-1.2, H-1.3, I-3).
//!
//! Every row goes through `Gate::check` with `harness_core::testing::MemStore`; rules are
//! written through the `MemoryStore::gate_rule_set` trait method (what a real backend
//! gets), and timeouts are played out through `ScriptedAsker` with no scripted answer.
//! The `testing` feature comes from the self dev-dependency (D-111), so plain
//! `cargo test -p harness-core --test gate_accept_t04_r2` runs this file.

use std::sync::Arc;

use harness_core::class::{Class, Mode};
use harness_core::gate::{AskCard, AskOutcome, Asker, Call, Decision, Gate, OnTimeout};
use harness_core::memory::{GateRule, MemoryStore};
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

fn hinted(class: Class, domain: &str, hints: &[&str]) -> Call {
    let mut c = call(class, domain);
    c.hints = hints.iter().map(|h| h.to_string()).collect();
    c
}

fn send(to: &[&str]) -> Call {
    let mut c = call(Class::Send, "mail.example");
    c.tool = "echo.send".into();
    c.recipients = to.iter().map(|s| s.to_string()).collect();
    c.args = json!({"to": to.join(","), "body": "hello"});
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

/// Lets the card time out (no scripted answer) and returns what happened.
async fn timeout(c: &AskCard) -> AskOutcome {
    ScriptedAsker::new().ask(c).await
}

fn stores() -> [fn() -> MemStore; 2] {
    [MemStore::new, MemStore::empty]
}

/// A store with the job_application rule present (seeded, or written for the empty store).
async fn with_job_rule(mk: fn() -> MemStore) -> (Arc<MemStore>, Gate) {
    let (s, g) = gate(mk());
    s.gate_rule_set(
        "*",
        "Submit:job_application",
        ASK,
        "job submissions are mine",
    )
    .await
    .unwrap();
    (s, g)
}

#[tokio::test]
async fn r2_gate_rule_set_noncanonical_domain_and_class_still_applies() {
    // D-110 (1): gate_rule_set('Bank.COM.', 'submit', ask) stores ('bank.com', 'Submit').
    for mk in stores() {
        let (s, g) = gate(mk());
        s.gate_rule_set("Bank.COM.", "submit", ASK, "user rule")
            .await
            .unwrap();
        assert_eq!(
            s.gate_rule("bank.com", "Submit").await.unwrap(),
            Some(ASK),
            "rule not stored under the canonical key"
        );
        let c = card(check(&g, &call(Class::Submit, "bank.com")).await);
        assert_eq!(c.class, Class::Submit);
        assert_eq!(c.on_timeout, OnTimeout::Skip);
        assert_eq!(timeout(&c).await, AskOutcome::TimeoutSkip);
        // the call's domain is canonicalised the same way
        for d in ["BANK.com", "bank.com.", " Bank.Com "] {
            assert!(
                matches!(check(&g, &call(Class::Submit, d)).await, Decision::Ask(_)),
                "call domain {d:?}"
            );
        }
        for other in ["shop.example", "notbank.com", "bank.org"] {
            assert_eq!(
                check(&g, &call(Class::Submit, other)).await,
                Decision::Allow,
                "{other}"
            );
        }
        // keys that do not canonicalise are rejected, not silently stored dead
        for (d, k) in [
            ("bank.com:443", "Submit"),
            ("", "Submit"),
            ("bank.com", "Sbumit"),
            ("bank.com", "Submit:"),
            ("bank.com", "Submit:job application"),
        ] {
            assert!(
                s.gate_rule_set(d, k, ASK, "").await.is_err(),
                "({d:?}, {k:?}) accepted"
            );
        }
        // D-5 through the trait: a Challenge allow, hinted or not, is rejected
        for k in ["Challenge", "challenge", "Challenge:x"] {
            assert!(s.gate_rule_set("*", k, ALLOW, "").await.is_err(), "{k}");
        }
    }
}

#[tokio::test]
async fn r2_unrelated_hint_allow_does_not_undo_job_application() {
    // D-110 (3): hints ['newsletter','job_application'] with ('*','Submit:newsletter','allow')
    // still ask; the most restrictive hint mode wins, in either order.
    for mk in stores() {
        let (s, g) = with_job_rule(mk).await;
        s.gate_rule_set("*", "Submit:newsletter", ALLOW, "")
            .await
            .unwrap();
        s.gate_rule_set("jobs.example", "Submit:newsletter", ALLOW, "")
            .await
            .unwrap();
        for hints in [
            ["newsletter", "job_application"],
            ["job_application", "newsletter"],
        ] {
            let c = card(check(&g, &hinted(Class::Submit, "jobs.example", &hints)).await);
            assert_eq!(c.class, Class::Submit);
            assert_eq!(c.on_timeout, OnTimeout::Skip, "{hints:?}");
            assert_eq!(timeout(&c).await, AskOutcome::TimeoutSkip);
        }
        // newsletter alone is allowed by its own rule
        assert_eq!(
            check(&g, &hinted(Class::Submit, "jobs.example", &["newsletter"])).await,
            Decision::Allow
        );
        // the call's hints are canonicalised (D-110 (1))
        let c = card(
            check(
                &g,
                &hinted(Class::Submit, "jobs.example", &[" Job_Application "]),
            )
            .await,
        );
        assert_eq!(c.on_timeout, OnTimeout::Skip);
    }
}

#[tokio::test]
async fn r2_job_application_on_account_asks_and_times_out_skip() {
    // D-110 (2)(4): a job application classed Account (password field) still hits
    // Submit:job_application, asks, and never proceeds on silence.
    for mk in stores() {
        let (_, g) = with_job_rule(mk).await;
        let c = card(
            check(
                &g,
                &hinted(Class::Account, "jobs.example", &["job_application"]),
            )
            .await,
        );
        assert_eq!(c.class, Class::Account);
        assert_eq!(c.on_timeout, OnTimeout::Skip);
        assert_eq!(timeout(&c).await, AskOutcome::TimeoutSkip);
        // without the hint the same Account call proceeds on timeout (H-1.3)
        let c = card(check(&g, &call(Class::Account, "jobs.example")).await);
        assert_eq!(c.on_timeout, OnTimeout::Proceed);
    }
}

#[tokio::test]
async fn r2_lower_class_hint_allow_does_not_lower_higher_class() {
    // D-113: ('*','Read:newsletter','allow') does not lower a Pay call hinted newsletter.
    for mk in stores() {
        let (s, g) = gate(mk());
        s.gate_rule_set("*", "Read:newsletter", ALLOW, "")
            .await
            .unwrap();
        s.gate_rule_set("shop.example", "Read:newsletter", ALLOW, "")
            .await
            .unwrap();
        let c = card(check(&g, &hinted(Class::Pay, "shop.example", &["newsletter"])).await);
        assert_eq!(c.class, Class::Pay);
        assert_eq!(c.on_timeout, OnTimeout::Skip);
        assert_eq!(timeout(&c).await, AskOutcome::TimeoutSkip);
        for class in [Class::Account, Class::Delete] {
            assert!(
                matches!(
                    check(&g, &hinted(class, "shop.example", &["newsletter"])).await,
                    Decision::Ask(_)
                ),
                "{class} hinted newsletter was lowered"
            );
        }
        // a Send to a new recipient hinted newsletter still asks
        let mut sn = send(&["stranger@example.org"]);
        sn.hints = vec!["newsletter".into()];
        assert!(matches!(check(&g, &sn).await, Decision::Ask(_)));
        // the rule still applies at its own class
        assert_eq!(
            check(&g, &hinted(Class::Read, "shop.example", &["newsletter"])).await,
            Decision::Allow
        );
    }
}

#[tokio::test]
async fn r2_user_ask_rule_on_send_to_known_contact_times_out_skip() {
    // D-110 (4): the user wrote an ask rule; silence must not send to a known contact.
    for mk in stores() {
        for domain in ["mail.example", "*"] {
            let (s, g) = gate(mk());
            s.add_contact("friend@example.com");
            let to = send(&["friend@example.com"]);
            assert_eq!(check(&g, &to).await, Decision::Allow, "before the rule");
            s.gate_rule_set(domain, "Send", ASK, "ask me before mail")
                .await
                .unwrap();
            let c = card(check(&g, &to).await);
            assert_eq!(c.class, Class::Send);
            assert_eq!(c.on_timeout, OnTimeout::Skip, "rule domain {domain}");
            assert_eq!(timeout(&c).await, AskOutcome::TimeoutSkip);
        }
    }
}

#[tokio::test]
async fn r2_send_new_and_account_time_out_proceed() {
    // H-1.3: Send(new) and Account proceed on timeout; D-113: Send with no recipients skips.
    for mk in stores() {
        let (s, g) = gate(mk());
        s.add_contact("friend@example.com");
        for to in [
            send(&["stranger@example.org"]),
            send(&["friend@example.com", "stranger@example.org"]),
        ] {
            let c = card(check(&g, &to).await);
            assert_eq!(c.on_timeout, OnTimeout::Proceed);
            assert_eq!(timeout(&c).await, AskOutcome::TimeoutProceed);
        }
        let c = card(check(&g, &call(Class::Account, "luma.example")).await);
        assert_eq!(c.on_timeout, OnTimeout::Proceed);
        assert_eq!(timeout(&c).await, AskOutcome::TimeoutProceed);

        let c = card(check(&g, &send(&[])).await);
        assert_eq!(c.class, Class::Send);
        assert_eq!(c.on_timeout, OnTimeout::Skip, "Send with no recipients");
        assert_eq!(timeout(&c).await, AskOutcome::TimeoutSkip);
    }
}

fn clean(s: &str) -> bool {
    !s.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            )
    })
}

fn assert_clean(c: &AskCard) {
    assert!(clean(&c.what), "what not clean: {:?}", c.what);
    for (k, v) in &c.values {
        assert!(clean(k), "key not clean: {k:?}");
        assert!(clean(v), "value not clean: {v:?}");
    }
    if let Some(site) = &c.site {
        assert!(clean(site), "site not clean: {site:?}");
    }
}

#[tokio::test]
async fn r2_card_what_with_esc_and_cr_is_sanitised() {
    // D-110 (5): classifier-given and args-built card strings are control-stripped and capped.
    let canary = std::env::var("SB_VAULT_CANARY").ok();
    let (_, g) = gate(MemStore::new());

    let mut pay = call(Class::Pay, "shop.example");
    pay.tool = "shop.checkout".into();
    pay.what = Some("pay 500 EUR\x1b[2K\r\x1b[32mpay 5 EUR to shop.example\x1b[0m".into());
    pay.values = vec![
        ("amount\r".into(), "500 EUR\x1b[2K\rfree".into()),
        ("note".into(), "\u{9b}31mred\u{202e}txet".into()),
        ("card".into(), "@vault(card)".into()),
    ];
    let c = card(check(&g, &pay).await);
    assert_clean(&c);
    assert!(c.what.contains("pay 500 EUR"), "what: {:?}", c.what);
    assert!(
        c.what.contains("pay 5 EUR to shop.example"),
        "what: {:?}",
        c.what
    );
    assert!(
        !c.what.contains("[2K") && !c.what.contains("[32m"),
        "{:?}",
        c.what
    );
    assert!(c
        .values
        .iter()
        .any(|(k, v)| k == "card" && v == "@vault(card)"));
    assert!(
        c.values.iter().any(|(_, v)| v.contains("500 EUR")),
        "{:?}",
        c.values
    );

    // A what that cleans to nothing falls back to the generated line.
    pay.what = Some("\x1b[2K\r\n\t".into());
    let c = card(check(&g, &pay).await);
    assert_clean(&c);
    assert!(c.what.contains("shop.checkout"), "what: {:?}", c.what);

    // Card built from args: values with ESC/CR are cleaned too.
    let mut p2 = call(Class::Pay, "shop.example");
    p2.args = json!({"amount": "9 EUR\r\x1b[1Avoid", "card": "@vault(card)"});
    let c = card(check(&g, &p2).await);
    assert_clean(&c);

    // Length cap.
    pay.what = Some("x".repeat(5000));
    let c = card(check(&g, &pay).await);
    assert!(c.what.chars().count() <= 200, "{}", c.what.chars().count());

    // Send card to a new recipient whose address carries a CR.
    let to = send(&["new@example.org\r\x1b[2Kfriend@example.com"]);
    let c = card(check(&g, &to).await);
    assert_clean(&c);

    if let Some(canary) = canary {
        let ser = serde_json::to_string(&c).unwrap();
        assert!(!ser.contains(&canary), "canary in a serialized ask card");
        assert!(!format!("{c:?}").contains(&canary), "canary in card Debug");
    }
}
