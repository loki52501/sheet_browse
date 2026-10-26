//! B-4 #8 `settle()` with network tracking (T-22, D-620).
//!
//! Settled means all three hold at once: no DOM mutation for the quiet window and no running
//! finite CSS animation on changed nodes (both in the page, `__sb.settle`), and no request in
//! flight that counts (Rust, from the [`crate::consequence::Net`] the observer feeds in wire
//! order: no round trip to know it).
//!
//! D-620, which requests count. A request counts for a settle that started at event mark `m`
//! when all of these hold:
//! 1. it was announced (`Network.requestWillBeSent`) on a session of the tab (its page or one
//!    of its out-of-process iframes) after `m`: requests started before the settle began
//!    (a poll, a long-poll, a stream opened at load) never hold it. `open` marks before its
//!    `Page.navigate`, an act step before its input, `Driver::settle` at its call;
//! 2. its type is not EventSource, WebSocket, Media, Ping, CSPViolationReport or Prefetch
//!    (streams and fire-and-forget requests that may never finish);
//! 3. it is younger than [`LONG_REQUEST`] (3 s, the settle cap): a long-poll past the cap no
//!    longer counts;
//! 4. neither `loadingFinished` nor `loadingFailed` came for it, its session did not detach,
//!    and, for a navigation request, its frame did not commit or start a download.
//!
//! D-625 (refines D-620 (1) and (3)): a request that does not hold the wait still makes the
//! answer honest. When a settle ends with a request of the tab still in flight, whether it
//! started before the settle began (D-620 (1): not waited for) or is older than
//! [`LONG_REQUEST`] (D-620 (3): no longer waited for), the result is `settled: false`. A
//! stream type (D-620 (2)) is never tracked, so it never does.
//!
//! Fail-safe: any doubt ends a settle earlier, never later. The cap is a hard deadline in
//! Rust (the in-page call gets the cap minus [`REPLY_MARGIN`] so its answer can arrive in
//! time); nothing waits past it. The act window's deadline (input + 3 s) is hard too: every
//! call in it, with its grace, ends by then (B-4 #8).
//!
//! The loop: wait (a condition on the event store, capped) until no counted request is in
//! flight, then one in-page settle; if a counted request started during it, or one is in
//! flight now, go again while time remains. A page with no network activity costs one round
//! trip, as before T-22. `Driver::settle` also counts the DOM mutations of the network waits
//! between its in-page calls ([`GAP_FN`]), so `changed` covers the whole settle.

use crate::runtime::CallError;
use crate::{Driver, DriverError, Registry, SettleOpts, Settled, TabId};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::time::Instant;

/// D-620 (3): a request in flight longer than this no longer holds settle.
pub(crate) const LONG_REQUEST: Duration = Duration::from_millis(3000);
/// The in-page settle of [`Driver::settle`] gets the time left minus this, so its answer
/// (with `changed`) arrives before the hard deadline.
pub(crate) const REPLY_MARGIN: Duration = Duration::from_millis(50);
/// The act window's network wait ends this long before the window's deadline when no call
/// was made yet, so its one index-only call (settle cap 0) can still answer in time.
pub(crate) const INDEX_ROOM: Duration = Duration::from_millis(100);

/// The in-page settle (`settle`, `w(..)`, open's network pass and the wait between act(id)
/// attempts): `{settled, changed}`. With `opts.gap` (a [`GAP_FN`] token), the mutations
/// counted since that mark are added to `changed`, and the mark is dropped.
pub(crate) const SETTLE_FN: &str = "function (opts) {
  const sb = globalThis.__sb;
  if (!sb) return { why: 'no runtime' };
  let gap = 0;
  const g = globalThis.__sbGap;
  if (g && opts && opts.gap != null && g.marks.has(opts.gap)) {
    const m = g.marks.get(opts.gap);
    g.n += g.mo.takeRecords().length;
    gap = g.n - m.n;
    clearTimeout(m.timer);
    g.marks.delete(opts.gap);
    if (!g.marks.size) {
      g.mo.disconnect();
      delete globalThis.__sbGap;
    }
  }
  return Promise.resolve(sb.settle(opts)).then((s) => {
    if (s && typeof s.changed === 'number') s.changed += gap;
    return s;
  });
}";

/// Start counting DOM mutations in the `sb` world (the page's own world never sees it)
/// until the next settle call that carries `tok`. Sent, not awaited, before a network wait
/// between two in-page settles, so `Settled.changed` covers those waits too (accept-T-22
/// note 2). The observer goes with its last mark, or `ttl` ms after the mark if no call
/// comes (a timer in the page, not a wait in the driver).
const GAP_FN: &str = "function (tok, ttl) {
  let g = globalThis.__sbGap;
  if (!g) {
    g = { n: 0, marks: new Map(), mo: null };
    g.mo = new MutationObserver((rs) => { g.n += rs.length; });
    g.mo.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    globalThis.__sbGap = g;
  }
  const timer = setTimeout(() => {
    g.marks.delete(tok);
    if (!g.marks.size && globalThis.__sbGap === g) {
      g.mo.disconnect();
      delete globalThis.__sbGap;
    }
  }, ttl);
  g.marks.set(tok, { n: g.n, timer: timer });
  return true;
}";

/// Tokens of [`GAP_FN`] marks: unique in the process.
static GAP_TOKENS: AtomicU64 = AtomicU64::new(1);

/// Whether a counted request (D-620) of `tab` is in flight now, and when the first of them
/// stops counting by age.
pub(crate) fn net_busy(
    r: &Registry,
    ev: &crate::consequence::Events,
    tab: TabId,
    since: u64,
    now: Instant,
) -> (bool, Option<Instant>) {
    let mut busy = false;
    let mut expiry: Option<Instant> = None;
    for q in ev.net().inflight() {
        if q.seq <= since || r.tab_of_session(&q.session) != Some(tab) {
            continue;
        }
        let ends = q.at + LONG_REQUEST;
        if ends <= now {
            continue;
        }
        busy = true;
        expiry = Some(expiry.map_or(ends, |e| e.min(ends)));
    }
    (busy, expiry)
}

/// D-625: whether any tracked request of `tab` announced after `since` is still in flight,
/// whatever its age (`since` 0: those started before the settle began too).
pub(crate) fn pending(
    r: &Registry,
    ev: &crate::consequence::Events,
    tab: TabId,
    since: u64,
) -> bool {
    ev.net()
        .inflight()
        .any(|q| q.seq > since && r.tab_of_session(&q.session) == Some(tab))
}

/// Whether a counted request of `tab` started after `mark` (finished or not).
pub(crate) fn started_since(
    r: &Registry,
    ev: &crate::consequence::Events,
    tab: TabId,
    mark: u64,
) -> bool {
    ev.net()
        .starts()
        .any(|(s, q)| q > mark && r.tab_of_session(s) == Some(tab))
}

/// How the loop calls the page.
pub(crate) struct Pass<'a> {
    /// `function (opts)` returning `{settled, ...}`; `changed` is summed when present.
    pub decl: &'a str,
    pub quiet_ms: u32,
    /// Cap of one in-page call (the act window); `None`: the time left.
    pub per_call: Option<Duration>,
    /// How long past its cap one call may take to answer; `None`: the deadline is hard.
    pub grace: Option<Duration>,
    /// The act window: make one call even when the network never went idle (the step needs
    /// an index; that call is index-only, settle cap 0, and its network wait ends
    /// [`INDEX_ROOM`] early so it answers in time), and end every call, grace included, by
    /// the deadline: each is capped at min(`per_call`, time left - [`REPLY_MARGIN`]).
    pub at_least_once: bool,
    /// D-625: a request announced after this mark still in flight at the end makes the
    /// answer unsettled, whatever its age. 0 for `settle` and `w(d)` (every request of the
    /// tab), the step's mark for the act window.
    pub pending_since: u64,
    /// Count the DOM mutations of the network waits between calls into `changed`
    /// ([`GAP_FN`]; the decl must be [`SETTLE_FN`]).
    pub count_gaps: bool,
}

/// What the loop found.
#[derive(Debug, Default)]
pub(crate) struct NetSettled {
    /// The last in-page answer and the document that gave it.
    pub last: Option<(Value, String)>,
    pub settled: bool,
    pub changed: u32,
    /// A request announced after the settle's mark was still in flight at the end, counted
    /// or aged out (D-620, D-625).
    pub net_busy: bool,
}

impl Driver {
    /// Wait until no counted request of `tab` is in flight, at most until `deadline`: a
    /// condition re-checked on every event, and at the first age-out (D-620 (3)). True when
    /// idle.
    pub(crate) async fn wait_net_idle(&self, tab: TabId, since: u64, deadline: Instant) -> bool {
        loop {
            // Judged no later than the deadline: a timer that wakes a little late must not
            // let a request age out (D-620 (3)) that was in flight at the deadline.
            let now = Instant::now().min(deadline);
            let (busy, expiry) = self.with_events(|r, ev| net_busy(r, ev, tab, since, now));
            if !busy {
                return true;
            }
            if now >= deadline {
                return false;
            }
            let until = expiry.map_or(deadline, |e| e.min(deadline));
            self.wait_state(until.saturating_duration_since(now), |r, ev| {
                (!net_busy(r, ev, tab, since, Instant::now().min(deadline)).0).then_some(())
            })
            .await;
        }
    }

    /// Every way out of the loop ends here: D-625's honesty about requests still in flight.
    fn honest(&self, mut out: NetSettled, tab: TabId, since: u64, pass: &Pass<'_>) -> NetSettled {
        let (any, mine) = self.with_events(|r, ev| {
            (
                pending(r, ev, tab, pass.pending_since),
                pending(r, ev, tab, since),
            )
        });
        if any {
            out.settled = false;
        }
        out.net_busy |= mine;
        out
    }

    /// Send [`GAP_FN`] to the frame's `sb` context (not awaited); its token, if sent.
    fn start_gap(&self, tab: TabId, frame: u32, ttl: Duration) -> Option<u64> {
        let ctx = self.with_registry(|r| r.sb_context(tab, frame).cloned())?;
        let tok = GAP_TOKENS.fetch_add(1, Ordering::Relaxed);
        let ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
        drop(self.cdp.start(
            Some(&ctx.session),
            "Runtime.callFunctionOn",
            json!({"functionDeclaration": GAP_FN, "executionContextId": ctx.id,
                   "arguments": [{"value": tok}, {"value": ttl_ms}], "returnByValue": true}),
        ));
        Some(tok)
    }

    /// The settle loop (module docs): network idle, then the in-page call, again while a
    /// counted request started meanwhile and time remains.
    pub(crate) async fn net_settle(
        &self,
        tab: TabId,
        frame: u32,
        since: u64,
        deadline: Instant,
        pass: Pass<'_>,
    ) -> Result<NetSettled, CallError> {
        let mut out = NetSettled::default();
        let mut gap: Option<u64> = None;
        loop {
            let first_needed = pass.at_least_once && out.last.is_none();
            // The act window's one call must still fit before its deadline.
            let wait_end = if first_needed {
                deadline.checked_sub(INDEX_ROOM).unwrap_or(deadline)
            } else {
                deadline
            };
            let idle = self.wait_net_idle(tab, since, wait_end).await;
            if !idle && !first_needed {
                out.settled = false;
                out.net_busy = true;
                return Ok(self.honest(out, tab, since, &pass));
            }
            let now = Instant::now();
            let to_end = deadline.saturating_duration_since(now);
            let cap = if !idle {
                // The network never went idle: nothing to settle for, only the index.
                Duration::ZERO
            } else if pass.grace.is_none() || pass.at_least_once {
                // A hard deadline: the page answers before it.
                pass.per_call
                    .unwrap_or(to_end)
                    .min(to_end.saturating_sub(REPLY_MARGIN))
            } else {
                // w(d): the time left, and the grace past it for the answer.
                to_end
            };
            if cap.is_zero() && idle && out.last.is_some() {
                out.settled = false;
                return Ok(self.honest(out, tab, since, &pass));
            }
            let call_deadline = match pass.grace {
                Some(g) if pass.at_least_once => (now + cap + g).min(deadline),
                Some(g) => now + cap + g,
                None => deadline,
            };
            let iter = self.events_mark();
            let cap_ms = u64::try_from(cap.as_millis()).unwrap_or(u64::MAX);
            let mut opts = json!({"quiet": pass.quiet_ms, "cap": cap_ms});
            if let Some(tok) = gap.take() {
                opts["gap"] = json!(tok);
            }
            let (v, doc) = self
                .call_sb_doc(tab, frame, pass.decl, &[opts], call_deadline)
                .await?;
            let settled = v.get("settled").and_then(Value::as_bool) == Some(true);
            if let Some(c) = v.get("changed").and_then(Value::as_u64) {
                out.changed = out
                    .changed
                    .saturating_add(u32::try_from(c).unwrap_or(u32::MAX));
            }
            out.last = Some((v, doc));
            if !idle {
                // The one call `at_least_once` makes after a wait that ended busy: busy it
                // stays, even if the request aged out of D-620 (3) during the call (its
                // effect may not be in the page yet).
                out.settled = false;
                out.net_busy = true;
                return Ok(self.honest(out, tab, since, &pass));
            }
            let (again, busy) = self.with_events(|r, ev| {
                let busy = net_busy(r, ev, tab, since, Instant::now()).0;
                (busy || started_since(r, ev, tab, iter), busy)
            });
            if !again {
                out.settled = settled;
                out.net_busy = false;
                return Ok(self.honest(out, tab, since, &pass));
            }
            if Instant::now() >= deadline {
                out.settled = false;
                out.net_busy = busy;
                return Ok(self.honest(out, tab, since, &pass));
            }
            if pass.count_gaps {
                let ttl =
                    deadline.saturating_duration_since(Instant::now()) + Duration::from_secs(1);
                gap = self.start_gap(tab, frame, ttl);
            }
        }
    }

    /// I-9 `settle` (B-4 #8): frame 0's DOM and the tab's network, capped at `opts.cap_ms`
    /// as a hard deadline. A dialog open on the tab blocks the page: `{settled: false}` at
    /// once. A request of the tab still in flight at the end, waited for or not, makes it
    /// `{settled: false}` (D-625).
    pub(crate) async fn settle_tab(
        &self,
        tab: TabId,
        opts: SettleOpts,
    ) -> Result<Settled, DriverError> {
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_millis(u64::from(opts.cap_ms));
        if !self.with_registry(|r| r.has_tab(tab)) {
            return Err(DriverError::TabGone(tab));
        }
        let unsettled = Settled {
            settled: false,
            changed: 0,
        };
        if self.open_dialog(tab).is_some() {
            return Ok(unsettled);
        }
        let mark = self.events_mark();
        let pass = Pass {
            decl: SETTLE_FN,
            quiet_ms: opts.quiet_ms,
            per_call: None,
            grace: None,
            at_least_once: false,
            pending_since: 0,
            count_gaps: true,
        };
        let run = tokio::time::timeout_at(
            deadline,
            self.or_dialog(tab, mark, self.net_settle(tab, 0, mark, deadline, pass)),
        );
        match run.await {
            Ok(Some(Ok(s))) => Ok(Settled {
                settled: s.settled,
                changed: s.changed,
            }),
            // The cap, or a dialog that opened: not settled.
            Err(_) | Ok(None) => Ok(unsettled),
            Ok(Some(Err(CallError::Deadline))) => Ok(unsettled),
            Ok(Some(Err(e))) => Err(e.into_driver("settle")),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::consequence::Events;
    use serde_json::json;

    fn tab_with_oopif() -> (Registry, TabId) {
        let mut r = Registry::new();
        r.apply(
            None,
            "Target.attachedToTarget",
            &json!({"sessionId": "S1", "waitingForDebugger": true,
                "targetInfo": {"targetId": "T1", "type": "page", "url": "about:blank"}}),
        );
        r.apply(
            None,
            "Target.attachedToTarget",
            &json!({"sessionId": "S2", "waitingForDebugger": true,
                "targetInfo": {"targetId": "T2", "type": "page", "url": "about:blank"}}),
        );
        r.apply(
            Some("S1"),
            "Page.frameAttached",
            &json!({"frameId": "F1", "parentFrameId": "T1"}),
        );
        r.apply(
            Some("S1"),
            "Target.attachedToTarget",
            &json!({"sessionId": "O1", "waitingForDebugger": true,
                "targetInfo": {"targetId": "F1", "type": "iframe", "url": "https://b.test/"}}),
        );
        let t = r.tab_by_target("T1").unwrap();
        (r, t)
    }

    fn start(e: &mut Events, sid: &str, id: &str) {
        e.apply(
            Some(sid),
            "Network.requestWillBeSent",
            &json!({"requestId": id, "type": "XHR", "frameId": "T1"}),
        );
    }

    /// D-620 (1): only requests started after the settle's mark, on the tab's own sessions
    /// (its OOPIFs' included), count.
    #[test]
    fn only_the_tabs_requests_after_the_mark_count() {
        let (r, t) = tab_with_oopif();
        let mut e = Events::default();
        start(&mut e, "S1", "before");
        let since = e.mark();
        let now = Instant::now();
        assert!(
            !net_busy(&r, &e, t, since, now).0,
            "started before the mark"
        );
        start(&mut e, "S2", "other-tab");
        assert!(
            !net_busy(&r, &e, t, since, Instant::now()).0,
            "another tab's"
        );
        assert!(!started_since(&r, &e, t, since));
        let iter = e.mark();
        start(&mut e, "O1", "in-oopif");
        let (busy, expiry) = net_busy(&r, &e, t, since, Instant::now());
        assert!(busy, "the tab's OOPIF counts");
        assert!(expiry.is_some_and(|x| x > Instant::now()));
        assert!(started_since(&r, &e, t, iter));
        assert!(!started_since(&r, &e, t, e.mark()));
        e.apply(
            Some("O1"),
            "Network.loadingFinished",
            &json!({"requestId": "in-oopif"}),
        );
        assert!(!net_busy(&r, &e, t, since, Instant::now()).0);
        assert!(started_since(&r, &e, t, iter), "it started, even if done");
    }

    /// D-620 (3): a request older than the cap no longer counts.
    #[test]
    fn a_request_past_the_cap_no_longer_counts() {
        let (r, t) = tab_with_oopif();
        let mut e = Events::default();
        let since = e.mark();
        start(&mut e, "S1", "long-poll");
        let now = Instant::now();
        assert!(net_busy(&r, &e, t, since, now).0);
        assert!(!net_busy(&r, &e, t, since, now + LONG_REQUEST).0);
    }
}
