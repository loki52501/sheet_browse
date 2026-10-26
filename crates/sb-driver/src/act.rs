//! I-9 `Driver::act` (T-20 actionability, T-21 input, gate and consequences).
//!
//! Per step: the live index of the step's frame is read (or reused from the previous step's
//! settle), the step is classified against it with `sb_act` (an unknown target is Submit,
//! D-402), and a step whose live class is above `ctx.class`, or that reaches a CHALLENGE,
//! stops the program before any input (rule 4, H-1.4). Then actionability (B-4 #6) with the
//! step's in-page op in the same round trip, the input (B-4 #7, pipelined), and the
//! consequence window (B-4 #9): the input's replies (or a dialog, which blocks them), then
//! one in-page `settle()` capped at 500 ms that also returns the index, which is the next
//! step's live index and the diff's after side (B-8). Network settle is T-22.
//!
//! Page strings in the result are escaped and capped (`diff.rs`), and the whole result is
//! scrubbed against the values of the vault keys the program typed before it is returned.

use crate::actionable::{input_target, step_label, tidy_why, StepEnd, Why, WhyKind};
use crate::consequence::{is_main, Kind};
use crate::diff::{changes, clean_url, page_text, render, TEXT_CAP};
use crate::input::{
    click_events, key_events, ready, upload_path, InputFut, SCROLL_FN, SCROLL_TO_FN, UPLOAD_FN,
};
use crate::runtime::{CallError, OBJECT_GROUP};
use crate::{
    ActResult, Consequence, Driver, DriverError, SettleOpts, StepOutcome, StepResult, TabId,
    ToolCtx,
};
use harness_core::gate::StopReason;
use harness_core::vault::{vault_literal, vault_refs, MIN_VALUE_CHARS};
use harness_core::Class;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use sb_act::{Scroll, Step};
use sb_sheet::{ElemRef, IndexRow};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::Path;
use std::time::Duration;
use tokio::time::Instant;
use zeroize::Zeroizing;

/// B-4 #9: after a step, listen until the page settles or for at most this long.
pub(crate) const WINDOW: Duration = Duration::from_millis(500);
/// How long past the window the settle-and-index call may take to answer (a round trip on
/// a loaded machine, as `w()`'s grace).
const WINDOW_GRACE: Duration = Duration::from_millis(300);
/// A main-frame navigation the step started, or a new tab's first document, is awaited at
/// most this long from the input (the settle cap, B-4 #8).
const NAV_CAP: Duration = Duration::from_millis(3000);
/// Cap of one index read (gate and diff) and of an in-page scroll or upload lookup.
const CALL_CAP: Duration = Duration::from_secs(3);
/// Cap of a new tab's title read.
const TITLE_CAP: Duration = Duration::from_millis(300);

const INDEX_FN: &str = "function () {
  const sb = globalThis.__sb;
  if (!sb) return { why: 'no runtime' };
  return sb.index();
}";

/// The consequence window's page side: settle (capped), then the index in the same call.
const POST_FN: &str = "async function (opts) {
  const sb = globalThis.__sb;
  if (!sb) return { why: 'no runtime' };
  const s = await sb.settle(opts);
  const ix = sb.index();
  return { settled: !!(s && s.settled), index: Array.isArray(ix) ? ix : null };
}";

const TITLE_FN: &str = "function () { return String(document.title || ''); }";

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
}

/// Index state of one program run.
#[derive(Default)]
struct Pages {
    /// The live index per frame, while no step has run since it was read.
    live: BTreeMap<u32, Vec<IndexRow>>,
    /// Each frame's index as first read in this program (the diff's before side).
    first: BTreeMap<u32, Vec<IndexRow>>,
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
        let Some(first) = steps.first() else {
            return Err(DriverError::Parse(sb_act::ParseError {
                step: 1,
                at: 0,
                msg: "the program has no steps".into(),
            }));
        };
        let files = ctx.run_dir.join("files");
        let mut out = Vec::with_capacity(steps.len());
        let mut stopped = None;
        let mut pages = Pages::default();
        // A dialog left open blocks the page's main thread: nothing can run until it is
        // answered.
        if let Some(text) = self.open_dialog(tab) {
            out.push(StepResult {
                n: 1,
                step: step_label(first),
                outcome: StepOutcome::Failed {
                    why: "a dialog is open on this tab: answer it with dialog() first".into(),
                },
                consequences: vec![Consequence::Dialog {
                    text: page_text(&text, TEXT_CAP),
                }],
                settled: None,
            });
            let mut r = ActResult {
                steps: out,
                diff: None,
                stopped: None,
            };
            scrub(ctx, &steps, &mut r);
            return Ok(r);
        }
        for (i, step) in steps.iter().enumerate() {
            let n = i + 1;
            let (outcome, consequences, settled, paused) = match step {
                Step::Wait(d) => {
                    let (o, s): StepEnd = self.wait_step(tab, *d).await?;
                    pages.live.clear();
                    (o, Vec::new(), s, false)
                }
                Step::StopHere => (StepOutcome::Ok, Vec::new(), None, false),
                _ => {
                    let frame = step_frame(step);
                    let before = self.live_index(tab, frame, &mut pages).await?;
                    if let Some((stop, why)) = gate_step(n, step, before.as_deref(), ctx.class) {
                        stopped = Some(stop);
                        out.push(StepResult {
                            n,
                            step: step_label(step),
                            outcome: StepOutcome::Failed { why },
                            consequences: Vec::new(),
                            settled: None,
                        });
                        break;
                    }
                    let mark = self.events_mark();
                    let tabs_before: BTreeSet<TabId> =
                        self.with_registry(|r| r.tabs().into_iter().map(|t| t.0).collect());
                    match self.start_step(tab, &session, step, ctx, &files).await? {
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
                                .map(|t| Consequence::Dialog {
                                    text: page_text(&t, TEXT_CAP),
                                })
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
                            let mut consequences = c.consequences;
                            if let Some(after) = c.after {
                                let quiet = consequences.is_empty()
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
        let diff = if self.open_dialog(tab).is_some() {
            None
        } else {
            self.program_diff(tab, &mut pages).await
        };
        let mut result = ActResult {
            steps: out,
            diff,
            stopped,
        };
        scrub(ctx, &steps, &mut result);
        Ok(result)
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
        let ix = self.read_index(tab, frame).await?;
        if let Some(ix) = &ix {
            pages.first.entry(frame).or_insert_with(|| ix.clone());
            pages.live.insert(frame, ix.clone());
        }
        Ok(ix)
    }

    async fn read_index(
        &self,
        tab: TabId,
        frame: u32,
    ) -> Result<Option<Vec<IndexRow>>, DriverError> {
        let deadline = Instant::now() + CALL_CAP;
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
                match self.actionable_op(tab, *target, Some(&focus)).await? {
                    Err(w) => Started::from_why(w, *target),
                    Ok((_, op)) if !focused(&op) => {
                        Started::failed("the element did not take focus")
                    }
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
                    Ok((_, op)) if !focused(&op) => {
                        Started::failed("the element did not take focus")
                    }
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
    async fn or_dialog<T>(&self, tab: TabId, mark: u64, fut: impl Future<Output = T>) -> Option<T> {
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

    /// The settle-and-index call in the step's frame, capped at the window.
    async fn post_step(&self, tab: TabId, frame: u32) -> Result<Post, DriverError> {
        let cap_ms = u64::try_from(WINDOW.as_millis()).unwrap_or(500);
        let opts = json!({"quiet": SettleOpts::default().quiet_ms, "cap": cap_ms});
        let deadline = Instant::now() + WINDOW + WINDOW_GRACE;
        match self
            .call_sb_doc(tab, frame, POST_FN, &[opts], deadline)
            .await
        {
            Ok((v, doc)) => Ok(Post {
                settled: v.get("settled").and_then(Value::as_bool),
                index: v.get("index").and_then(|ix| rows(ix, frame)),
                doc: Some(doc),
            }),
            Err(e) => {
                call_failure(e, "settle", frame)?;
                Ok(Post {
                    settled: None,
                    index: None,
                    doc: None,
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
        let mut c = Captured::default();
        match self.or_dialog(tab, mark, input).await {
            None => {}
            Some(Err(why)) => c.failed = Some(tidy_why(&why, 0, &[])),
            Some(Ok(())) => {
                let mut post = match self.or_dialog(tab, mark, self.post_step(tab, frame)).await {
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
                // current one once.
                if post
                    .as_ref()
                    .is_some_and(|p| p.doc != self.sb_key(tab, frame))
                {
                    post = match self.or_dialog(tab, mark, self.post_step(tab, frame)).await {
                        Some(p) => Some(p?),
                        None => None,
                    };
                }
                if let Some(p) = post {
                    c.settled = p.settled;
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
            c.consequences.push(Consequence::Navigated {
                url: clean_url(&url),
            });
        }
        for t in opened
            .into_iter()
            .filter(|t| self.with_registry(|r| r.has_tab(*t)))
        {
            let title = self.tab_title(t).await;
            c.consequences.push(Consequence::OpenedTab {
                tab: t,
                title: page_text(&title, TEXT_CAP),
            });
        }
        for (guid, name, url) in downloads {
            // The ask: Driver::download saves it to this run's files folder if allowed.
            self.with_events(|_, ev| {
                if let Some(d) = ev.download_mut(&guid) {
                    d.files_dir = Some(files.to_path_buf());
                }
            });
            let shown = crate::consequence::safe_name(&name, &url);
            c.consequences.push(Consequence::Download {
                name: page_text(&shown, TEXT_CAP),
                guid: page_text(&guid, 64),
            });
        }
        if let Some(text) = dialog {
            c.consequences.push(Consequence::Dialog {
                text: page_text(&text, TEXT_CAP),
            });
            c.paused = true;
        }
        if self.open_dialog(tab).is_some() {
            c.paused = true;
        }
        Ok(c)
    }

    /// A new tab's title: `document.title` from its sb world when it has one (TargetInfo
    /// titles go stale, B-601), else the registry's.
    async fn tab_title(&self, t: TabId) -> String {
        let ctx = self.with_registry(|r| r.sb_context(t, 0).cloned());
        if let Some(ctx) = ctx {
            let deadline = Instant::now() + TITLE_CAP;
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

    /// The program's diff (B-8) over every frame a step read, from its first index to now.
    async fn program_diff(&self, tab: TabId, pages: &mut Pages) -> Option<String> {
        let frames: Vec<u32> = pages.first.keys().copied().collect();
        let (mut before, mut after) = (Vec::new(), Vec::new());
        for f in frames {
            let now = match pages.live.get(&f) {
                Some(a) => a.clone(),
                None => match self.read_index(tab, f).await {
                    Ok(Some(a)) => a,
                    _ => continue,
                },
            };
            before.extend(pages.first[&f].iter().cloned());
            after.extend(now);
        }
        render(&changes(&before, &after))
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

/// `encodeURIComponent`'s set, and the form encoding's (space as `+` is added separately).
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');
const FORM: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'*')
    .remove(b' ');

/// The spellings of a typed vault value that can reach the result: as typed, ASCII upper and
/// lower case, URL-encoded (component and form), each also as the result's own escaping
/// (page text, cleaned URL) renders it.
fn spellings(v: &str) -> Vec<Zeroizing<String>> {
    let bases = [
        v.to_string(),
        v.to_ascii_uppercase(),
        v.to_ascii_lowercase(),
        utf8_percent_encode(v, URI_COMPONENT).to_string(),
        utf8_percent_encode(v, FORM).to_string().replace(' ', "+"),
    ];
    let mut out: Vec<Zeroizing<String>> = Vec::new();
    for b in bases {
        for s in [page_text(&b, usize::MAX), clean_url(&b), b.clone()] {
            if !s.is_empty() && !out.iter().any(|o| o.as_str() == s) {
                out.push(Zeroizing::new(s));
            }
        }
        drop(Zeroizing::new(b));
    }
    out
}

fn for_each_string(r: &mut ActResult, mut f: impl FnMut(&mut String)) {
    for s in &mut r.steps {
        if let StepOutcome::Failed { why } = &mut s.outcome {
            f(why);
        }
        for c in &mut s.consequences {
            match c {
                Consequence::Navigated { url } => f(url),
                Consequence::OpenedTab { title, .. } => f(title),
                Consequence::Dialog { text } => f(text),
                Consequence::Download { name, guid } => {
                    f(name);
                    f(guid);
                }
                Consequence::NoVisibleChange => {}
            }
        }
    }
    if let Some(d) = &mut r.diff {
        f(d);
    }
    if let Some(s) = &mut r.stopped {
        f(&mut s.why);
    }
}

/// H-2: a page can echo what was typed (a greeting, a URL after a GET form, a dialog). Every
/// string of the result is scrubbed against the values of the vault keys the program typed,
/// resolved inside `ctx.resolve_into` (the values never leave the sink), in every spelling
/// [`spellings`] lists, then against the process-wide scrubber (D-102) when one is
/// installed. The MCP wrapper scrubs again on the way out.
fn scrub(ctx: &ToolCtx, steps: &[Step], r: &mut ActResult) {
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
    for name in &names {
        let lit = vault_literal(name);
        let _ = ctx.resolve_into(&lit, |res| {
            let v = res.expose();
            if v.chars().count() < MIN_VALUE_CHARS {
                return;
            }
            let sp = spellings(v);
            for_each_string(r, |s| {
                for x in &sp {
                    if s.contains(x.as_str()) {
                        *s = s.replace(x.as_str(), &lit);
                    }
                }
            });
        });
    }
    for_each_string(r, |s| {
        let c = harness_core::scrub(s);
        if c != *s {
            *s = c;
        }
    });
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
    fn vault_spellings_cover_url_and_escaped_forms() {
        let sp = spellings("p@ss w\"rd#1");
        let all: Vec<&str> = sp.iter().map(|s| s.as_str()).collect();
        for want in [
            "p@ss w\"rd#1",
            "p%40ss%20w%22rd%231",
            "p%40ss+w%22rd%231",
            "p@ss w'rd＃1",
            "P@SS W\"RD#1",
        ] {
            assert!(all.contains(&want), "{want} in {all:?}");
        }
    }

    #[test]
    fn verbatim_windows_paths_become_plain() {
        assert_eq!(plain_path(Path::new(r"\\?\C:\a\b.pdf")), r"C:\a\b.pdf");
        assert_eq!(plain_path(Path::new("/tmp/a.pdf")), "/tmp/a.pdf");
    }
}
