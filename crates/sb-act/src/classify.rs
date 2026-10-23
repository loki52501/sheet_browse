//! B-7 step classification: D-402, refined by D-433 (Enter, form context), D-434 (hints),
//! D-58 (Pay from checkout text), D-403 (keys, implicit submission, more evidence),
//! D-404 (2FA set-up verbs, delete dialogs, the Delete key) and D-406 (control-character
//! keys, Enter takes the form max on every row, per-form evidence cache, more words),
//! D-407 (final word lists) and D-408 (Pay and Account words on non-submit buttons in a
//! form, Enter anywhere in a chord, more Delete words).
//!
//! Every refinement only raises a class (D-36 max). The one sanctioned lowering is
//! D-404 (1): second-factor text (2FA, 2-step, MFA, TOTP, security key, passkey,
//! authenticator app) is Account only with a set-up verb, so a bare "Two-step
//! verification" code prompt is Submit again. D-406 (3) records the scope of the
//! one-time-code exemption: a one-time-code field whose only card evidence is the phrase
//! "security code" is not a card field; CVC, CVV, CSC, cardholder, MM/YY, expiry and card
//! number on it still are.
//!
//! Sanctioned as coded by D-407, not lowerings: "Cancel and ..." / "Cancel or ..."
//! ("Cancel and close", "Cancel or go back") is Navigate like a bare Cancel, and Backspace
//! on a non-text row stays a click on the row. From D-407 on, a missing word in a list is a
//! Should item; Blocking is for structural paths (step kind, key, role, form shape) that
//! come out below their target's class.

use crate::{ActClass, Scroll, Step};
use harness_core::gate::{HINT_JOB_APPLICATION, HINT_SIGNATURE};
use harness_core::Class;
use sb_sheet::{FrameId, IndexRow};
use std::borrow::Cow;
use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};

/// Roles a plain (non-submit) click only toggles or selects: Fill (D-402).
const FILL_ROLES: &[&str] = &["checkbox", "radio", "switch", "option", "tab"];

/// Roles and `<input type>`s that take typed text (D-403 "printable characters into a
/// textbox"). A file input is never one, whatever its role (D-406 (2)).
const TEXT_ROLES: &[&str] = &["textbox", "searchbox", "combobox", "spinbutton"];
const TEXT_INPUT_TYPES: &[&str] = &[
    "text", "email", "password", "search", "tel", "url", "number",
];

/// Keys that move focus or the caret and never activate (D-403), alone or with Shift.
const NAV_KEYS: &[&str] = &[
    "tab",
    "escape",
    "esc",
    "arrowup",
    "arrowdown",
    "arrowleft",
    "arrowright",
    "up",
    "down",
    "left",
    "right",
    "home",
    "end",
    "pageup",
    "pagedown",
];

const MODIFIER_KEYS: &[&str] = &[
    "shift", "control", "ctrl", "alt", "option", "altgraph", "altgr", "meta", "cmd", "command",
    "os", "super", "win", "capslock", "fn",
];

fn plain(class: Class) -> ActClass {
    ActClass {
        class,
        hints: Vec::new(),
    }
}

fn merge(into: &mut ActClass, other: ActClass) {
    into.class = into.class.max(other.class);
    for h in other.hints {
        if !into.hints.contains(&h) {
            into.hints.push(h);
        }
    }
}

type FormKey<'a> = (FrameId, &'a str);

/// One classification call's view of the index (D-406 (5)): the rows of each form, grouped
/// in one pass the first time a form is needed, and each form's evidence, computed once
/// per form and reused by every step and every submit button of that form. Enter in a
/// form with many submits is then linear in the index, not submits x rows.
pub(crate) struct Ctx<'a> {
    index: &'a [IndexRow],
    forms: OnceCell<HashMap<FormKey<'a>, Vec<&'a IndexRow>>>,
    evidence: RefCell<HashMap<FormKey<'a>, FormEvidence>>,
    /// How many times form evidence was built (tests check the cache).
    #[cfg(test)]
    builds: std::cell::Cell<usize>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(index: &'a [IndexRow]) -> Self {
        Ctx {
            index,
            forms: OnceCell::new(),
            evidence: RefCell::new(HashMap::new()),
            #[cfg(test)]
            builds: std::cell::Cell::new(0),
        }
    }

    #[cfg(test)]
    pub(crate) fn evidence_builds(&self) -> usize {
        self.builds.get()
    }

    /// The row's form: rows with the same form id in the same frame; a row without a form
    /// id is its own context (D-433, B-431).
    fn form_of(&self, row: &'a IndexRow) -> Cow<'_, [&'a IndexRow]> {
        let Some(form) = row.form.as_deref() else {
            return Cow::Owned(vec![row]);
        };
        let forms = self.forms.get_or_init(|| {
            let mut map: HashMap<FormKey<'a>, Vec<&'a IndexRow>> = HashMap::new();
            for r in self.index {
                if let Some(f) = r.form.as_deref() {
                    map.entry((r.frame, f)).or_default().push(r);
                }
            }
            map
        });
        match forms.get(&(row.frame, form)) {
            Some(rows) if rows.iter().any(|r| std::ptr::eq(*r, row)) => Cow::Borrowed(rows),
            Some(rows) => {
                let mut rows = rows.clone();
                rows.push(row);
                Cow::Owned(rows)
            }
            None => Cow::Owned(vec![row]),
        }
    }

    /// The row's form evidence, cached per (frame, form id).
    fn evidence(&self, row: &'a IndexRow) -> FormEvidence {
        let key = row.form.as_deref().map(|f| (row.frame, f));
        if let Some(e) = key.and_then(|k| self.evidence.borrow().get(&k).copied()) {
            return e;
        }
        #[cfg(test)]
        self.builds.set(self.builds.get() + 1);
        let e = FormEvidence::of(&self.form_of(row));
        if let Some(k) = key {
            self.evidence.borrow_mut().insert(k, e);
        }
        e
    }

    pub(crate) fn step(&self, step: &Step) -> ActClass {
        let target = match step {
            Step::Wait(_) | Step::StopHere => return plain(Class::Read),
            Step::Scroll(Scroll::Down | Scroll::Up) => return plain(Class::Navigate),
            Step::Scroll(Scroll::To(target))
            | Step::Type { target, .. }
            | Step::Select { target, .. }
            | Step::Click { target }
            | Step::Key { target, .. }
            | Step::Upload { target, .. } => *target,
        };
        // A ref should be unique, but if a producer ever emits it twice the max over all
        // its rows wins, so a duplicate CHALLENGE row cannot be shadowed by an earlier one.
        let rows: Vec<&'a IndexRow> = self.index.iter().filter(|r| r.elem() == target).collect();
        if rows.is_empty() {
            // Not in the index: cannot be shown to be below Submit (D-402).
            return plain(Class::Submit);
        }
        if rows.iter().any(|r| r.flags.challenge) {
            return plain(Class::Challenge);
        }
        let mut out = plain(Class::Read);
        for row in rows {
            let c = match step {
                Step::Click { .. } => self.click(row),
                Step::Key { key, .. } => self.key_press(row, key),
                Step::Scroll(_) => plain(Class::Navigate),
                _ => plain(Class::Fill),
            };
            merge(&mut out, c);
        }
        out
    }

    /// A key step (D-403 (1), D-406 (1)).
    ///
    /// - `Enter`, `NumpadEnter`, `Return` in any segment of a chord or next to separators
    ///   and whitespace (`"Shift+Enter"`, `"Enter+Shift"`, `"Enter+"`, `" Enter"`,
    ///   `"Enter\t"`, D-408 (2)) and a key value holding a raw line break (`"\r"`, `"\n"`,
    ///   `"\r\n"`, from a quoted value) are [`Ctx::enter`].
    /// - Any other key holding a control character (`"\t"`, DEL, BS, ETX...) is unknown: a
    ///   click on its row, plus Delete for DEL on a non-text row as for the Delete key.
    /// - Fill: Tab, Escape, arrows, Home/End/PageUp/PageDown (alone or with Shift), modifiers
    ///   alone; and in a text field (not a file input), one printable character, Space
    ///   (U+0020 or U+00A0 only), Backspace or Delete, alone or with Shift.
    /// - Delete (any modifiers) on a non-text row is at least Delete (D-404 (3)).
    /// - Every other key, unknown keys included, is a click on its row (Space on a button,
    ///   a letter on a link, `F13`...), so a key step is never below a `c#` on the same
    ///   target except for the Fill cases above. Backspace on a non-text row is such a
    ///   click, not Delete (sanctioned as coded by D-407).
    fn key_press(&self, row: &'a IndexRow, key: &str) -> ActClass {
        if key.contains(['\r', '\n']) || names_enter(key) {
            return self.enter(row);
        }
        if key.chars().any(char::is_control) {
            let mut out = self.click(row);
            if key.contains('\u{7f}') && !is_text_field(row) {
                merge(&mut out, plain(Class::Delete));
            }
            return out;
        }
        let (mods, k) = split_key(key);
        if matches!(k.as_str(), "enter" | "numpadenter" | "return") {
            return self.enter(row);
        }
        let all_mods = mods.iter().all(|m| MODIFIER_KEYS.contains(&m.as_str()));
        if all_mods && MODIFIER_KEYS.contains(&k.as_str()) {
            return plain(Class::Fill);
        }
        let shift_only = mods.iter().all(|m| m == "shift");
        if shift_only && NAV_KEYS.contains(&k.as_str()) {
            return plain(Class::Fill);
        }
        let editing = k.chars().count() == 1
            || matches!(
                k.as_str(),
                "space" | "spacebar" | "backspace" | "delete" | "del"
            );
        if shift_only && editing && is_text_field(row) {
            return plain(Class::Fill);
        }
        let mut out = self.click(row);
        // D-404 (3): Delete on a selected mail, file or list row deletes it (Gmail, Drive).
        // Inside a text field it only edits text (above, or a click on the field with Ctrl).
        if matches!(k.as_str(), "delete" | "del") && !is_text_field(row) {
            merge(&mut out, plain(Class::Delete));
        }
        out
    }

    fn click(&self, row: &'a IndexRow) -> ActClass {
        if is_submit(row) {
            return submit_class(&self.evidence(row), &row.name);
        }
        let toks = tokens(&row.name);
        let mut class = if FILL_ROLES.contains(&row.role.as_str()) {
            Class::Fill
        } else {
            Class::Navigate
        };
        if delete_text(&toks, false) || self.confirms_delete(row, &toks) {
            class = class.max(Class::Delete);
        }
        if matches!(row.role.as_str(), "button" | "menuitem") {
            // D-406 (7): an "Upgrade" button changes the plan; an "Upgrade" link only opens
            // the pricing page.
            if has_any(&toks, &["upgrade"]) {
                class = class.max(Class::Pay);
            }
            class = class.max(self.form_button(row, &toks));
        }
        plain(class)
    }

    /// D-408 (1): a non-submit button or menuitem inside a form, or sharing its heading
    /// with a form row, gets a submit's own-name Pay and Account checks (checkout words,
    /// upgrade/renew, create/change text, a set-up verb over a second-factor form), since
    /// React-style `type=button` checkouts submit from script. It is also Pay when the form
    /// has Pay evidence (a card field, checkout text) and the name has any single Pay word
    /// ("Confirm", "Complete"). Form evidence alone does not raise it: a "Show details"
    /// button in a card form stays Navigate. Links are not buttons and never get here.
    fn form_button(&self, row: &'a IndexRow, toks: &[String]) -> Class {
        let mut forms: Vec<FormEvidence> = Vec::new();
        if row.form.is_some() {
            forms.push(self.evidence(row));
        } else if let Some(h) = row.heading.as_deref() {
            let mut seen = HashSet::new();
            for r in self.index {
                if r.frame != row.frame || r.heading.as_deref() != Some(h) {
                    continue;
                }
                if let Some(f) = r.form.as_deref() {
                    if seen.insert(f) {
                        forms.push(self.evidence(r));
                    }
                }
            }
        }
        if forms.is_empty() {
            return Class::Read;
        }
        let pay_name = pay_words(toks, &row.name) || has_any(toks, &["upgrade", "renew"]);
        let pay_ev = forms.iter().any(|e| e.pay);
        if pay_name || (pay_ev && has_any(toks, PAY_TOKENS)) {
            return Class::Pay;
        }
        let twofa = forms.iter().any(|e| e.twofa);
        if create_text(toks) || change_text(toks) || (twofa && !setup_verbs(toks).is_empty()) {
            return Class::Account;
        }
        Class::Read
    }

    /// D-404 (2), D-406 (6): an affirmative non-submit button (confirm, yes, ok, continue,
    /// proceed, delete, remove) whose own heading carries Delete wording, or whose form or
    /// dialog does ("Delete account?" + Confirm). Checkables and text fields are not
    /// buttons: a "Yes" checkbox only toggles, a "Type DELETE to confirm" field only takes
    /// text.
    fn confirms_delete(&self, row: &'a IndexRow, toks: &[String]) -> bool {
        if FILL_ROLES.contains(&row.role.as_str()) || is_text_field(row) || !affirmative(toks) {
            return false;
        }
        // The row's own heading, form or no form (D-406 (6): div modals without a dialog
        // landmark).
        if row
            .heading
            .as_deref()
            .is_some_and(|h| delete_text(&tokens(h), true))
        {
            return true;
        }
        if row.form.is_some() && self.evidence(row).delete {
            return true;
        }
        // A dialog landmark: rows in the same frame with landmark dialog and the same
        // heading. Their heading is the row's own (checked above), so only a form row's
        // name is left.
        row.landmark.as_deref() == Some("dialog")
            && self.index.iter().any(|r| {
                r.frame == row.frame
                    && r.landmark.as_deref() == Some("dialog")
                    && r.heading == row.heading
                    && r.role == "form"
                    && delete_text(&tokens(&r.name), true)
            })
    }

    /// Enter (D-433, D-403 (2), D-406 (2)). On any row inside a form, whatever its role
    /// (fields, buttons, links, switches, checkboxes, submits), it is the max of a click on
    /// the row and the form's implicit submission: at least Submit, raised by the form's
    /// own evidence even when the form has no indexed submit row, and by every submit row
    /// of the form. In a formless text field (chat composers, SPA search boxes) it is
    /// Submit raised by the field's headings. Elsewhere it is a click on the row.
    fn enter(&self, row: &'a IndexRow) -> ActClass {
        let mut out = self.click(row);
        if row.form.is_none() && !is_text_field(row) {
            return out;
        }
        let ev = self.evidence(row);
        // The field's own label is not submit-button text: only form evidence counts.
        merge(&mut out, submit_class(&ev, ""));
        if row.form.is_some() {
            for button in self.form_of(row).iter().filter(|r| is_submit(r)) {
                merge(&mut out, submit_class(&ev, &button.name));
            }
        }
        out
    }
}

/// A key name split into lowercased modifiers and key: `"Shift+Tab"` -> `(["shift"],
/// "tab")`. A trailing `+` is the plus key itself (`"+"`, `"Control++"`). A key made only
/// of U+0020 / U+00A0 (`" "`) is `"space"`; other whitespace trims to an empty, unknown key
/// (D-406 (1)). Control characters are handled before this.
fn split_key(key: &str) -> (Vec<String>, String) {
    let (mods, last) = match key.strip_suffix('+') {
        Some("") => ("", "+"),
        Some(rest) if rest.ends_with('+') => (rest.trim_end_matches('+'), "+"),
        _ => key.rsplit_once('+').unwrap_or(("", key)),
    };
    let last = if !last.is_empty() && last.chars().all(|c| c == ' ' || c == '\u{a0}') {
        "space".to_string()
    } else {
        last.trim().to_lowercase()
    };
    let mods = mods
        .split('+')
        .map(|m| m.trim().to_lowercase())
        .filter(|m| !m.is_empty())
        .collect();
    (mods, last)
}

/// Enter, Return or NumpadEnter as any segment of a key value split at `+`, whitespace
/// and control characters (D-408 (2)): `"Enter+Shift"`, `"Enter+"`, `" Enter"`,
/// `"Enter\t"`. Whole segments only, so `"Enterprise"` is not Enter.
fn names_enter(key: &str) -> bool {
    key.split(|c: char| c == '+' || c.is_whitespace() || c.is_control())
        .any(|s| {
            ["enter", "return", "numpadenter"]
                .iter()
                .any(|e| s.eq_ignore_ascii_case(e))
        })
}

fn is_text_field(row: &IndexRow) -> bool {
    let input_type = row.state.input_type.as_deref();
    if input_type.is_some_and(|t| t.eq_ignore_ascii_case("file")) {
        return false;
    }
    TEXT_ROLES.contains(&row.role.as_str())
        || input_type.is_some_and(|t| TEXT_INPUT_TYPES.iter().any(|x| t.eq_ignore_ascii_case(x)))
}

fn is_submit(row: &IndexRow) -> bool {
    row.flags.submit || row.state.submitish
}

/// A button that goes ahead with what its dialog asks (D-404 (2)): confirm, yes, ok,
/// continue, proceed, delete, remove. Bare Cancel, Close, No, Back, Keep are not.
fn affirmative(toks: &[String]) -> bool {
    has_any(
        toks,
        &[
            "confirm", "yes", "ok", "okay", "continue", "proceed", "delete", "remove",
        ],
    )
}

/// What a form says about its submission, independent of which submit is pressed: the
/// part of [`submit_class`] that reads every row of the form (D-406 (5)).
#[derive(Debug, Clone, Copy, Default)]
struct FormEvidence {
    /// A card or IBAN field, or checkout text in a heading or the form's name (D-58).
    pay: bool,
    /// New-password or two password fields, or change/create-account text (create only
    /// outside a login, D-433).
    account: bool,
    /// A second factor named in a heading or the form's name; Account only with a set-up
    /// verb on the submit button (D-404 (1)).
    twofa: bool,
    /// Delete wording in a heading or the form's name.
    delete: bool,
    /// A resume/CV upload or an apply/application heading (D-434).
    job: bool,
    /// E-sign wording in a heading, the form's name or any row's name (D-434).
    signature: bool,
}

impl FormEvidence {
    fn of(form: &[&IndexRow]) -> Self {
        // Section headings over the form's rows (each distinct text once), plus the names
        // of `form` rows.
        let mut seen = HashSet::new();
        let headings: Vec<(&str, Vec<String>)> = form
            .iter()
            .filter_map(|r| r.heading.as_deref())
            .filter(|h| seen.insert(*h))
            .map(|h| (h, tokens(h)))
            .collect();
        let form_names: Vec<(&str, Vec<String>)> = form
            .iter()
            .filter(|r| r.role == "form")
            .map(|r| (r.name.as_str(), tokens(&r.name)))
            .collect();
        let texts = || headings.iter().chain(form_names.iter());

        let passwords: Vec<&IndexRow> = form.iter().copied().filter(|r| is_password(r)).collect();
        // A login (one current-password field) is Submit; its "Sign in or create an
        // account" heading is not evidence (D-433), but "Change email" or "Set up 2FA" is
        // (D-403, D-404).
        let login = passwords.len() == 1 && has_autocomplete(passwords[0], "current-password");
        let account_fields =
            passwords.len() >= 2 || form.iter().any(|r| has_autocomplete(r, "new-password"));

        const RESUME: &[&str] = &["resume", "resumé", "résumé", "cv", "curriculum"];
        let resume_upload = form.iter().any(|r| {
            r.state
                .input_type
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case("file"))
                && [
                    Some(r.name.as_str()),
                    r.nearby.as_deref(),
                    r.heading.as_deref(),
                ]
                .into_iter()
                .flatten()
                .any(|t| has_any(&tokens(t), RESUME))
        });

        FormEvidence {
            pay: form.iter().any(|r| is_card_field(r)) || texts().any(|(raw, t)| pay_words(t, raw)),
            account: account_fields
                || texts().any(|(_, t)| change_text(t) || (!login && create_text(t))),
            twofa: texts().any(|(_, t)| !twofa_spans(t).is_empty()),
            delete: texts().any(|(_, t)| delete_text(t, true)),
            job: resume_upload
                || headings
                    .iter()
                    .any(|(_, t)| has_any(t, &["apply", "application", "applications"])),
            signature: texts().any(|(_, t)| signature_words(t))
                || form.iter().any(|r| signature_words(&tokens(&r.name))),
        }
    }
}

/// A form submit pressed through a button named `name` (empty for an implicit submission):
/// Submit raised to Pay / Account / Delete by the form evidence and the button's own text
/// (max wins), with the D-434 hints.
fn submit_class(ev: &FormEvidence, name: &str) -> ActClass {
    let toks = tokens(name);
    let mut class = Class::Submit;
    // D-406 (7): "Upgrade" on a submit changes a paid plan. D-407: so does "Renew", on the
    // submit only ("Renewal date" or "Renew" in a heading is not checkout text).
    if ev.pay || pay_words(&toks, name) || has_any(&toks, &["upgrade", "renew"]) {
        class = class.max(Class::Pay);
    }
    // A set-up verb on the button over a second-factor heading counts ("Two-factor
    // authentication" + "Enable"); a "Next" or "Verify" under one does not.
    if ev.account
        || create_text(&toks)
        || change_text(&toks)
        || (ev.twofa && !setup_verbs(&toks).is_empty())
    {
        class = class.max(Class::Account);
    }
    if ev.delete || delete_text(&toks, false) {
        class = class.max(Class::Delete);
    }
    let mut hints = Vec::new();
    if ev.job || has_any(&toks, &["application"]) {
        hints.push(HINT_JOB_APPLICATION.to_string());
    }
    if ev.signature || bare_sign(&toks) {
        hints.push(HINT_SIGNATURE.to_string());
    }
    ActClass { class, hints }
}

// ---- evidence -------------------------------------------------------------------------

/// Lowercased alphanumeric words: `"Create an Account!"` -> `[create, an, account]`.
fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

fn has_any(toks: &[String], words: &[&str]) -> bool {
    toks.iter().any(|t| words.contains(&t.as_str()))
}

/// A word from `first` followed within `within` words by a word from `then`.
fn near(toks: &[String], first: &[&str], then: &[&str], within: usize) -> bool {
    toks.iter().enumerate().any(|(i, t)| {
        first.contains(&t.as_str())
            && toks
                .iter()
                .skip(i + 1)
                .take(within)
                .any(|u| then.contains(&u.as_str()))
    })
}

fn has_autocomplete(row: &IndexRow, token: &str) -> bool {
    row.state
        .autocomplete
        .as_deref()
        .is_some_and(|a| a.split_whitespace().any(|t| t.eq_ignore_ascii_case(token)))
}

fn is_password(row: &IndexRow) -> bool {
    row.state
        .input_type
        .as_deref()
        .is_some_and(|t| t.eq_ignore_ascii_case("password"))
}

/// Card or IBAN field (D-402, D-433, D-403 (5)): autocomplete `cc-*`; a hosted card iframe
/// named card/payment/IBAN; or a field whose name or nearby label says one of
/// [`card_label`]'s words. On a one-time-code field the phrase "security code" alone is an
/// SMS or authenticator code, not a card's ("Enter the security code we sent you"); its
/// other card words still count (D-406 (3)).
fn is_card_field(row: &IndexRow) -> bool {
    let cc = row.state.autocomplete.as_deref().is_some_and(|a| {
        a.split_whitespace()
            .any(|t| t.to_ascii_lowercase().starts_with("cc-"))
    });
    if cc {
        return true;
    }
    match row.role.as_str() {
        // Hosted card frames (Stripe Elements and the like): opaque, named by the host.
        "iframe" => has_any(&tokens(&row.name), &["card", "payment", "iban"]),
        "button" | "link" => false,
        _ => {
            let security_code = !has_autocomplete(row, "one-time-code");
            std::iter::once(row.name.as_str())
                .chain(row.nearby.as_deref())
                .any(|t| card_label(&tokens(t), security_code))
        }
    }
}

/// Card field wording, whole words only: card number, IBAN, CVC/CVV/CSC, security code
/// (when `security_code`), cardholder / card holder / name on card, card expiry or
/// expiration, MM/YY. A passport "Expiration date" or "DD/MM/YYYY" is not a card's;
/// neither is a "Security settings" heading (headings are not field labels).
fn card_label(toks: &[String], security_code: bool) -> bool {
    has_any(
        toks,
        &[
            "cardnumber",
            "iban",
            "cvc",
            "cvc2",
            "cvv",
            "cvv2",
            "csc",
            "cvn",
            "cardholder",
        ],
    ) || near(toks, &["card"], &["number", "no", "num", "holder"], 1)
        || near(toks, &["name"], &["card"], 2)
        || (security_code && near(toks, &["security"], &["code"], 1))
        || (has_any(toks, &["card"]) && has_any(toks, &["expiry", "expiration", "exp", "expires"]))
        || near(toks, &["mm"], &["yy"], 1)
}

/// Checkout wording (D-58, D-403 (5), D-406 (7), D-407): checkout / check out, pay,
/// payment, purchase, buy, donate / donation, place, confirm, submit or complete order,
/// order now, place bid, top up, send money, transfer money / funds, add card, and
/// paid-subscription phrases. "Book now" and "Reserve" stay Submit (D-407: free bookings
/// are H-1's own Submit example); "Order history" and "Transfer ownership" do not match.
/// Small fillers between two words are allowed ("Place your order", "Add a new card").
/// `raw` is the text `toks` came from (for currency signs).
///
/// Matching is on whole words from `tokens()`, never substrings, so "Repay later",
/// "Payroll settings", "Display options", "PayPal account" and "Buyer info" do not match.
/// "Pay with PayPal", "Google Pay" and "Buy with 1-Click" do, which is intended: a false
/// Pay costs one ask, a missed Pay costs money (D-58).
fn pay_words(toks: &[String], raw: &str) -> bool {
    has_any(
        toks,
        &[
            "checkout", "pay", "payment", "purchase", "buy", "donate", "donation", "topup",
        ],
    ) || near(toks, &["check"], &["out"], 1)
        || near(
            toks,
            &["place", "confirm", "submit", "complete"],
            &["order"],
            2,
        )
        || near(toks, &["order"], &["now"], 1)
        || near(toks, &["place"], &["bid"], 2)
        || near(toks, &["top"], &["up"], 1)
        || near(toks, &["send", "transfer"], &["money", "funds"], 2)
        || near(toks, &["add"], &["card"], 3)
        || paid_subscription(toks, raw)
}

/// Single Pay words for a non-submit button in a form with Pay evidence (D-408 (1)): the
/// words of [`pay_words`] that carry the meaning on their own. Generic halves of phrases
/// ("top", "up", "out", "now", "add", "send") are left out, so "Back to top" stays Navigate.
const PAY_TOKENS: &[&str] = &[
    "checkout",
    "pay",
    "payment",
    "purchase",
    "buy",
    "donate",
    "donation",
    "topup",
    "order",
    "place",
    "confirm",
    "submit",
    "complete",
    "bid",
    "renew",
    "upgrade",
    "subscribe",
    "transfer",
];

/// "Start subscription", "Activate membership", or subscribe/subscription text with a
/// price ("Subscribe for $9.99/month", "Subscribe, 5 EUR per month"). A bare "Subscribe"
/// is a newsletter, not Pay. ("Subscribe and pay" already says pay.)
fn paid_subscription(toks: &[String], raw: &str) -> bool {
    const PERIODS: &[&str] = &["month", "mo", "year", "yr", "week", "wk"];
    let price =
        raw.chars()
            .any(|c| matches!(c, '$' | '€' | '£' | '¥' | '₹'))
            || near(toks, &["per"], PERIODS, 1)
            || toks.iter().zip(toks.iter().skip(1)).any(|(a, b)| {
                a.chars().all(|c| c.is_ascii_digit()) && PERIODS.contains(&b.as_str())
            });
    near(
        toks,
        &["start", "begin", "activate"],
        &["subscription", "membership"],
        2,
    ) || (has_any(toks, &["subscribe", "subscription", "membership"]) && price)
}

/// Create account, sign up (H-1 Account). Login headings say this too ("Sign in or create
/// an account"), so in a login it counts only on the button (D-433).
fn create_text(toks: &[String]) -> bool {
    has_any(toks, &["signup"])
        || near(toks, &["sign"], &["up"], 1)
        || near(toks, &["create"], &["account"], 3)
}

/// Change password or email, or setting up a second factor (H-1 Account). Counts in
/// headings even in a one-password form, where the password confirms the change
/// (D-403 (3)).
fn change_text(toks: &[String]) -> bool {
    near(
        toks,
        &["change", "update"],
        &["password", "email", "mail", "passcode"],
        3,
    ) || twofa_setup(toks)
}

/// Token spans `[start, end)` naming a second factor: 2FA, MFA, TOTP, two-factor, 2-step,
/// multi-factor, security key(s), passkey(s), authenticator app (D-404 (1), D-406 (7)).
fn twofa_spans(toks: &[String]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        let next = toks.get(i + 1).map(String::as_str);
        if matches!(t.as_str(), "2fa" | "mfa" | "totp" | "passkey" | "passkeys") {
            spans.push((i, i + 1));
        } else if matches!(
            (t.as_str(), next),
            ("two" | "2" | "multi", Some("factor" | "step"))
                | ("security", Some("key" | "keys"))
                | ("authenticator", Some("app" | "apps" | "application"))
        ) {
            spans.push((i, i + 2));
        }
    }
    spans
}

/// Start positions of a set-up verb, ascending (D-404 (1), D-407): enable, set up, turn on,
/// turn off, disable, add, create, remove, manage, change, reset. "Create a passkey" is
/// Account; "Sign in with a passkey" has no verb and is not.
fn setup_verbs(toks: &[String]) -> Vec<usize> {
    toks.iter()
        .enumerate()
        .filter(|(i, t)| {
            let next = toks.get(i + 1).map(String::as_str);
            match t.as_str() {
                "enable" | "disable" | "add" | "create" | "remove" | "manage" | "change"
                | "reset" => true,
                "set" => next == Some("up"),
                "setup" => true,
                "turn" => matches!(next, Some("on" | "off")),
                _ => false,
            }
        })
        .map(|(i, _)| i)
        .collect()
}

/// A second factor named with a set-up verb within four words either side ("Set up 2FA",
/// "Turn on 2-Step Verification", "Two-factor authentication: manage"). A bare
/// "Two-step verification" over an "Enter the code" login prompt is not Account
/// (D-404 (1)). One pass for the verbs, then a bounded window per span.
fn twofa_setup(toks: &[String]) -> bool {
    let verbs = setup_verbs(toks);
    twofa_spans(toks).iter().any(|&(start, end)| {
        let from = verbs.partition_point(|&v| v + 4 < start);
        verbs[from..]
            .iter()
            .take_while(|&&v| v < end + 4)
            .any(|&v| v < start || v >= end)
    })
}

/// delete / remove / deactivate / discard / erase / revoke / unsubscribe / trash, close
/// account, clear history / data / all (D-406 (7)), leave group, wipe device (D-408 (3)),
/// or "cancel <something>" (D-402, D-403 (4)). Not Delete:
/// - a bare "Cancel" (D-402), or a bare "Clear" (clears a search box);
/// - "cancel and ..." / "cancel or ..." ("Cancel and close", "Cancel or go back"): these
///   dismiss a dialog like a bare Cancel, so they are read as D-402's bare-Cancel case
///   rather than "cancel <something>" (sanctioned as coded by D-407);
/// - in heading text only (`heading`), "cancel anytime" / "cancel any time" / "cancel at
///   any time", which is subscription marketing next to a Subscribe button.
fn delete_text(toks: &[String], heading: bool) -> bool {
    has_any(
        toks,
        &[
            "delete",
            "remove",
            "deactivate",
            "discard",
            "erase",
            "revoke",
            "unsubscribe",
            "trash",
        ],
    ) || near(toks, &["close"], &["account"], 2)
        || near(toks, &["clear"], &["history", "data", "all"], 2)
        || near(toks, &["leave"], &["group"], 2)
        || near(toks, &["wipe"], &["device"], 2)
        || toks.iter().enumerate().any(|(i, t)| {
            t == "cancel"
                && toks.get(i + 1).is_some_and(|next| {
                    let next = next.as_str();
                    !matches!(next, "and" | "or")
                        && !(heading && matches!(next, "anytime" | "any" | "at"))
                })
        })
}

fn signature_words(toks: &[String]) -> bool {
    has_any(
        toks,
        &["signature", "signatures", "esign", "esignature", "docusign"],
    ) || near(toks, &["e"], &["sign", "signature"], 1)
}

/// A submit's own name says sign ("Sign", "Adopt and sign") but not sign in/up/out/on/off,
/// or names a signature.
fn bare_sign(name: &[String]) -> bool {
    name.iter().enumerate().any(|(i, t)| {
        t == "sign"
            && !name.get(i + 1).is_some_and(|next| {
                matches!(next.as_str(), "in" | "up" | "out" | "on" | "off" | "into")
            })
    }) || signature_words(name)
}
#[cfg(test)]
mod tests {
    use crate::{check_planner, classify, classify_step, parse, ActClass, PlannerError, Step};
    use harness_core::gate::{HINT_JOB_APPLICATION, HINT_SIGNATURE};
    use harness_core::Class;
    use sb_sheet::{ElemRef, Flags, IndexRow, State};

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

    fn form(mut r: IndexRow, f: &str) -> IndexRow {
        r.form = Some(f.into());
        r
    }

    fn heading(mut r: IndexRow, h: &str) -> IndexRow {
        r.heading = Some(h.into());
        r
    }

    fn submit(mut r: IndexRow) -> IndexRow {
        r.flags.submit = true;
        r.state.submitish = true;
        r
    }

    fn input(mut r: IndexRow, input_type: &str, autocomplete: Option<&str>) -> IndexRow {
        r.state.input_type = Some(input_type.into());
        r.state.autocomplete = autocomplete.map(str::to_string);
        r
    }

    /// Classifies a one-step program against `index`.
    fn class_of(program: &str, index: &[IndexRow]) -> ActClass {
        let steps = parse(program).unwrap();
        assert_eq!(steps.len(), 1, "{program}");
        classify_step(&steps[0], index)
    }

    fn cls(program: &str, index: &[IndexRow]) -> Class {
        class_of(program, index).class
    }

    fn register() -> Vec<IndexRow> {
        vec![
            heading(
                form(input(row(7, "textbox", "Email"), "email", None), "f1"),
                "Create your account",
            ),
            heading(
                form(
                    input(
                        row(9, "textbox", "Password"),
                        "password",
                        Some("new-password"),
                    ),
                    "f1",
                ),
                "Create your account",
            ),
            heading(
                form(submit(row(14, "button", "Register")), "f1"),
                "Create your account",
            ),
        ]
    }

    fn login() -> Vec<IndexRow> {
        let h = "Sign in or create an account";
        vec![
            heading(
                form(
                    input(row(7, "textbox", "Email"), "email", Some("username")),
                    "f2",
                ),
                h,
            ),
            heading(
                form(
                    input(
                        row(8, "textbox", "Password"),
                        "password",
                        Some("current-password"),
                    ),
                    "f2",
                ),
                h,
            ),
            heading(form(submit(row(14, "button", "Sign in")), "f2"), h),
        ]
    }

    fn checkout() -> Vec<IndexRow> {
        vec![
            form(row(20, "textbox", "Name on card"), "f3"),
            form(
                input(row(21, "textbox", "Card number"), "text", Some("cc-number")),
                "f3",
            ),
            form(submit(row(22, "button", "Pay now")), "f3"),
        ]
    }

    #[test]
    fn register_form_is_account() {
        let idx = register();
        assert_eq!(cls("c#14", &idx), Class::Account);
        assert_eq!(cls("k#7=Enter", &idx), Class::Account);
        assert_eq!(cls("t#7=a@b.c", &idx), Class::Fill);
        assert_eq!(cls("t#9=@vault(pw)", &idx), Class::Fill);
    }

    #[test]
    fn account_evidence_variants() {
        // Two password fields without autocomplete (password + confirm).
        let idx = vec![
            form(input(row(1, "textbox", "Password"), "password", None), "f1"),
            form(
                input(row(2, "textbox", "Confirm password"), "password", None),
                "f1",
            ),
            form(submit(row(3, "button", "Continue")), "f1"),
        ];
        assert_eq!(cls("c#3", &idx), Class::Account);
        // Button text only.
        for name in [
            "Create account",
            "Create an account",
            "Sign up",
            "Sign-up",
            "Signup",
        ] {
            let idx = vec![form(submit(row(3, "button", name)), "f1")];
            assert_eq!(cls("c#3", &idx), Class::Account, "{name}");
        }
        // Heading text: change email / change password.
        for h in [
            "Change email",
            "Change your e-mail address",
            "Update password",
        ] {
            let idx = vec![
                heading(
                    form(input(row(1, "textbox", "New email"), "email", None), "f1"),
                    h,
                ),
                heading(form(submit(row(3, "button", "Save")), "f1"), h),
            ];
            assert_eq!(cls("c#3", &idx), Class::Account, "{h}");
        }
        // The form's own name.
        let idx = vec![
            form(row(1, "form", "Create account"), "f1"),
            form(submit(row(3, "button", "Continue")), "f1"),
        ];
        assert_eq!(cls("c#3", &idx), Class::Account);
    }

    #[test]
    fn login_is_submit() {
        let idx = login();
        assert_eq!(cls("c#14", &idx), Class::Submit);
        assert_eq!(cls("k#8=Enter", &idx), Class::Submit);
        // A login without autocomplete and a neutral heading is Submit too.
        let idx = vec![
            heading(
                form(input(row(8, "textbox", "Password"), "password", None), "f2"),
                "Log in",
            ),
            heading(form(submit(row(14, "button", "Log in")), "f2"), "Log in"),
        ];
        assert_eq!(cls("c#14", &idx), Class::Submit);
        assert!(class_of("c#14", &idx).hints.is_empty());
    }

    #[test]
    fn checkout_is_pay() {
        let idx = checkout();
        assert_eq!(cls("c#22", &idx), Class::Pay);
        assert_eq!(cls("k#20=Enter", &idx), Class::Pay);
        assert_eq!(cls("t#21=4242424242424242", &idx), Class::Fill);
        // IBAN by label, card number by label without autocomplete, hosted card iframe.
        for (role, name) in [
            ("textbox", "IBAN"),
            ("textbox", "Credit card number"),
            ("iframe", "Secure card payment input frame"),
        ] {
            let idx = vec![
                form(row(1, role, name), "f3"),
                form(submit(row(2, "button", "Continue")), "f3"),
            ];
            assert_eq!(cls("c#2", &idx), Class::Pay, "{name}");
        }
        // Pay outranks Account (create an account during checkout).
        let mut idx = checkout();
        idx.push(form(
            input(
                row(23, "textbox", "Password"),
                "password",
                Some("new-password"),
            ),
            "f3",
        ));
        assert_eq!(cls("c#22", &idx), Class::Pay);
        // "Card" alone in a non-field or unrelated label is not evidence.
        let idx = vec![
            form(row(1, "textbox", "Gift card message"), "f3"),
            form(row(4, "textbox", "Libanon"), "f3"),
            form(submit(row(2, "button", "Card number help")), "f3"),
        ];
        assert_eq!(cls("c#2", &idx), Class::Submit);
    }

    /// A saved-card checkout: no card field anywhere, only an address and a submit.
    fn saved_card(button: &str, h: Option<&str>) -> Vec<IndexRow> {
        let mut idx = vec![
            form(row(1, "textbox", "Shipping address"), "f3"),
            form(submit(row(2, "button", button)), "f3"),
        ];
        if let Some(h) = h {
            idx = idx.into_iter().map(|r| heading(r, h)).collect();
        }
        idx
    }

    #[test]
    fn checkout_text_is_pay() {
        // Button name (D-58), no card field.
        for name in [
            "Place order",
            "Place your order",
            "Pay now",
            "Pay",
            "Pay $42.00",
            "Pay with PayPal",
            "Checkout",
            "Check out",
            "Confirm order",
            "Confirm your order",
            "Buy now",
            "Complete purchase",
            "Complete your purchase",
            "Add card",
            "Add a new card",
        ] {
            let idx = saved_card(name, None);
            assert_eq!(cls("c#2", &idx), Class::Pay, "{name}");
            // Enter in the checkout form raises through its submit.
            assert_eq!(cls("k#1=Enter", &idx), Class::Pay, "Enter: {name}");
        }
        // Form heading.
        let idx = saved_card("Continue", Some("Checkout"));
        assert_eq!(cls("c#2", &idx), Class::Pay);
        assert_eq!(cls("k#1=Enter", &idx), Class::Pay);
        // Typing into the checkout form is still Fill.
        assert_eq!(cls("t#1=1 Main St", &idx), Class::Fill);
        // Form name.
        let mut idx = saved_card("Continue", None);
        idx.push(form(row(3, "form", "Add card"), "f3"));
        assert_eq!(cls("c#2", &idx), Class::Pay);
        // A non-submit "Pay" link only navigates (the text rule is for submits).
        assert_eq!(cls("c#1", &[row(1, "link", "Pay")]), Class::Navigate);
    }

    #[test]
    fn checkout_text_negatives() {
        // Whole-word matching: "repay", "payroll", "display", "paypal" are not "pay".
        for (name, h) in [
            ("Save", "Payroll settings"),
            ("Apply", "Display options"),
            ("Repay later", "Loans"),
            ("Link PayPal account", "Wallets"),
            ("Check status", "Order history"),
            ("Place hold", "Library"),
            ("Add cardholder note", "Notes"),
        ] {
            let idx = saved_card(name, Some(h));
            assert_eq!(cls("c#2", &idx), Class::Submit, "{name} / {h}");
            assert_eq!(cls("k#1=Enter", &idx), Class::Submit, "Enter: {name} / {h}");
        }
        // Login and register forms are unaffected.
        assert_eq!(cls("c#14", &login()), Class::Submit);
        assert_eq!(cls("k#8=Enter", &login()), Class::Submit);
        assert_eq!(cls("c#14", &register()), Class::Account);
    }

    #[test]
    fn checkout_text_keeps_hints_and_max() {
        // Hints stay attached when checkout text raises the class (D-434).
        let idx = vec![
            heading(
                form(row(1, "textbox", "Full name"), "f6"),
                "Apply for Senior Engineer",
            ),
            heading(
                form(submit(row(2, "button", "Pay application fee")), "f6"),
                "Apply for Senior Engineer",
            ),
        ];
        assert_eq!(
            class_of("c#2", &idx),
            ActClass {
                class: Class::Pay,
                hints: vec![HINT_JOB_APPLICATION.to_string()]
            }
        );
        // Pay outranks Account: register during checkout (D-36 max).
        let mut idx = saved_card("Place order", None);
        idx.push(form(
            input(
                row(3, "textbox", "Password"),
                "password",
                Some("new-password"),
            ),
            "f3",
        ));
        assert_eq!(cls("c#2", &idx), Class::Pay);
        // Delete wording does not lower it either way; Pay is above Delete.
        let idx = saved_card("Remove card and pay", None);
        assert_eq!(cls("c#2", &idx), Class::Pay);
    }

    #[test]
    fn form_context_is_per_frame() {
        // Same form id f3 in frame 2 holds a card field; the frame 0 form does not.
        let mut card = form(
            input(row(21, "textbox", "Card number"), "text", Some("cc-number")),
            "f3",
        );
        card.frame = 2;
        let idx = vec![
            card,
            form(submit(row(22, "button", "Subscribe to newsletter")), "f3"),
        ];
        assert_eq!(cls("c#22", &idx), Class::Submit);
        assert_eq!(cls("t#f2.21=4242", &idx), Class::Fill);
    }

    #[test]
    fn child_frame_submit_sees_its_own_form() {
        // Form f3 exists in both frames; only the frame 2 copy has a card field.
        let in_f2 = |mut r: IndexRow| {
            r.frame = 2;
            r
        };
        let idx = vec![
            in_f2(form(
                input(row(21, "textbox", "Card number"), "text", Some("cc-number")),
                "f3",
            )),
            in_f2(form(row(23, "textbox", "Postcode"), "f3")),
            in_f2(form(submit(row(22, "button", "Continue")), "f3")),
            form(row(23, "textbox", "Postcode"), "f3"),
            form(submit(row(22, "button", "Continue")), "f3"),
        ];
        assert_eq!(cls("c#f2.22", &idx), Class::Pay);
        assert_eq!(cls("k#f2.23=Enter", &idx), Class::Pay);
        assert_eq!(cls("k#f2.22=Space", &idx), Class::Pay);
        // The main-frame copy of f3 does not borrow frame 2's card field.
        assert_eq!(cls("c#22", &idx), Class::Submit);
        assert_eq!(cls("k#23=Enter", &idx), Class::Submit);
    }

    #[test]
    fn duplicate_ref_takes_the_max() {
        let mut cap = row(5, "button", "Next");
        cap.flags.challenge = true;
        let idx = vec![row(5, "button", "Next"), cap];
        assert_eq!(cls("c#5", &idx), Class::Challenge);
    }

    #[test]
    fn delete_text() {
        // Non-submit buttons and links.
        for name in [
            "Unsubscribe",
            "Delete",
            "Remove item",
            "Move to trash",
            "Cancel booking",
            "Cancel my subscription",
        ] {
            assert_eq!(
                cls("c#1", &[row(1, "button", name)]),
                Class::Delete,
                "{name}"
            );
            assert_eq!(cls("c#1", &[row(1, "link", name)]), Class::Delete, "{name}");
        }
        // Submit buttons in a neutral form.
        let idx = vec![
            form(row(1, "textbox", "Reason"), "f4"),
            form(submit(row(2, "button", "Unsubscribe")), "f4"),
            form(submit(row(3, "button", "Cancel order")), "f4"),
        ];
        assert_eq!(cls("c#2", &idx), Class::Delete);
        assert_eq!(cls("c#3", &idx), Class::Delete);
        // Enter in that form: the max over its submits.
        assert_eq!(cls("k#1=Enter", &idx), Class::Delete);
    }

    #[test]
    fn bare_cancel_and_close_are_navigate() {
        for name in ["Cancel", "cancel.", "Close", "Cancel and close", "X"] {
            assert_eq!(
                cls("c#1", &[row(1, "button", name)]),
                Class::Navigate,
                "{name}"
            );
        }
        let idx = vec![
            form(row(1, "textbox", "Comment"), "f5"),
            form(submit(row(2, "button", "Post")), "f5"),
            form(row(3, "button", "Cancel"), "f5"),
        ];
        assert_eq!(cls("c#3", &idx), Class::Navigate);
        assert_eq!(cls("c#2", &idx), Class::Submit);
    }

    #[test]
    fn links_and_checkables() {
        assert_eq!(cls("c#1", &[row(1, "link", "Pricing")]), Class::Navigate);
        // A "Create account" link is navigation; the form behind it is classified on submit.
        assert_eq!(
            cls("c#1", &[row(1, "link", "Create account")]),
            Class::Navigate
        );
        for role in ["checkbox", "radio", "switch", "option", "tab"] {
            assert_eq!(
                cls("c#1", &[row(1, role, "Remember me")]),
                Class::Fill,
                "{role}"
            );
        }
        // A checkbox inside a register form is still only Fill.
        let mut idx = register();
        idx.push(form(row(15, "checkbox", "I agree to the terms"), "f1"));
        assert_eq!(cls("c#15", &idx), Class::Fill);
    }

    #[test]
    fn keys() {
        let mut idx = login();
        idx.push(row(30, "textbox", "Search"));
        idx.push(form(row(31, "textbox", "Comment"), "f9"));
        // Known non-activating keys (D-403 (1)).
        for k in [
            "Tab",
            "Shift+Tab",
            "Escape",
            "ArrowDown",
            "Shift+ArrowLeft",
            "Home",
            "PageDown",
            "Shift",
            "Control",
            "Control+Shift",
        ] {
            assert_eq!(cls(&format!("k#8={k}"), &idx), Class::Fill, "{k}");
            // On the submit button too: they move focus, they do not press it.
            assert_eq!(cls(&format!("k#14={k}"), &idx), Class::Fill, "#14 {k}");
        }
        // Typing and editing keys into a text field.
        for k in ["a", "Shift+A", "+", "Backspace", "Delete", "Space", "\" \""] {
            assert_eq!(cls(&format!("k#30={k}"), &idx), Class::Fill, "{k}");
        }
        assert_eq!(cls("k#8=enter", &idx), Class::Submit);
        assert_eq!(cls("k#8=Control+Enter", &idx), Class::Submit);
        assert_eq!(cls("k#8=NumpadEnter", &idx), Class::Submit);
        // Enter on the submit button itself.
        assert_eq!(cls("k#14=Enter", &idx), Class::Submit);
        // Enter in a formless textbox, and in a form with no submit row: Submit (D-403 (2)).
        assert_eq!(cls("k#30=Enter", &idx), Class::Submit);
        assert_eq!(cls("k#31=Enter", &idx), Class::Submit);
        assert_eq!(cls("k#31=Meta+Enter", &idx), Class::Submit);
        // Unknown keys and shortcuts are a click on the row, never Fill (Navigate on a
        // textbox, Submit on a submit).
        for k in ["F5", "Control+s", "Control++", "Alt+ArrowLeft", "MediaPlay"] {
            assert_eq!(cls(&format!("k#30={k}"), &idx), Class::Navigate, "{k}");
        }
        assert_eq!(cls("k#14=Space", &idx), Class::Submit);
        assert_eq!(cls("k#14=x", &idx), Class::Submit);
        assert_eq!(cls("k#14=F13", &idx), Class::Submit);
    }

    /// The review's probes (D-403 (1)): a key step on an activating target is a click on it.
    #[test]
    fn activating_keys_are_clicks() {
        let del = vec![row(1, "button", "Delete repository")];
        for k in [
            "Enter",
            "Space",
            "\" \"",
            "Shift+Enter",
            "NumpadEnter",
            "F13",
        ] {
            assert_eq!(cls(&format!("k#1={k}"), &del), Class::Delete, "{k}");
        }
        assert_eq!(cls("k#1=Tab", &del), Class::Fill);
        let pay = saved_card("Place order", None);
        for k in ["Space", "NumpadEnter", "Control+Enter", "Enter", "\" \""] {
            assert_eq!(cls(&format!("k#2={k}"), &pay), Class::Pay, "{k}");
        }
        let links = vec![
            row(13, "link", "Pricing"),
            row(14, "link", "Unsubscribe"),
            row(15, "checkbox", "Remember me"),
        ];
        assert_eq!(cls("k#13=Enter", &links), Class::Navigate);
        assert_eq!(cls("k#13=Space", &links), Class::Navigate);
        assert_eq!(cls("k#14=Enter", &links), Class::Delete);
        // Space on a checkbox toggles it: a click on a checkable is Fill.
        assert_eq!(cls("k#15=Space", &links), Class::Fill);
        // A non-submit button in a form: Enter takes the form max (D-406 (2)); round 2
        // pinned Navigate here, which lowered D-433.
        let mut idx = login();
        idx.push(form(row(15, "button", "Show password"), "f2"));
        assert_eq!(cls("k#15=Enter", &idx), Class::Submit);
        assert_eq!(cls("c#15", &idx), Class::Navigate);
    }

    /// Enter on a field submits its form implicitly, with or without a submit row
    /// (D-403 (2)).
    #[test]
    fn implicit_submission() {
        // Card-only form, no submit row (the review's probe).
        let card = vec![form(row(1, "textbox", "Card number"), "f3")];
        assert_eq!(cls("k#1=Enter", &card), Class::Pay);
        let card = vec![
            form(row(1, "textbox", "Promo code"), "f3"),
            form(
                input(row(2, "textbox", "Card"), "text", Some("cc-number")),
                "f3",
            ),
        ];
        assert_eq!(cls("k#1=Enter", &card), Class::Pay);
        // Form evidence without a submit row: Account, Delete, Pay text.
        let reg = vec![
            form(row(1, "textbox", "Email"), "f1"),
            form(
                input(
                    row(2, "textbox", "Password"),
                    "password",
                    Some("new-password"),
                ),
                "f1",
            ),
        ];
        assert_eq!(cls("k#1=Enter", &reg), Class::Account);
        let del = vec![heading(
            form(row(1, "textbox", "Type the repository name"), "f4"),
            "Delete this repository",
        )];
        assert_eq!(cls("k#1=Enter", &del), Class::Delete);
        // A checkbox in a form submits it on Enter too.
        let idx = vec![
            heading(form(row(1, "checkbox", "Save card"), "f3"), "Checkout"),
            form(row(2, "textbox", "Address"), "f3"),
        ];
        assert_eq!(cls("k#1=Enter", &idx), Class::Pay);
        // Formless textbox: Submit, raised by its heading.
        assert_eq!(
            cls("k#1=Enter", &[row(1, "textbox", "Message")]),
            Class::Submit
        );
        assert_eq!(
            cls(
                "k#1=Enter",
                &[heading(row(1, "textbox", "Gift note"), "Checkout")]
            ),
            Class::Pay
        );
        // A formless non-text row: a click on it.
        assert_eq!(
            cls("k#1=Enter", &[row(1, "checkbox", "Dark mode")]),
            Class::Fill
        );
        // Typing into these is still Fill.
        assert_eq!(cls("t#1=4242", &card), Class::Fill);
    }

    #[test]
    fn key_steps_keep_hints() {
        let job = vec![HINT_JOB_APPLICATION.to_string()];
        let h = "Apply for Senior Engineer";
        let idx = vec![
            heading(form(row(1, "textbox", "Full name"), "f6"), h),
            heading(form(submit(row(2, "button", "Submit")), "f6"), h),
        ];
        for p in ["k#2=Space", "k#2=Enter", "k#2=NumpadEnter", "k#1=Enter"] {
            assert_eq!(
                class_of(p, &idx),
                ActClass {
                    class: Class::Submit,
                    hints: job.clone()
                },
                "{p}"
            );
        }
        // No submit row: the implicit submission still carries the hint.
        let idx = vec![heading(form(row(1, "textbox", "Full name"), "f6"), h)];
        assert_eq!(class_of("k#1=Enter", &idx).hints, job);
        let sig = vec![form(submit(row(2, "button", "Sign document")), "f8")];
        assert_eq!(
            class_of("k#2=Space", &sig).hints,
            vec![HINT_SIGNATURE.to_string()]
        );
        // Non-activating keys carry no hints.
        assert!(class_of("k#2=Tab", &sig).hints.is_empty());
    }

    #[test]
    fn planner_rejects_key_submits_without_stop() {
        let del = vec![row(1, "button", "Delete repository")];
        let pay = saved_card("Place order", None);
        let card = vec![form(row(1, "textbox", "Card number"), "f3")];
        for (p, idx, class) in [
            ("k#1=Enter", &del, Class::Delete),
            ("k#1=Space", &del, Class::Delete),
            ("k#2=Space", &pay, Class::Pay),
            ("k#2=NumpadEnter", &pay, Class::Pay),
            ("k#1=Enter", &card, Class::Pay),
        ] {
            let steps = parse(p).unwrap();
            assert_eq!(
                check_planner(&steps, idx),
                Err(PlannerError::MissingStop { step: 1, class }),
                "{p}"
            );
            let steps = parse(&format!("{p}; ?s")).unwrap();
            assert_eq!(check_planner(&steps, idx), Ok(()), "{p}; ?s");
        }
        // A key on a plain link only navigates: no ?s needed.
        let steps = parse("k#13=Enter").unwrap();
        assert_eq!(check_planner(&steps, &[row(13, "link", "Pricing")]), Ok(()));
    }

    #[test]
    fn account_change_and_2fa_in_login_shaped_forms() {
        // The review's probe: New email + one current-password + Save.
        let pw = || {
            input(
                row(2, "textbox", "Current password"),
                "password",
                Some("current-password"),
            )
        };
        for h in [
            "Change email",
            "Change password",
            "Enable 2FA",
            "Set up two-factor authentication",
            "Turn on 2-Step Verification",
            "Manage security keys",
        ] {
            let idx = vec![
                heading(form(row(1, "textbox", "New email"), "f1"), h),
                heading(form(pw(), "f1"), h),
                heading(form(submit(row(6, "button", "Save")), "f1"), h),
            ];
            assert_eq!(cls("c#6", &idx), Class::Account, "{h}");
            assert_eq!(cls("k#1=Enter", &idx), Class::Account, "Enter: {h}");
        }
        // Form name, not heading.
        let idx = vec![
            form(row(1, "form", "Change your email"), "f1"),
            form(pw(), "f1"),
            form(submit(row(6, "button", "Save")), "f1"),
        ];
        assert_eq!(cls("c#6", &idx), Class::Account);
        // Button text.
        for name in [
            "Enable two-factor authentication",
            "Set up 2FA",
            "Turn on 2-Step Verification",
            "Add security key",
            "Enable MFA",
        ] {
            let idx = vec![form(submit(row(6, "button", name)), "f1")];
            assert_eq!(cls("c#6", &idx), Class::Account, "{name}");
        }
        // Negatives: a login stays Submit; a "Security settings" re-auth is Submit; a bare
        // second-factor heading is a code prompt, not a set-up (D-404 (1)).
        for h in [
            "Sign in or create an account",
            "Security settings",
            "Sign up for news",
            "Two-factor authentication",
            "2-Step Verification",
            "Security keys",
        ] {
            let idx = vec![
                heading(form(pw(), "f1"), h),
                heading(form(submit(row(6, "button", "Continue")), "f1"), h),
            ];
            assert_eq!(cls("c#6", &idx), Class::Submit, "{h}");
        }
        // "Two" or "step" alone is not 2FA.
        let idx = vec![heading(
            form(submit(row(6, "button", "Next step")), "f1"),
            "Step two of three",
        )];
        assert_eq!(cls("c#6", &idx), Class::Submit);
    }

    /// D-404 (1): a second factor is Account only with a set-up verb.
    #[test]
    fn twofa_needs_a_setup_verb() {
        let code = || {
            input(
                row(1, "textbox", "Enter the code"),
                "text",
                Some("one-time-code"),
            )
        };
        // Login code prompts stay Submit, by click and by Enter.
        for (h, button) in [
            ("Two-step verification", "Next"),
            ("2-Step Verification", "Verify"),
            ("Two-factor authentication", "Continue"),
            ("Multi-factor authentication", "Submit"),
            ("Use your security key", "Try again"),
            ("Enter your 2FA code", "Sign in"),
        ] {
            let idx = vec![
                heading(form(code(), "f1"), h),
                heading(form(submit(row(2, "button", button)), "f1"), h),
            ];
            assert_eq!(cls("c#2", &idx), Class::Submit, "{h} / {button}");
            assert_eq!(cls("k#1=Enter", &idx), Class::Submit, "Enter: {h}");
        }
        // A 2FA prompt right after a password login is still Submit.
        let idx = vec![
            heading(
                form(
                    input(
                        row(1, "textbox", "Password"),
                        "password",
                        Some("current-password"),
                    ),
                    "f1",
                ),
                "2-Step Verification",
            ),
            heading(
                form(submit(row(2, "button", "Next")), "f1"),
                "2-Step Verification",
            ),
        ];
        assert_eq!(cls("c#2", &idx), Class::Submit);
        // With a set-up verb in the heading, the form name or the button: Account.
        for h in [
            "Set up 2FA",
            "Turn off two-factor authentication",
            "Disable MFA",
            "Reset 2-step verification",
            "Add a security key",
            "Change your 2FA method",
            "Two-factor authentication: enable",
        ] {
            let idx = vec![
                heading(form(code(), "f1"), h),
                heading(form(submit(row(2, "button", "Next")), "f1"), h),
            ];
            assert_eq!(cls("c#2", &idx), Class::Account, "{h}");
            assert_eq!(cls("k#1=Enter", &idx), Class::Account, "Enter: {h}");
        }
        let idx = vec![
            form(row(3, "form", "Set up two-step verification"), "f1"),
            form(code(), "f1"),
            form(submit(row(2, "button", "Verify")), "f1"),
        ];
        assert_eq!(cls("c#2", &idx), Class::Account);
        // Bare second-factor heading, set-up verb on the button.
        for button in ["Enable", "Turn on", "Set up", "Manage"] {
            let h = "Two-factor authentication";
            let idx = vec![
                heading(form(code(), "f1"), h),
                heading(form(submit(row(2, "button", button)), "f1"), h),
            ];
            assert_eq!(cls("c#2", &idx), Class::Account, "{button}");
        }
        // A set-up verb far from the second factor does not count.
        let idx = vec![heading(
            form(submit(row(2, "button", "Next")), "f1"),
            "Two-step verification sent a code to your phone, you can change it in settings",
        )];
        assert_eq!(cls("c#2", &idx), Class::Submit);
    }

    /// D-404 (2): affirmative buttons in a Delete-headed form or dialog are Delete.
    #[test]
    fn delete_dialogs() {
        let dlg = |mut r: IndexRow, h: &str| {
            r.landmark = Some("dialog".into());
            r.heading = Some(h.into());
            r
        };
        let h = "Delete account?";
        let idx = vec![
            dlg(row(1, "button", "Cancel"), h),
            dlg(row(2, "button", "Confirm"), h),
            dlg(row(3, "button", "Close"), h),
            dlg(row(4, "button", "No"), h),
            dlg(row(5, "button", "Back"), h),
            dlg(row(6, "button", "Keep my account"), h),
            dlg(row(7, "button", "Yes"), h),
            dlg(row(8, "button", "OK"), h),
            dlg(row(9, "button", "Continue"), h),
            dlg(row(10, "button", "Proceed"), h),
            dlg(row(11, "checkbox", "Yes, I am sure"), h),
            dlg(row(12, "textbox", "Type OK to confirm"), h),
        ];
        for p in ["c#2", "c#7", "c#8", "c#9", "c#10", "k#2=Enter", "k#2=Space"] {
            assert_eq!(cls(p, &idx), Class::Delete, "{p}");
        }
        for p in ["c#1", "c#3", "c#4", "c#5", "c#6", "k#1=Enter"] {
            assert_eq!(cls(p, &idx), Class::Navigate, "{p}");
        }
        // A checkbox only toggles and a field only takes text, even in that dialog.
        assert_eq!(cls("c#11", &idx), Class::Fill);
        assert_eq!(cls("t#12=OK", &idx), Class::Fill);
        // The planner must stop after the dialog's Confirm.
        assert_eq!(
            check_planner(&parse("c#2").unwrap(), &idx),
            Err(PlannerError::MissingStop {
                step: 1,
                class: Class::Delete
            })
        );
        // A dialog with another heading (another dialog) is not borrowed.
        let mut other = idx.clone();
        other.push(dlg(row(20, "button", "Confirm"), "Save changes?"));
        assert_eq!(cls("c#20", &other), Class::Navigate);
        // A form: the heading can sit on any row of the form.
        let idx = vec![
            heading(
                form(row(1, "textbox", "Type the repository name"), "f4"),
                "Delete this repository",
            ),
            form(row(2, "button", "I understand, delete"), "f4"),
            form(row(3, "button", "Confirm"), "f4"),
            form(row(4, "button", "Cancel"), "f4"),
        ];
        assert_eq!(cls("c#2", &idx), Class::Delete);
        assert_eq!(cls("c#3", &idx), Class::Delete);
        assert_eq!(cls("c#4", &idx), Class::Navigate);
        // The heading on the button itself.
        let idx = vec![heading(
            form(row(2, "button", "I understand, delete"), "f4"),
            "Delete this repository",
        )];
        assert_eq!(cls("c#2", &idx), Class::Delete);
        // Form name as evidence.
        let idx = vec![
            form(row(1, "form", "Remove member"), "f4"),
            form(row(2, "button", "Yes"), "f4"),
        ];
        assert_eq!(cls("c#2", &idx), Class::Delete);
        // Neutral dialogs and forms stay as they were.
        let idx = vec![
            dlg(row(1, "button", "Confirm"), "Save changes?"),
            form(row(2, "button", "Continue"), "f5"),
        ];
        assert_eq!(cls("c#1", &idx), Class::Navigate);
        assert_eq!(cls("c#2", &idx), Class::Navigate);
    }

    /// D-404 (3): the Delete key deletes a selected row, edits text in a field.
    #[test]
    fn delete_key() {
        let idx = vec![
            row(1, "row", "Invoice from ACME, unread"),
            row(2, "option", "report.pdf"),
            row(3, "checkbox", "Select conversation"),
            row(4, "textbox", "Search mail"),
            input(row(5, "textbox", "Subject"), "text", None),
            row(6, "searchbox", "Search files"),
        ];
        for r in [1, 2, 3] {
            for k in ["Delete", "Shift+Delete", "Del", "delete", "Control+Delete"] {
                assert_eq!(cls(&format!("k#{r}={k}"), &idx), Class::Delete, "#{r} {k}");
            }
        }
        for r in [4, 5, 6] {
            for k in ["Delete", "Shift+Delete", "Backspace"] {
                assert_eq!(cls(&format!("k#{r}={k}"), &idx), Class::Fill, "#{r} {k}");
            }
        }
        // Delete on a pay button keeps the higher class.
        let pay = saved_card("Place order", None);
        assert_eq!(cls("k#2=Delete", &pay), Class::Pay);
        assert_eq!(
            check_planner(&parse("k#1=Delete").unwrap(), &idx),
            Err(PlannerError::MissingStop {
                step: 1,
                class: Class::Delete
            })
        );
    }

    #[test]
    fn delete_from_heading_and_form_name() {
        for h in [
            "Delete account?",
            "Remove this device",
            "Deactivate your account",
            "Close account",
            "Cancel your subscription",
            "Unsubscribe",
            "Discard draft?",
            "Erase all data",
            "Revoke access",
            "Move to trash",
        ] {
            let idx = vec![heading(form(submit(row(2, "button", "Confirm")), "f4"), h)];
            assert_eq!(cls("c#2", &idx), Class::Delete, "{h}");
        }
        // Form name.
        let idx = vec![
            form(row(1, "form", "Delete repository"), "f4"),
            form(submit(row(2, "button", "I understand")), "f4"),
        ];
        assert_eq!(cls("c#2", &idx), Class::Delete);
        // A login-shaped confirm (password re-entry) still says Delete.
        let idx = vec![
            heading(
                form(
                    input(
                        row(1, "textbox", "Password"),
                        "password",
                        Some("current-password"),
                    ),
                    "f4",
                ),
                "Delete account",
            ),
            heading(
                form(submit(row(2, "button", "Continue")), "f4"),
                "Delete account",
            ),
        ];
        assert_eq!(cls("c#2", &idx), Class::Delete);
        // New button words.
        for name in [
            "Deactivate account",
            "Close account",
            "Close my account",
            "Discard",
            "Erase",
            "Revoke",
        ] {
            let idx = vec![form(submit(row(2, "button", name)), "f4")];
            assert_eq!(cls("c#2", &idx), Class::Delete, "{name}");
            assert_eq!(
                cls("c#1", &[row(1, "button", name)]),
                Class::Delete,
                "{name}"
            );
        }
        // Negatives: marketing, neighbours of the words, and bare Close.
        for h in [
            "Cancel anytime",
            "Cancel at any time",
            "Recently removed",
            "Closed accounts",
            "Account settings",
        ] {
            let idx = vec![heading(form(submit(row(2, "button", "Save")), "f4"), h)];
            assert_eq!(cls("c#2", &idx), Class::Submit, "{h}");
        }
        assert_eq!(cls("c#1", &[row(1, "button", "Close")]), Class::Navigate);
    }

    #[test]
    fn more_pay_words() {
        for name in [
            "Submit payment",
            "Proceed to payment",
            "Confirm payment",
            "Buy",
            "Buy with 1-Click",
            "Purchase",
            "Donate",
            "Make a donation",
            "Subscribe and pay",
            "Subscribe for $9.99/month",
            "Subscribe, 5 EUR per month",
            "Start subscription",
            "Start my membership",
        ] {
            let idx = saved_card(name, None);
            assert_eq!(cls("c#2", &idx), Class::Pay, "{name}");
            assert_eq!(cls("k#2=Space", &idx), Class::Pay, "Space: {name}");
        }
        assert_eq!(
            cls("c#2", &saved_card("Continue", Some("Payment"))),
            Class::Pay
        );
        for (name, h) in [
            ("Save", "Buyer info"),
            ("Subscribe", "Stay in the loop"),
            ("Subscribe to our weekly newsletter", "News"),
            ("Join", "Buyback program"),
            ("Save", "Subscription preferences"),
        ] {
            let idx = saved_card(name, Some(h));
            assert_eq!(cls("c#2", &idx), Class::Submit, "{name} / {h}");
        }
    }

    #[test]
    fn more_card_fields() {
        let with_field = |f: IndexRow| {
            vec![
                form(f, "f3"),
                form(submit(row(2, "button", "Continue")), "f3"),
            ]
        };
        for name in [
            "CVC",
            "CVV",
            "CVV2",
            "Security code",
            "Expiry (MM/YY)",
            "MM / YY",
            "Card expiry",
            "Expiration date on card",
            "Cardholder name",
            "Card holder",
            "Name on card",
        ] {
            assert_eq!(
                cls("c#2", &with_field(row(1, "textbox", name))),
                Class::Pay,
                "{name}"
            );
        }
        // The label can be the nearby text when the name is only a placeholder.
        let mut f = row(1, "textbox", "123");
        f.nearby = Some("CVC".into());
        assert_eq!(cls("c#2", &with_field(f)), Class::Pay);
        // Negatives.
        for name in [
            "Security question",
            "Passport expiration date",
            "Expiry date (DD/MM/YYYY)",
            "Display name",
            "Gift card message",
        ] {
            assert_eq!(
                cls("c#2", &with_field(row(1, "textbox", name))),
                Class::Submit,
                "{name}"
            );
        }
        // An SMS security code is not a card's.
        let otp = input(
            row(1, "textbox", "Security code"),
            "text",
            Some("one-time-code"),
        );
        assert_eq!(cls("c#2", &with_field(otp)), Class::Submit);
        // A "Security settings" heading alone is not card evidence.
        let idx = vec![
            heading(
                form(row(1, "textbox", "Display name"), "f3"),
                "Security settings",
            ),
            heading(
                form(submit(row(2, "button", "Save")), "f3"),
                "Security settings",
            ),
        ];
        assert_eq!(cls("c#2", &idx), Class::Submit);
    }

    #[test]
    fn challenge_targets_stop_everything() {
        let mut cap = row(3, "iframe", "reCAPTCHA");
        cap.flags.challenge = true;
        cap.flags.opaque = true;
        let mut btn = submit(row(4, "button", "Verify"));
        btn.flags.challenge = true;
        let idx = vec![cap, btn];
        for p in [
            "c#3",
            "t#3=x",
            "s#3=a",
            "k#3=Enter",
            "u#3=a.png",
            "sc(#3)",
            "c#4",
        ] {
            assert_eq!(cls(p, &idx), Class::Challenge, "{p}");
        }
    }

    #[test]
    fn missing_target_is_submit() {
        let idx = login();
        for p in [
            "c#99",
            "t#99=x",
            "s#99=a",
            "k#99=a",
            "u#99=a.pdf",
            "sc(#99)",
            "c#f1.14",
        ] {
            let c = class_of(p, &idx);
            assert_eq!(c.class, Class::Submit, "{p}");
            assert!(c.hints.is_empty());
        }
    }

    #[test]
    fn non_target_steps() {
        let idx = login();
        assert_eq!(cls("sc(down)", &idx), Class::Navigate);
        assert_eq!(cls("sc(up)", &[]), Class::Navigate);
        assert_eq!(cls("sc(#7)", &idx), Class::Navigate);
        assert_eq!(cls("w(3s)", &idx), Class::Read);
        assert_eq!(cls("?s", &[]), Class::Read);
        assert_eq!(cls("s#7=Plus", &idx), Class::Fill);
        assert_eq!(cls("u#7=files/cv.pdf", &idx), Class::Fill);
    }

    #[test]
    fn job_application_hint() {
        let job = vec![HINT_JOB_APPLICATION.to_string()];
        // Resume upload.
        let mut cv = form(input(row(1, "button", "Choose file"), "file", None), "f6");
        cv.nearby = Some("Resume/CV".into());
        let idx = vec![cv, form(submit(row(2, "button", "Submit")), "f6")];
        assert_eq!(
            class_of("c#2", &idx),
            ActClass {
                class: Class::Submit,
                hints: job.clone()
            }
        );
        // Apply heading.
        let idx = vec![
            heading(
                form(row(1, "textbox", "Full name"), "f6"),
                "Apply for Senior Engineer",
            ),
            heading(
                form(submit(row(2, "button", "Submit")), "f6"),
                "Apply for Senior Engineer",
            ),
        ];
        assert_eq!(class_of("c#2", &idx).hints, job);
        assert_eq!(class_of("k#1=Enter", &idx).hints, job);
        // Button text.
        let idx = vec![form(submit(row(2, "button", "Submit application")), "f6")];
        assert_eq!(class_of("c#2", &idx).hints, job);
        // Kept when the class is raised (job portal that creates an account on apply).
        let idx = vec![
            heading(
                form(
                    input(
                        row(1, "textbox", "Password"),
                        "password",
                        Some("new-password"),
                    ),
                    "f6",
                ),
                "Application",
            ),
            heading(
                form(submit(row(2, "button", "Continue")), "f6"),
                "Application",
            ),
        ];
        assert_eq!(
            class_of("c#2", &idx),
            ActClass {
                class: Class::Account,
                hints: job.clone()
            }
        );
        // A plain file upload is not a job application; neither is a non-submit click.
        let idx = vec![
            form(input(row(1, "button", "Profile photo"), "file", None), "f7"),
            form(submit(row(2, "button", "Save")), "f7"),
            heading(row(3, "button", "Next"), "Apply for Senior Engineer"),
        ];
        assert!(class_of("c#2", &idx).hints.is_empty());
        assert!(class_of("c#3", &idx).hints.is_empty());
        assert!(class_of("u#1=files/cv.pdf", &idx).hints.is_empty());
    }

    #[test]
    fn signature_hint() {
        let sig = vec![HINT_SIGNATURE.to_string()];
        for name in [
            "Sign",
            "Sign document",
            "E-sign",
            "Adopt and sign",
            "Finish signature",
        ] {
            let idx = vec![form(submit(row(2, "button", name)), "f8")];
            assert_eq!(
                class_of("c#2", &idx),
                ActClass {
                    class: Class::Submit,
                    hints: sig.clone()
                },
                "{name}"
            );
        }
        let idx = vec![
            form(row(1, "img", "Signature pad"), "f8"),
            form(submit(row(2, "button", "Finish")), "f8"),
        ];
        assert_eq!(class_of("c#2", &idx).hints, sig);
        for name in ["Sign in", "Sign up", "Sign out", "Signup", "Design"] {
            let idx = vec![form(submit(row(2, "button", name)), "f8")];
            assert!(!class_of("c#2", &idx).hints.contains(&sig[0]), "{name}");
        }
    }

    #[test]
    fn program_level_union() {
        let mut idx = checkout();
        idx.extend(register());
        let steps = parse("t#7=a@b.c; c#14; ?s; c#22; ?s").unwrap();
        let c = classify(&steps, &idx);
        assert_eq!(c.class, Class::Pay);
        let steps: Vec<Step> = parse("t#7=a; w(1s)").unwrap();
        assert_eq!(classify(&steps, &idx).class, Class::Fill);
        assert_eq!(ElemRef::main(14), idx[5].elem());
    }

    // ---- D-406 (round-2 review probes) ----------------------------------------------

    /// A card form: #1 Promo code, #2 card number, #3 SUBMIT "Pay now"; #4 a file row.
    fn promo_card() -> Vec<IndexRow> {
        vec![
            form(row(1, "textbox", "Promo code"), "f3"),
            form(
                input(row(2, "textbox", "Card"), "text", Some("cc-number")),
                "f3",
            ),
            form(submit(row(3, "button", "Pay now")), "f3"),
            row(4, "row", "report.pdf"),
        ]
    }

    fn key_prog(target: u32, key: &str) -> String {
        format!("k#{target}={}", crate::quote(key))
    }

    /// D-406 (1): raw line breaks are Enter, only U+0020/U+00A0 are Space, any other
    /// control character is unknown (a click on the row, never Fill).
    #[test]
    fn d406_control_character_keys() {
        let idx = promo_card();
        for k in ["\r", "\n", "\r\n", "Shift+\r", "Control+\n", "a\rb"] {
            let p = key_prog(1, k);
            assert_eq!(cls(&p, &idx), Class::Pay, "{k:?}");
            assert_eq!(
                check_planner(&parse(&p).unwrap(), &idx),
                Err(PlannerError::MissingStop {
                    step: 1,
                    class: Class::Pay
                }),
                "{k:?}"
            );
        }
        // A raw CR inside quotes reaches the classifier as is.
        assert_eq!(cls("k#1=\"\r\"", &idx), Class::Pay);
        for k in [
            "\t", "\u{7f}", "\u{8}", "\u{3}", "Shift+\t", "\u{1b}", "\u{85}",
        ] {
            assert_eq!(
                cls(&key_prog(1, k), &idx),
                Class::Navigate,
                "{k:?} in the promo field"
            );
            // On the Pay submit it is a click on it.
            assert_eq!(cls(&key_prog(3, k), &idx), Class::Pay, "{k:?} on Pay now");
        }
        // DEL on a selected non-text row deletes it, like the Delete key (D-404 (3)).
        assert_eq!(cls(&key_prog(4, "\u{7f}"), &idx), Class::Delete);
        assert_eq!(cls(&key_prog(4, "\u{8}"), &idx), Class::Navigate);
        // Space is U+0020 or U+00A0 only; other whitespace is an unknown key.
        for k in [" ", "\u{a0}", "Shift+ ", "Space"] {
            assert_eq!(cls(&key_prog(1, k), &idx), Class::Fill, "{k:?}");
            assert_eq!(cls(&key_prog(3, k), &idx), Class::Pay, "{k:?} on Pay now");
        }
        for k in ["\u{2003}", "\u{3000}", "\u{2028}"] {
            assert_eq!(cls(&key_prog(1, k), &idx), Class::Navigate, "{k:?}");
        }
        // Printable characters still type.
        for k in ["a", "\u{e9}", "Shift+A", "Backspace"] {
            assert_eq!(cls(&key_prog(1, k), &idx), Class::Fill, "{k:?}");
        }
    }

    /// D-406 (2): Enter on any row inside a form takes the form max, whatever the role.
    #[test]
    fn d406_enter_on_activators_in_a_form_takes_the_form_max() {
        let mut idx = promo_card();
        let mut switch = form(row(5, "switch", "Save card"), "f3");
        switch.state.input_type = Some("checkbox".into());
        idx.extend([
            switch,
            form(row(6, "checkbox", "Save card"), "f3"),
            form(row(7, "button", "Show card number"), "f3"),
            form(row(8, "link", "Terms"), "f3"),
            form(row(9, "tab", "Card"), "f3"),
            form(row(10, "menuitem", "Options"), "f3"),
            form(row(11, "option", "Visa"), "f3"),
            form(row(12, "radio", "Mastercard"), "f3"),
        ]);
        for id in 5..=12 {
            for k in ["Enter", "NumpadEnter", "\r"] {
                assert_eq!(cls(&key_prog(id, k), &idx), Class::Pay, "#{id} {k:?}");
            }
        }
        // Space and clicks are still the row's own click.
        assert_eq!(cls("k#5=Space", &idx), Class::Fill);
        assert_eq!(cls("c#6", &idx), Class::Fill);
        assert_eq!(cls("c#7", &idx), Class::Navigate);
        assert_eq!(cls("k#8=Space", &idx), Class::Navigate);
        // "Show password" in a login form submits the login.
        let mut idx = login();
        idx.push(form(row(15, "button", "Show password"), "f2"));
        idx.push(form(row(16, "link", "Forgot password?"), "f2"));
        assert_eq!(cls("k#15=Enter", &idx), Class::Submit);
        assert_eq!(cls("k#16=Enter", &idx), Class::Submit);
        // Formless activators are still only their click.
        assert_eq!(
            cls("k#1=Enter", &[row(1, "switch", "Dark mode")]),
            Class::Fill
        );
        assert_eq!(
            cls("k#1=Enter", &[row(1, "link", "Pricing")]),
            Class::Navigate
        );
    }

    /// D-406 (2): a submitish field takes the max over the form's other submits.
    #[test]
    fn d406_submitish_field_takes_the_form_max() {
        for submitish in [false, true] {
            let mut email = form(input(row(1, "textbox", "Email"), "email", None), "f3");
            email.state.submitish = submitish;
            let idx = vec![
                email,
                form(row(2, "textbox", "Address"), "f3"),
                form(submit(row(3, "button", "Place order")), "f3"),
            ];
            assert_eq!(cls("k#1=Enter", &idx), Class::Pay, "submitish {submitish}");
        }
        // A SUBMIT button also takes the max over its form's other submits on Enter.
        let idx = vec![
            form(submit(row(1, "button", "Save for later")), "f3"),
            form(submit(row(2, "button", "Place order")), "f3"),
        ];
        assert_eq!(cls("c#1", &idx), Class::Submit);
        assert_eq!(cls("k#1=Enter", &idx), Class::Pay);
    }

    /// D-406 (2): a file input is not a text field for key steps.
    #[test]
    fn d406_file_input_keys_are_clicks() {
        for role in ["textbox", "button"] {
            let idx = vec![
                form(input(row(1, role, "Upload resume"), "file", None), "f6"),
                form(submit(row(2, "button", "Send")), "f6"),
                input(row(3, role, "Attach"), "file", None),
            ];
            for k in ["Space", " ", "a", "Backspace"] {
                assert_eq!(cls(&key_prog(1, k), &idx), Class::Navigate, "{role} {k:?}");
                assert_eq!(
                    cls(&key_prog(3, k), &idx),
                    Class::Navigate,
                    "{role} formless {k:?}"
                );
            }
            // Delete on it is the Delete key on a non-text row (D-404 (3)).
            assert_eq!(cls("k#1=Delete", &idx), Class::Delete, "{role}");
            // Enter in its form submits the form (the resume upload gives the hint).
            assert_eq!(
                class_of("k#1=Enter", &idx),
                ActClass {
                    class: Class::Submit,
                    hints: vec![HINT_JOB_APPLICATION.to_string()]
                },
                "{role}"
            );
            // A formless file input: Enter is a click on it.
            assert_eq!(cls("k#3=Enter", &idx), Class::Navigate, "{role}");
            // Focus keys and uploads are unchanged.
            assert_eq!(cls("k#1=Tab", &idx), Class::Fill);
            assert_eq!(cls("u#1=files/cv.pdf", &idx), Class::Fill);
        }
    }

    /// D-406 (3): the one-time-code exemption covers only the phrase "security code".
    #[test]
    fn d406_one_time_code_card_words_still_count() {
        let with_field = |f: IndexRow| {
            vec![
                heading(form(f, "f3"), "Details"),
                heading(form(submit(row(2, "button", "Next")), "f3"), "Details"),
            ]
        };
        let otp = |name: &str| input(row(1, "textbox", name), "text", Some("one-time-code"));
        for name in [
            "CVC",
            "CVV",
            "CSC",
            "Card number",
            "Cardholder name",
            "Card expiry",
            "Expiry (MM/YY)",
            "Card security code (CVC)",
        ] {
            assert_eq!(cls("c#2", &with_field(otp(name))), Class::Pay, "{name}");
            assert_eq!(
                cls("k#1=Enter", &with_field(otp(name))),
                Class::Pay,
                "Enter: {name}"
            );
        }
        // The label can come from nearby text too.
        let mut f = otp("123");
        f.nearby = Some("CVV".into());
        assert_eq!(cls("c#2", &with_field(f)), Class::Pay);
        // "Security code" on a one-time-code field is an SMS code.
        for name in [
            "Security code",
            "Enter the security code we sent you",
            "Card security code",
        ] {
            assert_eq!(cls("c#2", &with_field(otp(name))), Class::Submit, "{name}");
        }
        // Without one-time-code it is a card's.
        assert_eq!(
            cls("c#2", &with_field(row(1, "textbox", "Security code"))),
            Class::Pay
        );
    }

    /// D-406 (4): a challenge target anywhere wins over an earlier missing `?s`.
    #[test]
    fn d406_challenge_target_before_missing_stop() {
        let mut idx = login();
        let mut cap = row(42, "iframe", "reCAPTCHA");
        cap.flags.challenge = true;
        idx.push(cap);
        for (p, step) in [
            ("c#14; c#42; ?s", 2),
            ("c#14; k#42=Enter", 2),
            ("c#14; t#7=a; sc(#42)", 3),
            ("c#14; c#42; c#42", 2),
        ] {
            assert_eq!(
                check_planner(&parse(p).unwrap(), &idx),
                Err(PlannerError::ChallengeTarget { step }),
                "{p}"
            );
        }
        // Without a challenge target the ?s rule still applies.
        assert_eq!(
            check_planner(&parse("c#14; t#7=a").unwrap(), &idx),
            Err(PlannerError::MissingStop {
                step: 1,
                class: Class::Submit
            })
        );
    }

    /// D-406 (6): an affirmative button under a Delete heading is Delete with no form or
    /// dialog landmark (div modals).
    #[test]
    fn d406_affirmative_under_delete_heading_outside_form_or_dialog() {
        let h = "Delete account?";
        let idx = vec![
            heading(row(1, "button", "Confirm"), h),
            heading(row(2, "button", "Cancel"), h),
            heading(row(3, "button", "Yes"), h),
            heading(row(4, "checkbox", "Yes, I am sure"), h),
            heading(row(5, "button", "Confirm"), "Save changes?"),
            heading(row(6, "link", "OK"), "Remove this device"),
        ];
        for p in ["c#1", "k#1=Enter", "k#1=Space", "c#3", "c#6"] {
            assert_eq!(cls(p, &idx), Class::Delete, "{p}");
        }
        assert_eq!(cls("c#2", &idx), Class::Navigate);
        assert_eq!(cls("c#4", &idx), Class::Fill);
        assert_eq!(cls("c#5", &idx), Class::Navigate);
        // Marketing in the heading is still not Delete.
        assert_eq!(
            cls(
                "c#1",
                &[heading(row(1, "button", "Continue"), "Cancel anytime")]
            ),
            Class::Navigate
        );
    }

    /// D-406 (7): new Pay, Account and Delete words, with negatives.
    #[test]
    fn d406_new_words() {
        // Pay.
        for name in [
            "Submit order",
            "Submit your order",
            "Upgrade",
            "Upgrade to Pro",
            "Upgrade plan",
            "Subscribe for $5/month",
            "Subscribe, 12 per year",
        ] {
            let idx = saved_card(name, None);
            assert_eq!(cls("c#2", &idx), Class::Pay, "{name}");
            assert_eq!(cls("k#1=Enter", &idx), Class::Pay, "Enter: {name}");
        }
        assert_eq!(
            cls("c#2", &saved_card("Submit", Some("Submit your order"))),
            Class::Pay
        );
        // A non-submit "Upgrade" button changes the plan; a link only opens pricing.
        assert_eq!(
            cls("c#1", &[row(1, "button", "Upgrade to Pro")]),
            Class::Pay
        );
        assert_eq!(cls("c#1", &[row(1, "menuitem", "Upgrade")]), Class::Pay);
        for (role, name) in [
            ("link", "Upgrade"),
            ("button", "Upgraded features"),
            ("button", "Order history"),
        ] {
            assert_eq!(
                cls("c#1", &[row(1, role, name)]),
                Class::Navigate,
                "{role} {name}"
            );
        }
        for (name, h) in [
            ("Submit", "Order summary"),
            ("Submit feedback", "Your order"),
            ("Subscribe", "Newsletter"),
            ("Save", "Upgrade options"),
        ] {
            let idx = saved_card(name, Some(h));
            assert_eq!(cls("c#2", &idx), Class::Submit, "{name} / {h}");
        }
        // Account: passkeys and authenticator apps with a set-up verb.
        let code = || input(row(1, "textbox", "Code"), "text", Some("one-time-code"));
        for (h, button) in [
            ("Set up authenticator app", "Next"),
            ("Add a passkey", "Continue"),
            ("Manage passkeys", "Save"),
            ("Passkeys", "Add passkey"),
            ("Authenticator app", "Turn on"),
            ("Turn off authenticator app", "Confirm"),
        ] {
            let idx = vec![
                heading(form(code(), "f1"), h),
                heading(form(submit(row(2, "button", button)), "f1"), h),
            ];
            assert_eq!(cls("c#2", &idx), Class::Account, "{h} / {button}");
        }
        for name in ["Add passkey", "Set up authenticator app"] {
            let idx = vec![form(submit(row(2, "button", name)), "f1")];
            assert_eq!(cls("c#2", &idx), Class::Account, "{name}");
        }
        for (h, button) in [
            ("Sign in with a passkey", "Continue"),
            ("Enter the code from your authenticator app", "Verify"),
            ("Use your passkey", "Next"),
            ("Authenticator", "Next"),
        ] {
            let idx = vec![
                heading(form(code(), "f1"), h),
                heading(form(submit(row(2, "button", button)), "f1"), h),
            ];
            assert_eq!(cls("c#2", &idx), Class::Submit, "{h} / {button}");
            assert_eq!(cls("k#1=Enter", &idx), Class::Submit, "Enter: {h}");
        }
        // Delete.
        for name in [
            "Clear history",
            "Clear browsing history",
            "Clear data",
            "Clear all",
            "Clear all data",
            "Empty trash",
        ] {
            assert_eq!(
                cls("c#1", &[row(1, "button", name)]),
                Class::Delete,
                "{name}"
            );
            let idx = vec![form(submit(row(2, "button", name)), "f4")];
            assert_eq!(cls("c#2", &idx), Class::Delete, "{name}");
        }
        let idx = vec![heading(
            form(submit(row(2, "button", "Confirm")), "f4"),
            "Clear browsing data",
        )];
        assert_eq!(cls("c#2", &idx), Class::Delete);
        for name in [
            "Clear",
            "Clear filters",
            "Clear search",
            "Nuclear data",
            "Clearance",
        ] {
            assert_eq!(
                cls("c#1", &[row(1, "button", name)]),
                Class::Navigate,
                "{name}"
            );
        }
    }

    /// D-407: final Pay and Account word lists, with negatives.
    #[test]
    fn d407_word_lists() {
        for name in [
            "Complete order",
            "Complete your order",
            "Order now",
            "Place bid",
            "Place your bid",
            "Renew",
            "Renew subscription",
            "Top up",
            "Top-up",
            "Topup",
            "Send money",
            "Transfer money",
            "Transfer funds",
            "Transfer the funds",
        ] {
            let idx = saved_card(name, None);
            assert_eq!(cls("c#2", &idx), Class::Pay, "{name}");
            assert_eq!(cls("k#1=Enter", &idx), Class::Pay, "Enter: {name}");
        }
        // Heading text counts for every new word except Renew (on a submit only).
        for h in ["Complete your order", "Top up your balance", "Send money"] {
            let idx = saved_card("Continue", Some(h));
            assert_eq!(cls("c#2", &idx), Class::Pay, "{h}");
        }
        for (name, h) in [
            ("Save", "Renewal date"),
            ("Save", "Renew your subscription"),
            ("Save", "Order history"),
            ("Book now", "Your table"),
            ("Reserve", "Your table"),
            ("Transfer ownership", "Repository settings"),
            ("Send message", "Contact"),
            ("Continue", "Back to top"),
        ] {
            let idx = saved_card(name, Some(h));
            assert_eq!(cls("c#2", &idx), Class::Submit, "{name} / {h}");
        }
        assert_eq!(cls("c#2", &saved_card("Book now", None)), Class::Submit);
        for (role, name) in [
            ("link", "Order history"),
            ("link", "Renew"),
            ("button", "Renewal date"),
            ("button", "Transfer ownership"),
        ] {
            assert_eq!(
                cls("c#1", &[row(1, role, name)]),
                Class::Navigate,
                "{role} {name}"
            );
        }
        // Account: create is a set-up verb.
        let code = || input(row(1, "textbox", "Code"), "text", Some("one-time-code"));
        for (h, button) in [("Create a passkey", "Continue"), ("Passkeys", "Create")] {
            let idx = vec![
                heading(form(code(), "f1"), h),
                heading(form(submit(row(2, "button", button)), "f1"), h),
            ];
            assert_eq!(cls("c#2", &idx), Class::Account, "{h} / {button}");
            assert_eq!(cls("k#1=Enter", &idx), Class::Account, "Enter: {h}");
        }
        let idx = vec![form(submit(row(2, "button", "Create a passkey")), "f1")];
        assert_eq!(cls("c#2", &idx), Class::Account);
        let idx = vec![
            heading(form(code(), "f1"), "Sign in with a passkey"),
            heading(
                form(submit(row(2, "button", "Continue")), "f1"),
                "Sign in with a passkey",
            ),
        ];
        assert_eq!(cls("c#2", &idx), Class::Submit);
        // Sanctioned as coded (D-407): Cancel and/or ... is Navigate, Backspace on a
        // non-text row is a click on it.
        for name in ["Cancel and close", "Cancel or go back"] {
            assert_eq!(
                cls("c#1", &[row(1, "button", name)]),
                Class::Navigate,
                "{name}"
            );
        }
        assert_eq!(
            cls("k#1=Backspace", &[row(1, "row", "Invoice from ACME")]),
            Class::Navigate
        );
    }

    /// D-408: non-submit Pay/Account buttons in a form, Enter chords, Delete words.
    #[test]
    fn d408_form_buttons_enter_chords_delete_words() {
        let card_form = |name: &str| {
            vec![
                form(
                    input(row(1, "textbox", "Card number"), "text", Some("cc-number")),
                    "f3",
                ),
                form(input(row(2, "textbox", "Promo"), "text", None), "f3"),
                form(row(3, "button", name), "f3"),
            ]
        };
        // (1) A type=button "Complete purchase" in a card form is Pay on a click and Space.
        let idx = card_form("Complete purchase");
        assert_eq!(cls("c#3", &idx), Class::Pay);
        assert_eq!(cls("k#3=Space", &idx), Class::Pay);
        // A single Pay word in a card form; a menuitem too.
        assert_eq!(cls("c#3", &card_form("Confirm")), Class::Pay);
        let mut idx = card_form("Place bid");
        idx[2].role = "menuitem".into();
        assert_eq!(cls("c#3", &idx), Class::Pay);
        // Pay words on the name raise in a form without card evidence.
        for name in ["Order now", "Top up", "Send money", "Renew", "Donate"] {
            let idx = vec![
                form(row(1, "textbox", "Amount"), "f1"),
                form(row(3, "button", name), "f1"),
            ];
            assert_eq!(cls("c#3", &idx), Class::Pay, "{name}");
        }
        // Account: a type=button "Create account" in a signup form.
        let idx = vec![
            heading(
                form(input(row(1, "textbox", "Email"), "email", None), "f1"),
                "Sign up",
            ),
            heading(form(row(3, "button", "Create account"), "f1"), "Sign up"),
        ];
        assert_eq!(cls("c#3", &idx), Class::Account);
        // A set-up verb on a button sharing a Passkeys heading with a form.
        let idx = vec![
            heading(form(row(1, "textbox", "Name"), "f1"), "Passkeys"),
            heading(row(3, "button", "Add"), "Passkeys"),
        ];
        assert_eq!(cls("c#3", &idx), Class::Account);
        // Sharing a heading with a card form: a single Pay word is Pay.
        let idx = vec![
            heading(
                form(
                    input(row(1, "textbox", "Card number"), "text", Some("cc-number")),
                    "f3",
                ),
                "Payment",
            ),
            heading(row(3, "button", "Complete"), "Payment"),
        ];
        assert_eq!(cls("c#3", &idx), Class::Pay);
        // Not raised: a plain button in a card form, a formless button, and links.
        assert_eq!(cls("c#3", &card_form("Show details")), Class::Navigate);
        assert_eq!(
            cls("k#3=Space", &card_form("Show details")),
            Class::Navigate
        );
        assert_eq!(cls("c#3", &card_form("Back to top")), Class::Navigate);
        assert_eq!(
            cls("c#1", &[row(1, "button", "Create account")]),
            Class::Navigate
        );
        let mut idx = card_form("Complete purchase");
        idx[2].role = "link".into();
        assert_eq!(cls("c#3", &idx), Class::Navigate);
        assert_eq!(
            cls("c#1", &[form(row(1, "link", "Create account"), "f1")]),
            Class::Navigate
        );
        // (2) Enter anywhere in a chord, or next to separators and whitespace, on the Promo
        // field of a card form.
        let idx = card_form("Show details");
        for key in [
            "Enter",
            "Enter+",
            "Enter+Shift",
            "Enter+a",
            "Shift+Enter",
            " Enter",
            "Enter\t",
            "\tEnter",
            "Enter\0",
            "Return",
            "NumpadEnter+Control",
        ] {
            let step = Step::Key {
                target: ElemRef::main(2),
                key: key.into(),
            };
            assert_eq!(classify_step(&step, &idx).class, Class::Pay, "{key:?}");
        }
        assert_eq!(cls("k#2=Enter+Shift", &idx), Class::Pay);
        assert_eq!(cls("k#2=Enterprise", &idx), cls("c#2", &idx));
        // (3) Delete words.
        for name in [
            "Leave group",
            "Leave this group",
            "Wipe device",
            "Wipe this device",
        ] {
            assert_eq!(
                cls("c#1", &[row(1, "button", name)]),
                Class::Delete,
                "{name}"
            );
            let idx = vec![form(submit(row(1, "button", name)), "f1")];
            assert_eq!(cls("c#1", &idx), Class::Delete, "submit {name}");
        }
    }

    /// A page-wide form: `submits` SUBMIT rows, then text fields, `rows` in all.
    fn big_form(submits: u32, rows: u32) -> Vec<IndexRow> {
        (0..rows)
            .map(|i| {
                let r = if i < submits {
                    submit(row(i + 1, "button", &format!("Save row {i}")))
                } else {
                    input(
                        row(i + 1, "textbox", &format!("Quantity {i}")),
                        "text",
                        None,
                    )
                };
                heading(form(r, "f1"), "Inventory admin")
            })
            .collect()
    }

    /// D-406 (5): Enter in a 200-submit x 1 700-row form is linear in the index.
    #[test]
    fn d406_enter_in_a_big_form_is_linear() {
        use std::time::{Duration, Instant};
        let idx = big_form(200, 1700);
        let start = Instant::now();
        assert_eq!(cls("k#900=Enter", &idx), Class::Submit);
        let one = start.elapsed();
        // The target is well under 100 ms in a debug build; the quadratic scan of round 2
        // took seconds. The bound is wide so swarm load cannot flake it; the counter below
        // is the exact check.
        assert!(one < Duration::from_secs(5), "Enter took {one:?}");
        // Evidence is built once per form for a whole program, whatever the step count.
        let ctx = super::Ctx::new(&idx);
        for id in [201, 900, 1700, 5, 199] {
            let steps = parse(&format!("k#{id}=Enter")).unwrap();
            assert_eq!(ctx.step(&steps[0]).class, Class::Submit, "#{id}");
        }
        assert_eq!(ctx.evidence_builds(), 1);
        let steps = parse("k#900=Enter; ?s; k#901=Enter; ?s; c#1; ?s").unwrap();
        let start = Instant::now();
        assert_eq!(classify(&steps, &idx).class, Class::Submit);
        assert_eq!(check_planner(&steps, &idx), Ok(()));
        let prog = start.elapsed();
        assert!(prog < Duration::from_secs(5), "program took {prog:?}");
        // A Pay submit anywhere in it still raises Enter on every row.
        let mut idx = idx;
        idx[150].name = "Place order".into();
        assert_eq!(cls("k#900=Enter", &idx), Class::Pay);
    }
}
