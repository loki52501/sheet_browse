//! The CDP client (B-3, D-600): one writer task, one reader task, a reply map by id,
//! flattened sessions, pipelined sends and a 10 s command timeout (B-4 #11).
//!
//! [`Cdp::start`] queues a command synchronously and returns a future for its reply, so a
//! caller can put several commands on the wire before awaiting any of them. Every inbound
//! event goes first to the synchronous observer (the driver's registry, so it never lags),
//! then to the all-events broadcast and to the broadcast of its session.
//! [`CdpSender::start_then`] runs a callback on the reply inside the reader, in wire order
//! with the events, for state that must be applied exactly where the browser produced it.

use crate::Transport;
use bytes::Bytes;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

/// B-4 #11: a command with no reply after this long fails its caller with the method name.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

const ALL_EVENTS_CAPACITY: usize = 1024;
const SESSION_EVENTS_CAPACITY: usize = 256;

/// A failed command. The method is always named; params never are (they may carry a vault
/// value on its way into `Input.insertText`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CdpError {
    #[error("{method}: cdp error {code}: {message}")]
    Protocol {
        method: String,
        code: i64,
        message: String,
    },
    #[error("{method}: no reply after {} s", after.as_secs())]
    Timeout { method: String, after: Duration },
    #[error("{method}: cdp connection closed")]
    Closed { method: String },
}

/// One CDP event. `session` is `None` for the browser session.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub session: Option<String>,
    pub method: String,
    pub params: Value,
}

/// Called synchronously by the reader task for every event, before any broadcast.
pub type Observer = Box<dyn Fn(&Event, &CdpSender) + Send + Sync>;

/// Called by the reader task with a command's reply, see [`CdpSender::start_then`].
type ReplyHook = Box<dyn FnOnce(Result<Value, CdpError>) + Send>;

type Reply = Result<Value, (i64, String)>;

/// Where a reply goes: a waiting caller, or a hook run in the reader.
enum Slot {
    Caller(oneshot::Sender<Reply>),
    Hook { method: String, hook: ReplyHook },
}

/// `None` marks the client closed: sends after close fail at once.
type ReplyMap = Option<HashMap<u64, Slot>>;

struct Shared {
    replies: Mutex<ReplyMap>,
    sessions: Mutex<HashMap<String, broadcast::Sender<Arc<Event>>>>,
    all: broadcast::Sender<Arc<Event>>,
    next_id: AtomicU64,
    closed: watch::Sender<bool>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    /// Fail every pending caller (their senders drop) and refuse new sends.
    fn close(&self) {
        let pending = lock(&self.replies).take();
        drop(pending);
        // Flag first: `session_events` checks it under the sessions lock, so no channel is
        // registered after the clear below.
        self.closed.send_replace(true);
        // Ending the per-session broadcasts tells their subscribers the session is gone.
        lock(&self.sessions).clear();
    }
}

/// A cheap handle that can queue commands. The observer receives one, so it can answer an
/// event (for example run a target waiting for the debugger) without a lagging subscriber.
#[derive(Clone)]
pub struct CdpSender {
    shared: Arc<Shared>,
    tx: mpsc::UnboundedSender<Bytes>,
}

/// Removes a caller's reply slot when its future is dropped before the reply (cancelled,
/// timed out). A reply that arrives later finds no slot and is discarded.
struct SlotGuard {
    shared: Arc<Shared>,
    id: u64,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        if let Some(map) = lock(&self.shared.replies).as_mut() {
            map.remove(&self.id);
        }
    }
}

impl CdpSender {
    /// Queue `method` now and return a future for its reply. The message is on its way to
    /// the writer task before this returns, so sends pipeline: a later `start` never waits
    /// for an earlier reply. The 10 s timeout counts from this call.
    pub fn start(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> impl Future<Output = Result<Value, CdpError>> + Send + 'static {
        let deadline = tokio::time::Instant::now() + COMMAND_TIMEOUT;
        let method = method.to_string();
        let (tx, rx) = oneshot::channel();
        let queued = self
            .queue(session, &method, params, Slot::Caller(tx))
            .map(|id| {
                let guard = SlotGuard {
                    shared: self.shared.clone(),
                    id,
                };
                (rx, guard)
            });
        async move {
            let (rx, guard) = match queued {
                Some(q) => q,
                None => return Err(CdpError::Closed { method }),
            };
            let outcome = tokio::time::timeout_at(deadline, rx).await;
            drop(guard);
            match outcome {
                Err(_) => Err(CdpError::Timeout {
                    method,
                    after: COMMAND_TIMEOUT,
                }),
                Ok(Err(_)) => Err(CdpError::Closed { method }),
                Ok(Ok(Ok(v))) => Ok(v),
                Ok(Ok(Err((code, message)))) => Err(CdpError::Protocol {
                    method,
                    code,
                    message,
                }),
            }
        }
    }

    /// Queue `method` now; when its reply arrives the reader task calls `then` with it,
    /// synchronously, before it handles the next inbound message. So `then` sees the reply at
    /// its place in the event stream: the observer has applied every event before it and none
    /// after it. Keep `then` short (it blocks the reader). If the connection closes first,
    /// `then` is dropped uncalled. No 10 s timeout applies: nobody waits on it. False when the
    /// client is already closed.
    pub fn start_then(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
        then: impl FnOnce(Result<Value, CdpError>) + Send + 'static,
    ) -> bool {
        let slot = Slot::Hook {
            method: method.to_string(),
            hook: Box::new(then),
        };
        self.queue(session, method, params, slot).is_some()
    }

    /// Start and await one command.
    pub async fn call(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, CdpError> {
        self.start(session, method, params).await
    }

    fn queue(&self, session: Option<&str>, method: &str, params: Value, slot: Slot) -> Option<u64> {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let mut msg = serde_json::Map::new();
        msg.insert("id".into(), Value::from(id));
        msg.insert("method".into(), Value::from(method));
        msg.insert("params".into(), params);
        if let Some(s) = session {
            msg.insert("sessionId".into(), Value::from(s));
        }
        let bytes = Bytes::from(serde_json::to_vec(&Value::Object(msg)).ok()?);
        {
            let mut replies = lock(&self.shared.replies);
            replies.as_mut()?.insert(id, slot);
        }
        // The writer is gone only after close; then the slot goes too.
        if self.tx.send(bytes).is_err() {
            if let Some(map) = lock(&self.shared.replies).as_mut() {
                map.remove(&id);
            }
            return None;
        }
        Some(id)
    }

    /// Callers waiting for a reply right now (diagnostic; the cancellation tests read it).
    pub fn pending(&self) -> usize {
        lock(&self.shared.replies).as_ref().map_or(0, HashMap::len)
    }

    pub fn is_closed(&self) -> bool {
        *self.shared.closed.borrow()
    }
}

struct Inner {
    sender: CdpSender,
    tasks: [JoinHandle<()>; 2],
}

impl Drop for Inner {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        self.sender.shared.close();
    }
}

/// The CDP client over any [`Transport`]. Cloning shares the connection; the reader and
/// writer tasks stop when the last clone drops.
#[derive(Clone)]
pub struct Cdp {
    inner: Arc<Inner>,
}

#[derive(Deserialize)]
struct Inbound {
    id: Option<u64>,
    method: Option<String>,
    #[serde(default)]
    params: Value,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<WireError>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

#[derive(Deserialize)]
struct WireError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

impl Cdp {
    /// Must be called inside a tokio runtime: it spawns the reader and writer tasks.
    pub fn new(transport: Arc<dyn Transport>) -> Cdp {
        Cdp::with_observer(transport, Box::new(|_, _| {}))
    }

    /// As [`Cdp::new`], with `observer` called synchronously for every event before any
    /// broadcast subscriber sees it.
    pub fn with_observer(transport: Arc<dyn Transport>, observer: Observer) -> Cdp {
        let (all, _) = broadcast::channel(ALL_EVENTS_CAPACITY);
        let shared = Arc::new(Shared {
            replies: Mutex::new(Some(HashMap::new())),
            sessions: Mutex::new(HashMap::new()),
            all,
            next_id: AtomicU64::new(1),
            closed: watch::channel(false).0,
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let sender = CdpSender {
            shared: shared.clone(),
            tx,
        };
        let inbound = transport.recv();
        let writer = tokio::spawn(write_loop(transport, rx, shared.clone()));
        let reader = tokio::spawn(read_loop(inbound, sender.clone(), observer));
        Cdp {
            inner: Arc::new(Inner {
                sender,
                tasks: [writer, reader],
            }),
        }
    }

    /// See [`CdpSender::start`].
    pub fn start(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> impl Future<Output = Result<Value, CdpError>> + Send + 'static {
        self.inner.sender.start(session, method, params)
    }

    pub async fn call(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, CdpError> {
        self.start(session, method, params).await
    }

    /// A handle that can queue commands without keeping the tasks alive.
    pub fn sender(&self) -> CdpSender {
        self.inner.sender.clone()
    }

    /// Every event, from every session.
    pub fn events(&self) -> broadcast::Receiver<Arc<Event>> {
        self.inner.sender.shared.all.subscribe()
    }

    /// Events of one flattened session. The channel closes when the session detaches.
    pub fn session_events(&self, session: &str) -> broadcast::Receiver<Arc<Event>> {
        let mut sessions = lock(&self.inner.sender.shared.sessions);
        if let Some(tx) = sessions.get(session) {
            return tx.subscribe();
        }
        let (tx, rx) = broadcast::channel(SESSION_EVENTS_CAPACITY);
        if !self.is_closed() {
            sessions.insert(session.to_string(), tx);
        }
        rx
    }

    pub fn pending(&self) -> usize {
        self.inner.sender.pending()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.sender.is_closed()
    }

    /// Resolves once the transport has closed.
    pub async fn closed(&self) {
        let mut rx = self.inner.sender.shared.closed.subscribe();
        // An error means the sender is gone, which only happens after close.
        let _ = rx.wait_for(|c| *c).await;
    }
}

async fn write_loop(
    transport: Arc<dyn Transport>,
    mut rx: mpsc::UnboundedReceiver<Bytes>,
    shared: Arc<Shared>,
) {
    while let Some(msg) = rx.recv().await {
        if transport.send(msg).await.is_err() {
            shared.close();
            return;
        }
    }
}

async fn read_loop(mut inbound: mpsc::Receiver<Bytes>, sender: CdpSender, observer: Observer) {
    while let Some(raw) = inbound.recv().await {
        // A malformed message cannot be routed; its caller, if any, times out naming the method.
        let Ok(msg) = serde_json::from_slice::<Inbound>(&raw) else {
            continue;
        };
        if let Some(id) = msg.id {
            let slot = lock(&sender.shared.replies)
                .as_mut()
                .and_then(|m| m.remove(&id));
            let reply = match msg.error {
                Some(e) => Err((e.code, e.message)),
                None => Ok(msg.result.unwrap_or(Value::Null)),
            };
            match slot {
                // The caller may have just been dropped; nothing to do then.
                Some(Slot::Caller(tx)) => {
                    let _ = tx.send(reply);
                }
                // Run here, before the next message: wire order.
                Some(Slot::Hook { method, hook }) => {
                    hook(reply.map_err(|(code, message)| CdpError::Protocol {
                        method,
                        code,
                        message,
                    }))
                }
                None => {}
            }
            continue;
        }
        let Some(method) = msg.method else {
            continue;
        };
        let event = Event {
            session: msg.session_id,
            method,
            params: msg.params,
        };
        observer(&event, &sender);
        let detached = (event.method == "Target.detachedFromTarget")
            .then(|| event.params.get("sessionId").and_then(Value::as_str))
            .flatten()
            .map(str::to_string);
        let event = Arc::new(event);
        let _ = sender.shared.all.send(event.clone());
        {
            let mut sessions = lock(&sender.shared.sessions);
            if let Some(s) = event.session.as_deref() {
                if let Some(tx) = sessions.get(s) {
                    let _ = tx.send(event.clone());
                }
            }
            if let Some(gone) = detached {
                sessions.remove(&gone);
            }
        }
    }
    sender.shared.close();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TransportError;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    /// In-memory transport: the test reads what the client sent and injects what it receives.
    struct Mock {
        out: mpsc::UnboundedSender<Bytes>,
        inbound: Mutex<Option<mpsc::Receiver<Bytes>>>,
    }

    #[async_trait]
    impl Transport for Mock {
        async fn send(&self, msg: Bytes) -> Result<(), TransportError> {
            self.out.send(msg).map_err(|_| TransportError::Closed)
        }
        fn recv(&self) -> mpsc::Receiver<Bytes> {
            lock(&self.inbound)
                .take()
                .unwrap_or_else(|| mpsc::channel(1).1)
        }
    }

    struct Harness {
        cdp: Cdp,
        sent: mpsc::UnboundedReceiver<Bytes>,
        inject: mpsc::Sender<Bytes>,
    }

    fn harness_with(observer: Observer) -> Harness {
        let (out, sent) = mpsc::unbounded_channel();
        let (inject, inbound) = mpsc::channel(64);
        let mock = Arc::new(Mock {
            out,
            inbound: Mutex::new(Some(inbound)),
        });
        Harness {
            cdp: Cdp::with_observer(mock, observer),
            sent,
            inject,
        }
    }

    fn harness() -> Harness {
        harness_with(Box::new(|_, _| {}))
    }

    /// Every wait in these tests is capped, so a routing break fails in 5 s instead of hanging.
    async fn within<F: Future>(what: &str, f: F) -> F::Output {
        tokio::time::timeout(Duration::from_secs(5), f)
            .await
            .unwrap_or_else(|_| panic!("{what} within 5 s"))
    }

    impl Harness {
        async fn next_sent(&mut self) -> Value {
            let raw = tokio::time::timeout(Duration::from_secs(5), self.sent.recv())
                .await
                .expect("a message within 5 s")
                .expect("transport open");
            serde_json::from_slice(&raw).expect("json")
        }
        async fn inject(&self, v: Value) {
            self.inject
                .send(Bytes::from(v.to_string()))
                .await
                .expect("reader alive");
        }
    }

    #[tokio::test]
    async fn sends_pipeline_and_out_of_order_replies_reach_their_callers() {
        let mut h = harness();
        let a = h.cdp.start(None, "A.one", json!({"n": 1}));
        let b = h.cdp.start(None, "B.two", json!({"n": 2}));
        let c = h.cdp.start(Some("S1"), "C.three", json!({"n": 3}));
        // All three are on the wire before any reply exists.
        let mut ids = HashMap::new();
        for _ in 0..3 {
            let m = h.next_sent().await;
            ids.insert(m["method"].as_str().unwrap().to_string(), m["id"].clone());
            if m["method"] == "C.three" {
                assert_eq!(m["sessionId"], "S1");
            } else {
                assert!(m.get("sessionId").is_none());
            }
        }
        for name in ["C.three", "A.one", "B.two"] {
            h.inject(json!({"id": ids[name], "result": {"from": name}}))
                .await;
        }
        assert_eq!(b.await.unwrap(), json!({"from": "B.two"}));
        assert_eq!(c.await.unwrap(), json!({"from": "C.three"}));
        assert_eq!(a.await.unwrap(), json!({"from": "A.one"}));
        assert_eq!(h.cdp.pending(), 0);
    }

    #[tokio::test]
    async fn events_route_to_their_session_and_to_all() {
        let h = harness();
        let mut all = h.cdp.events();
        let mut s1 = h.cdp.session_events("S1");
        let mut s2 = h.cdp.session_events("S2");
        h.inject(json!({"method": "Page.frameNavigated", "params": {"x": 1}, "sessionId": "S1"}))
            .await;
        h.inject(json!({"method": "Target.targetCreated", "params": {"y": 2}}))
            .await;
        let e = within("S1 event", s1.recv()).await.unwrap();
        assert_eq!(e.method, "Page.frameNavigated");
        assert_eq!(e.session.as_deref(), Some("S1"));
        assert_eq!(e.params, json!({"x": 1}));
        let first = within("first event", all.recv()).await.unwrap();
        assert_eq!(first.session.as_deref(), Some("S1"));
        let second = within("second event", all.recv()).await.unwrap();
        assert_eq!(second.method, "Target.targetCreated");
        assert_eq!(second.session, None);
        assert!(matches!(
            s2.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            s1.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        // Detaching S1 closes its broadcast.
        h.inject(json!({"method": "Target.detachedFromTarget", "params": {"sessionId": "S1"}}))
            .await;
        assert!(matches!(
            within("S1 closed", s1.recv()).await,
            Err(broadcast::error::RecvError::Closed)
        ));
    }

    #[tokio::test]
    async fn observer_sees_each_event_before_subscribers() {
        // The observer itself looks at a subscriber of the all-events stream and of the
        // event's session: neither may hold the event yet. Broadcasting first turns this red.
        type Peek = Arc<
            Mutex<
                Option<(
                    broadcast::Receiver<Arc<Event>>,
                    broadcast::Receiver<Arc<Event>>,
                )>,
            >,
        >;
        let peek: Peek = Arc::new(Mutex::new(None));
        let seen = Arc::new(AtomicUsize::new(0));
        let early = Arc::new(AtomicUsize::new(0));
        let (p, s, x) = (peek.clone(), seen.clone(), early.clone());
        let h = harness_with(Box::new(move |e, _| {
            if e.method != "Page.loadEventFired" {
                return;
            }
            s.fetch_add(1, Ordering::SeqCst);
            if let Some((all, session)) = lock(&p).as_mut() {
                for rx in [all, session] {
                    if !matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Empty)) {
                        x.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        }));
        let mut all = h.cdp.events();
        let mut session = h.cdp.session_events("S1");
        *lock(&peek) = Some((h.cdp.events(), h.cdp.session_events("S1")));
        for _ in 0..2 {
            h.inject(json!({"method": "Page.loadEventFired", "params": {}, "sessionId": "S1"}))
                .await;
            let e = within("event", all.recv()).await.unwrap();
            assert_eq!(e.method, "Page.loadEventFired");
            let e = within("session event", session.recv()).await.unwrap();
            assert_eq!(e.method, "Page.loadEventFired");
            // Drain the observer's own receivers so the next round starts empty.
            if let Some((a, b)) = lock(&peek).as_mut() {
                let _ = (a.try_recv(), b.try_recv());
            }
        }
        assert_eq!(seen.load(Ordering::SeqCst), 2);
        assert_eq!(
            early.load(Ordering::SeqCst),
            0,
            "a subscriber had the event before the observer ran"
        );
    }

    #[tokio::test]
    async fn reply_hook_runs_in_wire_order_with_events() {
        // The hook must see the observer's state exactly as of its reply: events before the
        // reply applied, events after it not yet.
        let applied = Arc::new(AtomicUsize::new(0));
        let a = applied.clone();
        let mut h = harness_with(Box::new(move |e, _| {
            if e.method == "Test.tick" {
                a.fetch_add(1, Ordering::SeqCst);
            }
        }));
        let (tx, rx) = oneshot::channel();
        let a = applied.clone();
        assert!(h
            .cdp
            .sender()
            .start_then(Some("S1"), "Page.getFrameTree", json!({}), move |r| {
                let _ = tx.send((r, a.load(Ordering::SeqCst)));
            }));
        let m = within("sent", h.next_sent()).await;
        assert_eq!(m["method"], "Page.getFrameTree");
        assert_eq!(m["sessionId"], "S1");
        assert_eq!(h.cdp.pending(), 1);
        h.inject(json!({"method": "Test.tick", "params": {}})).await;
        h.inject(json!({"id": m["id"], "result": {"frameTree": {}}}))
            .await;
        h.inject(json!({"method": "Test.tick", "params": {}})).await;
        let (r, ticks) = within("hook", rx).await.expect("hook ran");
        assert_eq!(r.unwrap(), json!({"frameTree": {}}));
        assert_eq!(ticks, 1, "hook ran between the two events");
        assert_eq!(h.cdp.pending(), 0);
        // An error reply reaches the hook as a protocol error naming the method.
        let (tx, rx) = oneshot::channel();
        h.cdp
            .sender()
            .start_then(None, "Page.getFrameTree", json!({}), move |r| {
                let _ = tx.send(r);
            });
        let m = within("sent", h.next_sent()).await;
        h.inject(json!({"id": m["id"], "error": {"code": -32000, "message": "gone"}}))
            .await;
        let err = within("hook", rx).await.expect("hook ran").unwrap_err();
        assert!(err.to_string().contains("Page.getFrameTree"), "{err}");
        // After close a hook is refused at once.
        drop(h.inject);
        within("closed", h.cdp.closed()).await;
        assert!(!h.cdp.sender().start_then(None, "X.y", json!({}), |_| {}));
    }

    #[tokio::test(start_paused = true)]
    async fn command_times_out_after_10_s_naming_the_method() {
        let mut h = harness();
        let started = tokio::time::Instant::now();
        let call = h
            .cdp
            .start(None, "Page.navigate", json!({"url": "about:blank"}));
        let _ = h.next_sent().await;
        assert_eq!(h.cdp.pending(), 1);
        let err = call.await.unwrap_err();
        assert_eq!(
            err,
            CdpError::Timeout {
                method: "Page.navigate".into(),
                after: Duration::from_secs(10)
            }
        );
        assert_eq!(started.elapsed(), COMMAND_TIMEOUT);
        assert!(err.to_string().contains("Page.navigate"));
        assert_eq!(h.cdp.pending(), 0);
    }

    #[tokio::test]
    async fn transport_close_fails_every_pending_caller() {
        let mut h = harness();
        let a = h.cdp.start(None, "A.one", json!({}));
        let b = h.cdp.start(Some("S"), "B.two", json!({}));
        let _ = h.next_sent().await;
        let _ = h.next_sent().await;
        drop(h.inject);
        assert_eq!(
            a.await.unwrap_err(),
            CdpError::Closed {
                method: "A.one".into()
            }
        );
        assert_eq!(
            b.await.unwrap_err(),
            CdpError::Closed {
                method: "B.two".into()
            }
        );
        within("closed", h.cdp.closed()).await;
        assert!(h.cdp.is_closed());
        // A send after close fails at once rather than waiting out the timeout.
        let late = tokio::time::timeout(
            Duration::from_secs(1),
            h.cdp.start(None, "C.three", json!({})),
        )
        .await
        .expect("immediate");
        assert_eq!(
            late.unwrap_err(),
            CdpError::Closed {
                method: "C.three".into()
            }
        );
    }

    #[tokio::test]
    async fn error_reply_is_a_typed_protocol_error() {
        let mut h = harness();
        let call = h.cdp.start(None, "DOM.resolveNode", json!({"nodeId": 9}));
        let m = h.next_sent().await;
        h.inject(json!({"id": m["id"], "error": {"code": -32000, "message": "No node with given id found"}}))
            .await;
        assert_eq!(
            call.await.unwrap_err(),
            CdpError::Protocol {
                method: "DOM.resolveNode".into(),
                code: -32000,
                message: "No node with given id found".into()
            }
        );
    }

    #[tokio::test]
    async fn cancelled_caller_entry_is_removed() {
        let mut h = harness();
        let call = h.cdp.start(None, "Runtime.evaluate", json!({}));
        let m = h.next_sent().await;
        assert_eq!(h.cdp.pending(), 1);
        drop(call);
        assert_eq!(h.cdp.pending(), 0);
        // The late reply finds no slot and is dropped; the client keeps working.
        h.inject(json!({"id": m["id"], "result": {}})).await;
        let next = h.cdp.start(None, "Browser.getVersion", json!({}));
        let m2 = h.next_sent().await;
        assert_ne!(m2["id"], m["id"]);
        h.inject(json!({"id": m2["id"], "result": {"product": "x"}}))
            .await;
        assert_eq!(next.await.unwrap(), json!({"product": "x"}));
    }

    #[tokio::test]
    async fn cancelling_a_started_future_mid_wait_removes_the_entry() {
        let mut h = harness();
        let call = h.cdp.start(None, "Page.reload", json!({}));
        let _ = h.next_sent().await;
        let r = tokio::time::timeout(Duration::from_millis(1), call).await;
        assert!(r.is_err());
        assert_eq!(h.cdp.pending(), 0);
    }
}
