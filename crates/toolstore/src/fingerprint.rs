//! T-4 fingerprint check and relocate over an I-5 index (live or fetched), per D-304.
//!
//! Role aliases (D-49) come from sb-sheet's data/roles.txt through
//! `sb_sheet::data::{aria_roles, alias_for}` (D-59), the one table find, sheets and
//! fingerprints share. Names compare case-, space- and punctuation-insensitively (`Email *` = `email`).
//! Relocate re-finds a missing item among unused rows of the same role by name synonyms or
//! token overlap. Everything here is pure and linear in the index size. A row whose name
//! holds a control character or U+2028/U+2029, or a form id a header line cannot hold, is
//! never copied into a fingerprint (D-578).

use crate::header::{fp_item_writable, header_unsafe, FpItem};
use crate::Tool;
use sb_sheet::data::{alias_for, aria_roles};
use sb_sheet::IndexRow;

/// T-4 outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FingerprintResult {
    Match,
    /// Relocate (T-4, <= 200 ms) re-finds these by role + name synonyms.
    Partial {
        missing: Vec<FpItem>,
    },
    Miss,
}

/// Phrase synonym groups for relocate, compared after [`norm`].
const SYNONYMS: &[&[&str]] = &[
    &[
        "register",
        "sign up",
        "signup",
        "join",
        "rsvp",
        "get ticket",
        "get tickets",
        "request to join",
    ],
    &[
        "submit", "send", "continue", "next", "confirm", "done", "save",
    ],
    &[
        "email",
        "e mail",
        "email address",
        "your email",
        "work email",
    ],
    &["name", "full name", "your name"],
    &["log in", "login", "sign in", "signin"],
    &["log out", "logout", "sign out"],
    &["search", "find", "go"],
    &[
        "apply",
        "apply now",
        "submit application",
        "apply for this job",
    ],
    &["cancel", "close", "dismiss", "no thanks"],
    &[
        "phone",
        "telephone",
        "mobile",
        "phone number",
        "mobile number",
    ],
    &["first name", "given name"],
    &["last name", "surname", "family name"],
    &["message", "comment", "comments", "note"],
    &["accept", "agree", "i agree", "accept all", "ok", "got it"],
];

/// `Email *` -> `email`; `Sign-Up` -> `sign up`.
pub fn norm(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                ' '
            }
        })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Does `row_role` (ARIA) satisfy the item role (alias or ARIA, D-49)? Yes when they are
/// the same role, when the item is the alias a sheet line prints for the row
/// (`sb_sheet::data::alias_for`), or when the row's role is one the item alias stands for
/// (`sb_sheet::data::aria_roles`, so `textarea` covers `textbox`). Case-insensitive.
pub fn role_matches(item_role: &str, row_role: &str) -> bool {
    let item = item_role.trim().to_ascii_lowercase();
    let row = row_role.trim().to_ascii_lowercase();
    item == row || alias_for(&row) == item || aria_roles(&item).contains(&row)
}

fn is_form_item(item: &FpItem) -> bool {
    item.role.eq_ignore_ascii_case("form") && item.id.is_some()
}

/// The row an item matches exactly, skipping rows in `used`.
fn find_exact(item: &FpItem, index: &[IndexRow], used: &[usize]) -> Option<usize> {
    let want = item.name.as_deref().map(norm);
    index.iter().enumerate().position(|(i, r)| {
        !used.contains(&i)
            && role_matches(&item.role, &r.role)
            && want.as_ref().is_none_or(|w| norm(&r.name) == *w)
    })
}

fn form_present(id: &str, index: &[IndexRow]) -> bool {
    index.iter().any(|r| r.form.as_deref() == Some(id))
}

fn synonym_group(n: &str) -> Option<usize> {
    SYNONYMS.iter().position(|g| g.contains(&n))
}

/// Name similarity in [0, 1]: same synonym group = 1; else token overlap coefficient.
fn similarity(a: &str, b: &str) -> f32 {
    let (a, b) = (norm(a), norm(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    if let (Some(x), Some(y)) = (synonym_group(&a), synonym_group(&b)) {
        if x == y {
            return 1.0;
        }
    }
    let ta: Vec<&str> = a.split(' ').collect();
    let tb: Vec<&str> = b.split(' ').collect();
    let canon = |t: &str| synonym_group(t).map(|g| SYNONYMS[g][0].to_string());
    let same = |x: &str, y: &str| x == y || (canon(x).is_some() && canon(x) == canon(y));
    let inter = ta.iter().filter(|x| tb.iter().any(|y| same(x, y))).count();
    inter as f32 / ta.len().min(tb.len()) as f32
}

const MIN_SIMILARITY: f32 = 0.5;

/// Matched row (if any) for each non-form item, in item order.
fn exact_rows(items: &[FpItem], index: &[IndexRow]) -> Vec<Option<usize>> {
    let mut used = Vec::new();
    items
        .iter()
        .map(|item| {
            if is_form_item(item) {
                return None;
            }
            let hit = find_exact(item, index, &used);
            if let Some(i) = hit {
                used.push(i);
            }
            hit
        })
        .collect()
}

fn item_found(item: &FpItem, row: Option<usize>, index: &[IndexRow]) -> bool {
    if is_form_item(item) {
        form_present(item.id.as_deref().unwrap_or(""), index)
    } else {
        row.is_some()
    }
}

/// The form the found rows mostly live in.
fn dominant_form(rows: &[usize], index: &[IndexRow]) -> Option<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for &i in rows {
        if let Some(f) = &index[i].form {
            match counts.iter_mut().find(|(k, _)| k == f) {
                Some((_, n)) => *n += 1,
                None => counts.push((f.clone(), 1)),
            }
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(f, _)| f)
        .or_else(|| index.iter().find_map(|r| r.form.clone()))
}

/// Re-finds every missing item; the full new fingerprint, or `None` when any item cannot be
/// relocated or nothing is missing.
fn relocate_items(items: &[FpItem], index: &[IndexRow]) -> Option<Vec<FpItem>> {
    let rows = exact_rows(items, index);
    let mut used: Vec<usize> = rows.iter().flatten().copied().collect();
    let mut out: Vec<Option<FpItem>> = vec![None; items.len()];
    let mut missing_any = false;
    for (k, item) in items.iter().enumerate() {
        if item_found(item, rows[k], index) {
            out[k] = Some(item.clone());
        } else if !is_form_item(item) {
            missing_any = true;
            let form = dominant_form(&used, index);
            let mut best: Option<(f32, usize)> = None;
            for (i, r) in index.iter().enumerate() {
                if used.contains(&i) || !role_matches(&item.role, &r.role) {
                    continue;
                }
                // D-578: a name a header line cannot hold is never copied into @fingerprint.
                if item.name.is_some() && r.name.chars().any(header_unsafe) {
                    continue;
                }
                let mut score = match &item.name {
                    Some(n) => similarity(n, &r.name),
                    None => 1.0,
                };
                if score < MIN_SIMILARITY {
                    continue;
                }
                if form.is_some() && r.form == form {
                    score += 0.1;
                }
                if best.is_none_or(|(s, _)| score > s) {
                    best = Some((score, i));
                }
            }
            let (_, i) = best?;
            used.push(i);
            out[k] = Some(FpItem {
                role: item.role.clone(),
                name: item.name.as_ref().map(|_| index[i].name.clone()),
                id: item.id.clone(),
            });
        }
    }
    // Form items last, so they can follow the rows found above.
    for (k, item) in items.iter().enumerate() {
        if out[k].is_none() {
            missing_any = true;
            let form = dominant_form(&used, index)?;
            let patched = FpItem {
                role: item.role.clone(),
                name: item.name.clone(),
                id: Some(form),
            };
            // D-578: a form id a header line cannot hold (or read back) is no relocation.
            if !fp_item_writable(&patched) {
                return None;
            }
            out[k] = Some(patched);
        }
    }
    if !missing_any {
        return None;
    }
    out.into_iter().collect()
}

/// I-7 `fingerprint(tool, index)` (T-4, D-304): all items found -> match; at least half found
/// and every missing item relocatable -> partial(missing); otherwise miss. An empty
/// fingerprint matches.
pub fn fingerprint(tool: &Tool, index: &[IndexRow]) -> FingerprintResult {
    fingerprint_items(&tool.header.fingerprint, index)
}

/// [`fingerprint`] over bare items.
pub fn fingerprint_items(items: &[FpItem], index: &[IndexRow]) -> FingerprintResult {
    let rows = exact_rows(items, index);
    let missing: Vec<FpItem> = items
        .iter()
        .zip(&rows)
        .filter(|(item, row)| !item_found(item, **row, index))
        .map(|(item, _)| item.clone())
        .collect();
    if missing.is_empty() {
        return FingerprintResult::Match;
    }
    let found = items.len() - missing.len();
    if found * 2 >= items.len() && relocate_items(items, index).is_some() {
        FingerprintResult::Partial { missing }
    } else {
        FingerprintResult::Miss
    }
}

/// T-4 relocate: the patched `@fingerprint` with every missing item re-found by role and name
/// synonyms / token overlap, or `None` when something cannot be re-found (or nothing is
/// missing). Does not apply the half-found rule; `fingerprint` does.
pub fn relocate(tool: &Tool, index: &[IndexRow]) -> Option<Vec<FpItem>> {
    relocate_items(&tool.header.fingerprint, index)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::header::parse_fingerprint;
    use sb_sheet::{Flags, State};

    pub fn row(id: u32, role: &str, name: &str, form: Option<&str>) -> IndexRow {
        IndexRow {
            id,
            frame: 0,
            role: role.into(),
            name: name.into(),
            nearby: None,
            heading: None,
            state: State::default(),
            flags: Flags::default(),
            form: form.map(String::from),
            landmark: None,
            child_frame: None,
            bbox: None,
        }
    }

    pub fn page(button: &str) -> Vec<IndexRow> {
        vec![
            row(1, "link", "Home", None),
            row(2, "textbox", "Full name", Some("f2")),
            row(3, "textbox", "Email *", Some("f2")),
            row(4, "button", button, Some("f2")),
            row(5, "textbox", "Newsletter email", Some("f9")),
        ]
    }

    fn fp(s: &str) -> Vec<FpItem> {
        parse_fingerprint(s).unwrap()
    }

    #[test]
    fn match_on_aliases_and_case() {
        let items = fp(r#"form#f2 button"register" input"EMAIL" input"Full  name""#);
        assert_eq!(
            fingerprint_items(&items, &page("Register")),
            FingerprintResult::Match
        );
        assert_eq!(fingerprint_items(&[], &[]), FingerprintResult::Match);
        assert!(role_matches("select", "combobox"));
        assert!(role_matches("textbox", "textbox"));
        assert!(!role_matches("input", "button"));
    }

    /// B-300 / D-59: every alias the old built-in map handled still matches, now through
    /// sb-sheet's roles.txt. One test per alias.
    #[test]
    fn alias_input_matches_textbox_and_searchbox() {
        assert!(role_matches("input", "textbox"));
        assert!(role_matches("input", "searchbox"));
        assert!(role_matches("INPUT", "TextBox"));
        assert!(!role_matches("input", "combobox"));
    }

    #[test]
    fn alias_textarea_matches_textbox() {
        assert!(role_matches("textarea", "textbox"));
        assert!(!role_matches("textarea", "searchbox"));
    }

    #[test]
    fn alias_button_matches_button() {
        assert!(role_matches("button", "button"));
        assert!(!role_matches("button", "link"));
    }

    #[test]
    fn alias_select_matches_combobox_and_listbox() {
        assert!(role_matches("select", "combobox"));
        assert!(role_matches("select", "listbox"));
        assert!(!role_matches("select", "textbox"));
    }

    #[test]
    fn alias_checkbox_matches_checkbox() {
        assert!(role_matches("checkbox", "checkbox"));
        assert!(!role_matches("checkbox", "radio"));
    }

    #[test]
    fn alias_radio_matches_radio() {
        assert!(role_matches("radio", "radio"));
        assert!(!role_matches("radio", "checkbox"));
    }

    #[test]
    fn alias_link_matches_link() {
        assert!(role_matches("link", "link"));
        assert!(!role_matches("link", "button"));
    }

    #[test]
    fn alias_form_matches_form() {
        assert!(role_matches("form", "form"));
        assert!(!role_matches("form", "region"));
    }

    #[test]
    fn alias_tab_and_table_match() {
        assert!(role_matches("tab", "tab"));
        assert!(role_matches("table", "table"));
        assert!(role_matches("table", "grid"));
    }

    /// Aliases the old map lacked now come from roles.txt (one table for find and
    /// fingerprints, D-49).
    #[test]
    fn roles_txt_aliases_beyond_the_old_map() {
        assert!(role_matches("image", "img"));
        assert!(role_matches("dialog", "alertdialog"));
        assert!(role_matches("nav", "navigation"));
    }

    /// Where roles.txt differs from the old map, sb-sheet wins (D-59): `input` no longer
    /// covers `spinbutton` (roles.txt: `number`/`spinbutton`), `checkbox` no longer covers
    /// `switch` (roles.txt: `switch`).
    #[test]
    fn roles_txt_wins_over_the_old_map() {
        assert!(!role_matches("input", "spinbutton"));
        assert!(role_matches("number", "spinbutton"));
        assert!(role_matches("spinbutton", "spinbutton"));
        assert!(!role_matches("checkbox", "switch"));
        assert!(role_matches("switch", "switch"));
    }

    #[test]
    fn partial_when_button_renamed() {
        let items = fp(r#"form#f2 button"Register" input"Email""#);
        let idx = page("Sign up");
        assert_eq!(
            fingerprint_items(&items, &idx),
            FingerprintResult::Partial {
                missing: fp(r#"button"Register""#)
            }
        );
        assert_eq!(
            relocate_items(&items, &idx).unwrap(),
            fp(r#"form#f2 button"Sign up" input"Email""#)
        );
    }

    #[test]
    fn relocate_by_token_overlap_and_form_change() {
        let items = fp(r#"form#f2 input"Email" button"Register""#);
        let idx = vec![
            row(10, "textbox", "Email address", Some("f7")),
            row(11, "button", "Register now", Some("f7")),
        ];
        assert_eq!(
            fingerprint_items(&items, &idx),
            FingerprintResult::Miss,
            "only 0 of 3 exactly found"
        );
        assert_eq!(
            relocate_items(&items, &idx).unwrap(),
            fp(r#"form#f7 input"Email address" button"Register now""#)
        );
    }

    #[test]
    fn miss_when_nothing_relocatable() {
        let items = fp(r#"form#f2 button"Register" input"Email""#);
        let idx = vec![
            row(2, "textbox", "Email", Some("f2")),
            row(4, "button", "Delete account", Some("f2")),
        ];
        assert_eq!(fingerprint_items(&items, &idx), FingerprintResult::Miss);
        assert_eq!(relocate_items(&items, &idx), None);
        assert_eq!(fingerprint_items(&items, &[]), FingerprintResult::Miss);
    }

    #[test]
    fn relocate_none_when_nothing_missing() {
        let items = fp(r#"button"Register""#);
        assert_eq!(relocate_items(&items, &page("Register")), None);
    }

    #[test]
    fn rows_are_not_reused() {
        // Two Email items cannot both match the one Email row.
        let items = fp(r#"input"Email" input"Email""#);
        let idx = vec![row(1, "textbox", "Email", None)];
        assert_eq!(fingerprint_items(&items, &idx), FingerprintResult::Miss);
    }
}
