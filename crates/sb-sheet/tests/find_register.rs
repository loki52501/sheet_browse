//! docs/05 acceptance 6 on the hand-built register.html index (B-6.2, D-54, D-55, D-202).

mod common;

use common::register_rows;
use sb_sheet::{find, find_lines, FindHit, FindOpts, HitReason};

const QUERY: &str = "email, password, agree, register";

fn run(query: &str, k: usize) -> Vec<FindHit> {
    find(
        &register_rows(),
        &[],
        &FindOpts {
            query: query.into(),
            k,
        },
    )
}

fn ids(h: &[FindHit]) -> Vec<String> {
    h.iter().map(|h| h.elem.to_string()).collect()
}

#[test]
fn acceptance_6_k1() {
    let hits = run(QUERY, 1);
    assert_eq!(ids(&hits), ["#6", "#7", "#10", "#11", "#12", "#17"]);
    let reasons: Vec<HitReason> = hits.iter().map(|h| h.reason).collect();
    assert_eq!(
        reasons,
        [
            HitReason::Match { part: 0 },
            HitReason::Match { part: 1 },
            HitReason::Match { part: 2 },
            HitReason::Challenge,
            // Register is both the part-3 top hit and the winning submit: Match wins (D-202).
            HitReason::Match { part: 3 },
            HitReason::Overlay,
        ]
    );
    assert!(hits.iter().all(|h| !h.weak), "{hits:?}");
    assert_eq!(
        find_lines(&register_rows(), &hits),
        [
            "#6 input \"Email\" empty",
            "#7 input \"Password\" empty",
            "#10 checkbox \"I agree to the Terms of Service\" unchecked",
            "#11 iframe \"reCAPTCHA\" CHALLENGE OPAQUE",
            "#12 button \"Register\" SUBMIT",
            "#17 region \"Cookie consent\" OVERLAY",
        ]
        .join("\n")
    );
}

#[test]
fn k3_newsletter_email_ranks_below_signup_email() {
    let hits = run(QUERY, 3);
    let got = ids(&hits);
    for must in ["#6", "#7", "#10", "#11", "#12", "#17"] {
        assert!(
            got.contains(&must.to_string()),
            "{must} missing from {got:?}"
        );
    }
    // document order
    let mut sorted = hits.iter().map(|h| h.elem.id).collect::<Vec<_>>();
    sorted.sort_unstable();
    assert_eq!(sorted, hits.iter().map(|h| h.elem.id).collect::<Vec<_>>());
    let score = |id: &str| {
        hits.iter()
            .find(|h| h.elem.to_string() == id)
            .map(|h| h.score)
    };
    // At k = 3 the newsletter Email is the second "email" hit, one group bonus below #6.
    let nl = score("#13").expect("newsletter email at k = 3");
    let signup = score("#6").unwrap();
    assert!(signup > nl, "newsletter {nl} vs signup {signup}");
    assert!(
        (signup - nl - 1.0).abs() < 1e-4,
        "only the group bonus separates them"
    );
    // never the hidden promo rows
    assert!(!got.contains(&"#4".to_string()) && !got.contains(&"#5".to_string()));
}

#[test]
fn hidden_promo_never_appears() {
    for (q, k) in [
        ("email for the offer", 3),
        ("register for the offer", 3),
        ("offer", 5),
        (QUERY, 10),
    ] {
        let got = ids(&run(q, k));
        assert!(
            !got.contains(&"#4".to_string()) && !got.contains(&"#5".to_string()),
            "{q}: {got:?}"
        );
    }
}

#[test]
fn single_email_part_prefers_signup_and_appends_its_submit() {
    let hits = run("email", 1);
    assert_eq!(ids(&hits), ["#6", "#11", "#12", "#17"]);
    assert_eq!(hits[2].reason, HitReason::WinningSubmit);
}

#[test]
fn newsletter_query_wins_footer_form() {
    let hits = run("subscribe", 1);
    assert_eq!(ids(&hits), ["#11", "#14", "#17"]);
    assert_eq!(hits[1].reason, HitReason::Match { part: 0 });
}

#[test]
fn forced_role_and_synonyms_on_register() {
    // "sign up" is a synonym of register; forcing button skips the "Sign in" link.
    let hits = run("sign up:button", 1);
    assert!(ids(&hits).contains(&"#12".to_string()), "{hits:?}");
    assert!(!ids(&hits).contains(&"#3".to_string()), "{hits:?}");
    // e-mail spelled differently still finds the signup email
    let hits = run("e-mail", 1);
    assert_eq!(ids(&hits)[0], "#6");
    // name:select forces the combobox
    let hits = run("plan:select", 1);
    assert_eq!(ids(&hits), ["#9", "#11", "#12", "#17"]);
}

#[test]
fn empty_query_lists_only_challenge_and_overlay() {
    let hits = run("", 3);
    assert_eq!(ids(&hits), ["#11", "#17"]);
}

#[test]
fn password_value_never_in_find_lines() {
    let mut rows = register_rows();
    rows[6].state.value = Some(sb_sheet::FieldValue::Text("s3cr3t-fixture".into()));
    let hits = find(
        &rows,
        &[],
        &FindOpts {
            query: "password".into(),
            k: 1,
        },
    );
    let out = find_lines(&rows, &hits);
    assert!(out.contains("#7 input \"Password\" filled"), "{out}");
    assert!(!out.contains("s3cr3t"));
}
