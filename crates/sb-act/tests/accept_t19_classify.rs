//! T-19 acceptance items 4 and 5 (docs/05 B-7, docs/01 H-1, D-50, D-58, D-402, D-433,
//! D-434). Index fixtures are I-5 wire JSON, deserialized the same way as
//! sb-sheet's `index_row_wire_shape` test.

use harness_core::gate::{HINT_JOB_APPLICATION, HINT_SIGNATURE};
use harness_core::Class;
use sb_act::{check_planner, classify, classify_step, parse, PlannerError};
use sb_sheet::IndexRow;

/// bench/pages/register.html as the live walk would ship it (B-5): nav links, #signup (f1)
/// with email, new-password, full name, plan select, agree checkbox, the reCAPTCHA
/// container/iframe as one CHALLENGE + OPAQUE row (D-55) and the Register submit; the
/// newsletter form (f2); the cookie banner (OVERLAY). The hidden promo is skipped.
const REGISTER: &str = r#"[
  {"id":1,"frame":0,"role":"link","name":"Home","landmark":"nav"},
  {"id":2,"frame":0,"role":"link","name":"Pricing","landmark":"nav"},
  {"id":3,"frame":0,"role":"link","name":"Sign in","landmark":"nav"},
  {"id":4,"frame":0,"role":"textbox","name":"Email","heading":"Create your account",
   "state":{"value":{"kind":"empty"},"input_type":"email","autocomplete":"email"},
   "form":"f1","landmark":"main","box":{"x":16,"y":180,"w":608,"h":40}},
  {"id":5,"frame":0,"role":"textbox","name":"Password","heading":"Create your account",
   "state":{"value":{"kind":"empty"},"input_type":"password","autocomplete":"new-password"},
   "form":"f1","landmark":"main"},
  {"id":6,"frame":0,"role":"textbox","name":"Full name","heading":"Create your account",
   "state":{"value":{"kind":"empty"},"input_type":"text"},"form":"f1","landmark":"main"},
  {"id":7,"frame":0,"role":"combobox","name":"Plan","heading":"Create your account",
   "state":{"value":{"kind":"text","text":"Basic"},"options":["Basic","Plus","Pro"]},
   "form":"f1","landmark":"main"},
  {"id":8,"frame":0,"role":"checkbox","name":"I agree to the Terms of Service",
   "heading":"Create your account","state":{"checked":false,"input_type":"checkbox"},
   "form":"f1","landmark":"main"},
  {"id":9,"frame":0,"role":"iframe","name":"reCAPTCHA","heading":"Create your account",
   "flags":{"challenge":true,"opaque":true},"form":"f1","landmark":"main","child_frame":1},
  {"id":10,"frame":0,"role":"button","name":"Register","heading":"Create your account",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f1","landmark":"main"},
  {"id":11,"frame":0,"role":"textbox","name":"Email","heading":"Stay in the loop",
   "state":{"input_type":"email"},"form":"f2","landmark":"footer"},
  {"id":12,"frame":0,"role":"button","name":"Subscribe","heading":"Stay in the loop",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f2","landmark":"footer"},
  {"id":13,"frame":0,"role":"button","name":"Accept all","flags":{"overlay":true},"landmark":"region"},
  {"id":14,"frame":0,"role":"button","name":"Reject","flags":{"overlay":true},"landmark":"region"}
]"#;

/// A login page whose heading mentions create account (D-433: still a login, Submit).
const LOGIN: &str = r#"[
  {"id":20,"frame":0,"role":"textbox","name":"Email","heading":"Sign in or create an account",
   "state":{"input_type":"email","autocomplete":"username"},"form":"f4"},
  {"id":21,"frame":0,"role":"textbox","name":"Password","heading":"Sign in or create an account",
   "state":{"value":{"kind":"filled"},"input_type":"password","autocomplete":"current-password"},"form":"f4"},
  {"id":22,"frame":0,"role":"button","name":"Sign in","heading":"Sign in or create an account",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f4"}
]"#;

/// Saved-card checkout (D-58): no card field, only a saved-card radio and "Place order".
const CHECKOUT: &str = r#"[
  {"id":30,"frame":0,"role":"radio","name":"Visa ending 4242","heading":"Payment method",
   "state":{"checked":true,"input_type":"radio"},"form":"f5"},
  {"id":31,"frame":0,"role":"button","name":"Place order","heading":"Review your order",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f5"},
  {"id":32,"frame":0,"role":"link","name":"Return to cart"}
]"#;

/// Unsubscribe form, a cancel-something button, a bare Cancel and a bare Close.
const MANAGE: &str = r#"[
  {"id":40,"frame":0,"role":"textbox","name":"Email","heading":"Email preferences",
   "state":{"input_type":"email"},"form":"f6"},
  {"id":41,"frame":0,"role":"button","name":"Unsubscribe","heading":"Email preferences",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f6"},
  {"id":42,"frame":0,"role":"button","name":"Cancel","heading":"Email preferences","form":"f6"},
  {"id":43,"frame":0,"role":"button","name":"Close","landmark":"dialog"},
  {"id":44,"frame":0,"role":"button","name":"Cancel subscription","heading":"Your plan"},
  {"id":45,"frame":0,"role":"link","name":"Unsubscribe from all mail","landmark":"footer"}
]"#;

/// Job application with a CV upload, an e-sign form, and a frame-scoped submit.
const HINTS: &str = r#"[
  {"id":50,"frame":0,"role":"textbox","name":"Full name","heading":"Apply for Senior Engineer",
   "state":{"input_type":"text"},"form":"f7"},
  {"id":51,"frame":0,"role":"textbox","name":"Resume","heading":"Apply for Senior Engineer",
   "state":{"input_type":"file"},"form":"f7"},
  {"id":52,"frame":0,"role":"button","name":"Submit","heading":"Apply for Senior Engineer",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f7"},
  {"id":60,"frame":0,"role":"textbox","name":"Type your full name","heading":"Lease agreement",
   "state":{"input_type":"text"},"form":"f8"},
  {"id":61,"frame":0,"role":"button","name":"Sign document","heading":"Lease agreement",
   "state":{"submitish":true},"flags":{"submit":true},"form":"f8"},
  {"id":15,"frame":2,"role":"textbox","name":"Comment","state":{"input_type":"text"},"form":"f1"},
  {"id":16,"frame":2,"role":"button","name":"Post","state":{"submitish":true},"flags":{"submit":true},"form":"f1"}
]"#;

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
