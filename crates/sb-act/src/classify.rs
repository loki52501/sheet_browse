//! B-7 step classification: D-402, refined by D-433 (Enter, form context) and D-434 (hints).

use crate::{ActClass, Scroll, Step};
use harness_core::gate::{HINT_JOB_APPLICATION, HINT_SIGNATURE};
use harness_core::Class;
use sb_sheet::IndexRow;

/// Roles a plain (non-submit) click only toggles or selects: Fill (D-402).
const FILL_ROLES: &[&str] = &["checkbox", "radio", "switch", "option", "tab"];

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
    let Some(row) = index.iter().find(|r| r.elem() == target) else {
        // Not in the index: cannot be shown to be below Submit (D-402).
        return plain(Class::Submit);
    };
    if row.flags.challenge {
        return plain(Class::Challenge);
    }
    match step {
        Step::Click { .. } => click(row, index),
        Step::Key { key, .. } if is_enter(key) => enter(row, index),
        Step::Scroll(_) => plain(Class::Navigate),
        _ => plain(Class::Fill),
    }
}

/// `Enter`, `Control+Enter`, `Meta+Enter`... (D-433).
fn is_enter(key: &str) -> bool {
    key.rsplit('+')
        .next()
        .is_some_and(|k| k.trim().eq_ignore_ascii_case("enter"))
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
        return submit(row, &form_of(row, index));
    }
    let class = if delete_text(&tokens(&row.name)) {
        Class::Delete
    } else if FILL_ROLES.contains(&row.role.as_str()) {
        Class::Fill
    } else {
        Class::Navigate
    };
    plain(class)
}

fn enter(row: &IndexRow, index: &[IndexRow]) -> ActClass {
    if is_submit(row) {
        return click(row, index);
    }
    if row.form.is_none() {
        return plain(Class::Fill);
    }
    let form = form_of(row, index);
    let mut out: Option<ActClass> = None;
    for button in form.iter().filter(|r| is_submit(r)) {
        let c = submit(button, &form);
        match out.as_mut() {
            Some(o) => merge(o, c),
            None => out = Some(c),
        }
    }
    out.unwrap_or_else(|| plain(Class::Fill))
}

/// A form submit through `button`: Submit raised to Pay / Account / Delete (max wins), with
/// the D-434 hints.
fn submit(button: &IndexRow, form: &[&IndexRow]) -> ActClass {
    let mut class = Class::Submit;
    if form.iter().any(|r| is_card_field(r)) {
        class = class.max(Class::Pay);
    }
    if is_account(button, form) {
        class = class.max(Class::Account);
    }
    if delete_text(&tokens(&button.name)) {
        class = class.max(Class::Delete);
    }
    let mut hints = Vec::new();
    if is_job_application(button, form) {
        hints.push(HINT_JOB_APPLICATION.to_string());
    }
    if is_signature(button, form) {
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

/// Card or IBAN field (D-402, D-433).
fn is_card_field(row: &IndexRow) -> bool {
    let cc = row.state.autocomplete.as_deref().is_some_and(|a| {
        a.split_whitespace()
            .any(|t| t.to_ascii_lowercase().starts_with("cc-"))
    });
    if cc {
        return true;
    }
    let toks = tokens(&row.name);
    match row.role.as_str() {
        // Hosted card frames (Stripe Elements and the like): opaque, named by the host.
        "iframe" => has_any(&toks, &["card", "payment", "iban"]),
        "button" | "link" => false,
        _ => {
            has_any(&toks, &["cardnumber", "iban"])
                || near(&toks, &["card"], &["number", "no", "num"], 1)
        }
    }
}

/// Create account, sign up, change password or email (H-1 Account).
fn account_text(toks: &[String]) -> bool {
    has_any(toks, &["signup"])
        || near(toks, &["sign"], &["up"], 1)
        || near(toks, &["create"], &["account"], 3)
        || near(
            toks,
            &["change", "update"],
            &["password", "email", "mail", "passcode"],
            3,
        )
}

/// Section headings over the button and its form, plus the names of `form` rows.
fn form_texts<'a>(button: &'a IndexRow, form: &'a [&'a IndexRow]) -> impl Iterator<Item = &'a str> {
    std::iter::once(button.heading.as_deref())
        .chain(form.iter().map(|r| r.heading.as_deref()))
        .flatten()
        .chain(
            form.iter()
                .filter(|r| r.role == "form")
                .map(|r| r.name.as_str()),
        )
}

fn is_account(button: &IndexRow, form: &[&IndexRow]) -> bool {
    let passwords: Vec<&IndexRow> = form.iter().copied().filter(|r| is_password(r)).collect();
    if passwords.len() >= 2 || form.iter().any(|r| has_autocomplete(r, "new-password")) {
        return true;
    }
    if account_text(&tokens(&button.name)) {
        return true;
    }
    // A login (one current-password field) is Submit; its "Sign in or create an account"
    // heading is not evidence (D-433).
    let login = passwords.len() == 1 && has_autocomplete(passwords[0], "current-password");
    !login && form_texts(button, form).any(|t| account_text(&tokens(t)))
}

/// delete / remove / unsubscribe / trash, or "cancel <something>"; a bare Cancel is not.
fn delete_text(toks: &[String]) -> bool {
    has_any(toks, &["delete", "remove", "unsubscribe", "trash"])
        || toks.iter().enumerate().any(|(i, t)| {
            t == "cancel"
                && toks
                    .get(i + 1)
                    .is_some_and(|next| !matches!(next.as_str(), "and" | "or"))
        })
}

fn is_job_application(button: &IndexRow, form: &[&IndexRow]) -> bool {
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
    let heading = std::iter::once(button.heading.as_deref())
        .chain(form.iter().map(|r| r.heading.as_deref()))
        .flatten()
        .any(|h| has_any(&tokens(h), &["apply", "application", "applications"]));
    resume_upload || heading || has_any(&tokens(&button.name), &["application"])
}

fn is_signature(button: &IndexRow, form: &[&IndexRow]) -> bool {
    let name = tokens(&button.name);
    let bare_sign = name.iter().enumerate().any(|(i, t)| {
        t == "sign"
            && !name.get(i + 1).is_some_and(|next| {
                matches!(next.as_str(), "in" | "up" | "out" | "on" | "off" | "into")
            })
    });
    let says_signature = |t: &str| {
        let toks = tokens(t);
        has_any(
            &toks,
            &["signature", "signatures", "esign", "esignature", "docusign"],
        ) || near(&toks, &["e"], &["sign", "signature"], 1)
    };
    bare_sign
        || says_signature(&button.name)
        || form_texts(button, form).any(says_signature)
        || form.iter().any(|r| says_signature(&r.name))
}

#[cfg(test)]
mod tests {
    use crate::{classify, classify_step, parse, ActClass, Step};
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
        assert_eq!(cls("k#8=Tab", &idx), Class::Fill);
        assert_eq!(cls("k#8=Escape", &idx), Class::Fill);
        assert_eq!(cls("k#8=enter", &idx), Class::Submit);
        assert_eq!(cls("k#8=Control+Enter", &idx), Class::Submit);
        // Enter on the submit button itself.
        assert_eq!(cls("k#14=Enter", &idx), Class::Submit);
        // Enter outside any form, and in a form with no submit: Fill (D-402).
        assert_eq!(cls("k#30=Enter", &idx), Class::Fill);
        assert_eq!(cls("k#31=Enter", &idx), Class::Fill);
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
