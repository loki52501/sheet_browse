//! B-4 #3: the page runtime. `injected.js` is installed once per tracked target with
//! `Page.addScriptToEvaluateOnNewDocument(worldName: "sb")`, pipelined
//! with the attach setup (see `on_attached` in lib.rs), so every new document of the target
//! gets `globalThis.__sb` in the isolated world `sb`. A frame that still has no `sb` context
//! when the driver first needs one gets `Page.createIsolatedWorld` plus the source (the
//! script's own guard makes a second run a no-op).
//!
//! Calls are `Runtime.callFunctionOn` in the frame's `sb` context, which the registry tracks
//! (B-4 #2). Before the first call into a document the driver runs `walk({base})` there with
//! a base that is never reused in that tab frame (D-55, D-73, D-604). A context destroyed
//! mid-call is retried once in the frame's new context (B-4 #11).

use crate::{lock, Driver, DriverError, ExecContext, SettleOpts, TabId, SB_WORLD};
use sb_cdp::CdpError;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use tokio::time::Instant;

/// The page runtime, embedded at build time (B-5, I-11).
pub(crate) const INJECTED_JS: &str = include_str!("../../../injected/injected.js");

/// D-604: document `k` of a tab frame gets ids from `1 + k * BASE_STRIDE`.
pub(crate) const BASE_STRIDE: u32 = 100_000;

/// Params of the per-target install (B-4 #3). Not `runImmediately`: a paused new target's
/// initial about:blank document can outlive the commit of its first navigation (in another
/// renderer), so an `sb` context in a fresh tab must only ever belong to a navigated
/// document. Documents that already exist get [`Driver::sb_ctx`]'s fallback.
pub(crate) fn install_params() -> Value {
    json!({"source": INJECTED_JS, "worldName": SB_WORLD})
}

/// The base for document `k` of a tab frame, or `None` when its id range would leave u32
/// (fail closed: ids are never reused, D-604).
pub(crate) fn base_for(k: u32) -> Option<u32> {
    let base = k.checked_mul(BASE_STRIDE)?.checked_add(1)?;
    base.checked_add(BASE_STRIDE - 1)?;
    Some(base)
}

/// Walk with a base, optionally after an in-page settle (one round trip for `open`).
/// `{why: "no runtime"}` when the world has no `__sb` (the driver then installs it).
/// With a settle (open), it also waits, in parallel, for the document's first paint (a
/// `paint` performance entry), capped at [`PAINT_CAP_MS`] (D-607). In the first window after
/// launch the first paint lands about 1 s after load on chromium-1248, and until it does
/// requestAnimationFrame stalls for 100 to 700 ms, so the first act(id) waited out
/// nextFrame's 100 ms fallback (accept-T-20 row 1b); later windows paint before the settle
/// ends, so the wait costs them nothing. Skipped for hidden and about: documents (rAF and
/// paint never come there).
const WALK_FN: &str = "async function (base, settle, paintCap) {
  const sb = globalThis.__sb;
  if (!sb) return { why: 'no runtime' };
  let settled = null;
  if (settle) {
    const painted = new Promise((done) => {
      if (document.visibilityState === 'hidden' || location.protocol === 'about:' ||
        performance.getEntriesByType('paint').length) return done();
      let po = null;
      const end = () => {
        if (po) po.disconnect();
        clearTimeout(timer);
        done();
      };
      const timer = setTimeout(end, Math.min(paintCap, settle.cap));
      try {
        po = new PerformanceObserver(end);
        po.observe({ type: 'paint' });
      } catch (e) {
        end();
      }
    });
    settled = (await Promise.all([sb.settle(settle), painted]))[0];
  }
  const w = sb.walk({ base: base });
  if (w && w.why) return w;
  return { count: w ? w.count : 0, settled: settled };
}";

/// How long `open` waits for the document's first paint at most (D-607).
pub(crate) const PAINT_CAP_MS: u32 = 3000;

/// A document, by its `sb` context: the context's uniqueId, else session and id.
fn doc_key(c: &ExecContext) -> String {
    match &c.unique_id {
        Some(u) => u.clone(),
        None => format!("{}:{}", c.session, c.id),
    }
}

#[derive(Debug, Default)]
struct FrameDocs {
    /// Documents based so far in this tab frame.
    docs: u32,
    /// The document whose based walk is done.
    walked: Option<String>,
}

/// Per tab frame base allocation (D-604).
#[derive(Debug, Default)]
pub(crate) struct Bases {
    frames: HashMap<(TabId, u32), FrameDocs>,
}

impl Bases {
    fn is_walked(&self, tab: TabId, frame: u32, key: &str) -> bool {
        self.frames
            .get(&(tab, frame))
            .and_then(|f| f.walked.as_deref())
            == Some(key)
    }

    /// The next unused base of the tab frame; consumed even if the walk then fails.
    pub(crate) fn next_base(&mut self, tab: TabId, frame: u32) -> Option<u32> {
        let f = self.frames.entry((tab, frame)).or_default();
        let base = base_for(f.docs)?;
        f.docs += 1;
        Some(base)
    }

    /// Undo [`Bases::next_base`] for a base no document adopted (it is the last one given).
    pub(crate) fn give_back(&mut self, tab: TabId, frame: u32, base: u32) {
        if let Some(f) = self.frames.get_mut(&(tab, frame)) {
            if f.docs > 0 && base_for(f.docs - 1) == Some(base) {
                f.docs -= 1;
            }
        }
    }

    fn mark(&mut self, tab: TabId, frame: u32, key: String) {
        self.frames.entry((tab, frame)).or_default().walked = Some(key);
    }

    fn unmark(&mut self, tab: TabId, frame: u32) {
        if let Some(f) = self.frames.get_mut(&(tab, frame)) {
            f.walked = None;
        }
    }
}

fn bases(m: &Mutex<Bases>) -> MutexGuard<'_, Bases> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Why an `sb` call did not produce a value.
#[derive(Debug)]
pub(crate) enum CallError {
    /// The execution context went away under the call (navigation, frame removed).
    Lost,
    /// The caller's deadline passed first.
    Deadline,
    Driver(DriverError),
}

impl From<DriverError> for CallError {
    fn from(e: DriverError) -> CallError {
        CallError::Driver(e)
    }
}

impl CallError {
    pub(crate) fn into_driver(self, what: &str) -> DriverError {
        match self {
            CallError::Driver(e) => e,
            CallError::Lost => DriverError::Cdp(format!("{what}: the page's context went away")),
            CallError::Deadline => DriverError::Timeout(what.to_string()),
        }
    }
}

/// CDP's answers when the context a call targets is gone or goes away while it runs.
pub(crate) fn context_lost(e: &CdpError) -> bool {
    let CdpError::Protocol { message, .. } = e else {
        return false;
    };
    let m = message.to_ascii_lowercase();
    m.contains("cannot find context")
        || m.contains("context was destroyed")
        || m.contains("promise was collected")
        || m.contains("navigated or closed")
}

/// Text of a page exception, capped, one line.
fn exception_text(ex: &Value) -> String {
    let t = ex
        .pointer("/exception/description")
        .and_then(Value::as_str)
        .or_else(|| ex.get("text").and_then(Value::as_str))
        .unwrap_or("exception");
    let line = t.lines().next().unwrap_or_default();
    line.chars().take(160).collect()
}

impl Driver {
    async fn cdp_until(
        &self,
        session: &str,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, CallError> {
        // Nothing is sent once the caller's budget is spent (B-4 #6: no call ends after it).
        if Instant::now() >= deadline {
            return Err(CallError::Deadline);
        }
        let fut = self.cdp.start(Some(session), method, params);
        match tokio::time::timeout_at(deadline, fut).await {
            Err(_) => Err(CallError::Deadline),
            Ok(Err(e)) if context_lost(&e) => Err(CallError::Lost),
            Ok(Err(e)) => Err(CallError::Driver(e.into())),
            Ok(Ok(v)) => Ok(v),
        }
    }

    /// One `Runtime.callFunctionOn` in `ctx`, awaiting a returned promise, by value.
    pub(crate) async fn raw_call(
        &self,
        ctx: &ExecContext,
        decl: &str,
        args: &[Value],
        deadline: Instant,
    ) -> Result<Value, CallError> {
        let args: Vec<Value> = args.iter().map(|a| json!({ "value": a })).collect();
        let params = json!({
            "functionDeclaration": decl,
            "executionContextId": ctx.id,
            "arguments": args,
            "returnByValue": true,
            "awaitPromise": true,
        });
        let v = self
            .cdp_until(&ctx.session, "Runtime.callFunctionOn", params, deadline)
            .await?;
        if let Some(ex) = v.get("exceptionDetails") {
            return Err(CallError::Driver(DriverError::Cdp(format!(
                "Runtime.callFunctionOn: page exception: {}",
                exception_text(ex)
            ))));
        }
        Ok(v.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    /// The frame's `sb` context, installing the runtime into an existing document when the
    /// registry has none (B-4 #3).
    pub(crate) async fn sb_ctx(
        &self,
        tab: TabId,
        frame: u32,
        deadline: Instant,
    ) -> Result<ExecContext, CallError> {
        let (session, frame_id) = {
            let r = lock(&self.registry);
            if let Some(c) = r.sb_context(tab, frame) {
                return Ok(c.clone());
            }
            if !r.has_tab(tab) {
                return Err(DriverError::TabGone(tab).into());
            }
            match (r.frame_session(tab, frame), r.cdp_frame_id(tab, frame)) {
                (Some(s), Some(f)) => (s.to_string(), f.to_string()),
                _ => {
                    return Err(
                        DriverError::Cdp(format!("frame {frame} of {tab:?} is gone")).into(),
                    )
                }
            }
        };
        let made = self
            .cdp_until(
                &session,
                "Page.createIsolatedWorld",
                json!({"frameId": frame_id, "worldName": SB_WORLD}),
                deadline,
            )
            .await?;
        let id = made
            .get("executionContextId")
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                CallError::Driver(DriverError::Cdp(
                    "Page.createIsolatedWorld: no executionContextId".into(),
                ))
            })?;
        let ctx = self.install(&session, id, deadline).await?;
        Ok(ctx)
    }

    /// Evaluate the runtime source in context `id` and return the context, as the registry
    /// knows it when it does (with its uniqueId).
    async fn install(
        &self,
        session: &str,
        id: i64,
        deadline: Instant,
    ) -> Result<ExecContext, CallError> {
        let v = self
            .cdp_until(
                session,
                "Runtime.evaluate",
                json!({"expression": INJECTED_JS, "contextId": id, "returnByValue": true}),
                deadline,
            )
            .await?;
        if let Some(ex) = v.get("exceptionDetails") {
            return Err(CallError::Driver(DriverError::Cdp(format!(
                "install injected.js: {}",
                exception_text(ex)
            ))));
        }
        let known = lock(&self.registry).context_by_id(session, id).cloned();
        Ok(known.unwrap_or(ExecContext {
            session: session.to_string(),
            id,
            unique_id: None,
        }))
    }

    /// The frame's `sb` context with its document walked with a base (D-73: no read before
    /// a based walk). `settle`: settle in the page first, in the same round trip (open).
    /// `force`: walk even if the driver thinks this document is based (a `no base` answer).
    pub(crate) async fn based_ctx(
        &self,
        tab: TabId,
        frame: u32,
        deadline: Instant,
        settle: Option<SettleOpts>,
        force: bool,
    ) -> Result<ExecContext, CallError> {
        let mut ctx = self.sb_ctx(tab, frame, deadline).await?;
        let mut installed = false;
        loop {
            let key = doc_key(&ctx);
            if !force && settle.is_none() && bases(&self.bases).is_walked(tab, frame, &key) {
                return Ok(ctx);
            }
            if Instant::now() >= deadline {
                return Err(CallError::Deadline);
            }
            let base = bases(&self.bases).next_base(tab, frame).ok_or_else(|| {
                CallError::Driver(DriverError::Cdp(format!(
                    "frame {frame} of {tab:?} used up its element ids (D-604); reopen the tab"
                )))
            })?;
            let settle_arg = settle.map_or(
                Value::Null,
                |s| json!({"quiet": s.quiet_ms, "cap": s.cap_ms}),
            );
            // A base is never handed back once the walk was sent: the walk may have run (and
            // the document kept the base) before its context died, and a bfcache restore
            // would bring those ids back next to the next document's (D-604, D-606).
            let v = self
                .raw_call(
                    &ctx,
                    WALK_FN,
                    &[json!(base), settle_arg, json!(PAINT_CAP_MS)],
                    deadline,
                )
                .await?;
            match v.get("why").and_then(Value::as_str) {
                None => {
                    bases(&self.bases).mark(tab, frame, key);
                    return Ok(ctx);
                }
                Some("no runtime") if !installed => {
                    // walk() never ran (no runtime in the world): the base was not adopted,
                    // the only case where it is handed back.
                    bases(&self.bases).give_back(tab, frame, base);
                    installed = true;
                    ctx = self.install(&ctx.session.clone(), ctx.id, deadline).await?;
                }
                Some(why) => {
                    bases(&self.bases).unmark(tab, frame);
                    return Err(CallError::Driver(DriverError::Cdp(format!(
                        "walk with base {base}: {why}"
                    ))));
                }
            }
        }
    }

    /// Call `decl(args...)` in the frame's `sb` context after its based walk. A context lost
    /// mid-call is retried once in the frame's next context (B-4 #11).
    pub(crate) async fn call_sb(
        &self,
        tab: TabId,
        frame: u32,
        decl: &str,
        args: &[Value],
        deadline: Instant,
    ) -> Result<Value, CallError> {
        let mut retried = false;
        loop {
            let old = self.sb_key(tab, frame);
            let r = async {
                let ctx = self.based_ctx(tab, frame, deadline, None, false).await?;
                self.raw_call(&ctx, decl, args, deadline).await
            }
            .await;
            match r {
                Err(CallError::Lost) if !retried => {
                    retried = true;
                    self.next_ctx(tab, frame, old, deadline).await;
                }
                r => return r,
            }
        }
    }

    /// The document key of the frame's `sb` context in the registry now.
    pub(crate) fn sb_key(&self, tab: TabId, frame: u32) -> Option<String> {
        lock(&self.registry).sb_context(tab, frame).map(doc_key)
    }

    /// Wait (a registry condition, capped by `deadline`) until the frame has an `sb` context
    /// other than `old`. On the cap the caller's retry installs one itself.
    pub(crate) async fn next_ctx(
        &self,
        tab: TabId,
        frame: u32,
        old: Option<String>,
        deadline: Instant,
    ) {
        let cap = deadline.saturating_duration_since(Instant::now());
        self.wait_registry(cap, |r| {
            if !r.has_tab(tab) {
                return Some(());
            }
            r.sb_context(tab, frame)
                .filter(|c| old.as_deref() != Some(doc_key(c).as_str()))
                .map(|_| ())
        })
        .await;
    }

    /// Forget that the frame's current document is based, so the next call walks again.
    pub(crate) fn unmark_walked(&self, tab: TabId, frame: u32) {
        bases(&self.bases).unmark(tab, frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bases_never_repeat_and_fail_closed_at_the_u32_edge() {
        let mut b = Bases::default();
        let t = TabId(1);
        assert_eq!(b.next_base(t, 0), Some(1));
        assert_eq!(b.next_base(t, 0), Some(1 + BASE_STRIDE));
        // Other frames and tabs count on their own.
        assert_eq!(b.next_base(t, 1), Some(1));
        assert_eq!(b.next_base(TabId(2), 0), Some(1));
        assert_eq!(b.next_base(t, 0), Some(1 + 2 * BASE_STRIDE));
        // A base no document adopted is given back; an older one is not.
        b.give_back(t, 0, 1 + BASE_STRIDE);
        b.give_back(t, 0, 1 + 2 * BASE_STRIDE);
        assert_eq!(b.next_base(t, 0), Some(1 + 2 * BASE_STRIDE));
        let last = u32::MAX / BASE_STRIDE - 1;
        let base = base_for(last).expect("last full range fits");
        assert!(u64::from(base) + u64::from(BASE_STRIDE) - 1 <= u64::from(u32::MAX));
        assert_eq!(base_for(last + 1), None, "a range past u32 is refused");
    }

    #[test]
    fn walked_marks_follow_the_document() {
        let mut b = Bases::default();
        let t = TabId(1);
        b.mark(t, 0, "u1".into());
        assert!(b.is_walked(t, 0, "u1"));
        assert!(
            !b.is_walked(t, 0, "u2"),
            "a new document needs its own walk"
        );
        assert!(!b.is_walked(t, 1, "u1"));
        b.unmark(t, 0);
        assert!(!b.is_walked(t, 0, "u1"));
    }

    #[test]
    fn lost_context_errors_are_recognised() {
        let p = |m: &str| CdpError::Protocol {
            method: "Runtime.callFunctionOn".into(),
            code: -32000,
            message: m.into(),
        };
        assert!(context_lost(&p("Cannot find context with specified id")));
        assert!(context_lost(&p("Execution context was destroyed.")));
        assert!(context_lost(&p("Promise was collected")));
        assert!(!context_lost(&p("Object reference chain is too long")));
        assert!(!context_lost(&CdpError::Closed { method: "x".into() }));
    }

    #[test]
    fn doc_key_prefers_the_unique_id() {
        let c = ExecContext {
            session: "S".into(),
            id: 3,
            unique_id: Some("u-3".into()),
        };
        assert_eq!(doc_key(&c), "u-3");
        let c = ExecContext {
            unique_id: None,
            ..c
        };
        assert_eq!(doc_key(&c), "S:3");
    }
}
