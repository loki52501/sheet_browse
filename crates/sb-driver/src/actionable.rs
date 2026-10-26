//! B-4 #6, Rust side: call the in-page `__sb.act(id)` (which checks resolve, connected,
//! scroll into view, visible, stable across two frames, enabled and the hit test, and answers
//! `{x, y}` or `{why}`), and on a retryable `why` retry on the D-603 schedule: 0, 50, 100,
//! 250, 500 ms, then 500 ms steps, inside a 5 s budget. Between attempts the driver waits on
//! an in-page condition (the first DOM mutation), capped by the schedule's step: never a
//! sleep (rule 6). The final `why` becomes the step error (B-8).
//!
//! Also the T-20 part of `Driver::act`: parse with `sb_act`, run actionability per step,
//! one `StepResult` per step run. Input synthesis is T-21.

use crate::runtime::CallError;
use crate::{ActResult, Driver, DriverError, SettleOpts, StepOutcome, StepResult, TabId, ToolCtx};
use harness_core::gate::StopReason;
use harness_core::Class;
use sb_act::{Scroll, Step};
use sb_sheet::ElemRef;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::time::Instant;

/// The 5 s actionability budget of one step (B-4 #6).
pub(crate) const ACT_CAP: Duration = Duration::from_secs(5);
/// Waits after a retryable `why`, in order; then [`STEP_MS`] each (D-603).
const SCHEDULE_MS: [u64; 5] = [0, 50, 100, 250, 500];
const STEP_MS: u64 = 500;
/// An attempt started before the budget ran out may still answer this long after it.
const ANSWER_GRACE: Duration = Duration::from_millis(300);
/// A `why` shown to the model is capped (page text: element ids and classes).
const WHY_CAP: usize = 200;
/// The retryable `why` for a page that does not answer before the deadline.
const NOT_ANSWERING: &str = "not answering";

/// A hidden tab is answered before act(id) runs: rAF never fires there (D-605).
const ACT_FN: &str = "function (id) {
  const sb = globalThis.__sb;
  if (!sb) return { why: 'no runtime' };
  if (document.visibilityState === 'hidden') return { why: 'hidden tab' };
  return sb.act(id);
}";
/// Driver-internal answer of [`ACT_FN`]; never shown.
const HIDDEN_TAB: &str = "hidden tab";
/// The retryable why for a tab still hidden after `Page.bringToFront` (minimized window).
const TAB_NOT_VISIBLE: &str = "the tab is not visible";

/// The wait between attempts: resolves at the first DOM mutation (something changed, so a
/// retry can see a different answer), or at `ms`, the deadline of that condition.
const CHANGE_FN: &str = "function (ms) {
  return new Promise((done) => {
    let timer = 0;
    const mo = new MutationObserver(() => end(true));
    const end = (changed) => {
      mo.disconnect();
      clearTimeout(timer);
      done(changed);
    };
    mo.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    timer = setTimeout(() => end(false), ms);
  });
}";

/// `w(..)`: settle in the page, capped at the step's duration.
const SETTLE_FN: &str = "function (opts) {
  const sb = globalThis.__sb;
  if (!sb) return { why: 'no runtime' };
  return sb.settle(opts);
}";

/// The wait before retry number `retry` (0-based): the D-603 schedule.
pub(crate) fn backoff(retry: usize) -> Duration {
    Duration::from_millis(SCHEDULE_MS.get(retry).copied().unwrap_or(STEP_MS))
}

/// The wait before retry `retry` when `elapsed` of the `cap` budget is spent, capped by what
/// is left; `None` once the budget is gone (no more attempts).
pub(crate) fn next_wait(retry: usize, elapsed: Duration, cap: Duration) -> Option<Duration> {
    let left = cap.checked_sub(elapsed).filter(|d| !d.is_zero())?;
    Some(backoff(retry).min(left))
}

/// D-603 classes of an actionability `why`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WhyKind {
    /// The page may change on its own: wait and retry.
    Retry,
    Gone,
    Stale,
    /// Stops the program (H-1.4, D-5).
    Challenge,
    /// The document has no base: re-walk once, then terminal.
    NoBase,
    /// Anything else: terminal (fail closed).
    Unknown,
}

pub(crate) fn classify_why(why: &str) -> WhyKind {
    match why {
        "gone" => WhyKind::Gone,
        "stale" => WhyKind::Stale,
        "challenge" => WhyKind::Challenge,
        "no base" => WhyKind::NoBase,
        "hidden"
        | "not stable (moving)"
        | "disabled"
        | "outside the viewport"
        | NOT_ANSWERING
        | TAB_NOT_VISIBLE => WhyKind::Retry,
        w if w.starts_with("covered by ") || w.starts_with("does not receive pointer events") => {
            WhyKind::Retry
        }
        _ => WhyKind::Unknown,
    }
}

/// Where to act, in the frame's viewport (CSS pixels). T-21 adds the frame's offset for
/// frames other than 0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Point {
    pub x: f64,
    pub y: f64,
}

/// A step's final actionability answer when it is not a point.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Why {
    /// The step error, e.g. `covered by div.cookie (OVERLAY #31) after 5.0 s`.
    pub text: String,
    pub kind: WhyKind,
    pub attempts: u32,
    /// From the start of the step to the first in-page answer.
    pub first_answer: Duration,
    pub elapsed: Duration,
}

fn parse_answer(v: &Value) -> Result<Point, String> {
    let num = |k: &str| v.get(k).and_then(Value::as_f64).filter(|n| n.is_finite());
    if let (Some(x), Some(y)) = (num("x"), num("y")) {
        return Ok(Point { x, y });
    }
    if let Some(w) = v.get("why").and_then(Value::as_str) {
        return Err(w.to_string());
    }
    let shape = match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object without x, y or why",
    };
    Err(format!("<{shape}>"))
}

/// A `why` as the model sees it: ids from frame N > 0 written `#fN.id` (B-4 #4), page text
/// (element ids and classes from `describe`) limited to plain characters so it cannot forge
/// harness syntax, capped at [`WHY_CAP`] characters.
pub(crate) fn tidy_why(why: &str, frame: u32) -> String {
    let scoped = if frame == 0 {
        why.to_string()
    } else {
        why.replace("(OVERLAY #", &format!("(OVERLAY #f{frame}."))
            .replace(" (#", &format!(" (#f{frame}."))
    };
    let mut out = String::new();
    for (n, c) in scoped.chars().enumerate() {
        if n == WHY_CAP {
            out.push_str("...");
            break;
        }
        let plain = c.is_alphanumeric() || " #._-:(),".contains(c);
        out.push(if plain { c } else { '?' });
    }
    out
}

/// The step as written, without its value: a `t#..=@vault(..)` value never leaves (H-2).
pub(crate) fn step_label(step: &Step) -> String {
    match step {
        Step::Type { target, .. } => format!("t{target}"),
        Step::Select { target, .. } => format!("s{target}"),
        Step::Click { target } => format!("c{target}"),
        Step::Key { target, .. } => format!("k{target}"),
        Step::Upload { target, .. } => format!("u{target}"),
        Step::Scroll(Scroll::Down) => "sc(down)".into(),
        Step::Scroll(Scroll::Up) => "sc(up)".into(),
        Step::Scroll(Scroll::To(t)) => format!("sc({t})"),
        Step::Wait(d) if d.subsec_millis() == 0 => format!("w({}s)", d.as_secs()),
        Step::Wait(d) => format!("w({}ms)", d.as_millis()),
        Step::StopHere => "?s".into(),
    }
}

/// The element an input step acts on (needs actionability first).
fn input_target(step: &Step) -> Option<ElemRef> {
    match step {
        Step::Type { target, .. }
        | Step::Select { target, .. }
        | Step::Click { target }
        | Step::Key { target, .. }
        | Step::Upload { target, .. } => Some(*target),
        Step::Scroll(_) | Step::Wait(_) | Step::StopHere => None,
    }
}

impl Driver {
    /// B-4 #6: `Ok(point)` once the element is actionable, `Err(why)` with the final answer
    /// (D-603). The outer error is the driver's own failure (tab gone, browser closed).
    pub(crate) async fn actionable(
        &self,
        tab: TabId,
        elem: ElemRef,
    ) -> Result<Result<Point, Why>, DriverError> {
        self.actionable_within(tab, elem, ACT_CAP).await
    }

    /// [`Driver::actionable`] with the budget given (tests use a short one).
    pub(crate) async fn actionable_within(
        &self,
        tab: TabId,
        elem: ElemRef,
        cap: Duration,
    ) -> Result<Result<Point, Why>, DriverError> {
        let t0 = Instant::now();
        let deadline = t0 + cap;
        let (mut attempts, mut retry, mut rewalked, mut fronted) = (0u32, 0usize, false, false);
        let mut first: Option<Duration> = None;
        loop {
            let call_deadline = deadline.max(Instant::now()) + ANSWER_GRACE;
            let answer = match self
                .call_sb(tab, elem.frame, ACT_FN, &[json!(elem.id)], call_deadline)
                .await
            {
                Ok(v) => parse_answer(&v),
                Err(CallError::Deadline) => Err(NOT_ANSWERING.to_string()),
                // Lost twice: the document went away under the step; its ids are stale.
                Err(CallError::Lost) => Err("stale".to_string()),
                Err(CallError::Driver(e)) => return Err(e),
            };
            attempts += 1;
            let first_answer = *first.get_or_insert_with(|| t0.elapsed());
            let mut raw = match answer {
                Ok(p) => return Ok(Ok(p)),
                Err(w) => w,
            };
            if raw == HIDDEN_TAB {
                if !fronted {
                    // Make it the visible tab of its window, then ask again at once.
                    fronted = true;
                    self.bring_to_front(tab, deadline).await?;
                    continue;
                }
                raw = TAB_NOT_VISIBLE.to_string();
            }
            let kind = classify_why(&raw);
            let elapsed = t0.elapsed();
            let why = |text: String| Why {
                text,
                kind,
                attempts,
                first_answer,
                elapsed,
            };
            match kind {
                WhyKind::NoBase if !rewalked => {
                    // Walk again with a fresh base, then ask once more (D-73).
                    rewalked = true;
                    self.unmark_walked(tab, elem.frame);
                }
                WhyKind::Retry => match next_wait(retry, elapsed, cap) {
                    Some(wait) => {
                        retry += 1;
                        self.wait_change(tab, elem.frame, wait).await?;
                    }
                    None => {
                        let text = format!(
                            "{} after {:.1} s",
                            tidy_why(&raw, elem.frame),
                            cap.as_secs_f64()
                        );
                        return Ok(Err(why(text)));
                    }
                },
                _ => {
                    let base = match kind {
                        WhyKind::Unknown => {
                            format!(
                                "unknown answer from the page: {}",
                                tidy_why(&raw, elem.frame)
                            )
                        }
                        _ => tidy_why(&raw, elem.frame),
                    };
                    let text = if attempts > 1 {
                        format!("{base} after {:.1} s", elapsed.as_secs_f64())
                    } else {
                        base
                    };
                    return Ok(Err(why(text)));
                }
            }
        }
    }

    /// `Page.bringToFront` on the tab's own session (its window shows it; D-605).
    async fn bring_to_front(&self, tab: TabId, deadline: Instant) -> Result<(), DriverError> {
        let session = self
            .with_registry(|r| r.session(tab).map(str::to_string))
            .ok_or(DriverError::TabGone(tab))?;
        let fut = self
            .cdp
            .start(Some(&session), "Page.bringToFront", json!({}));
        match tokio::time::timeout_at(deadline.max(Instant::now()) + ANSWER_GRACE, fut).await {
            Ok(r) => r.map(|_| ()).map_err(DriverError::from),
            Err(_) => Err(DriverError::Timeout("Page.bringToFront".into())),
        }
    }

    /// Wait for the first DOM mutation in the frame, at most `wait` (a condition with a
    /// cap; a zero wait sends nothing). Page-side failures end the wait early: the next
    /// attempt then reports the page's state.
    async fn wait_change(&self, tab: TabId, frame: u32, wait: Duration) -> Result<(), DriverError> {
        if wait.is_zero() {
            return Ok(());
        }
        let ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
        let deadline = Instant::now() + wait + ANSWER_GRACE;
        match self
            .call_sb(tab, frame, CHANGE_FN, &[json!(ms)], deadline)
            .await
        {
            Ok(_) | Err(CallError::Lost) | Err(CallError::Deadline) => Ok(()),
            Err(CallError::Driver(DriverError::Cdp(_))) => Ok(()),
            Err(CallError::Driver(e)) => Err(e),
        }
    }

    /// `w(d)`: settle in the page with `d` as the cap (B-7; network settle is T-22).
    async fn wait_step(&self, tab: TabId, d: Duration) -> Result<StepEnd, DriverError> {
        let cap_ms = u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        let opts = json!({"quiet": SettleOpts::default().quiet_ms, "cap": cap_ms});
        let deadline = Instant::now() + d + ANSWER_GRACE;
        let v = self
            .call_sb(tab, 0, SETTLE_FN, &[opts], deadline)
            .await
            .map_err(|e| e.into_driver("w()"))?;
        match v.get("settled").and_then(Value::as_bool) {
            Some(s) => Ok((StepOutcome::Ok, Some(s))),
            None => Ok((
                StepOutcome::Failed {
                    why: "settle gave no answer".into(),
                },
                None,
            )),
        }
    }

    /// The T-20 body of [`Driver::act`]: one `StepResult` per step run, stopping at the
    /// first failure, at `?s`, or at a challenge (H-1.4).
    pub(crate) async fn act_steps(
        &self,
        tab: TabId,
        program: &str,
        ctx: &ToolCtx,
    ) -> Result<ActResult, DriverError> {
        // TODO(T-21): check each step's live class against ctx.class (stop when higher) and
        // resolve `@vault(k)` only inside ctx.resolve_into, straight into Input.insertText.
        let _ = ctx;
        if !self.with_registry(|r| r.has_tab(tab)) {
            return Err(DriverError::TabGone(tab));
        }
        let steps = sb_act::parse(program)?;
        if steps.is_empty() {
            return Err(DriverError::Parse(sb_act::ParseError {
                step: 1,
                at: 0,
                msg: "the program has no steps".into(),
            }));
        }
        let mut out = Vec::with_capacity(steps.len());
        let mut stopped = None;
        for (i, step) in steps.iter().enumerate() {
            let (outcome, settled) = match (step, input_target(step)) {
                (Step::Wait(d), _) => self.wait_step(tab, *d).await?,
                (Step::StopHere, _) => (StepOutcome::Ok, None),
                (_, Some(elem)) => match self.actionable(tab, elem).await? {
                    // TODO(T-21): input synthesis (B-4 #7) at `p`, then consequences (B-4 #9).
                    Ok(p) => (
                        StepOutcome::Failed {
                            why: format!(
                                "not done: input synthesis is T-21 (target actionable at {:.1}, {:.1})",
                                p.x, p.y
                            ),
                        },
                        None,
                    ),
                    Err(w) => {
                        if w.kind == WhyKind::Challenge {
                            stopped = Some(StopReason {
                                class: Class::Challenge,
                                why: format!("CHALLENGE at {elem}: stopped, hand back to the user"),
                                element: Some(elem.to_string()),
                            });
                        }
                        (StepOutcome::Failed { why: w.text }, None)
                    }
                },
                // TODO(T-21): sc(down|up|#N).
                (_, None) => (
                    StepOutcome::Failed {
                        why: "not done: scrolling is T-21".into(),
                    },
                    None,
                ),
            };
            let end = !matches!(outcome, StepOutcome::Ok) || matches!(step, Step::StopHere);
            out.push(StepResult {
                n: i + 1,
                step: step_label(step),
                outcome,
                consequences: Vec::new(),
                settled,
            });
            if end {
                break;
            }
        }
        Ok(ActResult {
            steps: out,
            diff: None,
            stopped,
        })
    }
}

type StepEnd = (StepOutcome, Option<bool>);

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn backoff_follows_the_schedule_then_500_ms_steps() {
        let got: Vec<u64> = (0..8).map(|i| backoff(i).as_millis() as u64).collect();
        assert_eq!(got, [0, 50, 100, 250, 500, 500, 500, 500]);
    }

    #[test]
    fn next_wait_is_capped_by_the_budget_and_ends_with_it() {
        assert_eq!(next_wait(0, MS(0), ACT_CAP), Some(MS(0)));
        assert_eq!(next_wait(3, MS(1000), ACT_CAP), Some(MS(250)));
        assert_eq!(next_wait(9, MS(4800), ACT_CAP), Some(MS(200)), "capped");
        assert_eq!(next_wait(9, MS(5000), ACT_CAP), None, "budget spent");
        assert_eq!(next_wait(0, MS(5001), ACT_CAP), None);
        // Summing the schedule: retries until 5 s, never past it.
        let (mut t, mut n) = (MS(0), 0);
        while let Some(w) = next_wait(n, t, ACT_CAP) {
            t += w + MS(35); // each attempt costs about two frames
            n += 1;
        }
        assert!(t <= ACT_CAP + MS(35), "{t:?}");
        assert!(n > 10, "{n} retries");
    }

    #[test]
    fn whys_are_terminal_or_retryable_per_d603() {
        for w in [
            "hidden",
            "not stable (moving)",
            "disabled",
            "outside the viewport",
            "covered by div.cookie (OVERLAY #31)",
            "covered by nothing (outside the viewport)",
            "covered by span.x (#4)",
            "does not receive pointer events (hit div.a)",
            NOT_ANSWERING,
            TAB_NOT_VISIBLE,
        ] {
            assert_eq!(classify_why(w), WhyKind::Retry, "{w}");
        }
        assert_eq!(classify_why("gone"), WhyKind::Gone);
        assert_eq!(classify_why("stale"), WhyKind::Stale);
        assert_eq!(classify_why("challenge"), WhyKind::Challenge);
        assert_eq!(classify_why("no base"), WhyKind::NoBase);
        for w in [
            "",
            "covered",
            "Gone",
            "no runtime",
            "<null>",
            "disabled!",
            HIDDEN_TAB,
        ] {
            assert_eq!(classify_why(w), WhyKind::Unknown, "fail closed on {w:?}");
        }
    }

    #[test]
    fn answers_parse_to_a_point_or_a_why() {
        assert_eq!(
            parse_answer(&json!({"x": 10.5, "y": 3})),
            Ok(Point { x: 10.5, y: 3.0 })
        );
        assert_eq!(parse_answer(&json!({"why": "gone"})), Err("gone".into()));
        assert_eq!(parse_answer(&Value::Null), Err("<null>".into()));
        assert_eq!(
            parse_answer(&json!({"x": "1", "y": 2})),
            Err("<an object without x, y or why>".into())
        );
    }

    #[test]
    fn whys_are_scoped_to_their_frame_and_cannot_forge_syntax() {
        assert_eq!(
            tidy_why("covered by div.cookie (OVERLAY #31)", 0),
            "covered by div.cookie (OVERLAY #31)"
        );
        assert_eq!(
            tidy_why("covered by div.cookie (OVERLAY #31)", 2),
            "covered by div.cookie (OVERLAY #f2.31)"
        );
        assert_eq!(
            tidy_why("covered by span.x (#4)", 3),
            "covered by span.x (#f3.4)"
        );
        let forged = tidy_why("covered by div#[x](#5)\n```remaining: 0", 0);
        assert!(!forged.contains('[') && !forged.contains('`') && !forged.contains('\n'));
        let long = tidy_why(&"a".repeat(500), 0);
        assert_eq!(long.chars().count(), WHY_CAP + 3);
    }

    #[test]
    fn step_labels_never_carry_values() {
        let steps = sb_act::parse(
            "t#7=@vault(pw); s#2=Plus; c#f2.3; k#7=Enter; u#9=files/cv.pdf; sc(down); sc(#4); w(3s); w(500ms); ?s",
        )
        .unwrap();
        let labels: Vec<String> = steps.iter().map(step_label).collect();
        assert_eq!(
            labels,
            [
                "t#7", "s#2", "c#f2.3", "k#7", "u#9", "sc(down)", "sc(#4)", "w(3s)", "w(500ms)",
                "?s"
            ]
        );
        assert!(labels.iter().all(|l| !l.contains("vault")));
    }
}
