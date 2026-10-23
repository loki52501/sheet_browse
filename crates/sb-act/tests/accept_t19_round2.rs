//! T-19 sb-act acceptance round 2, items 7-11: D-403 (key steps, implicit submission,
//! wider evidence), D-404 (2FA set-up verbs, delete dialogs, the Delete key, planner
//! challenge rejection) and a lowering guard over every fixture in the suite. Public API
//! only; fixtures are I-5 wire JSON in `common/mod.rs`.

use harness_core::Class;
use sb_act::{check_planner, classify, classify_step, parse, PlannerError};
use sb_sheet::IndexRow;

mod common;
use common::{ALL, CHECKOUT, D404, EVIDENCE, IMPLICIT, KEYS, REGISTER};

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

// ---- item 7: D-403 (1) key steps ----

#[test]
fn d403_activating_keys_on_delete_button_are_delete() {
    let idx = rows(KEYS);
    assert_eq!(class_of("c#100", &idx), Class::Delete);
    for k in ["Enter", "Space", "NumpadEnter"] {
        assert_eq!(class_of(&format!("k#100={k}"), &idx), Class::Delete, "{k}");
    }
}

#[test]
fn d403_activating_keys_on_submit_place_order_are_pay() {
    let idx = rows(KEYS);
    assert_eq!(class_of("c#101", &idx), Class::Pay);
    for k in ["Enter", "Space", "NumpadEnter"] {
        assert_eq!(class_of(&format!("k#101={k}"), &idx), Class::Pay, "{k}");
    }
}

#[test]
fn d403_activating_keys_on_link_are_navigate() {
    let idx = rows(KEYS);
    for k in ["Enter", "Space", "NumpadEnter"] {
        assert_eq!(
            class_of(&format!("k#103={k}"), &idx),
            Class::Navigate,
            "{k}"
        );
    }
}

#[test]
fn d403_focus_keys_are_fill_on_any_row() {
    let idx = rows(KEYS);
    for target in [100, 101, 102, 103, 104, 105, 106, 107] {
        for k in ["Tab", "Shift+Tab", "Escape", "ArrowDown"] {
            assert_eq!(
                class_of(&format!("k#{target}={k}"), &idx),
                Class::Fill,
                "k#{target}={k}"
            );
        }
    }
}

#[test]
fn d403_unknown_key_on_button_is_its_click_class() {
    let idx = rows(KEYS);
    let reg = rows(REGISTER);
    for (target, want, index) in [
        (100, Class::Delete, &idx),
        (101, Class::Pay, &idx),
        (105, Class::Navigate, &idx),
        (10, Class::Account, &reg),
        (12, Class::Submit, &reg),
    ] {
        let click = class_of(&format!("c#{target}"), index);
        assert_eq!(click, want, "c#{target}");
        for k in ["F7", "F13", "Control+K", "x"] {
            assert_eq!(
                class_of(&format!("k#{target}={k}"), index),
                click,
                "k#{target}={k}"
            );
        }
    }
}

// ---- item 8: D-403 (2) implicit submission ----

#[test]
fn d403_enter_in_card_only_form_without_submit_row_is_pay() {
    let idx = rows(IMPLICIT);
    assert!(!idx.iter().any(|r| r.flags.submit || r.state.submitish));
    for f in [110, 111, 112] {
        assert_eq!(class_of(&format!("k#{f}=Enter"), &idx), Class::Pay, "#{f}");
        assert_eq!(
            class_of(&format!("k#{f}=NumpadEnter"), &idx),
            Class::Pay,
            "#{f}"
        );
        // Typing is still Fill.
        assert_eq!(class_of(&format!("t#{f}=4242"), &idx), Class::Fill, "#{f}");
    }
}

#[test]
fn d403_enter_in_formless_textbox_is_submit() {
    let idx = rows(IMPLICIT);
    assert_eq!(class_of("k#120=Enter", &idx), Class::Submit);
    let keys = rows(KEYS);
    assert_eq!(class_of("k#104=Enter", &keys), Class::Submit);
    // Typing into it is Fill.
    assert_eq!(class_of("k#120=a", &idx), Class::Fill);
}

// ---- item 9: D-403 (3)(4)(5) evidence ----

#[test]
fn d403_change_email_with_one_current_password_is_account() {
    let idx = rows(EVIDENCE);
    assert_eq!(class_of("c#132", &idx), Class::Account);
    assert_eq!(class_of("k#130=Enter", &idx), Class::Account);
    assert_eq!(class_of("k#131=Enter", &idx), Class::Account);
    // The change is named only by the form's name.
    assert_eq!(class_of("c#138", &idx), Class::Account);
    assert_eq!(class_of("k#137=Enter", &idx), Class::Account);
}

#[test]
fn d403_delete_account_heading_with_confirm_submit_is_delete() {
    let idx = rows(EVIDENCE);
    assert_eq!(class_of("c#141", &idx), Class::Delete);
    assert_eq!(class_of("k#140=Enter", &idx), Class::Delete);
    assert_eq!(class_of("c#142", &idx), Class::Navigate); // bare Cancel
}

#[test]
fn d403_card_fields_without_autocomplete_are_pay() {
    let idx = rows(EVIDENCE);
    for (field, submit, what) in [
        (150, 151, "CVC"),
        (152, 153, "Card expiry"),
        (154, 155, "MM / YY"),
        (156, 157, "Security code"),
    ] {
        assert!(idx
            .iter()
            .filter(|r| r.id == field)
            .all(|r| r.state.autocomplete.is_none()));
        assert_eq!(class_of(&format!("c#{submit}"), &idx), Class::Pay, "{what}");
        assert_eq!(
            class_of(&format!("k#{field}=Enter"), &idx),
            Class::Pay,
            "{what}"
        );
    }
}

#[test]
fn d403_buyer_info_heading_alone_is_not_pay() {
    let idx = rows(EVIDENCE);
    assert_eq!(class_of("c#162", &idx), Class::Submit);
    assert_eq!(class_of("k#160=Enter", &idx), Class::Submit);
}

// ---- item 10: D-404 ----

#[test]
fn d404_two_step_verification_code_prompt_is_submit() {
    let idx = rows(D404);
    assert_eq!(class_of("c#171", &idx), Class::Submit);
    assert_eq!(class_of("c#172", &idx), Class::Submit);
    assert_eq!(class_of("k#170=Enter", &idx), Class::Submit);
    assert_eq!(class_of("t#170=123456", &idx), Class::Fill);
}

#[test]
fn d404_enable_two_factor_is_account() {
    let idx = rows(D404);
    assert_eq!(class_of("c#176", &idx), Class::Account); // set-up verb in the heading
    assert_eq!(class_of("k#175=Enter", &idx), Class::Account);
    assert_eq!(class_of("c#177", &idx), Class::Account); // set-up verb on the button
}

#[test]
fn d404_delete_key_on_mail_row_is_delete_and_in_textbox_is_fill() {
    let keys = rows(KEYS);
    for k in ["Delete", "Shift+Delete", "Del"] {
        assert_eq!(class_of(&format!("k#106={k}"), &keys), Class::Delete, "{k}");
        assert_eq!(class_of(&format!("k#104={k}"), &keys), Class::Fill, "{k}");
    }
    let ev = rows(EVIDENCE);
    assert_eq!(class_of("k#130=Delete", &ev), Class::Fill);
    assert_eq!(class_of("k#131=Delete", &ev), Class::Fill); // password field
                                                            // Delete never lowers a higher click class (Pay stays Pay).
    assert_eq!(class_of("k#101=Delete", &keys), Class::Pay);
}

#[test]
fn d404_delete_account_dialog_confirm_is_delete_cancel_is_navigate() {
    let idx = rows(D404);
    let confirm = idx.iter().find(|r| r.id == 180).unwrap();
    assert!(!confirm.flags.submit && !confirm.state.submitish && confirm.form.is_none());
    assert_eq!(class_of("c#180", &idx), Class::Delete);
    assert_eq!(class_of("k#180=Enter", &idx), Class::Delete);
    assert_eq!(class_of("c#181", &idx), Class::Navigate);
    // A Confirm in a different dialog does not borrow the Delete heading.
    assert_eq!(class_of("c#182", &idx), Class::Navigate);
}

#[test]
fn d404_planner_rejects_challenge_target_with_or_without_stop() {
    let idx = rows(REGISTER);
    for (p, step) in [
        ("c#9", 1),
        ("c#9; ?s", 1),
        ("t#4=a; c#9; ?s", 2),
        ("t#4=a; c#9", 2),
        ("t#9=x; ?s", 1),
        ("k#9=Enter; ?s", 1),
        ("sc(#9); ?s", 1),
        ("t#4=a; c#10; ?s; c#9; ?s", 4),
    ] {
        let e = check_planner(&parse(p).unwrap(), &idx).unwrap_err();
        assert_eq!(e, PlannerError::ChallengeTarget { step }, "{p}");
        assert!(e.to_string().contains(&format!("step {step}")), "{e}");
        assert!(e.to_string().contains("D-5"), "{e}");
    }
    // A program that never targets the challenge is still fine.
    assert_eq!(
        check_planner(&parse("t#4=a; c#10; ?s").unwrap(), &idx),
        Ok(())
    );
}

// ---- item 11: lowering guard ----

/// The purely field-based rule for a submit (B-7 as first written): a `cc-*` field in
/// the form -> Pay; two passwords or a new-password in the
/// form -> Account; else Submit. No text evidence at all.
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
    // A CHALLENGE row in the form is its own target; a submit next to one keeps the
    // class of its fields (D-403 last sentence), so the floor is not lifted to Challenge.
    if form.iter().any(|r| ac(r, "cc-")) {
        Class::Pay
    } else if passwords >= 2 || form.iter().any(|r| ac(r, "new-password")) {
        Class::Account
    } else {
        Class::Submit
    }
}

fn form_rows<'a>(row: &'a IndexRow, index: &'a [IndexRow]) -> Vec<&'a IndexRow> {
    match row.form.as_deref() {
        Some(f) => index
            .iter()
            .filter(|r| r.frame == row.frame && r.form.as_deref() == Some(f))
            .collect(),
        None => vec![row],
    }
}

#[test]
fn lowering_guard_every_fixture_submit_is_at_least_its_field_class() {
    let mut checked = 0;
    for (name, json) in ALL {
        let idx = rows(json);
        for row in &idx {
            let r = ref_of(row);
            if row.flags.challenge {
                assert_eq!(
                    class_of(&format!("c{r}"), &idx),
                    Class::Challenge,
                    "{name} {r}"
                );
                continue;
            }
            let form = form_rows(row, &idx);
            if row.flags.submit || row.state.submitish {
                let floor = field_rule(&form);
                for p in [format!("c{r}"), format!("k{r}=Enter"), format!("k{r}=F7")] {
                    let got = class_of(&p, &idx);
                    assert!(got >= floor, "{name} {p}: {got:?} < field rule {floor:?}");
                    checked += 1;
                }
            }
            // Enter in a field of a form with a submit row submits that form: never below
            // the form's field-based class.
            // D-403 (1) makes printable keys (Space too) typed into a text field Fill;
            // D-406 (2) says a file input is not a text field, so it gets the Space check.
            let file_input = row
                .state
                .input_type
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case("file"));
            let text_field = matches!(row.role.as_str(), "textbox" | "combobox") && !file_input;
            if (text_field || file_input) && row.form.is_some() {
                let floor = field_rule(&form);
                let p = format!("k{r}=Enter");
                let got = class_of(&p, &idx);
                assert!(got >= floor, "{name} {p}: {got:?} < field rule {floor:?}");
                assert!(got >= Class::Submit, "{name} {p}: {got:?}");
                checked += 1;
            }
            // A key step is never below a click on the same row, except the focus keys
            // D-403 (1) lists as Fill and Delete typed in a text field.
            let click = class_of(&format!("c{r}"), &idx);
            for k in ["Enter", "Space", "F7"] {
                if k == "Space" && text_field {
                    continue; // typing a space into a textbox (D-403 (1))
                }
                let p = format!("k{r}={k}");
                let got = class_of(&p, &idx);
                assert!(got >= click, "{name} {p}: {got:?} < click {click:?}");
                checked += 1;
            }
        }
    }
    assert!(checked > 100, "only {checked} checks");
}

#[test]
fn lowering_guard_sanity_register_account_checkout_pay() {
    let reg = rows(REGISTER);
    assert_eq!(class_of("c#10", &reg), Class::Account);
    assert_eq!(class_of("k#4=Enter", &reg), Class::Account);
    assert_eq!(
        classify(&parse("t#4=a@b.c; t#5=@vault(pw); c#10; ?s").unwrap(), &reg).class,
        Class::Account
    );
    let co = rows(CHECKOUT);
    assert_eq!(class_of("c#31", &co), Class::Pay);
    assert_eq!(class_of("k#31=Enter", &co), Class::Pay);
    let keys = rows(KEYS);
    assert_eq!(class_of("c#101", &keys), Class::Pay);
}
