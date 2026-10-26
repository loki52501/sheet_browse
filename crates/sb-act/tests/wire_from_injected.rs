//! D-405 (10): the wire shape is proven against the Rust types. The fixtures are the real
//! `__sb.index()` and `__sb.text()` output of bench/pages/register.html in Chromium, written by
//! injected/test/gen-fixtures.js (browser.test.js checks they still match the live output).
//! They must deserialize into `Vec<IndexRow>` / `Vec<TextBlock>` (I-5, I-6), and the real
//! Register row must classify as Account (B-7, D-402).

use harness_core::Class;
use sb_act::{classify, parse};
use sb_sheet::{BlockKind, FieldValue, IndexRow, TextBlock};

const INDEX: &str = include_str!("../../../injected/test/fixtures/register.index.json");
const TEXT: &str = include_str!("../../../injected/test/fixtures/register.text.json");

fn rows() -> Vec<IndexRow> {
    serde_json::from_str(INDEX).expect("injected index() JSON deserializes into Vec<IndexRow>")
}

fn blocks() -> Vec<TextBlock> {
    serde_json::from_str(TEXT).expect("injected text() JSON deserializes into Vec<TextBlock>")
}

fn by_name<'a>(rows: &'a [IndexRow], name: &str, role: &str) -> Vec<&'a IndexRow> {
    rows.iter()
        .filter(|r| r.name == name && r.role == role)
        .collect()
}

#[test]
fn index_rows_deserialize_with_key_rows() {
    let rows = rows();
    assert!(rows.len() > 15, "{} rows", rows.len());
    assert!(rows.iter().all(|r| r.frame == 0));
    assert!(
        rows.iter().all(|r| r.bbox.is_some()),
        "live rows carry a box"
    );

    let email = by_name(&rows, "Email", "textbox");
    assert_eq!(email.len(), 2);
    assert_eq!(email[0].form.as_deref(), Some("f1"));
    assert_eq!(email[1].form.as_deref(), Some("f2"));
    assert_eq!(email[0].state.input_type.as_deref(), Some("email"));
    assert_eq!(email[0].heading.as_deref(), Some("Create your account"));

    let pw = by_name(&rows, "Password", "textbox");
    assert_eq!(pw.len(), 1);
    assert_eq!(pw[0].state.value, Some(FieldValue::Empty));
    assert_eq!(pw[0].state.input_type.as_deref(), Some("password"));
    assert_eq!(pw[0].state.autocomplete.as_deref(), Some("new-password"));

    let reg = by_name(&rows, "Register", "button");
    assert_eq!(reg.len(), 1);
    assert!(reg[0].flags.submit && reg[0].state.submitish);
    assert_eq!(reg[0].form.as_deref(), Some("f1"));
    assert!(reg[0].state.covered, "the cookie banner covers Register");

    let ch: Vec<&IndexRow> = rows.iter().filter(|r| r.flags.challenge).collect();
    assert_eq!(
        ch.len(),
        1,
        "one CHALLENGE row for the reCAPTCHA container and frame"
    );
    assert_eq!(ch[0].role, "iframe");
    assert!(ch[0].flags.opaque);

    let ov: Vec<&IndexRow> = rows.iter().filter(|r| r.flags.overlay).collect();
    assert_eq!(ov.len(), 1);
    assert_eq!(ov[0].name, "Cookie consent");
    assert_eq!(ov[0].landmark.as_deref(), Some("region"));

    let plan = by_name(&rows, "Plan", "combobox");
    assert_eq!(plan[0].state.options, ["Basic", "Plus", "Pro"]);
    assert!(
        !rows.iter().any(|r| r.name.contains("offer")),
        "hidden promo absent"
    );
}

#[test]
fn register_submit_classifies_as_account() {
    let rows = rows();
    let reg = by_name(&rows, "Register", "button")[0];
    let program = format!("c#{}", reg.id);
    let steps = parse(&program).unwrap();
    assert_eq!(classify(&steps, &rows).class, Class::Account, "{program}");
    let ch = rows.iter().find(|r| r.flags.challenge).unwrap();
    let steps = parse(&format!("c#{}", ch.id)).unwrap();
    assert_eq!(classify(&steps, &rows).class, Class::Challenge);
}

#[test]
fn text_blocks_deserialize() {
    let blocks = blocks();
    assert!(!blocks.is_empty());
    for (i, b) in blocks.iter().enumerate() {
        assert_eq!(b.order as usize, i, "frame-local order is dense");
        assert_eq!(b.frame, 0);
    }
    let h1 = blocks
        .iter()
        .find(|b| b.kind == BlockKind::Heading && b.level == Some(1))
        .unwrap();
    assert_eq!(h1.spans[0].text, "Create your account");
    assert_eq!(h1.landmark.as_deref(), Some("main"));
    // linked spans point at index rows
    let rows = rows();
    let linked: Vec<_> = blocks
        .iter()
        .flat_map(|b| b.spans.iter())
        .filter_map(|s| s.elem)
        .collect();
    assert!(!linked.is_empty());
    for e in linked {
        assert!(rows.iter().any(|r| r.elem() == e), "{e} is an index row");
    }
    // consumed labels are not text again
    assert!(!blocks
        .iter()
        .any(|b| b.spans.iter().any(|s| s.text.contains("I agree"))));
}
