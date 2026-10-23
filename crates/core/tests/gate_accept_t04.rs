//! T-04 acceptance: the gate unit (docs/01 H-1, H-1.1..H-1.4, I-3; docs/02 acceptance
//! "touch_contact makes a Send to that person allow"; D-5, D-39, B-100).
//!
//! Every row goes through `Gate::check` with `harness_core::testing::MemStore`, so this
//! file needs the `testing` feature (D-108):
//! `cargo test -p harness-core --features testing --test gate_accept_t04`.
//! End-to-end `echo.send` through dispatch and the ledger is T-09's and is not here.

use std::sync::Arc;
use std::time::Duration;

use harness_core::class::{Class, Mode};
use harness_core::gate::{AskCard, Call, Decision, Gate, OnTimeout, StopReason};
use harness_core::memory::{GateRule, MemoryStore};
use harness_core::testing::MemStore;
use serde_json::json;

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

fn stop(d: Decision) -> StopReason {
    match d {
        Decision::Stop(r) => r,
        other => panic!("expected Stop, got {other:?}"),
    }
}

/// Both stores: with migration 0001's seed rules and with no rules at all.
fn stores() -> [fn() -> MemStore; 2] {
    [MemStore::new, MemStore::empty]
}

#[tokio::test]
async fn t04_send_known_recipient_allows() {
    for mk in stores() {
        let (s, g) = gate(mk());
        s.add_contact("friend@example.com");
        assert_eq!(
            check(&g, &send(&["friend@example.com"])).await,
            Decision::Allow
        );
    }
}

#[tokio::test]
async fn t04_send_unknown_recipient_asks_proceed() {
    for mk in stores() {
        let (s, g) = gate(mk());
        s.add_contact("friend@example.com");
        let c = card(check(&g, &send(&["stranger@example.org"])).await);
        assert_eq!(c.class, Class::Send);
        assert_eq!(c.on_timeout, OnTimeout::Proceed);
        // one unknown among known recipients still asks
        let c = card(check(&g, &send(&["friend@example.com", "stranger@example.org"])).await);
        assert_eq!(c.on_timeout, OnTimeout::Proceed);
    }
}

#[tokio::test]
async fn t04_send_empty_recipients_asks() {
    for mk in stores() {
        let (s, g) = gate(mk());
        s.add_contact("friend@example.com");
        let c = card(check(&g, &send(&[])).await);
        assert_eq!(c.class, Class::Send);
    }
}

#[tokio::test]
async fn t04_pay_asks_skip() {
    for mk in stores() {
        let (_, g) = gate(mk());
        let c = card(check(&g, &call(Class::Pay, "shop.example")).await);
        assert_eq!(c.class, Class::Pay);
        assert_eq!(c.on_timeout, OnTimeout::Skip);
    }
}

#[tokio::test]
async fn t04_delete_asks_skip() {
    for mk in stores() {
        let (_, g) = gate(mk());
        let c = card(check(&g, &call(Class::Delete, "mail.example")).await);
        assert_eq!(c.class, Class::Delete);
        assert_eq!(c.on_timeout, OnTimeout::Skip);
    }
}

#[tokio::test]
async fn t04_account_asks_proceed() {
    for mk in stores() {
        let (_, g) = gate(mk());
        let c = card(check(&g, &call(Class::Account, "luma.example")).await);
        assert_eq!(c.class, Class::Account);
        assert_eq!(c.on_timeout, OnTimeout::Proceed);
    }
}

#[tokio::test]
async fn t04_read_fill_navigate_submit_allow() {
    for mk in stores() {
        let (_, g) = gate(mk());
        for class in [Class::Read, Class::Fill, Class::Navigate, Class::Submit] {
            assert_eq!(
                check(&g, &call(class, "site.example")).await,
                Decision::Allow,
                "{class}"
            );
        }
    }
}

#[tokio::test]
async fn t04_challenge_stops_despite_forced_allow_rule() {
    for mk in stores() {
        let (s, g) = gate(mk());
        let allow = GateRule {
            mode: Mode::Allow,
            strict: false,
        };
        s.force_rule("*", "Challenge", allow);
        s.force_rule("captcha.example", "Challenge", allow);
        // the forced row is really in the table
        assert_eq!(
            s.gate_rule("*", "Challenge").await.unwrap(),
            Some(allow),
            "force_rule did not write the row"
        );
        let mut c = call(Class::Challenge, "captcha.example");
        c.args = json!({"element": "#31"});
        let r = stop(check(&g, &c).await);
        assert_eq!(r.class, Class::Challenge);
        assert_eq!(r.element.as_deref(), Some("#31"));
        // also with no domain
        let mut local = call(Class::Challenge, "x");
        local.domain = None;
        assert!(matches!(check(&g, &local).await, Decision::Stop(_)));
    }
}

#[tokio::test]
async fn t04_domain_submit_rule_scoped_to_domain() {
    for mk in stores() {
        let (s, g) = gate(mk());
        s.add_rule("bank.com", "Submit", Mode::Ask, false).unwrap();
        let c = card(check(&g, &call(Class::Submit, "bank.com")).await);
        assert_eq!(c.class, Class::Submit);
        assert_eq!(c.site.as_deref(), Some("bank.com"));
        for other in ["shop.example", "notbank.com", "bank.org"] {
            assert_eq!(
                check(&g, &call(Class::Submit, other)).await,
                Decision::Allow,
                "{other}"
            );
        }
    }
}

#[tokio::test]
async fn t04_submit_job_application_hint_asks() {
    // The seeded rule ('*','Submit:job_application','ask') from migration 0001 (H-1.2, D-39).
    let (_, g) = gate(MemStore::new());
    let mut c = call(Class::Submit, "jobs.example");
    assert_eq!(check(&g, &c).await, Decision::Allow);
    c.hints = vec!["job_application".into()];
    let card = card(check(&g, &c).await);
    assert_eq!(card.class, Class::Submit);
    // B-100 default: classes H-1.3 does not name time out to skip.
    assert_eq!(card.on_timeout, OnTimeout::Skip);
}

#[tokio::test]
async fn t04_strict_run_every_ask_times_out_skip() {
    let (s, g) = gate(MemStore::new());
    s.add_rule("bank.com", "Submit", Mode::Ask, false).unwrap();
    let mut job = call(Class::Submit, "jobs.example");
    job.hints = vec!["job_application".into()];
    let asks = vec![
        send(&["stranger@example.org"]),
        send(&[]),
        call(Class::Pay, "shop.example"),
        call(Class::Account, "luma.example"),
        call(Class::Delete, "mail.example"),
        call(Class::Submit, "bank.com"),
        job,
    ];
    for mut c in asks {
        c.strict = true;
        let card = card(check(&g, &c).await);
        assert_eq!(card.on_timeout, OnTimeout::Skip, "{} strict run", c.class);
    }
}

#[tokio::test]
async fn t04_strict_rule_times_out_skip() {
    let (s, g) = gate(MemStore::new());
    s.add_rule("mail.example", "Send", Mode::Ask, true).unwrap();
    s.add_rule("strict.example", "Account", Mode::Ask, true)
        .unwrap();
    s.add_rule("strict.example", "Pay", Mode::Ask, true)
        .unwrap();

    let c = card(check(&g, &send(&["stranger@example.org"])).await);
    assert_eq!(c.on_timeout, OnTimeout::Skip, "Send under strict rule");
    let c = card(check(&g, &call(Class::Account, "strict.example")).await);
    assert_eq!(c.on_timeout, OnTimeout::Skip, "Account under strict rule");
    let c = card(check(&g, &call(Class::Pay, "strict.example")).await);
    assert_eq!(c.on_timeout, OnTimeout::Skip, "Pay under strict rule");
    // the same class on a domain without the strict rule still proceeds
    let c = card(check(&g, &call(Class::Account, "lenient.example")).await);
    assert_eq!(
        c.on_timeout,
        OnTimeout::Proceed,
        "Account without strict rule"
    );
}

#[tokio::test]
async fn t04_touch_contact_then_send_allows() {
    // docs/02 acceptance: touch_contact makes a Send to that person allow on the next check.
    for mk in stores() {
        let (s, g) = gate(mk());
        let to = send(&["new.person@example.net"]);
        assert!(matches!(check(&g, &to).await, Decision::Ask(_)));
        s.touch_contact("New.Person@Example.net", "mail", "met at meetup")
            .await
            .unwrap();
        assert_eq!(check(&g, &to).await, Decision::Allow);
    }
}

#[tokio::test]
async fn t04_ask_card_contents() {
    let (_, g) = gate(MemStore::new());

    // Card built from scrubbed args: class, site, what, values, 60 s.
    let mut pay = call(Class::Pay, "shop.example");
    pay.tool = "shop.checkout".into();
    pay.args = json!({"card": "@vault(card)", "amount": "12 EUR"});
    let c = card(check(&g, &pay).await);
    assert_eq!(c.class, Class::Pay);
    assert_eq!(c.site.as_deref(), Some("shop.example"));
    assert!(c.what.contains("shop.checkout"), "what: {}", c.what);
    assert!(!c.what.trim().is_empty());
    assert!(c
        .values
        .contains(&("card".to_string(), "@vault(card)".to_string())));
    assert!(c
        .values
        .contains(&("amount".to_string(), "12 EUR".to_string())));
    assert_eq!(c.timeout, Duration::from_secs(60));

    // Card for a Send names the recipient.
    let s = card(check(&g, &send(&["stranger@example.org"])).await);
    assert_eq!(s.class, Class::Send);
    assert_eq!(s.site.as_deref(), Some("mail.example"));
    assert!(s.what.contains("stranger@example.org"), "what: {}", s.what);
    assert!(!s.values.is_empty());
    assert_eq!(s.timeout, Duration::from_secs(60));

    // Classifier-provided what and values are used as given.
    pay.what = Some("confirm order on shop.example".into());
    pay.values = vec![("total".into(), "12 EUR".into())];
    let c = card(check(&g, &pay).await);
    assert_eq!(c.what, "confirm order on shop.example");
    assert_eq!(c.values, pay.values);
    assert_eq!(c.timeout, Duration::from_secs(60));

    // The card never carries anything but the `@vault(name)` placeholder: the gate has no
    // vault and the canary (if set) cannot appear in a card built from scrubbed args.
    if let Ok(canary) = std::env::var("SB_VAULT_CANARY") {
        let ser = serde_json::to_string(&c).unwrap();
        assert!(
            !ser.contains(&canary),
            "canary found in a serialized ask card"
        );
        assert!(
            !format!("{c:?}").contains(&canary),
            "canary found in card Debug"
        );
    }
}
