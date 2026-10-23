//! T-19 sb-act acceptance round 3, items 12-18: D-406 (control-character keys, Enter takes
//! the form max on every row, the one-time-code exemption, challenge targets before `?s`,
//! the per-form evidence cache, delete headings without a form) and D-407 (final word
//! lists), plus the item-18 lowering guard over every fixture in `common::ALL`. Public API
//! only; fixtures are I-5 wire JSON in `common/mod.rs`.

use std::time::{Duration, Instant};

use harness_core::Class;
use sb_act::{check_planner, classify, classify_step, parse, quote, PlannerError, Step};
use sb_sheet::{ElemRef, IndexRow};

mod common;
use common::{ALL, CHECKOUT, D406, D407, HINTS, IMPLICIT, PLANNER};

fn rows(json: &str) -> Vec<IndexRow> {
    serde_json::from_str(json).expect("I-5 wire JSON")
}

fn class_of(program: &str, index: &[IndexRow]) -> Class {
    let steps = parse(program).unwrap();
    assert_eq!(steps.len(), 1, "{program}");
    classify_step(&steps[0], index).class
}

/// `#N` for frame 0, `#fF.N` otherwise (B-7 frame refs).
fn ref_of(r: &IndexRow) -> String {
    if r.frame == 0 {
        format!("#{}", r.id)
    } else {
        format!("#f{}.{}", r.frame, r.id)
    }
}

/// Class of `k<ref>=<key>` reached two ways: through the parser with the key quoted by
/// `q()` (D-52, so raw control characters survive), and as a hand-built `Step::Key`. Both
/// must agree.
fn key_class(row: &IndexRow, key: &str, index: &[IndexRow]) -> Class {
    let program = format!("k{}={}", ref_of(row), quote(key));
    let steps = parse(&program).unwrap_or_else(|e| panic!("{program:?}: {e}"));
    assert_eq!(steps.len(), 1, "{program:?}");
    assert_eq!(
        steps[0],
        Step::Key {
            target: ElemRef {
                frame: row.frame,
                id: row.id
            },
            key: key.to_string()
        },
        "{program:?}"
    );
    let parsed = classify_step(&steps[0], index).class;
    let built = classify_step(
        &Step::Key {
            target: ElemRef {
                frame: row.frame,
                id: row.id,
            },
            key: key.to_string(),
        },
        index,
    )
    .class;
    assert_eq!(parsed, built, "{program:?}");
    parsed
}

fn row(index: &[IndexRow], id: u32) -> &IndexRow {
    index
        .iter()
        .find(|r| r.id == id && r.frame == 0)
        .unwrap_or_else(|| panic!("no row #{id}"))
}

const LINE_BREAK_KEYS: &[&str] = &["\r", "\n", "\r\n", "Shift+\r"];
/// Tab, DEL, Backspace, ETX (Ctrl+C), and also ESC and NEL from D-406 (1)'s handoff list.
const CONTROL_KEYS: &[&str] = &["\t", "\u{7f}", "\u{8}", "\u{3}", "\u{1b}", "\u{85}"];

// ---- item 12: D-406 (1) key values with control characters ----

#[test]
fn d406_line_break_keys_on_pay_form_field_are_pay() {
    let idx = rows(D406);
    for f in [200, 201] {
        let r = row(&idx, f);
        assert_eq!(key_class(r, "Enter", &idx), Class::Pay, "#{f} Enter");
        for k in LINE_BREAK_KEYS {
            assert_eq!(key_class(r, k, &idx), Class::Pay, "#{f} {k:?}");
        }
    }
    // The card-only form without a submit row (D-403 (2)).
    let imp = rows(IMPLICIT);
    for f in [110, 111, 112] {
        for k in LINE_BREAK_KEYS {
            assert_eq!(key_class(row(&imp, f), k, &imp), Class::Pay, "#{f} {k:?}");
        }
    }
    // The `\n` escape inside a quoted value (D-52) reaches the same class.
    assert_eq!(class_of(r#"k#200="\n""#, &idx), Class::Pay);
    // On the Pay submit itself.
    for k in LINE_BREAK_KEYS {
        assert_eq!(key_class(row(&idx, 206), k, &idx), Class::Pay, "#206 {k:?}");
    }
}

#[test]
fn d406_control_keys_on_pay_form_field_are_at_least_click_never_fill() {
    let idx = rows(D406);
    for f in [200, 201] {
        let r = row(&idx, f);
        let click = class_of(&format!("c#{f}"), &idx);
        for k in CONTROL_KEYS {
            let got = key_class(r, k, &idx);
            assert!(got >= click, "#{f} {k:?}: {got:?} < click {click:?}");
            assert_ne!(got, Class::Fill, "#{f} {k:?}");
        }
    }
    let imp = rows(IMPLICIT);
    for f in [110, 111, 112] {
        let click = class_of(&format!("c#{f}"), &imp);
        for k in CONTROL_KEYS {
            let got = key_class(row(&imp, f), k, &imp);
            assert!(got >= click, "#{f} {k:?}: {got:?} < click {click:?}");
            assert_ne!(got, Class::Fill, "#{f} {k:?}");
        }
    }
    // On the Pay submit, a control-character key is its click class, Pay.
    for k in CONTROL_KEYS {
        assert_eq!(key_class(row(&idx, 206), k, &idx), Class::Pay, "#206 {k:?}");
    }
}

// ---- item 13: D-406 (2) Enter on any row in a form, file inputs ----

#[test]
fn d406_enter_on_switch_checkbox_link_radio_in_checkout_is_pay() {
    let idx = rows(D406);
    for (id, what) in [
        (202, "switch Save card"),
        (203, "checkbox Save card for next time"),
        (204, "link Terms of sale"),
        (205, "radio Visa ending 4242"),
    ] {
        assert_eq!(
            class_of(&format!("k#{id}=Enter"), &idx),
            Class::Pay,
            "{what}"
        );
        assert_eq!(
            class_of(&format!("k#{id}=NumpadEnter"), &idx),
            Class::Pay,
            "{what}"
        );
        // A click on it is still its own class (not Pay).
        assert!(class_of(&format!("c#{id}"), &idx) < Class::Pay, "{what}");
    }
    // The saved-card checkout (D-58): Enter on its radio is Pay.
    let co = rows(CHECKOUT);
    assert_eq!(class_of("k#30=Enter", &co), Class::Pay);
    assert_eq!(class_of("c#30", &co), Class::Fill);
}

#[test]
fn d406_enter_on_show_password_in_login_is_submit() {
    let idx = rows(D406);
    assert_eq!(class_of("c#212", &idx), Class::Navigate);
    assert_eq!(class_of("k#212=Enter", &idx), Class::Submit);
    assert_eq!(key_class(row(&idx, 212), "\r", &idx), Class::Submit);
    assert_eq!(class_of("c#213", &idx), Class::Submit);
}

#[test]
fn d406_file_input_space_is_click() {
    let idx = rows(D406);
    let hints = rows(HINTS);
    for (r, index) in [
        (row(&idx, 215), &idx),
        (row(&idx, 216), &idx),
        (row(&hints, 51), &hints),
    ] {
        let click = class_of(&format!("c{}", ref_of(r)), index);
        for k in ["Space", " ", "\u{a0}", "a", "Backspace"] {
            assert_eq!(key_class(r, k, index), click, "#{} {k:?}", r.id);
        }
    }
    // Round 2's observation is closed: Space on the resume input is no longer Fill.
    assert_ne!(class_of("k#51=Space", &hints), Class::Fill);
}

// ---- item 14: D-406 (3) one-time-code exemption ----

#[test]
fn d406_cvc_with_one_time_code_is_pay_security_code_is_submit() {
    let idx = rows(D406);
    assert_eq!(class_of("c#221", &idx), Class::Pay, "CVC + one-time-code");
    assert_eq!(
        class_of("k#220=Enter", &idx),
        Class::Pay,
        "CVC + one-time-code"
    );
    assert_eq!(
        class_of("c#223", &idx),
        Class::Submit,
        "Security code + otc"
    );
    assert_eq!(
        class_of("k#222=Enter", &idx),
        Class::Submit,
        "Security code + otc"
    );
}

/// One field plus a neutral "Continue" SUBMIT, with or without autocomplete one-time-code.
fn one_field_form(name: &str, otc: bool) -> Vec<IndexRow> {
    let ac = if otc {
        r#","autocomplete":"one-time-code""#
    } else {
        ""
    };
    rows(&format!(
        r#"[
  {{"id":1,"frame":0,"role":"textbox","name":"{name}","heading":"Your details",
   "state":{{"input_type":"text"{ac}}},"form":"f1"}},
  {{"id":2,"frame":0,"role":"button","name":"Continue","heading":"Your details",
   "state":{{"submitish":true}},"flags":{{"submit":true}},"form":"f1"}}
]"#
    ))
}

#[test]
fn d406_other_card_words_with_one_time_code_still_pay() {
    for name in [
        "CVC",
        "CVV",
        "CSC",
        "Card number",
        "Cardholder name",
        "Name on card",
        "Card expiry",
        "Expiry (MM/YY)",
        "MM / YY",
        "Card security code (CVC)",
    ] {
        let idx = one_field_form(name, true);
        assert_eq!(class_of("c#2", &idx), Class::Pay, "{name}");
        assert_eq!(class_of("k#1=Enter", &idx), Class::Pay, "{name}");
    }
    // The exemption never lowers anything else: for every name, one-time-code gives the
    // same class as no autocomplete, except the bare "security code" phrase.
    for name in ["Expiry", "Passport expiry", "Code", "CVC", "Card expiry"] {
        assert_eq!(
            class_of("c#2", &one_field_form(name, true)),
            class_of("c#2", &one_field_form(name, false)),
            "{name}"
        );
    }
}

// ---- item 15: D-406 (4) challenge before MissingStop ----

#[test]
fn d406_challenge_target_reported_before_missing_stop() {
    let idx = rows(PLANNER);
    // c#14 alone would be MissingStop at step 1.
    assert_eq!(
        check_planner(&parse("c#14").unwrap(), &idx),
        Err(PlannerError::MissingStop {
            step: 1,
            class: Class::Submit
        })
    );
    for (p, step) in [
        ("c#14; c#42; ?s", 2),
        ("c#14; t#7=x; c#42", 3),
        ("k#14=Enter; sc(#42)", 2),
        ("t#7=a; k#7=Enter; k#42=Enter; ?s", 3),
    ] {
        let e = check_planner(&parse(p).unwrap(), &idx).unwrap_err();
        assert_eq!(e, PlannerError::ChallengeTarget { step }, "{p}");
    }
    assert_eq!(
        check_planner(&parse("t#7=a; c#14; ?s").unwrap(), &idx),
        Ok(())
    );
}

// ---- item 16: D-406 (5) linear Enter on a big form ----

fn big_form() -> Vec<IndexRow> {
    let mut json = String::from("[");
    let mut id = 1u32;
    for i in 0..1500 {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            r#"{{"id":{id},"frame":0,"role":"textbox","name":"Question {i}","heading":"Survey",
"state":{{"input_type":"text"}},"form":"f1"}}"#
        ));
        id += 1;
    }
    for i in 0..200 {
        json.push_str(&format!(
            r#",{{"id":{id},"frame":0,"role":"button","name":"Save section {i}","heading":"Survey",
"state":{{"submitish":true}},"flags":{{"submit":true}},"form":"f1"}}"#
        ));
        id += 1;
    }
    json.push(']');
    let idx = rows(&json);
    assert_eq!(idx.len(), 1700);
    assert_eq!(idx.iter().filter(|r| r.flags.submit).count(), 200);
    idx
}

#[test]
fn d406_enter_in_200_submit_1700_row_form_under_250ms() {
    let idx = big_form();
    let budget = Duration::from_millis(250);
    let cases: [(&str, Vec<Step>); 4] = [
        ("k#1=Enter", parse("k#1=Enter").unwrap()),
        ("k#1700=Enter", parse("k#1700=Enter").unwrap()),
        (
            "classify k#1..k#5=Enter",
            parse("k#1=Enter; k#2=Enter; k#3=Enter; k#4=Enter; k#5=Enter").unwrap(),
        ),
        (
            "check_planner k#750=Enter; ?s",
            parse("k#750=Enter; ?s").unwrap(),
        ),
    ];
    for (what, steps) in &cases {
        for run in 0..3 {
            let t0 = Instant::now();
            let class = if what.starts_with("check_planner") {
                check_planner(steps, &idx).unwrap();
                Class::Submit
            } else if steps.len() == 1 {
                classify_step(&steps[0], &idx).class
            } else {
                classify(steps, &idx).class
            };
            let dt = t0.elapsed();
            eprintln!("{what} run {run}: {dt:?}");
            assert_eq!(class, Class::Submit, "{what}");
            assert!(dt < budget, "{what} run {run}: {dt:?} >= {budget:?}");
        }
    }
}

// ---- item 17: D-406 (6)(7), D-407 ----

#[test]
fn d406_confirm_under_delete_heading_without_form_or_dialog_is_delete() {
    let idx = rows(D407);
    let c = row(&idx, 230);
    assert!(c.form.is_none() && c.landmark.is_none() && !c.flags.submit);
    assert_eq!(class_of("c#230", &idx), Class::Delete);
    assert_eq!(class_of("k#230=Enter", &idx), Class::Delete);
    assert_eq!(class_of("c#231", &idx), Class::Navigate); // bare Cancel
    assert_eq!(class_of("c#232", &idx), Class::Navigate); // "Save changes?"
}

#[test]
fn d407_new_pay_words_are_pay() {
    let idx = rows(D407);
    for (id, what) in [
        (240, "Submit order"),
        (241, "Upgrade (submit)"),
        (242, "Complete order"),
        (243, "Order now"),
        (244, "Top up"),
        (245, "Send money"),
        (246, "Transfer funds"),
        (247, "Renew (submit)"),
        (248, "Upgrade (non-submit button)"),
    ] {
        assert_eq!(class_of(&format!("c#{id}"), &idx), Class::Pay, "{what}");
        assert_eq!(
            class_of(&format!("k#{id}=Enter"), &idx),
            Class::Pay,
            "{what}"
        );
    }
}

#[test]
fn d407_pay_negatives() {
    let idx = rows(D407);
    assert_eq!(
        class_of("c#251", &idx),
        Class::Navigate,
        "Order history link"
    );
    assert_eq!(class_of("c#249", &idx), Class::Navigate, "Renew link");
    assert_eq!(class_of("c#250", &idx), Class::Navigate, "Upgrade link");
    assert_eq!(
        class_of("c#253", &idx),
        Class::Submit,
        "Renewal date + Save"
    );
    assert_eq!(
        class_of("k#252=Enter", &idx),
        Class::Submit,
        "Renewal date field"
    );
    assert_eq!(class_of("c#254", &idx), Class::Submit, "Book now");
}

#[test]
fn d407_passkeys() {
    let idx = rows(D407);
    assert_eq!(
        class_of("c#260", &idx),
        Class::Account,
        "Create a passkey button"
    );
    assert_eq!(
        class_of("c#261", &idx),
        Class::Account,
        "Create a passkey heading"
    );
    assert_eq!(
        class_of("c#262", &idx),
        Class::Submit,
        "Sign in with a passkey heading"
    );
    assert_eq!(
        class_of("c#263", &idx),
        Class::Submit,
        "Sign in with a passkey button"
    );
}

#[test]
fn d407_clear_history_is_delete_cancel_and_close_is_navigate() {
    let idx = rows(D407);
    for (id, what) in [
        (270, "Clear history"),
        (271, "Clear browsing data"),
        (272, "Clear all"),
        (273, "Empty trash"),
    ] {
        assert_eq!(class_of(&format!("c#{id}"), &idx), Class::Delete, "{what}");
    }
    assert_eq!(class_of("c#280", &idx), Class::Navigate, "Cancel and close");
    assert_eq!(
        class_of("c#281", &idx),
        Class::Navigate,
        "Cancel or go back"
    );
}

// ---- item 18: lowering guard, D-406 rules, every fixture ----

fn field_rule(form: &[&IndexRow]) -> Class {
    let ac = |r: &IndexRow, p: &str| {
        r.state
            .autocomplete
            .as_deref()
            .is_some_and(|a| a.split_whitespace().any(|t| t.starts_with(p)))
    };
    let passwords = form
        .iter()
        .filter(|r| r.state.input_type.as_deref() == Some("password"))
        .count();
    if form.iter().any(|r| ac(r, "cc-")) {
        Class::Pay
    } else if passwords >= 2 || form.iter().any(|r| ac(r, "new-password")) {
        Class::Account
    } else {
        Class::Submit
    }
}

/// For every row of every fixture (CHALLENGE rows excepted, they are Challenge):
/// - D-406 (1): `\r`, `\n`, `\r\n` and `Shift+\r` equal Enter; every control-character key
///   is at least a click on the row;
/// - D-406 (2): in a form, Enter on any row (any role) is at least Submit, the form's
///   field rule, a click on the row and a click on every submit of the form;
/// - D-406 (2): on a file input, Space and a letter equal a click on the row.
#[test]
fn lowering_guard_d406_every_fixture() {
    let mut checked = 0;
    for (name, json) in ALL {
        let idx = rows(json);
        for r in &idx {
            let rf = ref_of(r);
            if r.flags.challenge {
                assert_eq!(key_class(r, "\r", &idx), Class::Challenge, "{name} {rf}");
                continue;
            }
            let click = class_of(&format!("c{rf}"), &idx);
            let enter = key_class(r, "Enter", &idx);
            for k in LINE_BREAK_KEYS {
                assert_eq!(key_class(r, k, &idx), enter, "{name} k{rf}={k:?}");
                checked += 1;
            }
            for k in CONTROL_KEYS {
                let got = key_class(r, k, &idx);
                assert!(got >= click, "{name} k{rf}={k:?}: {got:?} < {click:?}");
                checked += 1;
            }
            if let Some(f) = r.form.as_deref() {
                let form: Vec<&IndexRow> = idx
                    .iter()
                    .filter(|x| x.frame == r.frame && x.form.as_deref() == Some(f))
                    .collect();
                let mut floor = field_rule(&form).max(click).max(Class::Submit);
                for s in form
                    .iter()
                    .filter(|x| (x.flags.submit || x.state.submitish) && !x.flags.challenge)
                {
                    floor = floor.max(class_of(&format!("c{}", ref_of(s)), &idx));
                }
                assert!(enter >= floor, "{name} k{rf}=Enter: {enter:?} < {floor:?}");
                checked += 1;
            }
            let file = r
                .state
                .input_type
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case("file"));
            if file {
                for k in ["Space", "a"] {
                    assert_eq!(key_class(r, k, &idx), click, "{name} k{rf}={k}");
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 500, "only {checked} checks");
}
