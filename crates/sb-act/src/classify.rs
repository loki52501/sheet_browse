//! B-7 step classification: D-402, refined by D-433 (Enter, form context), D-434 (hints),
//! D-58 (Pay from checkout text) and D-403 (keys, implicit submission, more evidence).
//! Every refinement only raises a class (D-36 max), never lowers one.

use crate::{ActClass, Scroll, Step};
use harness_core::gate::{HINT_JOB_APPLICATION, HINT_SIGNATURE};
use harness_core::Class;
use sb_sheet::IndexRow;

/// Roles a plain (non-submit) click only toggles or selects: Fill (D-402).
const FILL_ROLES: &[&str] = &["checkbox", "radio", "switch", "option", "tab"];

/// Roles where Enter activates the element itself instead of submitting its form (D-403).
const ACTIVATOR_ROLES: &[&str] = &[
    "button",
    "link",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "switch",
    "tab",
    "treeitem",
];

/// Roles and `<input type>`s that take typed text (D-403 "printable characters into a
/// textbox").
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

pub(crate) fn classify_step(step: &Step, index: &[IndexRow]) -> ActClass {
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
    // A ref should be unique, but if a producer ever emits it twice the max over all its
    // rows wins, so a duplicate CHALLENGE row cannot be shadowed by an earlier one.
    let rows: Vec<&IndexRow> = index.iter().filter(|r| r.elem() == target).collect();
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
            Step::Click { .. } => click(row, index),
            Step::Key { key, .. } => key_press(row, index, key),
            Step::Scroll(_) => plain(Class::Navigate),
            _ => plain(Class::Fill),
        };
        merge(&mut out, c);
    }
    out
}

/// A key name split into lowercased modifiers and key: `"Shift+Tab"` -> `(["shift"],
/// "tab")`. A trailing `+` is the plus key itself (`"+"`, `"Control++"`), and a
/// whitespace-only key (`" "`) is `"space"`.
fn split_key(key: &str) -> (Vec<String>, String) {
    let (mods, last) = match key.strip_suffix('+') {
        Some("") => ("", "+"),
        Some(rest) if rest.ends_with('+') => (rest.trim_end_matches('+'), "+"),
        _ => key.rsplit_once('+').unwrap_or(("", key)),
    };
    let last = if !last.is_empty() && last.trim().is_empty() {
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

fn is_text_field(row: &IndexRow) -> bool {
    TEXT_ROLES.contains(&row.role.as_str())
        || row
            .state
            .input_type
            .as_deref()
            .is_some_and(|t| TEXT_INPUT_TYPES.iter().any(|x| t.eq_ignore_ascii_case(x)))
}

/// A key step (D-403 (1)). Keys known not to activate are Fill: Tab, Escape, arrows,
/// Home/End/PageUp/PageDown (alone or with Shift), modifiers alone, and printable
/// characters, Space, Backspace and Delete typed into a text field. Enter in any form
/// (`Enter`, `NumpadEnter`, `Control+Enter`...) is [`enter`]. Every other key, including
/// any key this list does not know, is a click on its row, so a key step is never below a
/// `c#` on the same target (Space on a button, a letter on a link, `F13`...).
fn key_press(row: &IndexRow, index: &[IndexRow], key: &str) -> ActClass {
    let (mods, k) = split_key(key);
    if matches!(k.as_str(), "enter" | "numpadenter" | "return") {
        return enter(row, index);
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
    click(row, index)
}

fn is_submit(row: &IndexRow) -> bool {
    row.flags.submit || row.state.submitish
}

/// The row's form: rows with the same form id in the same frame; a row without a form id
/// is its own context (D-433, B-431).
fn form_of<'a>(row: &'a IndexRow, index: &'a [IndexRow]) -> Vec<&'a IndexRow> {
    match row.form.as_deref() {
        Some(form) => {
            let mut rows: Vec<&IndexRow> = index
                .iter()
                .filter(|r| r.frame == row.frame && r.form.as_deref() == Some(form))
                .collect();
            if !rows.iter().any(|r| std::ptr::eq(*r, row)) {
                rows.push(row);
            }
            rows
        }
        None => vec![row],
    }
}

fn click(row: &IndexRow, index: &[IndexRow]) -> ActClass {
    if is_submit(row) {
        return submit(row, &row.name, &form_of(row, index));
    }
    let class = if delete_text(&tokens(&row.name), false) {
        Class::Delete
    } else if FILL_ROLES.contains(&row.role.as_str()) {
        Class::Fill
    } else {
        Class::Navigate
    };
    plain(class)
}

/// Enter (D-433, D-403 (1)(2)). On a submit, button, link or other activator it is a click
/// on that row. On any other row in a form it is an implicit submission: at least Submit,
/// raised by the form's own evidence even when the form has no indexed submit row, and
/// by every submit row of the form (max). In a formless text field (chat composers, SPA
/// search boxes) it is Submit raised by the field's headings. Never below a click on the
/// row.
fn enter(row: &IndexRow, index: &[IndexRow]) -> ActClass {
    let mut out = click(row, index);
    if is_submit(row) || ACTIVATOR_ROLES.contains(&row.role.as_str()) {
        return out;
    }
    if row.form.is_none() && !is_text_field(row) {
        return out;
    }
    let form = form_of(row, index);
    // The field's own label is not submit-button text: only form evidence counts.
    merge(&mut out, submit(row, "", &form));
    for button in form.iter().filter(|r| is_submit(r)) {
        merge(&mut out, submit(button, &button.name, &form));
    }
    out
}

/// A form submit anchored at `anchor` (the submit button, or the field Enter was pressed
/// in): Submit raised to Pay / Account / Delete (max wins), with the D-434 hints. `name` is
/// the submit button's own text, empty for an implicit submission.
fn submit(anchor: &IndexRow, name: &str, form: &[&IndexRow]) -> ActClass {
    let name_toks = tokens(name);
    let mut class = Class::Submit;
    if form.iter().any(|r| is_card_field(r)) || is_pay(anchor, name, form) {
        class = class.max(Class::Pay);
    }
    if is_account(anchor, &name_toks, form) {
        class = class.max(Class::Account);
    }
    if delete_text(&name_toks, false)
        || form_texts(anchor, form).any(|t| delete_text(&tokens(t), true))
    {
        class = class.max(Class::Delete);
    }
    let mut hints = Vec::new();
    if is_job_application(anchor, &name_toks, form) {
        hints.push(HINT_JOB_APPLICATION.to_string());
    }
    if is_signature(anchor, &name_toks, form) {
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
/// [`card_label`]'s words.
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
        // An SMS or authenticator code ("Enter the security code we sent you").
        _ if has_autocomplete(row, "one-time-code") => false,
        _ => std::iter::once(row.name.as_str())
            .chain(row.nearby.as_deref())
            .any(|t| card_label(&tokens(t))),
    }
}

/// Card field wording, whole words only: card number, IBAN, CVC/CVV/CSC, security code,
/// cardholder / card holder / name on card, card expiry or expiration, MM/YY. A passport
/// "Expiration date" or "DD/MM/YYYY" is not a card's; neither is a "Security settings"
/// heading (headings are not field labels).
fn card_label(toks: &[String]) -> bool {
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
        || near(toks, &["security"], &["code"], 1)
        || (has_any(toks, &["card"]) && has_any(toks, &["expiry", "expiration", "exp", "expires"]))
        || near(toks, &["mm"], &["yy"], 1)
}

/// Checkout wording (D-58, D-403 (5)): checkout / check out, pay, payment, purchase, buy,
/// donate / donation, place or confirm order, add card, and paid-subscription phrases.
/// Small fillers between two words are allowed ("Place your order", "Add a new card").
///
/// Matching is on whole words from `tokens()`, never substrings, so "Repay later",
/// "Payroll settings", "Display options", "PayPal account" and "Buyer info" do not match.
/// "Pay with PayPal", "Google Pay" and "Buy with 1-Click" do, which is intended: a false
/// Pay costs one ask, a missed Pay costs money (D-58).
fn pay_text(text: &str) -> bool {
    let toks = tokens(text);
    has_any(
        &toks,
        &[
            "checkout", "pay", "payment", "purchase", "buy", "donate", "donation",
        ],
    ) || near(&toks, &["check"], &["out"], 1)
        || near(&toks, &["place", "confirm"], &["order"], 2)
        || near(&toks, &["add"], &["card"], 3)
        || paid_subscription(&toks, text)
}

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

/// The submit button's name, or the form's headings or name, say checkout (D-58). Raise-only:
/// the caller takes the max with the other evidence (D-36).
fn is_pay(anchor: &IndexRow, name: &str, form: &[&IndexRow]) -> bool {
    pay_text(name) || form_texts(anchor, form).any(pay_text)
}

/// Create account, sign up (H-1 Account). Login headings say this too ("Sign in or create
/// an account"), so in a login it counts only on the button (D-433).
fn create_text(toks: &[String]) -> bool {
    has_any(toks, &["signup"])
        || near(toks, &["sign"], &["up"], 1)
        || near(toks, &["create"], &["account"], 3)
}

/// Change password or email, two-factor / 2-step / MFA, security keys (H-1 Account). Counts
/// in headings even in a one-password form, where the password confirms the change
/// (D-403 (3)).
fn change_text(toks: &[String]) -> bool {
    near(
        toks,
        &["change", "update"],
        &["password", "email", "mail", "passcode"],
        3,
    ) || has_any(toks, &["2fa", "mfa", "totp"])
        || near(toks, &["two", "2", "multi"], &["factor", "step"], 1)
        || near(toks, &["security"], &["key", "keys"], 1)
}

/// Section headings over the anchor and its form, plus the names of `form` rows.
fn form_texts<'a>(anchor: &'a IndexRow, form: &'a [&'a IndexRow]) -> impl Iterator<Item = &'a str> {
    std::iter::once(anchor.heading.as_deref())
        .chain(form.iter().map(|r| r.heading.as_deref()))
        .flatten()
        .chain(
            form.iter()
                .filter(|r| r.role == "form")
                .map(|r| r.name.as_str()),
        )
}

fn is_account(anchor: &IndexRow, name: &[String], form: &[&IndexRow]) -> bool {
    let passwords: Vec<&IndexRow> = form.iter().copied().filter(|r| is_password(r)).collect();
    if passwords.len() >= 2 || form.iter().any(|r| has_autocomplete(r, "new-password")) {
        return true;
    }
    if create_text(name) || change_text(name) {
        return true;
    }
    // A login (one current-password field) is Submit; its "Sign in or create an account"
    // heading is not evidence (D-433), but "Change email" or "Two-factor" is (D-403).
    let login = passwords.len() == 1 && has_autocomplete(passwords[0], "current-password");
    form_texts(anchor, form).any(|t| {
        let t = tokens(t);
        change_text(&t) || (!login && create_text(&t))
    })
}

/// delete / remove / deactivate / discard / erase / revoke / unsubscribe / trash, close
/// account, or "cancel <something>" (D-402, D-403 (4)). Not Delete:
/// - a bare "Cancel" (D-402);
/// - "cancel and ..." / "cancel or ..." ("Cancel and close", "Cancel or go back"): these
///   dismiss a dialog like a bare Cancel, so they are read as D-402's bare-Cancel case
///   rather than "cancel <something>";
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
        || toks.iter().enumerate().any(|(i, t)| {
            t == "cancel"
                && toks.get(i + 1).is_some_and(|next| {
                    let next = next.as_str();
                    !matches!(next, "and" | "or")
                        && !(heading && matches!(next, "anytime" | "any" | "at"))
                })
        })
}

fn is_job_application(anchor: &IndexRow, name: &[String], form: &[&IndexRow]) -> bool {
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
    let heading = std::iter::once(anchor.heading.as_deref())
        .chain(form.iter().map(|r| r.heading.as_deref()))
        .flatten()
        .any(|h| has_any(&tokens(h), &["apply", "application", "applications"]));
    resume_upload || heading || has_any(name, &["application"])
}

fn says_signature(t: &str) -> bool {
    signature_words(&tokens(t))
}

fn signature_words(toks: &[String]) -> bool {
    has_any(
        toks,
        &["signature", "signatures", "esign", "esignature", "docusign"],
    ) || near(toks, &["e"], &["sign", "signature"], 1)
}

fn is_signature(anchor: &IndexRow, name: &[String], form: &[&IndexRow]) -> bool {
    let bare_sign = name.iter().enumerate().any(|(i, t)| {
        t == "sign"
            && !name.get(i + 1).is_some_and(|next| {
                matches!(next.as_str(), "in" | "up" | "out" | "on" | "off" | "into")
            })
    });
    bare_sign
        || signature_words(name)
        || form_texts(anchor, form).any(says_signature)
        || form.iter().any(|r| says_signature(&r.name))
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
        // A non-submit button in a form: Enter presses the button, it does not submit.
        let mut idx = login();
        idx.push(form(row(15, "button", "Show password"), "f2"));
        assert_eq!(cls("k#15=Enter", &idx), Class::Navigate);
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
            "Two-factor authentication",
            "Enable 2FA",
            "2-Step Verification",
            "Security keys",
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
        // Negatives: a login stays Submit; a "Security settings" re-auth is Submit.
        for h in [
            "Sign in or create an account",
            "Security settings",
            "Sign up for news",
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
}
