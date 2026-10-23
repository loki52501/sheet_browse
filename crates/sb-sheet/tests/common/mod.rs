//! Hand-built index of bench/pages/register.html, in document order, as injected.js would
//! emit it PLUS the display:none promo rows marked `visible: false` (as srv-web's
//! approximated walk might, W-2). Shared by find_register.rs and skeleton_sheet.rs.
#![allow(dead_code)]

use sb_sheet::{BlockKind, ElemRef, FieldValue, Flags, IndexRow, Span, State, TextBlock};

pub const URL: &str = "http://127.0.0.1:8000/register.html";
pub const TITLE: &str = "Create your account · Fixture Co";

fn row(id: u32, role: &str, name: &str) -> IndexRow {
    IndexRow {
        id,
        frame: 0,
        role: role.into(),
        name: name.into(),
        nearby: None,
        heading: None,
        state: State::default(),
        flags: Flags::default(),
        form: None,
        landmark: None,
        child_frame: None,
        bbox: None,
    }
}

fn input(id: u32, name: &str, ty: &str) -> IndexRow {
    let mut r = row(id, "textbox", name);
    r.state.input_type = Some(ty.into());
    r.state.value = Some(FieldValue::Empty);
    r
}

fn in_form(mut r: IndexRow, form: &str, heading: &str, landmark: &str) -> IndexRow {
    r.form = Some(form.into());
    r.heading = Some(heading.into());
    r.landmark = Some(landmark.into());
    r
}

fn at(mut r: IndexRow, landmark: &str) -> IndexRow {
    r.landmark = Some(landmark.into());
    r
}

pub fn register_rows() -> Vec<IndexRow> {
    const SIGNUP: &str = "Create your account";
    const NEWS: &str = "Stay in the loop";
    let mut v = vec![
        at(row(1, "link", "Home"), "nav"),
        at(row(2, "link", "Pricing"), "nav"),
        at(row(3, "link", "Sign in"), "nav"),
    ];

    // .promo display:none: srv-web may still emit it, never visible.
    let mut promo_email = at(input(4, "Email for the offer", "email"), "main");
    promo_email.heading = Some("Limited offer".into());
    promo_email.state.visible = false;
    v.push(promo_email);
    let mut promo_btn = at(row(5, "button", "Register for the offer"), "main");
    promo_btn.heading = Some("Limited offer".into());
    promo_btn.state.visible = false;
    v.push(promo_btn);

    let mut email = in_form(input(6, "Email", "email"), "f1", SIGNUP, "main");
    email.state.autocomplete = Some("email".into());
    v.push(email);
    let mut pw = in_form(input(7, "Password", "password"), "f1", SIGNUP, "main");
    pw.state.autocomplete = Some("new-password".into());
    v.push(pw);
    v.push(in_form(input(8, "Full name", "text"), "f1", SIGNUP, "main"));
    let mut plan = in_form(row(9, "combobox", "Plan"), "f1", SIGNUP, "main");
    plan.state.options = vec!["Basic".into(), "Plus".into(), "Pro".into()];
    plan.state.value = Some(FieldValue::Text("Basic".into()));
    v.push(plan);
    let mut agree = in_form(
        row(10, "checkbox", "I agree to the Terms of Service"),
        "f1",
        SIGNUP,
        "main",
    );
    agree.state.checked = Some(false);
    agree.state.input_type = Some("checkbox".into());
    v.push(agree);
    // D-55: the .g-recaptcha container and its iframe are ONE row.
    let mut cap = in_form(row(11, "iframe", "reCAPTCHA"), "f1", SIGNUP, "main");
    cap.flags.challenge = true;
    cap.flags.opaque = true;
    v.push(cap);
    let mut reg = in_form(row(12, "button", "Register"), "f1", SIGNUP, "main");
    reg.flags.submit = true;
    reg.state.submitish = true;
    v.push(reg);

    v.push(in_form(input(13, "Email", "email"), "f2", NEWS, "footer"));
    let mut sub = in_form(row(14, "button", "Subscribe"), "f2", NEWS, "footer");
    sub.flags.submit = true;
    sub.state.submitish = true;
    v.push(sub);
    let mut privacy = at(row(15, "link", "Privacy"), "footer");
    privacy.heading = Some(NEWS.into());
    v.push(privacy);
    let mut terms = at(row(16, "link", "Terms"), "footer");
    terms.heading = Some(NEWS.into());
    v.push(terms);

    let mut banner = row(17, "region", "Cookie consent");
    banner.flags.overlay = true;
    v.push(banner);
    v.push(at(row(18, "button", "Accept all"), "region"));
    v.push(at(row(19, "button", "Reject"), "region"));
    v
}

fn span(text: &str, elem: Option<u32>) -> Span {
    Span {
        text: text.into(),
        elem: elem.map(ElemRef::main),
    }
}

fn block(order: u32, kind: BlockKind, level: Option<u8>, lm: &str, spans: Vec<Span>) -> TextBlock {
    TextBlock {
        order,
        frame: 0,
        kind,
        level,
        landmark: Some(lm.into()),
        heading: None,
        table: None,
        header: false,
        spans,
    }
}

/// Visible text blocks (label text consumed as names is not re-emitted, B-5).
pub fn register_text() -> Vec<TextBlock> {
    use BlockKind::*;
    let mut v = vec![
        block(
            0,
            Paragraph,
            None,
            "nav",
            vec![
                span("Home", Some(1)),
                span("Pricing", Some(2)),
                span("Sign in", Some(3)),
            ],
        ),
        block(
            1,
            Heading,
            Some(1),
            "main",
            vec![span("Create your account", None)],
        ),
        block(
            2,
            Paragraph,
            None,
            "main",
            vec![span("Start your free trial. No card required.", None)],
        ),
        block(
            3,
            Heading,
            Some(2),
            "footer",
            vec![span("Stay in the loop", None)],
        ),
        block(
            4,
            Paragraph,
            None,
            "footer",
            vec![
                span("Privacy", Some(15)),
                span(" · ", None),
                span("Terms", Some(16)),
            ],
        ),
        block(
            5,
            Paragraph,
            None,
            "region",
            vec![span("We use cookies to run this site.", None)],
        ),
    ];
    v[2].heading = Some("Create your account".into());
    v[4].heading = Some("Stay in the loop".into());
    v
}
