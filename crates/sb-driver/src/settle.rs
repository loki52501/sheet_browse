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
//! Fail-safe: any doubt ends a settle earlier, never later. The cap is a hard deadline in
//! Rust (the in-page call gets the cap minus [`REPLY_MARGIN`] so its answer can arrive in
//! time); nothing waits past it.
//!
//! The loop: wait (a condition on the event store, capped) until no counted request is in
//! flight, then one in-page settle; if a counted request started during it, or one is in
//! flight now, go again while time remains. A page with no network activity costs one round
//! trip, as before T-22.

use crate::runtime::CallError;
use crate::{Driver, DriverError, Registry, SettleOpts, Settled, TabId};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::time::Instant;

/// D-620 (3): a request in flight longer than this no longer holds settle.
pub(crate) const LONG_REQUEST: Duration = Duration::from_millis(3000);
/// The in-page settle of [`Driver::settle`] gets the time left minus this, so its answer
/// (with `changed`) arrives before the hard deadline.
pub(crate) const REPLY_MARGIN: Duration = Duration::from_millis(50);

/// The in-page settle (`settle`, `w(..)`, open's network pass and the wait between act(id)
/// attempts): `{settled, changed}`.
pub(crate) const SETTLE_FN: &str = "function (opts) {
  const sb = globalThis.__sb;
  if (!sb) return { why: 'no runtime' };
  return sb.settle(opts);
}";

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
    /// Make one call even when the network never went idle (the act window needs an index).
    pub at_least_once: bool,
}

/// What the loop found.
#[derive(Debug, Default)]
pub(crate) struct NetSettled {
    /// The last in-page answer and the document that gave it.
    pub last: Option<(Value, String)>,
    pub settled: bool,
    pub changed: u32,
    /// A counted request was still in flight at the end.
    pub net_busy: bool,
}

impl Driver {
    /// (busy, first age-out) for `tab` now.
    pub(crate) fn net_state(&self, tab: TabId, since: u64) -> (bool, Option<Instant>) {
        self.with_events(|r, ev| net_busy(r, ev, tab, since, Instant::now()))
    }

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
        loop {
            let idle = self.wait_net_idle(tab, since, deadline).await;
            if !idle && !(pass.at_least_once && out.last.is_none()) {
                out.settled = false;
                out.net_busy = true;
                return Ok(out);
            }
            let now = Instant::now();
            let left = match pass.grace {
                // A hard deadline: the page answers before it.
                None => deadline
                    .saturating_duration_since(now)
                    .saturating_sub(REPLY_MARGIN),
                Some(_) => deadline.saturating_duration_since(now),
            };
            // The act window: its first call gets the whole window (as before T-22), later
            // ones what is left of the network budget.
            let cap = match pass.per_call {
                Some(c) if out.last.is_some() => c.min(left),
                Some(c) => c,
                None => left,
            };
            if cap.is_zero() && out.last.is_some() {
                out.settled = false;
                out.net_busy = !idle;
                return Ok(out);
            }
            let call_deadline = match pass.grace {
                Some(g) => now + cap + g,
                None => deadline,
            };
            let iter = self.events_mark();
            let cap_ms = u64::try_from(cap.as_millis()).unwrap_or(u64::MAX);
            let opts = json!({"quiet": pass.quiet_ms, "cap": cap_ms});
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
                return Ok(out);
            }
            let (again, busy) = self.with_events(|r, ev| {
                let busy = net_busy(r, ev, tab, since, Instant::now()).0;
                (busy || started_since(r, ev, tab, iter), busy)
            });
            if !again {
                out.settled = settled;
                out.net_busy = false;
                return Ok(out);
            }
            if Instant::now() >= deadline {
                out.settled = false;
                out.net_busy = busy;
                return Ok(out);
            }
        }
    }

    /// I-9 `settle` (B-4 #8): frame 0's DOM and the tab's network, capped at `opts.cap_ms`
    /// as a hard deadline. A dialog open on the tab blocks the page: `{settled: false}` at
    /// once.
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
