//! sb-act: pure (no I/O). The act DSL (B-7): `t#7=hello; s#12=Plus; c#14; w(3s); ?s`.
//! DRAFT signatures (I-10), owned by area page (T-19 / B-7).
//!
//! Values (D-52): a bare value runs to the next `;` or the end of the program; a quoted
//! value `t#7="a; b"` may contain anything, with `\"`, `\\` and `\n` escapes. Planners
//! build values with the tool page API's `q(s)` helper ([`quote`]), so an argument can
//! never inject a step.

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
    /// `w(3s)`: wait with settle, capped at the duration.
    Wait(Duration),
    /// `?s`: stop here and return state (required before a Submit in planners).
    StopHere,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    Down,
    Up,
    To(ElemRef),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("act parse error in step {step} at byte {at}: {msg}")]
pub struct ParseError {
    /// 1-based step number.
    pub step: usize,
    /// Byte offset into the program.
    pub at: usize,
    pub msg: String,
}

/// Quotes a value for a program (`q(s)` in the tool page API): `"..."` with escapes (D-52).
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

/// I-10.
pub fn parse(program: &str) -> Result<Vec<Step>, ParseError> {
    let _ = program;
    todo!("T-19/B-7: parser")
}

/// A class plus the gate hints that came with it (`job_application`, `signature`, D-39).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActClass {
    pub class: Class,
    pub hints: Vec<String>,
}

/// One step's class from its target's row and form context (B-7): Submit from SUBMIT flag
/// and form; Pay from card/IBAN fields in the same form (`autocomplete=cc-*`); Account from
/// password (`input_type=password`) or email-change fields or "create account"; Delete from
/// delete/cancel/unsubscribe text. Hints: `job_application` for a Submit in a form with a
/// resume/CV upload or an apply/application heading; `signature` for e-sign submits.
pub fn classify_step(step: &Step, index: &[IndexRow]) -> ActClass {
    let _ = (step, index);
    todo!("T-19/B-7: step classification")
}

/// I-10: the program's class is the max of its steps (`Read` for an empty program); hints
/// are the union over steps.
pub fn classify(steps: &[Step], index: &[IndexRow]) -> ActClass {
    let mut out = ActClass {
        class: Class::Read,
        hints: Vec::new(),
    };
    for s in steps {
        let c = classify_step(s, index);
        out.class = out.class.max(c.class);
        for h in c.hints {
            if !out.hints.contains(&h) {
                out.hints.push(h);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlannerError {
    #[error("step {step} is {class} but the next step is not ?s (D-50)")]
    MissingStop { step: usize, class: Class },
}

/// The planner `?s` rule (D-50, resolves B-1): every step classified Submit or higher must be
/// immediately followed by `?s`, so nothing runs past a submit before the model sees the
/// state. T-2's example `...; c#btn; ?s` is the canonical shape. Permission comes from the
/// gate's up-front check of the whole program's class, not from `?s`.
pub fn check_planner(steps: &[Step], index: &[IndexRow]) -> Result<(), PlannerError> {
    let _ = (steps, index);
    todo!("T-19/B-7: planner ?s rule")
}
