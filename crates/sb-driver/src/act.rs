//! I-9 `Driver::act` (T-20 actionability, T-21 input, gate and consequences).
//!
//! Per step: the live index of the step's frame is read (or reused from the previous step's
//! settle), the step is classified against it with `sb_act` (an unknown target is Submit,
//! D-402), and a step whose live class is above `ctx.class`, or that reaches a CHALLENGE,
//! stops the program before any input (rule 4, H-1.4). Then actionability (B-4 #6) with the
//! step's in-page op in the same round trip, the input (B-4 #7, pipelined), and the
//! consequence window (B-4 #9): the input's replies (or a dialog, which blocks them), then
//! one in-page `settle()` capped at 500 ms that also returns the index, which is the next
//! step's live index and the diff's after side (B-8). T-22 (B-614, D-620): while a request
//! the step started is in flight the window waits for it (a condition, at most 3 s from the
//! input) before that call, and a step whose request is still in flight at the end never
//! reports "no visible change".
//!
//! Every in-page read after a step's input ends by that step's window deadline (input + 3 s;
//! for `w(d)`, its start + d + its grace) minus [`REPLY_MARGIN`]: the new-tab title reads
//! and the diff's reads of frames the window left no index for (B-4 #8). A read with no time
//! left is not made: the diff then has fewer rows, or none for that document, and an
//! after-state that is unknown never counts as "no visible change".
//!
//! Page strings are kept raw while the program runs. When it ends, every raw page string
//! (consequences, diff row names) is scrubbed against the values of the vault keys the
//! program typed, then escaped and capped (`diff.rs`), then the shown result is scrubbed
//! again (rule 2, D-616): a cap can never cut a value before the scrub sees it whole.

use crate::actionable::{input_target, step_label, tidy_why, StepEnd, Why, WhyKind, W_GRACE};
use crate::consequence::{is_main, raw_name, sanitize_name, Kind};
use crate::diff::{changes, clean_url, page_text, render, TEXT_CAP};
use crate::input::{
    click_events, key_events, ready, upload_path, InputFut, SCROLL_FN, SCROLL_TO_FN, UPLOAD_FN,
};
use crate::registry::Closed;
use crate::runtime::{CallError, OBJECT_GROUP};
use crate::scrub::{scrub_with, spellings};
use crate::settle::{Pass, REPLY_MARGIN};
use crate::{
    ActResult, Consequence, Driver, DriverError, SettleOpts, StepOutcome, StepResult, TabId,
    ToolCtx,
};
use harness_core::gate::StopReason;
use harness_core::vault::{vault_literal, vault_refs, MIN_VALUE_CHARS};
use harness_core::Class;
use sb_act::{Scroll, Step};
use sb_sheet::{ElemRef, IndexRow};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::Path;
use std::time::Duration;
use tokio::time::Instant;

/// B-4 #9: after a step, listen until the page settles or for at most this long.
pub(crate) const WINDOW: Duration = Duration::from_millis(500);
/// How long past the window the settle-and-index call may take to answer (a round trip on
/// a loaded machine, as `w()`'s grace).
const WINDOW_GRACE: Duration = Duration::from_millis(300);
/// A main-frame navigation the step started, or a new tab's first document, is awaited at
/// most this long from the input (the settle cap, B-4 #8).
const NAV_CAP: Duration = Duration::from_millis(3000);
/// Cap of one index read before a step's input (the gate's) and of an in-page scroll or
/// upload lookup. Reads after an input end by the step's window instead ([`reads_end`]).
const CALL_CAP: Duration = Duration::from_secs(3);
/// Cap of a new tab's title read.
const TITLE_CAP: Duration = Duration::from_millis(300);

const INDEX_FN: &str = "function () {
  const sb = globalThis.__sb;
  if (!sb) return { why: 'no runtime' };
  return sb.index();
}";

/// The consequence window's page side: settle (capped), then the index in the same call.
/// A hidden page (a `target=_blank` click put a new tab in front of it) is never settled:
/// its timers are throttled, so whether its settle answered within the window was a race
/// (accept-T-22 note 4). It gives its index at once, unsettled, and a page hidden by the
/// end of its settle is unsettled too (D-627).
const POST_FN: &str = "async function (opts) {
  const sb = globalThis.__sb;
  if (!sb) return { why: 'no runtime' };
  const hidden = () => document.visibilityState === 'hidden';
  const s = hidden() ? null : await sb.settle(opts);
  const ix = sb.index();
  return { settled: !!(s && s.settled) && !hidden(), index: Array.isArray(ix) ? ix : null };
}";

const TITLE_FN: &str = "function () { return String(document.title || ''); }";

/// Whether an id the live index does not list is still an element of this document:
/// `live`, or the resolver's why (`stale`: from an earlier document; `gone`).
const RESOLVE_FN: &str = "function (id) {
  const sb = globalThis.__sb;
  if (!sb) return 'no runtime';
  const el = sb.resolve(id);
  if (!el) return 'gone';
  if (el.why) return String(el.why);
  return 'live';
}";

/// Raw page strings are cut here on arrival (as the event log does); escaped and capped
/// after the scrub.
const RAW_CAP: usize = 2000;

/// The why of a type or key step whose element did not end up focused itself (D-615).
const NO_FOCUS: &str =
    "the element did not take focus (another element has it; target that element instead)";

/// The why of a step that did not run because a dialog is open on the tab.
const DIALOG_OPEN: &str = "a dialog is open on this tab: answer it with dialog() first";

fn failed_step(n: usize, step: &Step, why: impl Into<String>) -> StepResult {
    StepResult {
        n,
        step: step_label(step),
        outcome: StepOutcome::Failed { why: why.into() },
        consequences: Vec::new(),
        settled: None,
    }
}

/// A step that did not run because a dialog is open (raw text; shown at the end).
fn dialog_step(n: usize, step: &Step, text: String) -> StepResult {
    let mut s = failed_step(n, step, DIALOG_OPEN);
    s.consequences.push(Consequence::Dialog { text });
    s
}

/// When the in-page reads after a step's input must have answered: the step's window
/// deadline minus [`REPLY_MARGIN`], so act() returns by the deadline (B-4 #8). `None` when
/// that instant has passed: no read is made then.
fn reads_end(window_end: Instant) -> Option<Instant> {
    window_end
        .checked_sub(REPLY_MARGIN)
        .filter(|end| *end > Instant::now())
}

/// Index rows from an in-page answer, with the frame number the driver knows them by.
fn rows(v: &Value, frame: u32) -> Option<Vec<IndexRow>> {
    let mut rows = Vec::<IndexRow>::deserialize(v).ok()?;
    for r in &mut rows {
        r.frame = frame;
    }
    Some(rows)
}

/// The frame a step works in: its target's, or 0 (page scroll).
fn step_frame(step: &Step) -> u32 {
    match step {
        Step::Scroll(Scroll::To(t)) => t.frame,
        s => input_target(s).map_or(0, |t| t.frame),
    }
}

fn step_target(step: &Step) -> Option<ElemRef> {
    match step {
        Step::Scroll(Scroll::To(t)) => Some(*t),
        s => input_target(s),
    }
}

/// Rule 4 and H-1.4 for one step against its live index: `Some((stop, step why))` when the
/// step reaches a CHALLENGE or its live class is above the class the gate approved. A
/// missing index classifies every target as Submit (D-402).
pub(crate) fn gate_step(
    n: usize,
    step: &Step,
    index: Option<&[IndexRow]>,
    approved: Class,
) -> Option<(StopReason, String)> {
    let rows = index.unwrap_or(&[]);
    let elem = step_target(step);
    let element = elem.map(|e| e.to_string());
    // A challenge page answers one CHALLENGE document row (D-405 (3)).
    let page_challenge = rows
        .iter()
        .any(|r| r.flags.challenge && r.role == "document");
    let class = if page_challenge {
        Class::Challenge
    } else {
        sb_act::classify_step(step, rows).class
    };
    if class == Class::Challenge {
        let at = element.as_deref().unwrap_or("the page");
        let why = format!("CHALLENGE at {at}: stopped, hand back to the user");
        let stop = StopReason {
            class,
            why: why.clone(),
            element,
        };
        return Some((stop, why));
    }
    if class > approved {
        let why = format!(
            "stopped before input: step {n} {} is {class} on the live page, above the approved {approved} (H-1.4)",
            step_label(step)
        );
        let stop = StopReason {
            class,
            why: why.clone(),
            element,
        };
        return Some((stop, why));
    }
    None
}

/// How a step's input started.
enum Started {
    Input {
        fut: InputFut,
        /// Scroll steps: whether the scroll position moved.
        moved: Option<bool>,
    },
    Failed {
        why: String,
        challenge: Option<ElemRef>,
    },
}

impl Started {
    fn input(fut: InputFut) -> Started {
        Started::Input { fut, moved: None }
    }

    fn failed(why: impl Into<String>) -> Started {
        Started::Failed {
            why: why.into(),
            challenge: None,
        }
    }

    fn from_why(w: Why, target: ElemRef) -> Started {
        Started::Failed {
            challenge: (w.kind == WhyKind::Challenge).then_some(target),
            why: w.text,
        }
    }
}

/// A page-side failure of an in-page call as a step reason; the driver's own failures
/// (tab gone, browser closed) stay errors.
fn call_failure(e: CallError, what: &str, frame: u32) -> Result<String, DriverError> {
    match e {
        CallError::Lost => Ok("stale".into()),
        CallError::Deadline => Ok(format!("{what}: the page did not answer")),
        CallError::Driver(e @ (DriverError::Cdp(_) | DriverError::Timeout(_))) => {
            Ok(tidy_why(&e.to_string(), frame, &[]))
        }
        CallError::Driver(e) => Err(e),
    }
}

/// What the consequence window found.
#[derive(Default)]
struct Captured {
    consequences: Vec<Consequence>,
    settled: Option<bool>,
    /// The step frame's index after the step, from the document that is current now.
    after: Option<Vec<IndexRow>>,
    /// A dialog is open: the page is paused and the program stops after this step.
    paused: bool,
    failed: Option<String>,
    /// A request the step started was still in flight when the window ended (D-620).
    net_busy: bool,
    /// The window's deadline (input + 3 s): every read after the input ends by it.
    window_end: Option<Instant>,
}

/// What a step's consequence window watches: the tab, its session, the step's frame, the
/// log mark and the tabs before the input, and the run's files folder (for downloads).
struct Watch<'a> {
    tab: TabId,
    session: &'a str,
    frame: u32,
    mark: u64,
    tabs_before: BTreeSet<TabId>,
    files: &'a Path,
}

/// The settle-and-index answer.
struct Post {
    settled: Option<bool>,
    index: Option<Vec<IndexRow>>,
    doc: Option<String>,
    net_busy: bool,
}

/// Index state of one program run.
#[derive(Default)]
struct Pages {
    /// The live index per frame, while no step has run since it was read.
    live: BTreeMap<u32, Vec<IndexRow>>,
    /// Each frame's index as first read in this program (the diff's before side).
    first: BTreeMap<u32, Vec<IndexRow>>,
    /// The deadline of the last window (an input step's, or a `w(d)`'s) that cleared
    /// `live`: the diff's reads of the frames missing from `live` end by it ([`reads_end`]).
    window_end: Option<Instant>,
}

/// A Windows verbatim path (`\\?\C:\...`, from canonicalize) as a plain one, which every
/// consumer accepts.
fn plain_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => s.into_owned(),
    }
}

impl Driver {
    /// The body of [`Driver::act`]: one `StepResult` per step run, stopping at the first
    /// failure, at `?s`, at a dialog, or at a gate or challenge stop (H-1.4).
    pub(crate) async fn act_steps(
        &self,
        tab: TabId,
        program: &str,
        ctx: &ToolCtx,
    ) -> Result<ActResult, DriverError> {
        let session = self
            .with_registry(|r| r.session(tab).map(str::to_string))
            .ok_or(DriverError::TabGone(tab))?;
        let steps = sb_act::parse(program)?;
        if steps.is_empty() {
            return Err(DriverError::Parse(sb_act::ParseError {
                step: 1,
                at: 0,
                msg: "the program has no steps".into(),
            }));
        }
        let files = ctx.run_dir.join("files");
        let mut out = Vec::with_capacity(steps.len());
        let mut stopped = None;
        let mut pages = Pages::default();
        for (i, step) in steps.iter().enumerate() {
            let n = i + 1;
            // A dialog left open, or opened by a timer since the last step, blocks the
            // page's main thread: nothing can run until it is answered.
            if let Some(text) = self.open_dialog(tab) {
                out.push(dialog_step(n, step, text));
                break;
            }
            let (outcome, consequences, settled, paused) = match step {
                Step::Wait(d) => {
                    // w(d) answers by its start + d + its grace: the reads after it too.
                    let end = Instant::now() + *d + W_GRACE;
                    let (o, s): StepEnd = self.wait_step(tab, *d).await?;
                    pages.live.clear();
                    pages.window_end = Some(end);
                    (o, Vec::new(), s, false)
                }
                Step::StopHere => (StepOutcome::Ok, Vec::new(), None, false),
                _ => {
                    let frame = step_frame(step);
                    // A frame the registry calls opaque or CHALLENGE (or inside one) is never
                    // read or acted in, judged now (B-5, D-5): the step fails before any call;
                    // a CHALLENGE also stops the program (H-1.4).
                    if let Some(closed) = self.with_registry(|r| r.closed_frame(tab, frame)) {
                        let why = closed.why(frame);
                        if closed == Closed::Challenge {
                            stopped = Some(StopReason {
                                class: Class::Challenge,
                                why: why.clone(),
                                element: step_target(step).map(|e| e.to_string()),
                            });
                        }
                        out.push(failed_step(n, step, why));
                        break;
                    }
                    // Every read before the input races a dialog: one that opens now is
                    // reported at once (no stall to the read's cap, no Submit stop for a
                    // missing index).
                    let mark = self.events_mark();
                    let read = self.live_index(tab, frame, &mut pages);
                    let Some(before) = self.or_dialog(tab, mark, read).await else {
                        out.push(dialog_step(
                            n,
                            step,
                            self.open_dialog(tab).unwrap_or_default(),
                        ));
                        break;
                    };
                    let before = before?;
                    if let Some((stop, why)) = gate_step(n, step, before.as_deref(), ctx.class) {
                        // A target the fresh index does not list: an id from an earlier
                        // document (or a removed element) fails the step; no approval could
                        // make it run, so it is not a stop. A CHALLENGE is never asked about
                        // (no call into a challenge page, D-5).
                        let dead = if stop.class == Class::Challenge {
                            Some(None)
                        } else {
                            let dead = self.dead_target(tab, step, before.as_deref());
                            self.or_dialog(tab, mark, dead).await
                        };
                        let Some(dead) = dead else {
                            out.push(dialog_step(
                                n,
                                step,
                                self.open_dialog(tab).unwrap_or_default(),
                            ));
                            break;
                        };
                        match dead {
                            Some(w) => out.push(failed_step(n, step, w)),
                            None => {
                                stopped = Some(stop);
                                out.push(failed_step(n, step, why));
                            }
                        }
                        break;
                    }
                    let mark = self.events_mark();
                    let tabs_before: BTreeSet<TabId> =
                        self.with_registry(|r| r.tabs().into_iter().map(|t| t.0).collect());
                    let start = self.start_step(tab, &session, step, ctx, &files);
                    let started = match self.or_dialog(tab, mark, start).await {
                        Some(s) => s?,
                        None => Started::failed(
                            "a dialog opened before the input: answer it with dialog() first",
                        ),
                    };
                    match started {
                        Started::Failed { why, challenge } => {
                            if let Some(e) = challenge {
                                stopped = Some(StopReason {
                                    class: Class::Challenge,
                                    why: format!(
                                        "CHALLENGE at {e}: stopped, hand back to the user"
                                    ),
                                    element: Some(e.to_string()),
                                });
                            }
                            // A dialog the page opened meanwhile is why it stopped answering.
                            let dialog: Vec<Consequence> = self
                                .open_dialog(tab)
                                .map(|text| Consequence::Dialog { text })
                                .into_iter()
                                .collect();
                            let paused = !dialog.is_empty();
                            (StepOutcome::Failed { why }, dialog, None, paused)
                        }
                        Started::Input { fut, moved } => {
                            let watch = Watch {
                                tab,
                                session: &session,
                                frame,
                                mark,
                                tabs_before,
                                files: &files,
                            };
                            let c = self.capture(&watch, fut).await?;
                            pages.live.clear();
                            pages.window_end = c.window_end;
                            let mut consequences = c.consequences;
                            if let Some(after) = c.after {
                                // Only the step frame's index is compared: a step in a frame
                                // can change its parent, so it never claims no change (D-617;
                                // frame merging is T-22).
                                // A request the step started still in flight: its effect may
                                // not be in the page yet (B-614, D-620).
                                let quiet = frame == 0
                                    && !c.net_busy
                                    && consequences.is_empty()
                                    && c.failed.is_none()
                                    && moved != Some(true)
                                    && before
                                        .as_deref()
                                        .is_some_and(|b| changes(b, &after).is_empty());
                                if quiet {
                                    consequences.push(Consequence::NoVisibleChange);
                                }
                                pages.live.insert(frame, after);
                            }
                            let outcome = match c.failed {
                                Some(why) => StepOutcome::Failed { why },
                                None => StepOutcome::Ok,
                            };
                            (outcome, consequences, c.settled, c.paused)
                        }
                    }
                }
            };
            let end = !matches!(outcome, StepOutcome::Ok)
                || matches!(step, Step::StopHere)
                || paused
                || stopped.is_some();
            out.push(StepResult {
                n,
                step: step_label(step),
                outcome,
                consequences,
                settled,
            });
            if end {
                break;
            }
        }
        let rows = if self.open_dialog(tab).is_some() {
            None
        } else {
            self.program_rows(tab, &mut pages).await
        };
        Ok(finish(ctx, &steps, out, stopped, rows))
    }

    /// For a step whose target the live index does not list: `Some(why)` when the page
    /// says the id is `stale` (an earlier document) or `gone`; `None` when it is a live
    /// element, or on any doubt (the gate's Submit stop then stands, D-402).
    async fn dead_target(
        &self,
        tab: TabId,
        step: &Step,
        index: Option<&[IndexRow]>,
    ) -> Option<String> {
        let (Some(rows), Some(t)) = (index, step_target(step)) else {
            return None;
        };
        if rows.iter().any(|r| r.elem() == t) {
            return None;
        }
        let deadline = Instant::now() + CALL_CAP;
        match self
            .call_sb(tab, t.frame, RESOLVE_FN, &[json!(t.id)], deadline)
            .await
        {
            Ok(v) => match v.as_str() {
                Some(w @ ("stale" | "gone")) => Some(w.to_string()),
                _ => None,
            },
            Err(CallError::Lost) => Some("stale".into()),
            Err(_) => None,
        }
    }

    /// The frame's live index: the previous step's, else read now (one round trip). `None`
    /// when the page gives none (the gate then fails closed).
    async fn live_index(
        &self,
        tab: TabId,
        frame: u32,
        pages: &mut Pages,
    ) -> Result<Option<Vec<IndexRow>>, DriverError> {
        if let Some(ix) = pages.live.get(&frame) {
            return Ok(Some(ix.clone()));
        }
        // Before the step's input: its own cap.
        let ix = self
            .read_index(tab, frame, Instant::now() + CALL_CAP)
            .await?;
        if let Some(ix) = &ix {
            pages.first.entry(frame).or_insert_with(|| ix.clone());
            pages.live.insert(frame, ix.clone());
        }
        Ok(ix)
    }

    /// The frame's index, read by `deadline` (a hard one: no answer by then is `None`).
    async fn read_index(
        &self,
        tab: TabId,
        frame: u32,
        deadline: Instant,
    ) -> Result<Option<Vec<IndexRow>>, DriverError> {
        match self.call_sb(tab, frame, INDEX_FN, &[], deadline).await {
            Ok(v) => Ok(rows(&v, frame)),
            Err(e) => call_failure(e, "index", frame).map(|_| None),
        }
    }

    /// Classify-free part of a step: actionability with its op, then the queued input.
    async fn start_step(
        &self,
        tab: TabId,
        session: &str,
        step: &Step,
        ctx: &ToolCtx,
        files: &Path,
    ) -> Result<Started, DriverError> {
        let focus = json!({"focus": true});
        let focus_text = json!({"focus": true, "text": true});
        let focused = |op: &Value| op.get("focused") == Some(&Value::Bool(true));
        Ok(match step {
            Step::Click { target } => match self.actionable_op(tab, *target, None).await? {
                Err(w) => Started::from_why(w, *target),
                Ok((p, _)) => Started::input(self.dispatch(
                    session,
                    "Input.dispatchMouseEvent",
                    click_events(p),
                )),
            },
            Step::Type { target, text } => {
                if vault_refs(text).is_err() {
                    return Ok(Started::failed(
                        "malformed @vault(..) reference in the value",
                    ));
                }
                match self.actionable_op(tab, *target, Some(&focus_text)).await? {
                    Err(w) => Started::from_why(w, *target),
                    Ok((_, op)) if !focused(&op) => Started::failed(NO_FOCUS),
                    Ok(_) => match self.type_text(session, text, ctx) {
                        Ok(f) => Started::input(f),
                        Err(why) => Started::failed(why),
                    },
                }
            }
            Step::Select { target, choice } => {
                let op = json!({ "select": choice });
                match self.actionable_op(tab, *target, Some(&op)).await? {
                    Err(w) => Started::from_why(w, *target),
                    Ok((_, a)) if a.get("selected") == Some(&Value::Bool(true)) => {
                        Started::input(ready())
                    }
                    Ok((_, a)) => {
                        let why = a
                            .get("why")
                            .and_then(Value::as_str)
                            .unwrap_or("not selected");
                        Started::failed(tidy_why(why, target.frame, &[]))
                    }
                }
            }
            Step::Key { target, key } => {
                let events = match key_events(key) {
                    Ok(e) => e,
                    Err(why) => return Ok(Started::failed(why)),
                };
                match self.actionable_op(tab, *target, Some(&focus)).await? {
                    Err(w) => Started::from_why(w, *target),
                    Ok((_, op)) if !focused(&op) => Started::failed(NO_FOCUS),
                    Ok(_) => {
                        Started::input(self.dispatch(session, "Input.dispatchKeyEvent", events))
                    }
                }
            }
            Step::Upload { target, path } => self.start_upload(tab, *target, path, files).await?,
            Step::Scroll(s) => {
                let (frame, decl, arg) = match s {
                    Scroll::Down => (0, SCROLL_FN, json!(1)),
                    Scroll::Up => (0, SCROLL_FN, json!(-1)),
                    Scroll::To(t) => (t.frame, SCROLL_TO_FN, json!(t.id)),
                };
                let deadline = Instant::now() + CALL_CAP;
                match self.call_sb(tab, frame, decl, &[arg], deadline).await {
                    Err(e) => Started::failed(call_failure(e, "scroll", frame)?),
                    Ok(v) => match v.get("why").and_then(Value::as_str) {
                        Some(why) => Started::failed(tidy_why(why, frame, &[])),
                        None => Started::Input {
                            fut: ready(),
                            moved: Some(v.get("moved") == Some(&Value::Bool(true))),
                        },
                    },
                }
            }
            Step::Wait(_) | Step::StopHere => Started::input(ready()),
        })
    }

    /// `u#N=path`: the path inside the run's files folder, the file input by reference, then
    /// `DOM.setFileInputFiles` (which fires `input` and `change`).
    async fn start_upload(
        &self,
        tab: TabId,
        target: ElemRef,
        path: &str,
        files: &Path,
    ) -> Result<Started, DriverError> {
        let file = match upload_path(files, path) {
            Ok(f) => f,
            Err(why) => return Ok(Started::failed(why)),
        };
        let deadline = Instant::now() + CALL_CAP;
        let (sess, obj) = match self
            .call_sb_ref(tab, target.frame, UPLOAD_FN, &[json!(target.id)], deadline)
            .await
        {
            Ok(r) => r,
            Err(e) => return Ok(Started::failed(call_failure(e, "upload", target.frame)?)),
        };
        let release = |cdp: &sb_cdp::CdpSender, s: &str| {
            drop(cdp.start(
                Some(s),
                "Runtime.releaseObjectGroup",
                json!({ "objectGroup": OBJECT_GROUP }),
            ));
        };
        let node = obj.get("subtype").and_then(Value::as_str) == Some("node");
        let Some(object_id) = obj.get("objectId").and_then(Value::as_str).filter(|_| node) else {
            release(&self.cdp.sender(), &sess);
            let why = obj
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or("not a file input");
            let why = match why {
                "gone" | "stale" | "disabled" | "not a file input" | "no runtime" => why,
                _ => "not a file input",
            };
            return Ok(Started::failed(why));
        };
        let set = self.cdp.start(
            Some(&sess),
            "DOM.setFileInputFiles",
            json!({ "files": [plain_path(&file)], "objectId": object_id }),
        );
        let sender = self.cdp.sender();
        Ok(Started::input(Box::pin(async move {
            let r = set.await.map(|_| ()).map_err(|e| e.to_string());
            release(&sender, &sess);
            r
        })))
    }

    /// `fut`, unless a dialog opens on the tab first (after `mark`): then `None` and `fut`
    /// is dropped (a dialog blocks the page, so the input's reply and in-page calls would
    /// wait for the user's answer). Woken by every event the observer applies.
    pub(crate) async fn or_dialog<T>(
        &self,
        tab: TabId,
        mark: u64,
        fut: impl Future<Output = T>,
    ) -> Option<T> {
        let mut changed = self.changed.clone();
        tokio::pin!(fut);
        loop {
            changed.borrow_and_update();
            if self.dialog_since(tab, mark) {
                return None;
            }
            tokio::select! {
                biased;
                v = &mut fut => return Some(v),
                r = changed.changed() => {
                    if r.is_err() {
                        return Some(fut.await);
                    }
                }
            }
        }
    }

    /// The settle-and-index call in the step's frame, capped at the window; while a request
    /// the step started (after `mark`) is in flight, the call waits for it first. Every call
    /// ends by `t0` + [`NAV_CAP`], its grace included (D-620, B-4 #8).
    async fn post_step(
        &self,
        tab: TabId,
        frame: u32,
        mark: u64,
        t0: Instant,
    ) -> Result<Post, DriverError> {
        let pass = Pass {
            decl: POST_FN,
            quiet_ms: SettleOpts::default().quiet_ms,
            per_call: Some(WINDOW),
            grace: Some(WINDOW_GRACE),
            at_least_once: true,
            pending_since: mark,
            count_gaps: false,
        };
        match self.net_settle(tab, frame, mark, t0 + NAV_CAP, pass).await {
            Ok(s) => {
                let (index, doc) = match s.last {
                    Some((v, doc)) => (v.get("index").and_then(|ix| rows(ix, frame)), Some(doc)),
                    None => (None, None),
                };
                Ok(Post {
                    settled: Some(s.settled),
                    index,
                    doc,
                    net_busy: s.net_busy,
                })
            }
            Err(e) => {
                call_failure(e, "settle", frame)?;
                // The window ended without a settled answer (the call missed its cap, or
                // its document went away): not settled, deterministically (accept-T-22
                // note 4).
                Ok(Post {
                    settled: Some(false),
                    index: None,
                    doc: None,
                    net_busy: self.with_events(|r, ev| crate::settle::pending(r, ev, tab, mark)),
                })
            }
        }
    }

    /// B-4 #9: the input's replies (or a dialog), then the window; then what the log says
    /// happened on this tab after `mark`, and the tabs it opened.
    async fn capture(&self, w: &Watch<'_>, input: InputFut) -> Result<Captured, DriverError> {
        let Watch {
            tab,
            session,
            frame,
            mark,
            ref tabs_before,
            files,
        } = *w;
        let t0 = Instant::now();
        let nav_cap = t0 + NAV_CAP;
        let main = self
            .with_registry(|r| r.cdp_frame_id(tab, 0).map(str::to_string))
            .unwrap_or_default();
        let mut c = Captured {
            window_end: Some(nav_cap),
            ..Captured::default()
        };
        match self.or_dialog(tab, mark, input).await {
            None => {}
            Some(Err(why)) => c.failed = Some(tidy_why(&why, 0, &[])),
            Some(Ok(())) => {
                let mut post = match self
                    .or_dialog(tab, mark, self.post_step(tab, frame, mark, t0))
                    .await
                {
                    Some(p) => Some(p?),
                    None => None,
                };
                // A main-frame navigation the step started that has neither committed nor
                // turned into a download yet: wait for it (a condition, capped).
                if post.is_some() && self.with_events(|_, ev| nav_pending(ev, session, &main, mark))
                {
                    let cap = nav_cap.saturating_duration_since(Instant::now());
                    self.wait_state(cap, |_, ev| {
                        (!nav_pending(ev, session, &main, mark)).then_some(())
                    })
                    .await;
                }
                // The document that answered was replaced since (a navigation): read the
                // current one once. Its calls end by the window's deadline (post_step); none
                // is sent once that has passed.
                if post
                    .as_ref()
                    .is_some_and(|p| p.doc != self.sb_key(tab, frame))
                {
                    post = match self
                        .or_dialog(tab, mark, self.post_step(tab, frame, mark, t0))
                        .await
                    {
                        Some(p) => Some(p?),
                        None => None,
                    };
                }
                if let Some(p) = post {
                    c.settled = p.settled;
                    c.net_busy = p.net_busy;
                    if p.doc.is_some() && p.doc == self.sb_key(tab, frame) {
                        c.after = p.index;
                    }
                }
            }
        }
        // Tabs this one opened (T-18 registry opener mapping): wait, capped, for their first
        // document, then read their titles.
        let opened: Vec<TabId> = self.with_registry(|r| {
            r.tabs()
                .into_iter()
                .map(|t| t.0)
                .filter(|t| !tabs_before.contains(t) && r.opener(*t) == Some(tab))
                .collect()
        });
        if !opened.is_empty() {
            let cap = nav_cap.saturating_duration_since(Instant::now());
            self.wait_registry(cap, |r| {
                opened
                    .iter()
                    .all(|t| {
                        !r.has_tab(*t)
                            || (r
                                .frame_url(*t, 0)
                                .is_some_and(|u| !u.is_empty() && u != "about:blank")
                                && r.sb_context(*t, 0).is_some())
                    })
                    .then_some(())
            })
            .await;
        }
        // What the log says.
        let (nav, dialog, downloads) = self.with_events(|r, ev| {
            let mut nav = None;
            let mut dialog = None;
            let mut downloads = Vec::new();
            for l in ev.since(mark) {
                let ours = l
                    .session
                    .as_deref()
                    .map(|s| s.split('\u{1f}').next().unwrap_or(s))
                    .and_then(|s| r.tab_of_session(s))
                    == Some(tab);
                match &l.kind {
                    Kind::Navigated { url } if is_main(l, session, &main) => {
                        nav = Some(url.clone())
                    }
                    Kind::Dialog { text } if ours => dialog = Some(text.clone()),
                    Kind::Download { guid } => {
                        let from_tab = ours
                            || ev
                                .download(guid)
                                .is_some_and(|d| r.frame_by_cdp_id(tab, &d.frame).is_some());
                        if from_tab {
                            if let Some(d) = ev.download(guid) {
                                downloads.push((guid.clone(), d.name.clone(), d.url.clone()));
                            }
                        }
                    }
                    _ => {}
                }
            }
            (nav, dialog, downloads)
        });
        if let Some(url) = nav {
            c.consequences.push(Consequence::Navigated { url });
        }
        for t in opened
            .into_iter()
            .filter(|t| self.with_registry(|r| r.has_tab(*t)))
        {
            let title: String = self
                .tab_title(t, nav_cap)
                .await
                .chars()
                .take(RAW_CAP)
                .collect();
            c.consequences
                .push(Consequence::OpenedTab { tab: t, title });
        }
        for (guid, name, url) in downloads {
            // The ask: Driver::download saves it to this run's files folder if allowed.
            self.with_events(|_, ev| {
                if let Some(d) = ev.download_mut(&guid) {
                    d.files_dir = Some(files.to_path_buf());
                }
            });
            c.consequences.push(Consequence::Download {
                name: raw_name(&name, &url),
                guid,
            });
        }
        if let Some(text) = dialog {
            c.consequences.push(Consequence::Dialog { text });
            c.paused = true;
        }
        if self.open_dialog(tab).is_some() {
            c.paused = true;
        }
        Ok(c)
    }

    /// A new tab's title: `document.title` from its sb world when it has one (TargetInfo
    /// titles go stale, B-601), else the registry's. The read ends by the window's deadline
    /// `window_end` minus the reply margin, and is not made when no time is left.
    async fn tab_title(&self, t: TabId, window_end: Instant) -> String {
        let ctx = self.with_registry(|r| r.sb_context(t, 0).cloned());
        if let (Some(ctx), Some(end)) = (ctx, reads_end(window_end)) {
            let deadline = (Instant::now() + TITLE_CAP).min(end);
            if let Ok(v) = self.raw_call(&ctx, TITLE_FN, &[], deadline).await {
                if let Some(s) = v.as_str().filter(|s| !s.trim().is_empty()) {
                    return s.to_string();
                }
            }
        }
        self.with_registry(|r| {
            r.tabs()
                .into_iter()
                .find(|x| x.0 == t)
                .map(|x| x.2)
                .unwrap_or_default()
        })
    }

    /// The rows of the program's diff (B-8): every frame a step read, from its first index
    /// (before) to now (after). Rendered after the scrub ([`finish`]).
    ///
    /// A frame the last window left no index for is read now, by that window's deadline
    /// minus the reply margin (B-4 #8): with no time left, or no answer by then, the frame
    /// is left out of both sides (fewer rows, or no diff), never shown as unchanged.
    async fn program_rows(&self, tab: TabId, pages: &mut Pages) -> Option<DiffRows> {
        let frames: Vec<u32> = pages.first.keys().copied().collect();
        let (mut before, mut after) = (Vec::new(), Vec::new());
        for f in frames {
            let now = match pages.live.get(&f) {
                Some(a) => a.clone(),
                None => {
                    // `live` is only cleared by a window, which sets `window_end`.
                    let Some(end) = pages.window_end.and_then(reads_end) else {
                        continue;
                    };
                    match self.read_index(tab, f, end).await {
                        Ok(Some(a)) => a,
                        _ => continue,
                    }
                }
            };
            before.extend(pages.first[&f].iter().cloned());
            after.extend(now);
        }
        Some((before, after))
    }
}

/// Whether a main-frame navigation started after `mark` is still open: no commit, no
/// download and no dialog after it.
fn nav_pending(ev: &crate::consequence::Events, session: &str, main: &str, mark: u64) -> bool {
    let mut pending = false;
    for l in ev.since(mark) {
        match &l.kind {
            Kind::NavStarted if is_main(l, session, main) => pending = true,
            Kind::Navigated { .. } if is_main(l, session, main) => pending = false,
            Kind::Download { .. } | Kind::Dialog { .. } => pending = false,
            _ => {}
        }
    }
    pending
}

/// The diff's rows: (before, after).
type DiffRows = (Vec<IndexRow>, Vec<IndexRow>);

/// The vault keys the program's `t#` steps type, each once, in order.
fn typed_names(steps: &[Step]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for s in steps {
        if let Step::Type { text, .. } = s {
            for n in vault_refs(text).unwrap_or_default() {
                if !names.iter().any(|x| x == n) {
                    names.push(n.to_string());
                }
            }
        }
    }
    names
}

/// H-2, rule 2: every string in `strings` scrubbed against the values of the keys in
/// `names`, resolved inside `ctx.resolve_into` (a value never leaves the sink), in every
/// spelling [`spellings`] lists and with cut prefixes ([`scrub_with`]), then against the
/// process-wide scrubber (D-102) when one is installed. The MCP wrapper scrubs again on the
/// way out.
fn scrub_all(ctx: &ToolCtx, names: &[String], strings: &mut [&mut String]) {
    for name in names {
        let lit = vault_literal(name);
        let _ = ctx.resolve_into(&lit, |res| {
            let v = res.expose();
            if v.chars().count() < MIN_VALUE_CHARS {
                return;
            }
            let sp = spellings(v);
            for s in strings.iter_mut() {
                scrub_with(s, &sp, &lit);
            }
        });
    }
    for s in strings.iter_mut() {
        let c = harness_core::scrub(s);
        if c != **s {
            **s = c;
        }
    }
}

/// The raw page strings of the consequences.
fn raw_strings<'a>(steps: &'a mut [StepResult], out: &mut Vec<&'a mut String>) {
    for s in steps {
        consequence_strings(&mut s.consequences, out);
    }
}

fn consequence_strings<'a>(cs: &'a mut [Consequence], out: &mut Vec<&'a mut String>) {
    for c in cs {
        match c {
            Consequence::Navigated { url } => out.push(url),
            Consequence::OpenedTab { title, .. } => out.push(title),
            Consequence::Dialog { text } => out.push(text),
            Consequence::Download { name, guid } => {
                out.push(name);
                out.push(guid);
            }
            Consequence::NoVisibleChange => {}
        }
    }
}

/// A consequence as the model sees it: escaped and capped (pr-ready 5).
fn shown(c: &mut Consequence) {
    match c {
        Consequence::Navigated { url } => *url = clean_url(url),
        Consequence::OpenedTab { title, .. } => *title = page_text(title, TEXT_CAP),
        Consequence::Dialog { text } => *text = page_text(text, TEXT_CAP),
        Consequence::Download { name, guid } => {
            *name = page_text(&sanitize_name(name), TEXT_CAP);
            *guid = page_text(guid, 64);
        }
        Consequence::NoVisibleChange => {}
    }
}

/// Every string of a finished result.
fn all_strings<'a>(r: &'a mut ActResult, out: &mut Vec<&'a mut String>) {
    for s in &mut r.steps {
        if let StepOutcome::Failed { why } = &mut s.outcome {
            out.push(why);
        }
        consequence_strings(&mut s.consequences, out);
    }
    if let Some(d) = &mut r.diff {
        out.push(d);
    }
    if let Some(s) = &mut r.stopped {
        out.push(&mut s.why);
    }
}

/// The result of a run (rule 2, D-616): the raw page strings (consequences and the diff's
/// row names and form ids) are scrubbed first, while every typed value in them is whole;
/// then escaped, capped and rendered; then every shown string (whys and the diff included)
/// is scrubbed again, cut prefixes included.
fn finish(
    ctx: &ToolCtx,
    steps: &[Step],
    mut out: Vec<StepResult>,
    stopped: Option<StopReason>,
    rows: Option<DiffRows>,
) -> ActResult {
    let names = typed_names(steps);
    let mut rows = rows;
    {
        let mut raw: Vec<&mut String> = Vec::new();
        raw_strings(&mut out, &mut raw);
        if let Some((before, after)) = rows.as_mut() {
            for r in before.iter_mut().chain(after.iter_mut()) {
                raw.push(&mut r.name);
                if let Some(f) = r.form.as_mut() {
                    raw.push(f);
                }
            }
        }
        scrub_all(ctx, &names, &mut raw);
    }
    for s in &mut out {
        for c in &mut s.consequences {
            shown(c);
        }
    }
    let diff = rows.and_then(|(before, after)| render(&changes(&before, &after)));
    let mut result = ActResult {
        steps: out,
        diff,
        stopped,
    };
    let mut strings: Vec<&mut String> = Vec::new();
    all_strings(&mut result, &mut strings);
    scrub_all(ctx, &names, &mut strings);
    result
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

    fn login() -> Vec<IndexRow> {
        let mut btn = row(14, "button", "Sign in", Some("f2"));
        btn.flags.submit = true;
        btn.state.submitish = true;
        let mut cap = row(42, "iframe", "reCAPTCHA", None);
        cap.flags.challenge = true;
        vec![
            row(7, "textbox", "Email", Some("f2")),
            btn,
            cap,
            row(20, "link", "Pricing", None),
        ]
    }

    fn step(p: &str) -> Step {
        sb_act::parse(p).unwrap().remove(0)
    }

    #[test]
    fn a_submit_above_the_approved_fill_stops_before_input() {
        let ix = login();
        let (stop, why) = gate_step(2, &step("c#14"), Some(&ix), Class::Fill).expect("stop");
        assert_eq!(stop.class, Class::Submit);
        assert_eq!(stop.element.as_deref(), Some("#14"));
        assert!(why.contains("above the approved Fill"), "{why}");
        assert!(gate_step(1, &step("c#14"), Some(&ix), Class::Submit).is_none());
        assert!(gate_step(1, &step("t#7=a@b.c"), Some(&ix), Class::Fill).is_none());
        assert!(
            gate_step(1, &step("c#20"), Some(&ix), Class::Fill).is_some(),
            "Navigate > Fill"
        );
    }

    #[test]
    fn unknown_targets_and_missing_indexes_fail_closed_as_submit() {
        let ix = login();
        let (stop, _) = gate_step(1, &step("t#99=x"), Some(&ix), Class::Navigate).unwrap();
        assert_eq!(stop.class, Class::Submit, "D-402");
        let (stop, _) = gate_step(1, &step("t#7=x"), None, Class::Fill).unwrap();
        assert_eq!(
            stop.class,
            Class::Submit,
            "no index: every target is Submit"
        );
        assert!(gate_step(1, &step("sc(down)"), None, Class::Navigate).is_none());
    }

    #[test]
    fn a_challenge_row_or_page_always_stops() {
        let ix = login();
        for p in ["c#42", "k#42=Enter", "sc(#42)"] {
            let (stop, why) = gate_step(1, &step(p), Some(&ix), Class::Pay).expect(p);
            assert_eq!(stop.class, Class::Challenge, "{p}");
            assert!(why.starts_with("CHALLENGE at #42"), "{why}");
        }
        let mut doc = row(1, "document", "challenge page", None);
        doc.flags.challenge = true;
        let (stop, _) = gate_step(1, &step("sc(down)"), Some(&[doc]), Class::Pay).unwrap();
        assert_eq!(stop.class, Class::Challenge);
    }

    #[test]
    fn verbatim_windows_paths_become_plain() {
        assert_eq!(plain_path(Path::new(r"\\?\C:\a\b.pdf")), r"C:\a\b.pdf");
        assert_eq!(plain_path(Path::new("/tmp/a.pdf")), "/tmp/a.pdf");
    }
}
