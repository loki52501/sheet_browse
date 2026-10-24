//! B-6.1 skeleton and B-6.3 sheet on the hand-built register.html index (D-200, D-203).

mod common;

use common::{register_rows, register_text, TITLE, URL};
use sb_sheet::{
    sheet, skeleton, tokens, FieldValue, IndexRow, PageMeta, SheetOpts, SkeletonOpts, TextBlock,
};

fn opts() -> SkeletonOpts {
    SkeletonOpts {
        meta: PageMeta {
            url: URL.into(),
            title: TITLE.into(),
            site_tools: None,
            dialog: None,
        },
    }
}

fn sheet_of(region: Option<&str>) -> String {
    sheet(
        &register_rows(),
        &register_text(),
        &SheetOpts {
            region: region.map(str::to_string),
        },
    )
}

const SKELETON: &str = "Create your account · Fixture Co
http://127.0.0.1:8000/register.html
landmarks: nav, main, footer, region
form f1 \"Create your account\": 5 fields, submit \"Register\"
form f2 \"Stay in the loop\": 1 field, submit \"Subscribe\"
h1 Create your account · h2 Stay in the loop
17 elements: 5 links, 4 buttons, 6 fields
! CHALLENGE #11 iframe \"reCAPTCHA\"
! OVERLAY #17 region \"Cookie consent\"
main: form";

#[test]
fn register_skeleton_exact_and_in_budget() {
    let s = skeleton(&register_rows(), &register_text(), &opts());
    assert_eq!(s, SKELETON);
    let t = tokens::estimate(&s);
    assert!((80..=150).contains(&t), "{t} tokens");
    assert!(!s.contains('\u{2014}') && !s.contains("--"));
    // hidden promo is not counted or named
    assert!(!s.contains("offer"));
    // deterministic
    assert_eq!(s, skeleton(&register_rows(), &register_text(), &opts()));
}

#[test]
fn skeleton_site_tools_line_verbatim_and_dialog() {
    let mut o = opts();
    o.meta.site_tools = Some("tools for this site: fixture.register(name,email)".into());
    o.meta.dialog = Some("Leave site?".into());
    let s = skeleton(&register_rows(), &register_text(), &o);
    assert!(s.contains("\n! dialog: \"Leave site?\"\n"), "{s}");
    assert!(
        s.contains("\ntools for this site: fixture.register(name,email)\nmain: form"),
        "{s}"
    );
    assert!(tokens::estimate(&s) <= 150);
}

#[test]
fn skeleton_rows_only() {
    let s = skeleton(&register_rows(), &[], &SkeletonOpts::default());
    assert!(
        s.starts_with("landmarks: nav, main, footer, region\n"),
        "{s}"
    );
    assert!(
        s.contains("h Create your account · h Stay in the loop"),
        "{s}"
    );
    assert!(s.ends_with("main: form"), "{s}");
}

#[test]
fn example_json_matches_hand_built_fixture() {
    #[derive(serde::Deserialize)]
    struct Page {
        url: String,
        title: String,
        rows: Vec<IndexRow>,
        text: Vec<TextBlock>,
    }
    let p: Page = serde_json::from_str(include_str!("../examples/register.index.json")).unwrap();
    assert_eq!(p.url, URL);
    assert_eq!(p.title, TITLE);
    assert_eq!(p.rows, register_rows());
    assert_eq!(p.text, register_text());
}

#[test]
fn whole_page_sheet() {
    let s = sheet_of(None);
    let expect = [
        "#1 link \"Home\"",
        "#2 link \"Pricing\"",
        "#3 link \"Sign in\"",
        "#4 input \"Email for the offer\" empty hidden",
        "#5 button \"Register for the offer\" hidden",
        "#6 input \"Email\" empty",
        "#7 input \"Password\" empty",
        "#8 input \"Full name\" empty",
        "#9 select \"Plan\" \"Basic\" [Basic, Plus, Pro]",
        "#10 checkbox \"I agree to the Terms of Service\" unchecked",
        "#11 iframe \"reCAPTCHA\" CHALLENGE OPAQUE",
        "#12 button \"Register\" SUBMIT",
        "#13 input \"Email\" empty",
        "#14 button \"Subscribe\" SUBMIT",
        "#15 link \"Privacy\"",
        "#16 link \"Terms\"",
        "#17 region \"Cookie consent\" OVERLAY",
        "#18 button \"Accept all\"",
        "#19 button \"Reject\"",
    ]
    .join("\n");
    assert_eq!(s, expect);
    assert!(!s.contains('\u{2014}'));
}

#[test]
fn sheet_regions() {
    let f1 = sheet_of(Some("f1"));
    assert_eq!(f1.lines().count(), 7);
    assert!(f1.starts_with("#6 ") && f1.ends_with("#12 button \"Register\" SUBMIT"));
    assert_eq!(
        sheet_of(Some("footer")),
        "#13 input \"Email\" empty\n#14 button \"Subscribe\" SUBMIT\n#15 link \"Privacy\"\n#16 link \"Terms\""
    );
    assert_eq!(sheet_of(Some("nav")).lines().count(), 3);
    assert_eq!(
        sheet_of(Some("region")),
        "#18 button \"Accept all\"\n#19 button \"Reject\""
    );
    // heading, plain or `## `, case-insensitive
    assert_eq!(
        sheet_of(Some("## stay in the loop")),
        sheet_of(Some("Stay in the loop"))
    );
    assert_eq!(sheet_of(Some("## Stay in the loop")).lines().count(), 4);
    assert_eq!(sheet_of(Some("F2")).lines().count(), 2);
    let none = sheet_of(Some("checkout"));
    assert!(
        none.starts_with("no region \"checkout\". regions: nav, main, footer, region, f1, f2"),
        "{none}"
    );
}

#[test]
fn sheet_never_prints_password_text() {
    let mut rows = register_rows();
    rows[6].state.value = Some(FieldValue::Text("s3cr3t-fixture".into()));
    let s = sheet(&rows, &[], &SheetOpts::default());
    assert!(s.contains("#7 input \"Password\" filled"));
    assert!(!s.contains("s3cr3t"));
}

#[test]
fn sheet_never_prints_challenge_frame_content() {
    let mut rows = register_rows();
    rows[10].child_frame = Some(3);
    let mut inside = rows[9].clone();
    inside.frame = 3;
    inside.id = 1;
    inside.name = "I'm not a robot".into();
    rows.insert(11, inside);
    for region in [None, Some("f1"), Some("main")] {
        let s = sheet(
            &rows,
            &[],
            &SheetOpts {
                region: region.map(str::to_string),
            },
        );
        assert!(
            !s.contains("robot") && !s.contains("#f3."),
            "{region:?}: {s}"
        );
    }
    let sk = skeleton(&rows, &[], &SkeletonOpts::default());
    assert!(!sk.contains("robot"));
}
