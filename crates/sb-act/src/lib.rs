//! sb-act: pure (no I/O). The act DSL (B-7): `t#7=hello; s#12=Plus; c#14; w(3s); ?s`.
//! Signatures are I-10, owned by area page (T-19 / B-7).
//!
//! Values (D-52): a bare value runs to the next `;` or the end of the program and is
//! trimmed; a quoted value `t#7="a; b"` may contain anything, with `\"`, `\\` and `\n`
//! escapes. Planners build values with the tool page API's `q(s)` helper ([`quote`]), so an
//! argument can never inject a step.
//!
//! Grammar details beyond B-7: empty steps are skipped (D-430), `w()` needs a unit and is
//! clamped to [`MAX_WAIT`] (D-431), lexing rules (D-432). Classification follows D-402,
//! refined by D-433 (Enter, form context), D-434 (hints), D-58 (Pay from checkout text),
//! D-403 (key steps, implicit submission, wider Pay/Account/Delete evidence), D-404 (2FA
//! only with a set-up verb, delete dialogs, the Delete key, planner challenge rejection)
//! and D-406 (control-character keys, Enter takes the form max on every row, challenge
//! targets before `?s`, per-form evidence cache, more words), D-407 (final word lists:
//! more Pay words, `create` as a 2FA set-up verb) and D-408 (non-submit buttons in a form
//! take a submit's Pay and Account name checks, Enter anywhere in a chord, leave group and
//! wipe device are Delete).
//!
//! Every refinement only raises a class (D-36). The one sanctioned lowering is D-404 (1):
//! second-factor text is Account only with a set-up verb, so a bare "Two-step
//! verification" code prompt is Submit again. D-406 (3) bounds the one-time-code
//! exemption: only the phrase "security code" on a one-time-code field is not card
//! evidence; its other card words still are.
//!
//! D-407 sanctions two behaviours as coded (not lowerings): "Cancel and ..." / "Cancel or
//! ..." is Navigate like a bare Cancel, and Backspace on a non-text row is a click on the
//! row, not Delete. From D-407 on, a missing word in a list is a Should item, not Blocking.

mod classify;
mod parse;

use harness_core::Class;
use sb_sheet::{ElemRef, IndexRow};
use std::time::Duration;

/// One parsed step (B-7 table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// `t#7=text`: replace the field's text. `text` may contain `@vault(name)` literals,
    /// resolved only into `Input.insertText` (H-2).
    Type { target: ElemRef, text: String },
    /// `s#12=Plus`: select by label or value.
    Select { target: ElemRef, choice: String },
    /// `c#14`: click or check.
    Click { target: ElemRef },
    /// `k#7=Enter`.
    Key { target: ElemRef, key: String },
    /// `u#9=files/cv.pdf`.
    Upload { target: ElemRef, path: String },
    /// `sc(down|up|#N)`.
    Scroll(Scroll),
    /// `w(3s)`: wait with settle, capped at the duration (itself clamped to [`MAX_WAIT`]).
    Wait(Duration),
    /// `?s`: stop here and return state (required right after a Submit in planners, D-50).
    StopHere,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    Down,
    Up,
    To(ElemRef),
}

/// Longest `w()` a program can ask for; longer waits are clamped to it (D-431).
pub const MAX_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("act parse error in step {step} at byte {at}: {msg}")]
pub struct ParseError {
    /// 1-based step number, counting only non-empty steps (D-430).
    pub step: usize,
    /// Byte offset into the program.
    pub at: usize,
    pub msg: String,
}

/// Quotes a value for a program (`q(s)` in the tool page API): `"..."` with escapes (D-52).
/// For every string `s`, `parse(&format!("t#1={}", quote(s)))` is one `Type` step with
/// `text == s`.
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// I-10. Parses a B-7 program into steps. `@vault(name)` stays a literal in the value.
pub fn parse(program: &str) -> Result<Vec<Step>, ParseError> {
    parse::parse(program)
}

/// A class plus the gate hints that came with it (`job_application`, `signature`, D-39).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActClass {
    pub class: Class,
    pub hints: Vec<String>,
}

/// One step's class from its target's row and form context (B-7, D-402, D-433, D-434,
/// D-58, D-403, D-404, D-406, D-407, D-408).
///
/// CHALLENGE target: Challenge. Target missing from the index: Submit. A click on a
/// SUBMIT/submitish row is Submit raised by the form (max wins): Pay for card/IBAN/CVC/
/// expiry/cardholder fields or checkout text (checkout, pay, payment, purchase, buy,
/// donate, place/confirm/submit order, add card, paid subscriptions) on the button, form
/// heading or form name, or "upgrade" on the button; Account for new-password / two
/// password fields / create-account, sign-up text (not in a login), or change-password,
/// change-email text and 2FA/security-key/passkey/authenticator-app text with a set-up verb
/// such as enable/set up/turn on/manage (even in a login; a bare "Two-step verification"
/// code prompt is Submit); Delete for delete/remove/deactivate/discard/erase/revoke/
/// unsubscribe/trash/close account/clear history, data or all/`cancel <x>` on the button,
/// form heading or form name. Other clicks: Delete by the same text rule, or for an
/// affirmative button (confirm/yes/ok/continue/proceed) whose own heading, form or dialog
/// carries Delete evidence; Pay for an "Upgrade" button (not a link); for a button or
/// menuitem (not a link) in a form or sharing a heading with one, the submit's own-name Pay
/// and Account checks, and Pay for any single Pay word when the form has Pay evidence
/// (D-408 (1)); Fill on
/// checkbox/radio/switch/option/tab, else Navigate (a bare Cancel or Close is Navigate).
///
/// Key steps (D-403 (1), D-406 (1)): Tab, Escape, arrows, Home/End/PageUp/PageDown,
/// modifiers alone and typing into a text field (not a file input) are Fill; Delete on a
/// non-text row is Delete; Enter (in any chord segment, D-408 (2)), or a raw `\r`/`\n` in the key value, on any row in a form
/// is the max of a click on the row and the form's implicit submission (at least Submit,
/// with the form's evidence even without a submit row, and the max over every submit of
/// the form), and in a formless text field it is Submit raised by its headings; any other
/// key, unknown keys and keys holding a control character included, is a click on its
/// row. Type, select and upload: Fill; scroll: Navigate; wait and `?s`: Read.
/// Hints: `job_application` for a submit in a form with a resume/CV upload or an
/// apply/application heading; `signature` for e-sign submits.
pub fn classify_step(step: &Step, index: &[IndexRow]) -> ActClass {
    classify::Ctx::new(index).step(step)
}

/// I-10: the program's class is the max of its steps (`Read` for an empty program); hints
/// are the union over steps. Form evidence is computed once per form for the whole
/// program (D-406 (5)).
pub fn classify(steps: &[Step], index: &[IndexRow]) -> ActClass {
    let ctx = classify::Ctx::new(index);
    let mut out = ActClass {
        class: Class::Read,
        hints: Vec::new(),
    };
    for s in steps {
        let c = ctx.step(s);
        out.class = out.class.max(c.class);
        for h in c.hints {
            if !out.hints.contains(&h) {
                out.hints.push(h);
            }
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlannerError {
    #[error("step {step} is {class} but the next step is not ?s (D-50)")]
    MissingStop { step: usize, class: Class },
    /// The step targets a CHALLENGE row. A challenge means stop (D-5), so no planner may
    /// aim a step at one, `?s` or not (D-404 (4)).
    #[error("step {step} targets a challenge; stop and hand back to the user (D-5)")]
    ChallengeTarget { step: usize },
}

/// The planner `?s` rule (D-50, resolves B-1): every step classified Submit or higher must be
/// immediately followed by `?s`, so nothing runs past a submit before the model sees the
/// state. T-2's example `...; c#btn; ?s` is the canonical shape. Permission comes from the
/// gate's up-front check of the whole program's class, not from `?s`.
///
/// Classification uses `index`, so a target missing from it counts as Submit (D-402): with
/// an empty index every targeted step needs a `?s` after it.
///
/// A step whose target is a CHALLENGE row is rejected outright with
/// [`PlannerError::ChallengeTarget`], even when `?s` follows (D-5, D-404 (4)). Every step
/// is checked for a challenge before the `?s` rule, so `c#14; c#42; ?s` with #42 a
/// CHALLENGE row is `ChallengeTarget { step: 2 }`, not a `MissingStop` the planner would
/// repair first (D-406 (4)).
pub fn check_planner(steps: &[Step], index: &[IndexRow]) -> Result<(), PlannerError> {
    let ctx = classify::Ctx::new(index);
    let classes: Vec<Class> = steps.iter().map(|s| ctx.step(s).class).collect();
    if let Some(i) = classes.iter().position(|c| *c == Class::Challenge) {
        return Err(PlannerError::ChallengeTarget { step: i + 1 });
    }
    for (i, &class) in classes.iter().enumerate() {
        if class >= Class::Submit && steps.get(i + 1) != Some(&Step::StopHere) {
            return Err(PlannerError::MissingStop { step: i + 1, class });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_sheet::{Flags, State};

    fn row(id: u32, role: &str, name: &str, form: Option<&str>) -> IndexRow {
        IndexRow {
            id,
            frame: 0,
            role: role.into(),
            name: name.into(),
            nearby: None,
            heading: None,
            state: State::default(),
            flags: Flags::default(),
            form: form.map(str::to_string),
            landmark: None,
            child_frame: None,
            bbox: None,
        }
    }

    /// Login form f2: #7 email, #8 password (current-password), #14 Sign in (SUBMIT), plus a
    /// non-form link #20.
    fn login() -> Vec<IndexRow> {
        let mut pw = row(8, "textbox", "Password", Some("f2"));
        pw.state.input_type = Some("password".into());
        pw.state.autocomplete = Some("current-password".into());
        let mut btn = row(14, "button", "Sign in", Some("f2"));
        btn.flags.submit = true;
        btn.state.submitish = true;
        vec![
            row(7, "textbox", "Email", Some("f2")),
            pw,
            btn,
            row(20, "link", "Pricing", None),
        ]
    }

    #[test]
    fn quote_escapes_step_separators_and_quotes() {
        assert_eq!(quote(r#"a; b"#), r#""a; b""#);
        assert_eq!(quote("say \"hi\"\n\\"), r#""say \"hi\"\n\\""#);
    }

    #[test]
    fn empty_program_is_read() {
        let c = classify(&[], &[]);
        assert_eq!(c.class, Class::Read);
        assert!(c.hints.is_empty());
    }

    #[test]
    fn program_class_is_max_and_hints_are_a_union() {
        let mut idx = login();
        let mut up = row(30, "textbox", "Resume", Some("f9"));
        up.state.input_type = Some("file".into());
        let mut apply = row(31, "button", "Send", Some("f9"));
        apply.flags.submit = true;
        idx.extend([up, apply]);
        let steps = parse("t#7=a@b.c; c#20; c#31; ?s; c#14; ?s").unwrap();
        let c = classify(&steps, &idx);
        assert_eq!(c.class, Class::Submit);
        assert_eq!(c.hints, vec![harness_core::gate::HINT_JOB_APPLICATION]);
    }

    #[test]
    fn planner_rule_accepts_the_t2_shape() {
        let idx = login();
        let ok = parse("t#7=a@b.c; t#8=@vault(pw); c#14; ?s").unwrap();
        assert_eq!(check_planner(&ok, &idx), Ok(()));
        // Non-submit steps need no ?s; a ?s anywhere else is allowed.
        let nav = parse("?s; c#20; sc(down); w(1s)").unwrap();
        assert_eq!(check_planner(&nav, &idx), Ok(()));
        assert_eq!(check_planner(&[], &idx), Ok(()));
    }

    #[test]
    fn planner_rule_rejects_a_submit_without_stop() {
        let idx = login();
        let last = parse("t#7=a@b.c; c#14").unwrap();
        assert_eq!(
            check_planner(&last, &idx),
            Err(PlannerError::MissingStop {
                step: 2,
                class: Class::Submit
            })
        );
        let run_on = parse("c#14; t#7=again; ?s").unwrap();
        assert_eq!(
            check_planner(&run_on, &idx),
            Err(PlannerError::MissingStop {
                step: 1,
                class: Class::Submit
            })
        );
        // Enter in the login form submits it.
        let enter = parse("t#8=@vault(pw); k#8=Enter; w(2s)").unwrap();
        assert!(matches!(
            check_planner(&enter, &idx),
            Err(PlannerError::MissingStop { step: 2, .. })
        ));
        // A target missing from the index is Submit, so it needs ?s too.
        let missing = parse("c#99").unwrap();
        assert_eq!(
            check_planner(&missing, &idx),
            Err(PlannerError::MissingStop {
                step: 1,
                class: Class::Submit
            })
        );
        let err = check_planner(&missing, &idx).unwrap_err().to_string();
        assert_eq!(err, "step 1 is Submit but the next step is not ?s (D-50)");
    }

    #[test]
    fn planner_rule_covers_higher_classes() {
        let mut idx = login();
        let mut card = row(40, "textbox", "Card number", Some("f3"));
        card.state.autocomplete = Some("cc-number".into());
        let mut pay = row(41, "button", "Pay now", Some("f3"));
        pay.flags.submit = true;
        let mut cap = row(42, "iframe", "reCAPTCHA", None);
        cap.flags.challenge = true;
        idx.extend([card, pay, cap]);
        let steps = parse("t#40=4242; c#41").unwrap();
        assert_eq!(
            check_planner(&steps, &idx),
            Err(PlannerError::MissingStop {
                step: 2,
                class: Class::Pay
            })
        );
        // A challenge target is rejected with or without ?s (D-404 (4)).
        for p in [
            "c#42; c#14; ?s",
            "c#42; ?s",
            "t#7=a; k#42=Enter; ?s",
            "sc(#42)",
        ] {
            assert_eq!(
                check_planner(&parse(p).unwrap(), &idx),
                Err(PlannerError::ChallengeTarget {
                    step: if p.starts_with("t#") { 2 } else { 1 }
                }),
                "{p}"
            );
        }
        let err = check_planner(&parse("c#42; ?s").unwrap(), &idx)
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "step 1 targets a challenge; stop and hand back to the user (D-5)"
        );
        // A program that only reads past it is fine.
        assert_eq!(check_planner(&parse("sc(down); ?s").unwrap(), &idx), Ok(()));
        assert_eq!(
            check_planner(&parse("t#40=4242; c#41; ?s").unwrap(), &idx),
            Ok(())
        );
    }
}
