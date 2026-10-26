//! T-18 acceptance (tester, round 1): the CDP client's B-3 / B-4 #11 behaviour (D-600),
//! driven through the public API over an in-memory transport written here, independent of
//! the client's own unit tests.

use async_trait::async_trait;
use bytes::Bytes;
use sb_cdp::{Cdp, CdpError, Event, Transport, TransportError, COMMAND_TIMEOUT};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

struct Mem {
    out: mpsc::UnboundedSender<Bytes>,
    inbound: Mutex<Option<mpsc::Receiver<Bytes>>>,
}

#[async_trait]
impl Transport for Mem {
    async fn send(&self, msg: Bytes) -> Result<(), TransportError> {
        self.out.send(msg).map_err(|_| TransportError::Closed)
    }
    fn recv(&self) -> mpsc::Receiver<Bytes> {
        self.inbound
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| mpsc::channel(1).1)
    }
}

struct Wire {
    cdp: Cdp,
    sent: mpsc::UnboundedReceiver<Bytes>,
    inject: Option<mpsc::Sender<Bytes>>,
}

fn wire() -> Wire {
    let (out, sent) = mpsc::unbounded_channel();
    let (inject, inbound) = mpsc::channel(256);
    let t = Arc::new(Mem {
        out,
        inbound: Mutex::new(Some(inbound)),
    });
    Wire {
        cdp: Cdp::new(t as Arc<dyn Transport>),
        sent,
        inject: Some(inject),
    }
}

impl Wire {
    async fn sent(&mut self) -> Value {
        let raw = tokio::time::timeout(Duration::from_secs(5), self.sent.recv())
            .await
            .expect("client wrote a message within 5 s")
            .expect("transport open");
        serde_json::from_slice(&raw).unwrap()
    }
    async fn reply(&self, v: Value) {
        self.inject
            .as_ref()
            .unwrap()
            .send(Bytes::from(v.to_string()))
            .await
            .unwrap();
    }
}

/// Row 2: twenty commands are on the wire before any reply; replies come back in reverse
/// and shuffled order across two sessions and each reaches its own caller.
#[tokio::test]
async fn accept_pipelined_commands_and_out_of_order_replies() {
    let mut w = wire();
    let mut calls = Vec::new();
    for i in 0..20 {
        let session = if i % 2 == 0 { None } else { Some("S-odd") };
        calls.push(w.cdp.start(session, &format!("M.m{i}"), json!({"i": i})));
    }
    // Nothing has been awaited yet: all twenty must already be written.
    let mut by_method = HashMap::new();
    for _ in 0..20 {
        let m = w.sent().await;
        let name = m["method"].as_str().unwrap().to_string();
        let i: usize = name.trim_start_matches("M.m").parse().unwrap();
        assert_eq!(m["params"]["i"], i);
        if i % 2 == 1 {
            assert_eq!(m["sessionId"], "S-odd");
        } else {
            assert!(m.get("sessionId").is_none());
        }
        by_method.insert(i, m["id"].clone());
    }
    assert_eq!(w.cdp.pending(), 20);
    let order: Vec<usize> = (0..20)
        .rev()
        .filter(|i| i % 3 != 0)
        .chain((0..20).filter(|i| i % 3 == 0))
        .collect();
    for i in order {
        w.reply(json!({"id": by_method[&i], "result": {"echo": i}}))
            .await;
    }
    for (i, c) in calls.into_iter().enumerate() {
        assert_eq!(c.await.unwrap(), json!({"echo": i}), "caller {i}");
    }
    assert_eq!(w.cdp.pending(), 0);
}

/// The next event on `rx`, failing (not hanging) after 5 s.
async fn next(rx: &mut broadcast::Receiver<Arc<Event>>) -> Arc<Event> {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("event within 5 s")
        .expect("channel open")
}

/// Row 3: events go to their own session's channel only; detaching one session closes its
/// channel and leaves the other open.
#[tokio::test]
async fn accept_session_routing_and_detach_closes_only_that_channel() {
    let w = wire();
    let mut a = w.cdp.session_events("A");
    let mut b = w.cdp.session_events("B");
    w.reply(json!({"method": "Page.loadEventFired", "params": {"n": 1}, "sessionId": "A"}))
        .await;
    w.reply(json!({"method": "Page.loadEventFired", "params": {"n": 2}, "sessionId": "B"}))
        .await;
    w.reply(json!({"method": "Page.loadEventFired", "params": {"n": 3}, "sessionId": "A"}))
        .await;
    let a1 = next(&mut a).await;
    let a2 = next(&mut a).await;
    let b1 = next(&mut b).await;
    assert_eq!(
        (a1.params["n"].clone(), a2.params["n"].clone()),
        (json!(1), json!(3))
    );
    assert_eq!(b1.params["n"], 2);
    assert_eq!(b1.session.as_deref(), Some("B"));
    // The detach event itself arrives on the browser session.
    w.reply(json!({"method": "Target.detachedFromTarget", "params": {"sessionId": "A", "targetId": "T"}}))
        .await;
    let closed = tokio::time::timeout(Duration::from_secs(5), a.recv())
        .await
        .expect("A's channel resolved within 5 s");
    assert!(
        matches!(closed, Err(broadcast::error::RecvError::Closed)),
        "{closed:?}"
    );
    w.reply(json!({"method": "Page.loadEventFired", "params": {"n": 4}, "sessionId": "B"}))
        .await;
    assert_eq!(next(&mut b).await.params["n"], 4, "B still open");
}

/// Row 4a: a command with no reply fails after exactly 10 s naming its method.
#[tokio::test(start_paused = true)]
async fn accept_timeout_is_10_s_and_names_the_method() {
    let mut w = wire();
    assert_eq!(COMMAND_TIMEOUT, Duration::from_secs(10));
    let t0 = tokio::time::Instant::now();
    let c = w.cdp.start(Some("S"), "DOM.getDocument", json!({}));
    let _ = w.sent().await;
    let e = c.await.unwrap_err();
    assert_eq!(t0.elapsed(), Duration::from_secs(10));
    assert!(matches!(e, CdpError::Timeout { ref method, .. } if method == "DOM.getDocument"));
    assert!(e.to_string().contains("DOM.getDocument"), "{e}");
    assert_eq!(w.cdp.pending(), 0, "timed-out slot removed");
}

/// Row 4b: the transport closing fails every pending caller at once with `Closed`.
#[tokio::test]
async fn accept_transport_close_fails_pending_callers() {
    let mut w = wire();
    let a = w.cdp.start(None, "Browser.getVersion", json!({}));
    let b = w.cdp.start(Some("S"), "Runtime.evaluate", json!({}));
    let _ = w.sent().await;
    let _ = w.sent().await;
    w.inject.take();
    let both = tokio::time::timeout(Duration::from_secs(2), async { (a.await, b.await) })
        .await
        .expect("pending callers failed well before the 10 s timeout");
    assert_eq!(
        both.0.unwrap_err(),
        CdpError::Closed {
            method: "Browser.getVersion".into()
        }
    );
    assert_eq!(
        both.1.unwrap_err(),
        CdpError::Closed {
            method: "Runtime.evaluate".into()
        }
    );
    assert_eq!(w.cdp.pending(), 0);
}

/// Row 4c: dropping a caller's future removes its reply slot; a late reply is ignored.
#[tokio::test]
async fn accept_cancelled_caller_slot_is_removed() {
    let mut w = wire();
    let keep = w.cdp.start(None, "Keep.me", json!({}));
    let gone = w.cdp.start(None, "Drop.me", json!({}));
    let k = w.sent().await;
    let g = w.sent().await;
    assert_eq!(w.cdp.pending(), 2);
    drop(gone);
    assert_eq!(w.cdp.pending(), 1);
    w.reply(json!({"id": g["id"], "result": {"late": true}}))
        .await;
    w.reply(json!({"id": k["id"], "result": {"ok": 1}})).await;
    assert_eq!(keep.await.unwrap(), json!({"ok": 1}));
    assert_eq!(w.cdp.pending(), 0);
}
