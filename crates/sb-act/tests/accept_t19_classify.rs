//! T-19 acceptance items 4 and 5 (docs/05 B-7, docs/01 H-1, D-50, D-58, D-402, D-433,
//! D-434). Index fixtures are I-5 wire JSON, deserialized the same way as
//! sb-sheet's `index_row_wire_shape` test.

use harness_core::gate::{HINT_JOB_APPLICATION, HINT_SIGNATURE};
use harness_core::Class;
use sb_act::{check_planner, classify, classify_step, parse, PlannerError};
use sb_sheet::IndexRow;

mod common;
use common::{CHECKOUT, HINTS, LOGIN, MANAGE, REGISTER};

fn rows(json: &str) -> Vec<IndexRow> {
    serde_json::from_str(json).expect("I-5 wire JSON")
}

fn class_of(program: &str, index: &[IndexRow]) -> Class {
    let steps = parse(program).unwrap();
    assert_eq!(steps.len(), 1, "{program}");
    classify_step(&steps[0], index).class
}

#[test]
fn register_fixture_deserializes_from_wire_json() {
    let idx = rows(REGISTER);
    assert_eq!(idx.len(), 14);
    assert!(idx[8].flags.challenge && idx[8].flags.opaque);
    assert!(idx[9].flags.submit && idx[9].state.submitish);
    assert_eq!(idx[4].state.autocomplete.as_deref(), Some("new-password"));
}

#[test]
fn register_submit_is_account() {
    let idx = rows(REGISTER);
    assert_eq!(class_of("c#10", &idx), Class::Account);
    // Enter in a signup field submits the form: same class (D-433).
    assert_eq!(class_of("k#4=Enter", &idx), Class::Account);
}

#[test]
fn recaptcha_row_is_challenge() {
    let idx = rows(REGISTER);
    assert_eq!(class_of("c#9", &idx), Class::Challenge);
    // Any step aimed at the CHALLENGE row, not only a click.
    assert_eq!(class_of("t#9=x", &idx), Class::Challenge);
}

#[test]
fn register_fields_are_fill() {
    let idx = rows(REGISTER);
    assert_eq!(class_of("t#4=a@b.c", &idx), Class::Fill);
    assert_eq!(class_of("t#5=@vault(pw)", &idx), Class::Fill);
    assert_eq!(class_of("s#7=Plus", &idx), Class::Fill);
    assert_eq!(class_of("c#8", &idx), Class::Fill); // checkbox
    assert_eq!(class_of("k#4=Tab", &idx), Class::Fill);
}

#[test]
fn other_steps_have_their_b7_class() {
    let idx = rows(REGISTER);
    assert_eq!(class_of("c#2", &idx), Class::Navigate); // link
    assert_eq!(class_of("sc(down)", &idx), Class::Navigate);
    assert_eq!(class_of("sc(#10)", &idx), Class::Navigate);
    assert_eq!(class_of("w(3s)", &idx), Class::Read);
    assert_eq!(class_of("?s", &idx), Class::Read);
    assert_eq!(class_of("c#12", &idx), Class::Submit); // newsletter Subscribe
    assert_eq!(class_of("c#99", &idx), Class::Submit); // missing target (D-402)
}

#[test]
fn login_is_submit_not_account() {
    let idx = rows(LOGIN);
    assert_eq!(class_of("c#22", &idx), Class::Submit);
    assert_eq!(class_of("k#21=Enter", &idx), Class::Submit);
    assert_eq!(class_of("t#21=@vault(pw)", &idx), Class::Fill);
}

#[test]
fn saved_card_place_order_is_pay() {
    let idx = rows(CHECKOUT);
    assert_eq!(class_of("c#31", &idx), Class::Pay);
    assert_eq!(class_of("c#30", &idx), Class::Fill); // choosing the saved card
    assert_eq!(class_of("c#32", &idx), Class::Navigate);
}

#[test]
fn unsubscribe_is_delete_and_bare_cancel_is_navigate() {
    let idx = rows(MANAGE);
    assert_eq!(class_of("c#41", &idx), Class::Delete); // submit "Unsubscribe"
    assert_eq!(class_of("c#45", &idx), Class::Delete); // non-submit unsubscribe link
    assert_eq!(class_of("c#44", &idx), Class::Delete); // "Cancel subscription"
    assert_eq!(class_of("c#42", &idx), Class::Navigate); // bare Cancel
    assert_eq!(class_of("c#43", &idx), Class::Navigate); // bare Close
}

#[test]
fn program_class_is_max_of_steps() {
    let idx = rows(REGISTER);
    let p = parse("t#4=a@b.c; t#5=@vault(pw); s#7=Plus; c#8; c#10; ?s").unwrap();
    assert_eq!(classify(&p, &idx).class, Class::Account);
    let p = parse("t#4=a@b.c; c#9; c#10; ?s").unwrap();
    assert_eq!(classify(&p, &idx).class, Class::Challenge);
    let p = parse("t#4=a@b.c; sc(down); w(1s)").unwrap();
    assert_eq!(classify(&p, &idx).class, Class::Navigate);
    let p = parse("w(1s); ?s").unwrap();
    assert_eq!(classify(&p, &idx).class, Class::Read);
    assert_eq!(classify(&[], &idx).class, Class::Read);
}

#[test]
fn hint_job_application() {
    let idx = rows(HINTS);
    let p = parse("t#50=Ada; u#51=files/cv.pdf; c#52; ?s").unwrap();
    let c = classify(&p, &idx);
    assert_eq!(c.class, Class::Submit);
    assert!(c.hints.iter().any(|h| h == HINT_JOB_APPLICATION), "{c:?}");
    assert!(!c.hints.iter().any(|h| h == HINT_SIGNATURE), "{c:?}");
}

#[test]
fn hint_signature() {
    let idx = rows(HINTS);
    let p = parse("t#60=Ada Lovelace; c#61; ?s").unwrap();
    let c = classify(&p, &idx);
    assert!(c.class >= Class::Submit, "{c:?}");
    assert!(c.hints.iter().any(|h| h == HINT_SIGNATURE), "{c:?}");
    assert!(!c.hints.iter().any(|h| h == HINT_JOB_APPLICATION), "{c:?}");
}

#[test]
fn no_hints_on_plain_login_or_signup() {
    let login = rows(LOGIN);
    let c = classify(&parse("c#22; ?s").unwrap(), &login);
    assert!(
        c.hints.is_empty(),
        "'Sign in' must not be a signature: {c:?}"
    );
    let reg = rows(REGISTER);
    let c = classify(&parse("c#10; ?s").unwrap(), &reg);
    assert!(c.hints.is_empty(), "{c:?}");
}

#[test]
fn frame_refs_classify_against_their_frame() {
    let idx = rows(HINTS);
    assert_eq!(class_of("t#f2.15=nice", &idx), Class::Fill);
    assert_eq!(class_of("c#f2.16", &idx), Class::Submit);
    // #16 in frame 0 does not exist: missing target is Submit, never borrowed from frame 2.
    assert_eq!(class_of("t#16=x", &idx), Class::Submit);
}

// ---- item 5: planner ?s rule (D-50) ----

#[test]
fn planner_accepts_fill_submit_stop() {
    let idx = rows(REGISTER);
    let p = parse("t#4=a; c#10; ?s").unwrap();
    assert_eq!(check_planner(&p, &idx), Ok(()));
}

#[test]
fn planner_rejects_submit_alone() {
    let idx = rows(REGISTER);
    let p = parse("c#10").unwrap();
    let e = check_planner(&p, &idx).unwrap_err();
    assert_eq!(
        e,
        PlannerError::MissingStop {
            step: 1,
            class: Class::Account
        }
    );
    assert!(e.to_string().contains("step 1"), "{e}");
}

#[test]
fn planner_rejects_step_after_submit() {
    let idx = rows(REGISTER);
    let p = parse("c#10; t#6=b").unwrap();
    assert_eq!(
        check_planner(&p, &idx),
        Err(PlannerError::MissingStop {
            step: 1,
            class: Class::Account
        })
    );
    // Numbering follows D-430 (empty steps skipped).
    let p = parse(";; t#4=a;; c#10; t#6=b").unwrap();
    let e = check_planner(&p, &idx).unwrap_err();
    assert_eq!(
        e,
        PlannerError::MissingStop {
            step: 2,
            class: Class::Account
        }
    );
    assert!(e.to_string().contains("step 2"), "{e}");
}
