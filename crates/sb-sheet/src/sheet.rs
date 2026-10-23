//! B-6.3 sheet and the sheet line format (D-203). Owned by coder A (T-10).
//!
//! Line: `#id alias "name" [state...] [FLAGS...]`, e.g. `#7 input "Email" empty`,
//! `#12 button "Register" SUBMIT`, `#11 iframe "reCAPTCHA" CHALLENGE OPAQUE`.
//! State words, in this order: value (`empty`, `filled`, or a quoted text capped at 40
//! chars; a password field only ever prints `empty|filled`), `checked|unchecked`,
//! `disabled`, `hidden`, `covered`, `new-tab`, options `[a, b, c, +N more]`.

use crate::data::alias_for;
use crate::{FieldValue, FrameId, IndexRow, SheetOpts, TextBlock};
use std::collections::HashSet;

/// Name cap in a sheet line, in chars.
const NAME_CAP: usize = 80;
/// Text value cap (D-203).
const VALUE_CAP: usize = 40;
/// Options shown before `+N more`.
const OPTIONS_CAP: usize = 5;
/// Per-option label cap.
const OPTION_CAP: usize = 30;

/// Collapses whitespace, swaps `"` for `'` (so the quoting stays unambiguous) and caps at
/// `cap` chars with a trailing `...`.
pub(crate) fn clean(text: &str, cap: usize) -> String {
    let collapsed = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('"', "'");
    if collapsed.chars().count() <= cap {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(cap.saturating_sub(3)).collect();
    out.truncate(out.trim_end().len());
    out.push_str("...");
    out
}

/// A password-type field: its value is only ever `empty|filled` (H-2, B-5).
pub(crate) fn is_password(row: &IndexRow) -> bool {
    row.state
        .input_type
        .as_deref()
        .is_some_and(|t| t.eq_ignore_ascii_case("password"))
        || row
            .state
            .autocomplete
            .as_deref()
            .is_some_and(|a| a.to_ascii_lowercase().contains("password"))
}

/// Frames whose content must never be emitted or matched: frames hosted by a CHALLENGE or
/// OPAQUE row, and frames nested inside those (W-5, D-55).
pub(crate) fn sealed_frames(index: &[IndexRow]) -> HashSet<FrameId> {
    let mut sealed: HashSet<FrameId> = index
        .iter()
        .filter(|r| r.flags.challenge || r.flags.opaque)
        .filter_map(|r| r.child_frame)
        .filter(|f| *f != 0)
        .collect();
    loop {
        let before = sealed.len();
        for r in index {
            if let Some(child) = r.child_frame {
                if child != 0 && sealed.contains(&r.frame) {
                    sealed.insert(child);
                }
            }
        }
        if sealed.len() == before {
            return sealed;
        }
    }
}

/// One numbered element line: `#7 input "Email" empty` (D-203).
pub fn sheet_line(row: &IndexRow) -> String {
    let mut out = format!("{} {}", row.elem(), alias_for(&row.role));
    let name = clean(&row.name, NAME_CAP);
    if !name.is_empty() {
        out.push_str(&format!(" \"{name}\""));
    }
    let st = &row.state;
    if let Some(v) = &st.value {
        let word = match v {
            FieldValue::Empty => "empty".to_string(),
            FieldValue::Filled => "filled".to_string(),
            FieldValue::Text(t) if t.is_empty() => "empty".to_string(),
            // Never the text of a password field, whatever the producer sent.
            FieldValue::Text(_) if is_password(row) => "filled".to_string(),
            FieldValue::Text(t) => format!("\"{}\"", clean(t, VALUE_CAP)),
        };
        out.push(' ');
        out.push_str(&word);
    }
    match st.checked {
        Some(true) => out.push_str(" checked"),
        Some(false) => out.push_str(" unchecked"),
        None => {}
    }
    if !st.enabled {
        out.push_str(" disabled");
    }
    if !st.visible {
        out.push_str(" hidden");
    }
    if st.covered {
        out.push_str(" covered");
    }
    if st.opens_new_tab {
        out.push_str(" new-tab");
    }
    if !st.options.is_empty() {
        let shown: Vec<String> = st
            .options
            .iter()
            .take(OPTIONS_CAP)
            .map(|o| clean(o, OPTION_CAP))
            .collect();
        let mut opts = shown.join(", ");
        if st.options.len() > OPTIONS_CAP {
            opts.push_str(&format!(", +{} more", st.options.len() - OPTIONS_CAP));
        }
        out.push_str(&format!(" [{opts}]"));
    }
    let f = &row.flags;
    if f.submit || st.submitish {
        out.push_str(" SUBMIT");
    }
    if f.challenge {
        out.push_str(" CHALLENGE");
    }
    if f.overlay {
        out.push_str(" OVERLAY");
    }
    if f.opaque {
        out.push_str(" OPAQUE");
    }
    out
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.trim().to_lowercase())
}

/// Picks the rows of `region` (B-6.3) as a mask over `index`: a landmark, a form id, or a
/// heading (`## Heading` forces heading mode). Matching order: landmark, form id, `form` =
/// every form row, exact heading, heading substring. `None` when nothing matches.
fn region_mask(index: &[IndexRow], region: &str) -> Option<Vec<bool>> {
    let raw = region.trim();
    let heading_only = raw.starts_with('#');
    let r = raw.trim_start_matches('#').trim();
    if r.is_empty() {
        return None;
    }
    let pick = |pred: &dyn Fn(&IndexRow) -> bool| -> Option<Vec<bool>> {
        let m: Vec<bool> = index.iter().map(pred).collect();
        m.contains(&true).then_some(m)
    };
    let by_heading_eq = |row: &IndexRow| row.heading.as_deref().is_some_and(|h| eq_ci(h, r));
    let by_heading_has = |row: &IndexRow| row.heading.as_deref().is_some_and(|h| contains_ci(h, r));
    if heading_only {
        return pick(&by_heading_eq).or_else(|| pick(&by_heading_has));
    }
    pick(&|row| row.landmark.as_deref().is_some_and(|l| eq_ci(l, r)))
        .or_else(|| pick(&|row| row.form.as_deref().is_some_and(|f| eq_ci(f, r))))
        .or_else(|| {
            if eq_ci(r, "form") {
                pick(&|row| row.form.is_some())
            } else {
                None
            }
        })
        .or_else(|| pick(&by_heading_eq))
        .or_else(|| pick(&by_heading_has))
}

/// Extends a row mask with the content of every frame hosted by a selected row (an iframe
/// inside a form brings its frame's rows), transitively.
fn with_hosted_frames(index: &[IndexRow], mut mask: Vec<bool>) -> Vec<bool> {
    let mut frames: HashSet<FrameId> = HashSet::new();
    loop {
        let before = frames.len();
        for (i, row) in index.iter().enumerate() {
            if row.frame != 0 && frames.contains(&row.frame) {
                mask[i] = true;
            }
            if mask[i] {
                if let Some(c) = row.child_frame.filter(|c| *c != 0) {
                    frames.insert(c);
                }
            }
        }
        if frames.len() == before {
            return mask;
        }
    }
}

/// The regions a caller can ask for, for the "no such region" line.
fn known_regions(index: &[IndexRow], text_blocks: &[TextBlock]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: String| {
        if !s.is_empty() && !out.iter().any(|x| x.eq_ignore_ascii_case(&s)) {
            out.push(s);
        }
    };
    for row in index {
        if let Some(l) = &row.landmark {
            add(l.clone());
        }
    }
    for row in index {
        if let Some(f) = &row.form {
            add(f.clone());
        }
    }
    for row in index {
        if let Some(h) = &row.heading {
            add(format!("## {}", clean(h, 60)));
        }
    }
    for b in text_blocks {
        if let Some(l) = &b.landmark {
            add(l.clone());
        }
    }
    out
}

pub(crate) fn sheet(index: &[IndexRow], text_blocks: &[TextBlock], opts: &SheetOpts) -> String {
    let sealed = sealed_frames(index);
    let mask: Vec<bool> = match opts.region.as_deref().map(str::trim) {
        None | Some("") => vec![true; index.len()],
        Some(region) => match region_mask(index, region) {
            Some(mask) => with_hosted_frames(index, mask),
            None => {
                let known = known_regions(index, text_blocks);
                return if known.is_empty() {
                    format!("no region \"{}\"", clean(region, 60))
                } else {
                    format!(
                        "no region \"{}\". regions: {}",
                        clean(region, 60),
                        known.join(", ")
                    )
                };
            }
        },
    };
    let lines: Vec<String> = index
        .iter()
        .zip(mask)
        .filter(|(row, keep)| *keep && !sealed.contains(&row.frame))
        .map(|(row, _)| sheet_line(row))
        .collect();
    if lines.is_empty() {
        return "no elements".to_string();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Flags, State};

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

    #[test]
    fn line_basics() {
        let mut r = row(7, "textbox", "Email");
        r.state.value = Some(FieldValue::Empty);
        assert_eq!(sheet_line(&r), "#7 input \"Email\" empty");
        let mut b = row(14, "button", "Register");
        b.flags.submit = true;
        assert_eq!(sheet_line(&b), "#14 button \"Register\" SUBMIT");
        let mut f = row(3, "iframe", "reCAPTCHA");
        f.flags.challenge = true;
        f.flags.opaque = true;
        assert_eq!(sheet_line(&f), "#3 iframe \"reCAPTCHA\" CHALLENGE OPAQUE");
        let mut fr = row(15, "link", "Docs");
        fr.frame = 2;
        fr.state.opens_new_tab = true;
        assert_eq!(sheet_line(&fr), "#f2.15 link \"Docs\" new-tab");
        assert_eq!(sheet_line(&row(4, "generic", "")), "#4 generic");
    }

    #[test]
    fn password_text_never_printed() {
        let mut r = row(7, "textbox", "Password");
        r.state.input_type = Some("password".into());
        r.state.value = Some(FieldValue::Text("hunter2-not-a-secret".into()));
        let line = sheet_line(&r);
        assert_eq!(line, "#7 input \"Password\" filled");
        assert!(!line.contains("hunter2"));
        // autocomplete evidence alone is enough
        let mut a = row(8, "textbox", "Pin");
        a.state.autocomplete = Some("current-password".into());
        a.state.value = Some(FieldValue::Text("xyz".into()));
        assert_eq!(sheet_line(&a), "#8 input \"Pin\" filled");
        a.state.value = Some(FieldValue::Text(String::new()));
        assert_eq!(sheet_line(&a), "#8 input \"Pin\" empty");
    }

    #[test]
    fn value_and_states() {
        let mut r = row(5, "textbox", "Bio \"me\"");
        r.state.value = Some(FieldValue::Text("x".repeat(60)));
        r.state.enabled = false;
        r.state.visible = false;
        r.state.covered = true;
        let line = sheet_line(&r);
        assert!(line.starts_with("#5 input \"Bio 'me'\" \""), "{line}");
        assert!(
            line.contains(&format!("\"{}...\"", "x".repeat(37))),
            "{line}"
        );
        assert!(line.ends_with(" disabled hidden covered"), "{line}");
        let mut c = row(10, "checkbox", "I agree");
        c.state.checked = Some(false);
        assert_eq!(sheet_line(&c), "#10 checkbox \"I agree\" unchecked");
        c.state.checked = Some(true);
        assert_eq!(sheet_line(&c), "#10 checkbox \"I agree\" checked");
    }

    #[test]
    fn options_capped() {
        let mut s = row(9, "combobox", "Plan");
        s.state.options = vec!["Basic".into(), "Plus".into(), "Pro".into()];
        assert_eq!(sheet_line(&s), "#9 select \"Plan\" [Basic, Plus, Pro]");
        s.state.options = (1..=8).map(|i| format!("o{i}")).collect();
        assert_eq!(
            sheet_line(&s),
            "#9 select \"Plan\" [o1, o2, o3, o4, o5, +3 more]"
        );
    }

    #[test]
    fn clean_caps_and_collapses() {
        assert_eq!(clean("  a \n b  ", 10), "a b");
        assert_eq!(clean("abcdefghij", 10), "abcdefghij");
        assert_eq!(clean("abcdefghijk", 10), "abcdefg...");
    }

    #[test]
    fn sealed_frames_nest() {
        let mut host = row(1, "iframe", "captcha");
        host.flags.challenge = true;
        host.child_frame = Some(2);
        let mut inner = row(1, "iframe", "inner");
        inner.frame = 2;
        inner.child_frame = Some(3);
        let mut ok = row(9, "iframe", "same origin");
        ok.child_frame = Some(4);
        let s = sealed_frames(&[host, inner, ok]);
        assert!(s.contains(&2) && s.contains(&3) && !s.contains(&4));
    }

    #[test]
    fn sheet_regions_and_sealed_content() {
        let mut a = row(1, "link", "Home");
        a.landmark = Some("nav".into());
        let mut e = row(2, "textbox", "Email");
        e.form = Some("f1".into());
        e.heading = Some("Create your account".into());
        e.landmark = Some("main".into());
        let mut cap = row(3, "iframe", "reCAPTCHA");
        cap.form = Some("f1".into());
        cap.heading = Some("Create your account".into());
        cap.flags.challenge = true;
        cap.child_frame = Some(5);
        let mut inside = row(1, "checkbox", "I'm not a robot");
        inside.frame = 5;
        let mut same = row(4, "iframe", "Widget");
        same.form = Some("f1".into());
        same.child_frame = Some(6);
        let mut w = row(1, "button", "Inner");
        w.frame = 6;
        let idx = vec![a, e, cap, inside, same, w];
        let all = sheet(&idx, &[], &SheetOpts::default());
        assert_eq!(all.lines().count(), 5);
        assert!(!all.contains("robot"));
        let f1 = sheet(
            &idx,
            &[],
            &SheetOpts {
                region: Some("f1".into()),
            },
        );
        assert_eq!(
            f1,
            "#2 input \"Email\"\n#3 iframe \"reCAPTCHA\" CHALLENGE\n#4 iframe \"Widget\"\n#f6.1 button \"Inner\""
        );
        let nav = sheet(
            &idx,
            &[],
            &SheetOpts {
                region: Some("NAV".into()),
            },
        );
        assert_eq!(nav, "#1 link \"Home\"");
        let h = sheet(
            &idx,
            &[],
            &SheetOpts {
                region: Some("## create your account".into()),
            },
        );
        assert_eq!(h.lines().count(), 2);
        let none = sheet(
            &idx,
            &[],
            &SheetOpts {
                region: Some("pricing table".into()),
            },
        );
        assert!(none.starts_with("no region \"pricing table\". regions: nav, main, f1"));
    }
}
