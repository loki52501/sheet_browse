//! I-2 MCP wrapper (docs/01 H-6). CONTRACT: change only through `interface_changes`.
//!
//! `serve(server, tools, harness)` is the only way a tool server exposes tools. Per call it:
//! classifies (declared class, raised by [`ToolHandler::classify`]), consults the gate,
//! resolves vault at the sink (via [`ToolCtx::resolve_into`]), scrubs the result, writes
//! the ledger, measures duration, attaches the run id. Tool servers never write the
//! ledger or read the vault directly.
//!
//! D-48: `serve` takes its dependencies as a [`Harness`] (core cannot build a `PgStore`:
//! core-pg depends on core), and [`dispatch`] runs the same pipeline in-process for tests.
//!
//! # Ledger events written here (H-3, D-41; the shapes `ledger::report` reads)
//!
//! * tool event, one per call that ran: `server` = the tool server, `tool`, `class` = the
//!   approved class, `mode` = `allow` | `answered_proceed` | `timeout_proceed`, or `stop`
//!   when the handler returned [`ToolOutput::stopped`] (then `why` = the stop reason),
//!   `args` scrubbed, `result_summary` = summary or the first line of the text (or
//!   `error: ...`), `consequences`, `duration_ms` of the handler call, `ok`, `why` (the error
//!   message when not ok);
//! * ask outcome: `server = core`, `tool`, `class`, `mode` = one of the four outcomes,
//!   `args` scrubbed, `result_summary` = `why` = the card's what line, `duration_ms` = how
//!   long the ask waited, `consequences = {"site", "on_timeout"}`;
//! * stop (gate Stop before the call or before a mid-call ask, a mid-call Challenge, or a
//!   handler's stopped output): `server = core`, `mode = stop`, `why` = the reason,
//!   `consequences = {"element"}`. One per stopped call (D-127, D-121 (4)): a gate Stop
//!   before the call is the call's only event; a handler that returns `stopped` writes the
//!   tool event and then one trailing core stop, which carries the call's first stop (a
//!   mid-call one if any, its element filled from the handler's, else the handler's). A
//!   mid-call stop on a call that did not return `stopped` (it went on, failed or panicked)
//!   is written right before the tool event. A mid-call stop is held until the call
//!   returns; only the first one per call is logged, a later one goes to stderr. Accepted
//!   gap: a process that dies inside the handler after a mid-call stop loses that line
//!   (nothing ran on its behalf: the ask returned `TimeoutSkip`).
//!
//! # Fail closed (rule 4, D-119)
//!
//! Before the handler runs, any error (run lookup, gate, ledger on the ask path) means it
//! does not run. A run with no `runs` row reads as strict. Every `Asker` call is capped at
//! `card.timeout` + [`ASK_CAP_MARGIN`]; a cap hit is `TimeoutSkip`. A handler or
//! classifier panic is caught: the model gets `isError` and the ledger an `ok = false` tool
//! event. One gap is accepted: on the Allow path the tool event is written after the
//! handler ran (the event needs the result), so if the store fails right then the side
//! effect is recorded on stderr only. Probing the store before every allowed call would add
//! a round trip per call (CLAUDE.md priorities), and the ask path already writes first.
//!
//! # Transport (D-104)
//!
//! Newline-delimited JSON-RPC 2.0 on stdin/stdout, protocol `2025-06-18`: `initialize`,
//! `notifications/initialized`, `ping`, `tools/list`, `tools/call`, and the server's
//! `notifications/tools/list_changed` when the registry changes. Nothing but JSON-RPC goes
//! to stdout; diagnostics go to stderr, scrubbed. Outgoing messages scrub their payload
//! strings only (D-119 (6)): the envelope (`id`, `jsonrpc`, `protocolVersion`) is ours and
//! a short vault value must not rewrite it. A line that is not UTF-8 or not JSON answers
//! -32700 and the server keeps reading. An incoming line longer than [`MAX_LINE_BYTES`]
//! (8 MiB, the newline not counted) is not buffered: the bytes past the cap are read and
//! dropped up to the next newline, the line answers -32600 with `id: null`, and the server
//! keeps reading (D-127). -32600 (invalid request), not -32700 (parse error): the line was
//! never parsed, so the server cannot say it is not JSON, only that it will not take it
//! as a request; its id is unknown, so the reply's id is null as JSON-RPC 2.0 requires.

use crate::class::Class;
use crate::gate::{
    clean_card, AskCard, AskOutcome, Asker, Call, Decision, Gate, OnTimeout, StopReason,
    ASK_TIMEOUT,
};
use crate::ledger::Ledger;
use crate::memory::{ts_now, LedgerEvent, LedgerMode, MemoryStore};
use crate::run::{RunFolder, RunId};
use crate::vault::{Resolved, Vault, VaultError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

/// `ledger.events.server` of the events the wrapper writes for the harness itself.
const CORE: &str = "core";

/// MCP protocol revision this wrapper speaks (D-104).
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Cap on `ledger.events.result_summary` when it is taken from the output text.
const SUMMARY_MAX_CHARS: usize = 300;

/// Longest incoming JSON-RPC line `serve_io` accepts, in bytes without the newline. Tool
/// arguments are short text (a form, a mail body, a path); 8 MiB is far above any real
/// call and keeps one runaway line from growing the read buffer without bound.
pub(crate) const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// How long past `card.timeout` the wrapper waits for an `Asker` before it counts the ask
/// as `TimeoutSkip` (an asker that ignores its timer must not hang the call).
pub(crate) const ASK_CAP_MARGIN: Duration = Duration::from_secs(5);

/// One tool a server exposes: the I-2 fields plus `description` (D-48).
#[derive(Clone)]
pub struct ToolSpec {
    /// MCP tool name, e.g. `fetch`, `fs.read`, `luma.register`.
    pub name: String,
    /// MCP tool description shown to the model. One or two lines.
    pub description: String,
    /// Declared class: the floor. Classification may raise it, never lower it.
    pub class: Class,
    /// JSON Schema of the arguments, published as the MCP `inputSchema`.
    pub args: Value,
    pub handler: Arc<dyn ToolHandler>,
}

impl std::fmt::Debug for ToolSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSpec")
            .field("name", &self.name)
            .field("class", &self.class)
            .finish_non_exhaustive()
    }
}

/// What a handler's classifier learned from the arguments, before the call runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Classified {
    /// Raise the class (e.g. `act("c#19")` on a card form → Pay). `None` = declared class.
    /// The wrapper uses `max(declared, this)`; a lower value is ignored.
    pub class: Option<Class>,
    /// eTLD+1 the call touches, for `gate.rules` lookup.
    pub domain: Option<String>,
    /// Send recipients, checked against `memory.contacts`. A Send handler must fill this:
    /// an empty list is treated as Send(new).
    pub recipients: Vec<String>,
    /// Hints such as `job_application`, `signature` (D-39).
    pub hints: Vec<String>,
    /// Ask card line: "submit form f2 on luma.com (Register)".
    pub what: Option<String>,
    /// Ask card values, already scrubbed (vault as `@vault(name)`).
    pub values: Vec<(String, String)>,
}

#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Called before the gate. May read state it needs (the live page index for `act`), but
    /// must have no side effects. Default: nothing to raise. Classification can go stale
    /// between here and the call, so a handler that runs multi-step programs re-checks each
    /// step against `ctx.class` and stops (via `ToolOutput::stopped`) on a higher class.
    async fn classify(&self, ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let _ = (ctx, args);
        Ok(Classified::default())
    }

    /// Runs the tool. `args` still contain `@vault(name)` literals; resolve them only at the
    /// sink with `ctx.resolve_into`. The returned text is scrubbed by the wrapper anyway.
    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError>;
}

/// Tools a server currently exposes. Site tools are added when a page is fetched or opened
/// and removed when the tab goes (T-3, W-7); `serve` sends `notifications/tools/list_changed`.
///
/// A name's class never goes down while the registry lives (rule 4, D-127): every name
/// keeps a floor, the highest class it was ever registered with, which outlives `remove`.
/// Re-registering a name (a repaired or re-forged site tool, a tab opened again) keeps
/// `max(floor, new class)`; the new handler, description and schema replace the old ones.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: Arc<RwLock<Tools>>,
    /// Bumped by every `add` and every `remove` that removed something; `serve_io` watches it.
    version: Arc<watch::Sender<u64>>,
}

impl Default for ToolRegistry {
    fn default() -> ToolRegistry {
        ToolRegistry::new(Vec::new())
    }
}

/// The registered tools and every name's class floor (D-127).
struct Tools {
    specs: Vec<ToolSpec>,
    /// Highest class each name was ever registered with; kept after `remove`.
    floors: HashMap<String, Class>,
}

impl Tools {
    /// Raises `spec.class` to the name's floor and the floor to `spec.class`.
    fn floored(&mut self, mut spec: ToolSpec) -> ToolSpec {
        let floor = self.floors.entry(spec.name.clone()).or_insert(spec.class);
        *floor = (*floor).max(spec.class);
        spec.class = *floor;
        spec
    }
}

impl ToolRegistry {
    /// A name listed twice keeps the higher class for both entries (`get` finds the first).
    pub fn new(tools: Vec<ToolSpec>) -> ToolRegistry {
        let mut t = Tools {
            specs: Vec::new(),
            floors: HashMap::new(),
        };
        for spec in &tools {
            t.floored(spec.clone());
        }
        t.specs = tools.into_iter().map(|s| t.floored(s)).collect();
        ToolRegistry {
            tools: Arc::new(RwLock::new(t)),
            version: Arc::new(watch::channel(0).0),
        }
    }

    /// Adds or replaces by name. The class is never lowered: a spec whose class is below
    /// the name's floor is registered at the floor (rule 4, D-127).
    pub fn add(&self, spec: ToolSpec) {
        {
            let mut t = self.tools.write().unwrap_or_else(|e| e.into_inner());
            let spec = t.floored(spec);
            t.specs.retain(|s| s.name != spec.name);
            t.specs.push(spec);
        }
        self.bump();
    }

    /// Drops the tool; its class floor stays, so adding it back cannot lower the class.
    pub fn remove(&self, name: &str) {
        let removed = {
            let mut t = self.tools.write().unwrap_or_else(|e| e.into_inner());
            let before = t.specs.len();
            t.specs.retain(|s| s.name != name);
            t.specs.len() != before
        };
        if removed {
            self.bump();
        }
    }

    pub fn get(&self, name: &str) -> Option<ToolSpec> {
        let t = self.tools.read().unwrap_or_else(|e| e.into_inner());
        t.specs.iter().find(|s| s.name == name).cloned()
    }

    pub fn list(&self) -> Vec<ToolSpec> {
        self.tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .specs
            .clone()
    }

    fn bump(&self) {
        // send_modify notifies receivers and works with none subscribed.
        self.version.send_modify(|v| *v = v.wrapping_add(1));
    }

    fn changes(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }
}

/// Everything the wrapper needs, built by each server's `main` (store from core-pg) or by a
/// test (a fake store, a scripted asker, an in-memory vault).
#[derive(Clone)]
pub struct Harness {
    pub store: Arc<dyn MemoryStore>,
    pub vault: Arc<Vault>,
    pub asker: Arc<dyn Asker>,
    /// `runs/` root.
    pub runs_root: PathBuf,
    /// The run this server process belongs to (set by the session that launched it).
    pub run: Option<RunId>,
}

/// The call a [`ToolCtx`] belongs to, for the events a mid-call ask writes.
#[derive(Default)]
struct CallInfo {
    server: String,
    tool: String,
    /// Scrubbed.
    args: Value,
    /// The classifier's hints: a mid-call ask meets the same hint rules as its call.
    hints: Vec<String>,
    /// The classifier's domain, scrubbed: a mid-call card with no site is checked against
    /// the call's own site, so its domain rules (a stop rule, a strict ask) still apply.
    domain: Option<String>,
}

/// Per-call context handed to a handler. Holds the vault privately: a handler can resolve
/// into a sink but cannot read a value.
pub struct ToolCtx {
    pub run_id: RunId,
    /// `runs/<id>/`; deliverables go in `files/`, frames in `frames/`.
    pub run_dir: PathBuf,
    /// During `classify`: the declared class. During `call`: the class the gate approved.
    pub class: Class,
    store: Arc<dyn MemoryStore>,
    vault: Arc<Vault>,
    asker: Arc<dyn Asker>,
    registry: ToolRegistry,
    ledger: Arc<Ledger>,
    call: CallInfo,
    /// Set while the handler's `call` runs: a mid-call stop is held, not written, until the
    /// call returns, so a stopped call writes one core stop (D-127, D-121 (4)).
    hold_stops: bool,
    /// The first stop a mid-call ask hit while `hold_stops` was set.
    held_stop: Mutex<Option<StopReason>>,
}

impl ToolCtx {
    pub fn new(
        run_id: RunId,
        run_dir: PathBuf,
        class: Class,
        harness: &Harness,
        registry: ToolRegistry,
    ) -> ToolCtx {
        let ledger = shared_ledger(
            &harness.store,
            RunFolder {
                id: run_id.clone(),
                dir: run_dir.clone(),
            },
        );
        ToolCtx {
            run_id,
            run_dir,
            class,
            store: harness.store.clone(),
            vault: harness.vault.clone(),
            asker: harness.asker.clone(),
            registry,
            ledger,
            call: CallInfo::default(),
            hold_stops: false,
            held_stop: Mutex::new(None),
        }
    }

    /// See [`Vault::resolve_into`].
    pub fn resolve_into<R>(
        &self,
        template: &str,
        sink: impl FnOnce(&Resolved<'_>) -> R,
    ) -> Result<R, VaultError> {
        self.vault.resolve_into(template, sink)
    }

    /// Memory for tool-store health (`toolstore::record`) and contacts (`touch_contact`
    /// after a send). Never `append`: the wrapper owns the ledger (H-6).
    pub fn store(&self) -> &Arc<dyn MemoryStore> {
        &self.store
    }

    /// An ask that arises mid-call (download about to land, dialog, a step whose live class
    /// exceeds `self.class`). The wrapper logs the card and outcome as `server = core` events.
    ///
    /// The card is sanitised like a gate card (scrubbed, [`clean_card`], scrubbed again), the
    /// timer is [`crate::gate::ASK_TIMEOUT`], and the card's class and site go through
    /// [`Gate::check`] with the call's hints and the run's `strict` (D-119 (1), rule 4); a
    /// card with no site is checked against the call's classified domain (D-123):
    ///
    /// * a `Challenge` card or a gate `Stop` is never shown: it is logged as a core stop and
    ///   returns `TimeoutSkip` (D-5; the handler should return `ToolOutput::stopped`). The
    ///   stop is written when the call returns, one core stop per call (D-127);
    /// * otherwise `on_timeout` can only be tightened: the card proceeds on timeout only if
    ///   the handler's card, the gate's card (a strict or hint rule makes it skip) and
    ///   [`Gate::on_timeout`] for the class and the run's `strict` all proceed. A gate
    ///   error makes it skip.
    ///
    /// The asker is capped at the card's timer plus a margin (then `TimeoutSkip`). If the
    /// outcome cannot be written to the ledger, a proceed becomes a skip: nothing runs that
    /// the ledger does not show.
    pub async fn ask(&self, card: AskCard) -> AskOutcome {
        if card.class == Class::Challenge {
            let why = clean_line(
                &self.vault,
                &format!("challenge during {}: {}", self.call.tool, card.what),
            );
            self.mid_call_stop(StopReason {
                class: Class::Challenge,
                why,
                element: None,
            })
            .await;
            return AskOutcome::TimeoutSkip;
        }
        let strict = run_strict(&self.store, &self.run_id).await;
        let mut card = sanitise_card(&self.vault, card);

        // Rule 4: the escalation is a side effect like any call, so the gate sees it.
        let call = Call {
            run_id: Some(self.run_id.clone()),
            server: self.call.server.clone(),
            tool: self.call.tool.clone(),
            class: card.class,
            domain: card.site.clone().or_else(|| self.call.domain.clone()),
            recipients: Vec::new(),
            hints: self.call.hints.clone(),
            args: self.call.args.clone(),
            what: Some(card.what.clone()),
            values: card.values.clone(),
            strict,
        };
        let gate_timeout = match Gate::new(self.store.clone()).check(&call).await {
            Ok(Decision::Stop(reason)) => {
                self.mid_call_stop(clean_stop(&self.vault, reason)).await;
                return AskOutcome::TimeoutSkip;
            }
            Ok(Decision::Ask(g)) => g.on_timeout,
            Ok(Decision::Allow) => OnTimeout::Proceed,
            Err(err) => {
                diag(&self.vault, &format!("gate (mid-call ask): {err}"));
                OnTimeout::Skip
            }
        };
        card.on_timeout = tighter(
            card.on_timeout,
            tighter(gate_timeout, Gate::on_timeout(card.class, strict)),
        );

        let started = Instant::now();
        let outcome = ask_capped(&*self.asker, &card, &self.vault).await;
        let e = self.outcome_event(&card, outcome, started);
        match self.ledger.write(e).await {
            Ok(_) => outcome,
            Err(err) => {
                diag(&self.vault, &format!("ledger write failed (ask): {err}"));
                match outcome {
                    AskOutcome::AnsweredProceed => AskOutcome::AnsweredSkip,
                    AskOutcome::TimeoutProceed => AskOutcome::TimeoutSkip,
                    other => other,
                }
            }
        }
    }

    /// Register or drop site tools for this server (T-3).
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    fn event(&self, server: &str, class: Option<Class>, mode: Option<LedgerMode>) -> LedgerEvent {
        LedgerEvent {
            run_id: Some(self.run_id.clone()),
            ts: ts_now(),
            server: server.to_string(),
            tool: self.call.tool.clone(),
            class,
            mode,
            args: self.call.args.clone(),
            result_summary: String::new(),
            consequences: Value::Null,
            duration_ms: 0,
            ok: true,
            why: None,
        }
    }

    /// `server = core`, mode = the outcome, why = the card's what line.
    fn outcome_event(&self, card: &AskCard, outcome: AskOutcome, started: Instant) -> LedgerEvent {
        let mut e = self.event(CORE, Some(card.class), Some(outcome_mode(outcome)));
        e.result_summary = card.what.clone();
        e.why = Some(card.what.clone());
        e.duration_ms = millis(started);
        e.consequences = json!({
            "site": card.site,
            "on_timeout": card.on_timeout,
        });
        e
    }

    fn stop_event(&self, reason: &StopReason) -> LedgerEvent {
        let mut e = self.event(CORE, Some(reason.class), Some(LedgerMode::Stop));
        let why = self.vault.scrub(&reason.why);
        e.result_summary = why.clone();
        e.why = Some(why);
        e.consequences = json!({
            "element": reason.element.as_deref().map(|el| self.vault.scrub(el)),
        });
        e
    }

    /// A core stop event; a ledger failure goes to stderr (the caller skips anyway).
    async fn write_stop(&self, reason: &StopReason) {
        if let Err(err) = self.ledger.write(self.stop_event(reason)).await {
            diag(&self.vault, &format!("ledger write failed (stop): {err}"));
        }
    }

    /// A stop a mid-call ask hit (`reason` already clean). While the handler runs it is held
    /// for [`dispatch`] to write when the call returns; only the call's first stop is kept
    /// (a later one goes to stderr). Outside the handler (an ask from `classify`) it is
    /// written at once.
    async fn mid_call_stop(&self, reason: StopReason) {
        if !self.hold_stops {
            self.write_stop(&reason).await;
            return;
        }
        let mut held = self.held_stop.lock().unwrap_or_else(|e| e.into_inner());
        if held.is_none() {
            *held = Some(reason);
        } else {
            drop(held);
            diag(
                &self.vault,
                &format!(
                    "second stop in one call to {}, not logged: {}",
                    self.call.tool, reason.why
                ),
            );
        }
    }

    /// Takes the held mid-call stop (see [`ToolCtx::mid_call_stop`]).
    fn take_held_stop(&self) -> Option<StopReason> {
        self.held_stop
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

/// `runs.strict` of the run. A missing `runs` row or a failed lookup reads as strict: the
/// safe reading when strictness is unknown (D-119 (2)).
async fn run_strict(store: &Arc<dyn MemoryStore>, run: &RunId) -> bool {
    match store.run_get(run).await {
        Ok(Some(r)) => r.strict,
        Ok(None) | Err(_) => true,
    }
}

/// Skip if either says skip.
fn tighter(a: OnTimeout, b: OnTimeout) -> OnTimeout {
    match (a, b) {
        (OnTimeout::Proceed, OnTimeout::Proceed) => OnTimeout::Proceed,
        _ => OnTimeout::Skip,
    }
}

/// Asks with a cap of `card.timeout` + [`ASK_CAP_MARGIN`]. A cap hit, or a `TimeoutProceed`
/// on a card that says skip, is `TimeoutSkip`: an asker cannot loosen the card.
async fn ask_capped(asker: &dyn Asker, card: &AskCard, vault: &Vault) -> AskOutcome {
    ask_capped_with(asker, card, vault, ASK_CAP_MARGIN).await
}

async fn ask_capped_with(
    asker: &dyn Asker,
    card: &AskCard,
    vault: &Vault,
    margin: Duration,
) -> AskOutcome {
    let cap = card.timeout.saturating_add(margin);
    match tokio::time::timeout(cap, asker.ask(card)).await {
        Ok(AskOutcome::TimeoutProceed) if card.on_timeout == OnTimeout::Skip => {
            AskOutcome::TimeoutSkip
        }
        Ok(o) => o,
        Err(_) => {
            diag(
                vault,
                &format!("ask: no outcome within {}s; skipped", cap.as_secs()),
            );
            AskOutcome::TimeoutSkip
        }
    }
}

/// A mid-call card through the one card sanitiser (D-70): scrubbed first so cleaning
/// cannot cut or reshape a vault value past the scrubber, then [`clean_card`], then
/// scrubbed again. The timer is always [`ASK_TIMEOUT`].
fn sanitise_card(vault: &Vault, card: AskCard) -> AskCard {
    let scrub = |s: &str| vault.scrub(s);
    let class = card.class;
    let on_timeout = card.on_timeout;
    let cleaned = clean_card(AskCard {
        class,
        what: scrub(&card.what),
        site: card.site.as_deref().map(scrub),
        values: card
            .values
            .iter()
            .map(|(k, v)| (scrub(k), scrub(v)))
            .collect(),
        timeout: ASK_TIMEOUT,
        on_timeout,
    });
    let what = scrub(&cleaned.what);
    AskCard {
        class,
        what: if what.trim().is_empty() {
            format!("{class} step needs your answer")
        } else {
            what
        },
        site: cleaned.site.as_deref().map(scrub),
        values: cleaned
            .values
            .iter()
            .map(|(k, v)| (scrub(k), scrub(v)))
            .collect(),
        timeout: ASK_TIMEOUT,
        on_timeout,
    }
}

/// One clean line for stderr or a ledger `why` (D-119 (7)): scrubbed, cleaned by
/// [`clean_card`] (one line, controls stripped, capped), scrubbed again.
pub(crate) fn clean_line(vault: &Vault, s: &str) -> String {
    let cleaned = clean_card(AskCard {
        class: Class::Read,
        what: vault.scrub(s),
        site: None,
        values: Vec::new(),
        timeout: ASK_TIMEOUT,
        on_timeout: OnTimeout::Skip,
    });
    vault.scrub(&cleaned.what)
}

/// One `Ledger` per (store, run folder) while any call holds it, so concurrent calls in one
/// process share the ordering lock and append their jsonl lines in id order.
fn shared_ledger(store: &Arc<dyn MemoryStore>, run: RunFolder) -> Arc<Ledger> {
    type Key = (usize, PathBuf);
    static LEDGERS: OnceLock<Mutex<HashMap<Key, Weak<Ledger>>>> = OnceLock::new();
    let key = (Arc::as_ptr(store) as *const () as usize, run.dir.clone());
    let mut map = LEDGERS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(l) = map.get(&key).and_then(Weak::upgrade) {
        if l.run().id == run.id {
            return l;
        }
    }
    map.retain(|_, w| w.strong_count() > 0);
    let ledger = Arc::new(Ledger::new(store.clone(), run));
    map.insert(key, Arc::downgrade(&ledger));
    ledger
}

/// A tool's result before scrubbing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// What the model reads. Short text (rule 5: no raw HTML, no pixels).
    pub text: String,
    /// One line for `ledger.events.result_summary`; defaults to the first line of `text`.
    pub summary: Option<String>,
    /// `ledger.events.consequences` (navigated, opened tab, download, file written...).
    pub consequences: Value,
    /// Files produced under the run folder.
    pub files: Vec<PathBuf>,
    /// Set when the call stopped part-way (Challenge, H-1.4): `text` is the partial result
    /// and the wrapper writes a `server = core, mode = stop` event with the element.
    pub stopped: Option<StopReason>,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("bad arguments: {0}")]
    Args(String),
    #[error(transparent)]
    Vault(#[from] VaultError),
    /// Anything else; the message is scrubbed before it reaches the model or the ledger.
    #[error("{0}")]
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("config: {0}")]
    Config(String),
    #[error("transport: {0}")]
    Transport(String),
}

/// I-2. Serves `tools` over MCP stdio as `server` until stdin closes.
pub async fn serve(server: &str, tools: Vec<ToolSpec>, harness: Harness) -> Result<(), ServeError> {
    let reader = tokio::io::BufReader::new(tokio::io::stdin());
    serve_io(
        server,
        ToolRegistry::new(tools),
        harness,
        reader,
        tokio::io::stdout(),
    )
    .await
}

/// [`serve`] over any reader and writer (tests drive it over `tokio::io::duplex`). Returns
/// when `reader` reaches EOF and every call in flight has been answered.
pub async fn serve_io<R, W>(
    server: &str,
    registry: ToolRegistry,
    harness: Harness,
    reader: R,
    mut writer: W,
) -> Result<(), ServeError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let server: Arc<str> = Arc::from(server);
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let mut tx = Some(tx);
    let mut changes = registry.changes();
    changes.borrow_and_update();
    let mut reader = reader;
    // Raw bytes, so one bad line is a -32700 instead of a dead server. `read_line_capped` is
    // cancel safe: bytes read before another branch wins stay in `buf` (and `overlong`).
    let mut buf: Vec<u8> = Vec::new();
    let mut overlong = false;
    let mut initialized = false;
    let mut watching = true;

    loop {
        tokio::select! {
            read = read_line_capped(&mut reader, &mut buf, &mut overlong, MAX_LINE_BYTES), if tx.is_some() => match read {
                Ok(read) => {
                    let reply = match read {
                        // EOF: stop reading; answer the calls in flight, then return.
                        LineRead::Eof => {
                            tx = None;
                            None
                        }
                        LineRead::TooLong => Some(rpc_error(
                            Value::Null,
                            -32600,
                            "invalid request: line longer than 8 MiB",
                        )),
                        LineRead::Line => {
                            let raw = std::mem::take(&mut buf);
                            match (tx.as_ref(), std::str::from_utf8(&raw)) {
                                (None, _) => None,
                                (Some(sender), Ok(line)) => handle_line(&server, &registry, &harness, line, sender, &mut initialized),
                                (Some(_), Err(_)) => Some(rpc_error(Value::Null, -32700, "parse error: not UTF-8")),
                            }
                        }
                    };
                    if let Some(reply) = reply {
                        write_message(&mut writer, &harness.vault, reply).await?;
                    }
                }
                Err(e) => return Err(ServeError::Transport(clean_line(&harness.vault, &e.to_string()))),
            },
            msg = rx.recv() => match msg {
                Some(msg) => write_message(&mut writer, &harness.vault, msg).await?,
                // Every sender is gone: input closed and no call in flight.
                None => break,
            },
            changed = changes.changed(), if watching => match changed {
                Ok(()) => {
                    changes.borrow_and_update();
                    if initialized {
                        let note = json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/tools/list_changed",
                        });
                        write_message(&mut writer, &harness.vault, note).await?;
                    }
                }
                Err(_) => watching = false,
            },
        }
    }
    Ok(())
}

/// What [`read_line_capped`] found.
#[derive(Debug, PartialEq, Eq)]
enum LineRead {
    /// `buf` holds one line, without its newline (the last line may lack one).
    Line,
    /// A line over the cap ended (newline or EOF); its bytes were dropped, `buf` is empty.
    TooLong,
    /// End of input with nothing pending.
    Eof,
}

/// Reads one `\n`-terminated line into `buf` (cleared by the caller between lines), never
/// holding more than `cap` bytes: once a line passes `cap`, `buf` is freed, `*overlong` is
/// set and the rest of the line is consumed and dropped. Cancel safe: all state lives in
/// `buf`, `*overlong` and the reader's buffer, and each chunk is copied and consumed with no
/// await in between, so a future dropped at its await point loses nothing.
async fn read_line_capped<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    overlong: &mut bool,
    cap: usize,
) -> std::io::Result<LineRead> {
    loop {
        let avail = reader.fill_buf().await?;
        if avail.is_empty() {
            if std::mem::take(overlong) {
                return Ok(LineRead::TooLong);
            }
            return Ok(if buf.is_empty() {
                LineRead::Eof
            } else {
                LineRead::Line
            });
        }
        let (take, used, done) = match avail.iter().position(|&b| b == b'\n') {
            Some(i) => (i, i + 1, true),
            None => (avail.len(), avail.len(), false),
        };
        if !*overlong {
            if buf.len() + take > cap {
                *overlong = true;
                *buf = Vec::new();
            } else {
                buf.extend_from_slice(&avail[..take]);
            }
        }
        reader.consume(used);
        if done {
            if std::mem::take(overlong) {
                return Ok(LineRead::TooLong);
            }
            return Ok(LineRead::Line);
        }
    }
}

/// One whole line with its payload scrubbed ([`scrub_payload`]), then flushed.
async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    vault: &Vault,
    msg: Value,
) -> Result<(), ServeError> {
    let msg = scrub_payload(vault, msg);
    let mut line = serde_json::to_vec(&msg).map_err(|e| ServeError::Transport(e.to_string()))?;
    line.push(b'\n');
    let io = |e: std::io::Error| ServeError::Transport(clean_line(vault, &e.to_string()));
    writer.write_all(&line).await.map_err(io)?;
    writer.flush().await.map_err(io)
}

/// Scrubs what carries outside text (D-119 (6)): `result.content[*].text`, each
/// `result.tools[*]` name, description and inputSchema, and `error.message`. The envelope
/// (`jsonrpc`, `id`, `method`, `protocolVersion`, capability keys) is the server's own and is
/// left alone, so a short vault value (a PIN `2025`) cannot rewrite the id or the handshake.
fn scrub_payload(vault: &Vault, mut msg: Value) -> Value {
    if let Some(result) = msg.get_mut("result") {
        if let Some(content) = result.get_mut("content").and_then(Value::as_array_mut) {
            for item in content {
                if let Some(t) = item.get_mut("text") {
                    *t = scrub_value(vault, t);
                }
            }
        }
        if let Some(tools) = result.get_mut("tools").and_then(Value::as_array_mut) {
            for tool in tools {
                for key in ["name", "description", "inputSchema"] {
                    if let Some(v) = tool.get_mut(key) {
                        *v = scrub_value(vault, v);
                    }
                }
            }
        }
    }
    if let Some(m) = msg.get_mut("error").and_then(|e| e.get_mut("message")) {
        *m = scrub_value(vault, m);
    }
    msg
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Handles one input line. Returns the immediate reply, if any; `tools/call` is spawned and
/// answers through `tx`.
fn handle_line(
    server: &Arc<str>,
    registry: &ToolRegistry,
    harness: &Harness,
    line: &str,
    tx: &mpsc::UnboundedSender<Value>,
    initialized: &mut bool,
) -> Option<Value> {
    if line.trim().is_empty() {
        return None;
    }
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Some(rpc_error(Value::Null, -32700, "parse error")),
    };
    let Some(obj) = msg.as_object() else {
        return Some(rpc_error(Value::Null, -32600, "invalid request"));
    };
    let id = obj.get("id").cloned();
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        // A response to something we sent (we send no requests): nothing to do.
        if obj.contains_key("result") || obj.contains_key("error") {
            return None;
        }
        return Some(rpc_error(
            id.unwrap_or(Value::Null),
            -32600,
            "invalid request",
        ));
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = id else {
        // A notification: never answered.
        if method == "notifications/initialized" {
            *initialized = true;
        }
        return None;
    };
    if !(id.is_string() || id.is_number()) {
        return Some(rpc_error(Value::Null, -32600, "invalid request id"));
    }
    match method {
        "initialize" => Some(rpc_result(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": true}},
                "serverInfo": {"name": &**server, "version": env!("CARGO_PKG_VERSION")},
            }),
        )),
        "ping" => Some(rpc_result(id, json!({}))),
        "tools/list" => {
            let tools: Vec<Value> = registry
                .list()
                .into_iter()
                .map(|s| {
                    json!({
                        "name": s.name,
                        "description": s.description,
                        "inputSchema": s.args,
                    })
                })
                .collect();
            Some(rpc_result(id, json!({ "tools": tools })))
        }
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(rpc_error(id, -32602, "tools/call needs params.name"));
            };
            let args = match params.get("arguments") {
                None | Some(Value::Null) => json!({}),
                Some(a @ Value::Object(_)) => a.clone(),
                Some(_) => {
                    return Some(rpc_error(id, -32602, "params.arguments must be an object"))
                }
            };
            if registry.get(name).is_none() {
                let msg = harness.vault.scrub(&format!("unknown tool: {name}"));
                return Some(rpc_error(id, -32602, &msg));
            }
            let (server, registry, harness, tx) = (
                server.clone(),
                registry.clone(),
                harness.clone(),
                tx.clone(),
            );
            let name = name.to_string();
            tokio::spawn(async move {
                let vault = harness.vault.clone();
                // Backstop: dispatch already catches handler panics; a panic anywhere else in
                // the pipeline still becomes an error reply, not a lost request.
                let call = tokio::spawn(async move {
                    dispatch(&server, &registry, &harness, &name, args).await
                });
                let result = match call.await {
                    Ok(Ok(out)) => json!({
                        "content": [{"type": "text", "text": model_text(&out)}],
                        "isError": false,
                    }),
                    Ok(Err(e)) => json!({
                        "content": [{"type": "text", "text": vault.scrub(&e.to_string())}],
                        "isError": true,
                    }),
                    Err(_) => json!({
                        "content": [{"type": "text", "text": PANICKED}],
                        "isError": true,
                    }),
                };
                let _ = tx.send(rpc_result(id, result));
            });
            None
        }
        _ => Some(rpc_error(id, -32601, "method not found")),
    }
}

/// The text the model reads: the output, plus the H-1.4 stop line when it stopped.
fn model_text(out: &ToolOutput) -> String {
    match &out.stopped {
        None => out.text.clone(),
        Some(r) => {
            let at = r
                .element
                .as_deref()
                .map(|e| format!(" at {e}"))
                .unwrap_or_default();
            let stop = format!(
                "stopped: {}{at}: {}",
                r.class.as_str().to_ascii_lowercase(),
                r.why
            );
            if out.text.is_empty() {
                stop
            } else {
                format!("{}\n{stop}", out.text)
            }
        }
    }
}

/// The H-6 pipeline for one call without a transport: classify, gate (asking through
/// `harness.asker`), run, scrub, ledger, duration. `serve` uses it per request; tests call it
/// directly (T-09: echo.send with a known/unknown contact, forced Pay, vault canary).
pub async fn dispatch(
    server: &str,
    registry: &ToolRegistry,
    harness: &Harness,
    tool: &str,
    args: Value,
) -> Result<ToolOutput, ToolError> {
    let vault = &harness.vault;
    let Some(spec) = registry.get(tool) else {
        return Err(ToolError::Args(
            vault.scrub(&format!("unknown tool {tool:?}")),
        ));
    };
    let Some(run_id) = harness.run.clone() else {
        return Err(ToolError::Failed("no run".into()));
    };
    let folder = RunFolder::open(&harness.runs_root, &run_id)
        .map_err(|e| ToolError::Failed(vault.scrub(&format!("run folder: {e}"))))?;
    let mut ctx = ToolCtx::new(
        run_id.clone(),
        folder.dir.clone(),
        spec.class,
        harness,
        registry.clone(),
    );
    let args_scrubbed = scrub_value(vault, &args);
    ctx.call = CallInfo {
        server: server.to_string(),
        tool: spec.name.clone(),
        args: args_scrubbed.clone(),
        hints: Vec::new(),
        domain: None,
    };

    // Classify: the declared class is the floor (rule 4, H-1.5). A panic is an error here.
    let classified = match CatchUnwind(spec.handler.classify(&ctx, &args)).await {
        Ok(Ok(c)) => c,
        res => {
            let err = match res {
                Ok(Err(err)) => scrub_error(vault, err),
                _ => ToolError::Failed(PANICKED.into()),
            };
            let mut e = ctx.event(server, Some(spec.class), None);
            e.ok = false;
            e.result_summary = clip(&format!("error: {}", first_line(&err.to_string())));
            e.why = Some(format!("classify: {err}"));
            write_or_diag(&ctx, e).await;
            return Err(err);
        }
    };
    let class = classified.class.map_or(spec.class, |c| c.max(spec.class));
    ctx.call.hints = classified.hints.clone();

    // A missing `runs` row reads as strict (D-119 (2)); a failed lookup runs nothing.
    let strict = harness
        .store
        .run_get(&run_id)
        .await
        .map_err(|e| ToolError::Failed(clean_line(vault, &format!("run lookup: {e}"))))?
        .is_none_or(|r| r.strict);
    let scrub = |s: &str| vault.scrub(s);
    let call = Call {
        run_id: Some(run_id.clone()),
        server: server.to_string(),
        tool: spec.name.clone(),
        class,
        domain: classified.domain.as_deref().map(scrub),
        recipients: classified.recipients.iter().map(|r| scrub(r)).collect(),
        hints: classified.hints.clone(),
        args: args_scrubbed.clone(),
        what: classified.what.as_deref().map(scrub),
        values: classified
            .values
            .iter()
            .map(|(k, v)| (scrub(k), scrub(v)))
            .collect(),
        strict,
    };
    ctx.call.domain = call.domain.clone();
    let decision = Gate::new(harness.store.clone())
        .check(&call)
        .await
        .map_err(|e| ToolError::Failed(clean_line(vault, &format!("gate: {e}"))))?;

    let mode = match decision {
        Decision::Allow => LedgerMode::Allow,
        Decision::Ask(card) => {
            let started = Instant::now();
            let outcome = ask_capped(&*harness.asker, &card, vault).await;
            let e = ctx.outcome_event(&card, outcome, started);
            // On the ask path nothing runs that the ledger does not show.
            ctx.ledger
                .write(e)
                .await
                .map_err(|e| ToolError::Failed(clean_line(vault, &format!("ledger: {e}"))))?;
            if !outcome.proceeds() {
                let reason = match outcome {
                    AskOutcome::AnsweredSkip => "you said no",
                    _ => "no answer before the ask timed out",
                };
                return Ok(ToolOutput {
                    text: vault.scrub(&format!("skipped: {reason} ({})", card.what)),
                    summary: Some(vault.scrub(&format!("skipped: {reason}"))),
                    consequences: Value::Null,
                    files: Vec::new(),
                    stopped: None,
                });
            }
            outcome_mode(outcome)
        }
        Decision::Stop(reason) => {
            let reason = clean_stop(vault, reason);
            ctx.ledger
                .write(ctx.stop_event(&reason))
                .await
                .map_err(|e| ToolError::Failed(clean_line(vault, &format!("ledger: {e}"))))?;
            return Ok(ToolOutput {
                text: model_text(&ToolOutput {
                    stopped: Some(reason.clone()),
                    ..ToolOutput::default()
                }),
                summary: Some(reason.why.clone()),
                consequences: Value::Null,
                files: Vec::new(),
                stopped: Some(reason),
            });
        }
    };

    // Run at the approved class.
    ctx.class = class;
    ctx.hold_stops = true;
    let started = Instant::now();
    // A panic may come after the side effect: it is a failed call the ledger shows.
    let result = CatchUnwind(spec.handler.call(&ctx, args))
        .await
        .unwrap_or_else(|_| Err(ToolError::Failed(PANICKED.into())));
    let duration_ms = millis(started);
    // One core stop per call (D-127, D-121 (4)): a mid-call stop is written before the tool
    // event, or, when the handler returned `stopped`, as that call's trailing stop.
    let held = ctx.take_held_stop();

    let mut e = ctx.event(server, Some(class), Some(mode));
    e.duration_ms = duration_ms;
    match result {
        Ok(out) => {
            let out = scrub_output(vault, out);
            e.result_summary = match &out.summary {
                Some(s) if !s.trim().is_empty() => s.clone(),
                _ => clip(first_line(&out.text)),
            };
            e.consequences = out.consequences.clone();
            if !out.files.is_empty() {
                let files: Vec<String> = out
                    .files
                    .iter()
                    .map(|f| f.to_string_lossy().into_owned())
                    .collect();
                e.consequences = match e.consequences {
                    Value::Null => json!({ "files": files }),
                    Value::Object(mut o) => {
                        o.entry("files").or_insert_with(|| json!(files));
                        Value::Object(o)
                    }
                    other => json!({ "result": other, "files": files }),
                };
            }
            match &out.stopped {
                Some(r) => {
                    // H-1.4: a stopped call is not a completed action.
                    e.mode = Some(LedgerMode::Stop);
                    e.why = Some(r.why.clone());
                    write_or_diag(&ctx, e).await;
                    // The trailing core stop shows the call's first stop: the mid-call one
                    // when there was one (its element filled from the handler's).
                    let trailing = match held {
                        Some(h) => StopReason {
                            element: h.element.or_else(|| r.element.clone()),
                            ..h
                        },
                        None => r.clone(),
                    };
                    write_or_diag(&ctx, ctx.stop_event(&trailing)).await;
                }
                None => {
                    if let Some(h) = &held {
                        write_or_diag(&ctx, ctx.stop_event(h)).await;
                    }
                    write_or_diag(&ctx, e).await;
                }
            }
            Ok(out)
        }
        Err(err) => {
            let err = scrub_error(vault, err);
            e.ok = false;
            e.result_summary = clip(&format!("error: {}", first_line(&err.to_string())));
            e.why = Some(err.to_string());
            if let Some(h) = &held {
                write_or_diag(&ctx, ctx.stop_event(h)).await;
            }
            write_or_diag(&ctx, e).await;
            Err(err)
        }
    }
}

/// After the tool ran, a ledger failure must not turn a done side effect into an error the
/// model would retry: it is reported on stderr instead.
async fn write_or_diag(ctx: &ToolCtx, e: LedgerEvent) {
    if let Err(err) = ctx.ledger.write(e).await {
        diag(&ctx.vault, &format!("ledger write failed: {err}"));
    }
}

/// One scrubbed line on stderr (stdout carries only JSON-RPC).
fn diag(vault: &Vault, msg: &str) {
    eprintln!("{}", clean_line(vault, msg));
}

fn outcome_mode(o: AskOutcome) -> LedgerMode {
    match o {
        AskOutcome::AnsweredProceed => LedgerMode::AnsweredProceed,
        AskOutcome::AnsweredSkip => LedgerMode::AnsweredSkip,
        AskOutcome::TimeoutProceed => LedgerMode::TimeoutProceed,
        AskOutcome::TimeoutSkip => LedgerMode::TimeoutSkip,
    }
}

fn millis(started: Instant) -> i32 {
    i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX)
}

fn first_line(s: &str) -> &str {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

fn clip(s: &str) -> String {
    if s.chars().count() <= SUMMARY_MAX_CHARS {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(SUMMARY_MAX_CHARS - 3).collect();
        out.push_str("...");
        out
    }
}

/// Every string, object key and number in `v` through `vault.scrub` (D-102). A number that
/// spells a vault value becomes the string `@vault(name)`.
pub fn scrub_value(vault: &Vault, v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(vault.scrub(s)),
        Value::Number(n) => {
            let s = n.to_string();
            let t = vault.scrub(&s);
            if t == s {
                v.clone()
            } else {
                Value::String(t)
            }
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| scrub_value(vault, x)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (vault.scrub(k), scrub_value(vault, x)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A stop reason (the gate's, or a handler's `stopped`) as one clean line each for `why`
/// and `element` (D-119 (7)): [`clean_line`] scrubs, cleans and scrubs again, so no control
/// sequence or line break reaches the ledger `why`, the returned `stopped` or the model's
/// `stopped: ...` line.
fn clean_stop(vault: &Vault, r: StopReason) -> StopReason {
    StopReason {
        class: r.class,
        why: clean_line(vault, &r.why),
        element: r.element.map(|e| clean_line(vault, &e)),
    }
}

fn scrub_output(vault: &Vault, out: ToolOutput) -> ToolOutput {
    ToolOutput {
        text: vault.scrub(&out.text),
        summary: out.summary.map(|s| vault.scrub(&s)),
        consequences: scrub_value(vault, &out.consequences),
        files: out
            .files
            .into_iter()
            .map(|f| PathBuf::from(vault.scrub(&f.to_string_lossy())))
            .collect(),
        stopped: out.stopped.map(|r| clean_stop(vault, r)),
    }
}

fn scrub_error(vault: &Vault, e: ToolError) -> ToolError {
    match e {
        ToolError::Args(m) => ToolError::Args(vault.scrub(&m)),
        ToolError::Failed(m) => ToolError::Failed(vault.scrub(&m)),
        // VaultError messages carry names and offsets, never values; an Io one can carry a
        // path, so it is flattened and scrubbed.
        ToolError::Vault(VaultError::Io(io)) => {
            ToolError::Failed(vault.scrub(&format!("vault file: {io}")))
        }
        other @ ToolError::Vault(_) => other,
    }
}

/// The text a caught panic leaves: the payload is never shown (it could carry a value).
const PANICKED: &str = "the tool failed unexpectedly (panic); see the server log";

/// Polls a handler future and turns a panic into `Err` (`futures::FutureExt::catch_unwind`
/// without the dependency). The future is not polled again after it panicked.
struct CatchUnwind<'a, T>(Pin<Box<dyn Future<Output = T> + Send + 'a>>);

impl<T> Future for CatchUnwind<'_, T> {
    type Output = std::thread::Result<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let fut = self.0.as_mut();
        match std::panic::catch_unwind(AssertUnwindSafe(move || fut.poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(v)) => Poll::Ready(Ok(v)),
            Err(payload) => Poll::Ready(Err(payload)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Nop;
    #[async_trait]
    impl ToolHandler for Nop {
        async fn call(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::default())
        }
    }

    fn spec(name: &str, class: Class) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: String::new(),
            class,
            args: serde_json::json!({"type": "object"}),
            handler: Arc::new(Nop),
        }
    }

    #[test]
    fn registry_adds_replaces_removes() {
        let r = ToolRegistry::new(vec![spec("fetch", Class::Read)]);
        r.add(spec("luma.register", Class::Submit));
        r.add(spec("luma.register", Class::Pay));
        assert_eq!(r.list().len(), 2);
        assert_eq!(r.get("luma.register").unwrap().class, Class::Pay);
        r.remove("luma.register");
        assert!(r.get("luma.register").is_none());
    }

    #[test]
    fn registry_changes_are_watched() {
        let r = ToolRegistry::new(vec![spec("fetch", Class::Read)]);
        let mut rx = r.changes();
        assert!(!rx.has_changed().unwrap());
        r.add(spec("a", Class::Read));
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();
        r.remove("nope");
        assert!(!rx.has_changed().unwrap(), "removing nothing is no change");
        r.clone().remove("a");
        assert!(rx.has_changed().unwrap(), "clones share the notifier");
    }

    fn addr_vault() -> Vault {
        Vault::in_memory([(
            "addr".to_string(),
            "1 Main St\nSpringfield".to_string(),
            crate::vault::EntryKind::Personal,
        )])
    }

    #[test]
    fn clean_line_scrubs_before_it_reshapes() {
        let v = addr_vault();
        // Whitespace collapse would turn the value into "1 Main St Springfield".
        let line = clean_line(&v, "ship to 1 Main St\nSpringfield\x1b[31m now");
        assert_eq!(line, "ship to @vault(addr) now");
        // Truncation at the cap would cut the value in half.
        let long = format!("{}1 Main St\nSpringfield", "x".repeat(195));
        let line = clean_line(&v, &long);
        assert!(
            !line.contains("Main") && !line.contains("Springfield"),
            "{line}"
        );
        assert!(!line.contains('\n'));
    }

    #[test]
    fn sanitised_cards_keep_the_timer_and_the_value_out() {
        let v = addr_vault();
        let card = AskCard {
            class: Class::Account,
            what: "update address to 1 Main St\nSpringfield\r".into(),
            site: Some("shop.example".into()),
            values: vec![("address".into(), "1 Main St\nSpringfield".into())],
            timeout: std::time::Duration::from_millis(1),
            on_timeout: OnTimeout::Proceed,
        };
        let c = sanitise_card(&v, card);
        assert_eq!(c.timeout, ASK_TIMEOUT);
        assert_eq!(
            c.on_timeout,
            OnTimeout::Proceed,
            "tightening is the gate's job"
        );
        assert_eq!(c.what, "update address to @vault(addr)");
        assert_eq!(c.values, vec![("address".into(), "@vault(addr)".into())]);
        assert_eq!(
            tighter(OnTimeout::Proceed, OnTimeout::Proceed),
            OnTimeout::Proceed
        );
        assert_eq!(
            tighter(OnTimeout::Proceed, OnTimeout::Skip),
            OnTimeout::Skip
        );
        assert_eq!(
            tighter(OnTimeout::Skip, OnTimeout::Proceed),
            OnTimeout::Skip
        );
    }

    /// An asker that never answers and ignores its timer.
    struct Hang;
    #[async_trait]
    impl Asker for Hang {
        async fn ask(&self, _card: &AskCard) -> AskOutcome {
            std::future::pending().await
        }
    }

    /// An asker that claims a timeout proceeded whatever the card says.
    struct Loose;
    #[async_trait]
    impl Asker for Loose {
        async fn ask(&self, _card: &AskCard) -> AskOutcome {
            AskOutcome::TimeoutProceed
        }
    }

    fn short_card(on_timeout: OnTimeout) -> AskCard {
        AskCard {
            class: Class::Send,
            what: "w".into(),
            site: None,
            values: Vec::new(),
            timeout: Duration::from_millis(20),
            on_timeout,
        }
    }

    #[tokio::test]
    async fn a_hung_asker_is_capped_to_skip() {
        let v = Vault::in_memory([]);
        let card = short_card(OnTimeout::Proceed);
        let started = Instant::now();
        let o = tokio::time::timeout(
            Duration::from_secs(20),
            ask_capped_with(&Hang, &card, &v, Duration::from_millis(30)),
        )
        .await
        .expect("the cap ends the ask");
        assert_eq!(o, AskOutcome::TimeoutSkip, "a cap hit never proceeds");
        assert!(started.elapsed() >= Duration::from_millis(50));
    }

    #[tokio::test]
    async fn an_asker_cannot_loosen_the_card() {
        let v = Vault::in_memory([]);
        let skip = short_card(OnTimeout::Skip);
        assert_eq!(ask_capped(&Loose, &skip, &v).await, AskOutcome::TimeoutSkip);
        let proceed = short_card(OnTimeout::Proceed);
        assert_eq!(
            ask_capped(&Loose, &proceed, &v).await,
            AskOutcome::TimeoutProceed
        );
    }

    #[test]
    fn payload_scrub_leaves_the_envelope_alone() {
        let v = Vault::in_memory([(
            "pin".to_string(),
            "2025".to_string(),
            crate::vault::EntryKind::Secret,
        )]);
        let msg = json!({"jsonrpc": "2.0", "id": 2025, "result": {
            "protocolVersion": "2025-06-18",
            "content": [{"type": "text", "text": "pin 2025"}],
            "tools": [{"name": "t2025", "description": "d 2025",
                       "inputSchema": {"type": "object", "default": 2025}}],
        }});
        let out = scrub_payload(&v, msg);
        assert_eq!(out["id"], json!(2025));
        assert_eq!(out["jsonrpc"], json!("2.0"));
        assert_eq!(out["result"]["protocolVersion"], json!("2025-06-18"));
        assert_eq!(
            out["result"]["content"][0]["text"],
            json!("pin @vault(pin)")
        );
        assert_eq!(out["result"]["tools"][0]["name"], json!("t@vault(pin)"));
        assert_eq!(
            out["result"]["tools"][0]["description"],
            json!("d @vault(pin)")
        );
        assert_eq!(
            out["result"]["tools"][0]["inputSchema"]["default"],
            json!("@vault(pin)")
        );
        let err = scrub_payload(
            &v,
            json!({"jsonrpc": "2.0", "id": 2025, "error": {"code": -32602, "message": "unknown tool: x2025"}}),
        );
        assert_eq!(err["id"], json!(2025));
        assert_eq!(err["error"]["code"], json!(-32602));
        assert_eq!(err["error"]["message"], json!("unknown tool: x@vault(pin)"));
    }

    #[test]
    fn numbers_spelling_a_value_are_scrubbed() {
        let v = Vault::in_memory([(
            "pin".to_string(),
            "48151623".to_string(),
            crate::vault::EntryKind::Secret,
        )]);
        let out = scrub_value(&v, &json!({"a": 48151623, "b": [1, "x48151623"]}));
        assert_eq!(out, json!({"a": "@vault(pin)", "b": [1, "x@vault(pin)"]}));
    }

    /// Polls `fut` once; `Pending` means it consumed what was there and is waiting.
    async fn poll_once<F: Future + Unpin>(fut: &mut F) -> Poll<F::Output> {
        std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut *fut).poll(cx))).await
    }

    /// D-127: an over-long line never holds more than the cap in `buf`, and a read dropped
    /// part-way (another select branch won) loses nothing: state lives in `buf` and
    /// `overlong`, so the next call carries on.
    #[tokio::test]
    async fn capped_reads_are_bounded_and_cancel_safe() {
        const CAP: usize = 1024;
        let (mut tx, rx) = tokio::io::duplex(1 << 16);
        let mut reader = tokio::io::BufReader::new(rx);
        let mut buf = Vec::new();
        let mut overlong = false;

        // Half a line, then the read is dropped while it waits for more.
        tx.write_all(&[b'x'; CAP / 2]).await.unwrap();
        {
            let mut f = Box::pin(read_line_capped(&mut reader, &mut buf, &mut overlong, CAP));
            assert!(poll_once(&mut f).await.is_pending());
        }
        assert_eq!(buf.len(), CAP / 2, "bytes read before the drop are kept");
        assert!(!overlong);

        // 40 caps more without a newline: buf is freed once the line passes the cap.
        tx.write_all(&[b'x'; 40 * CAP]).await.unwrap();
        {
            let mut f = Box::pin(read_line_capped(&mut reader, &mut buf, &mut overlong, CAP));
            assert!(poll_once(&mut f).await.is_pending());
        }
        assert!(overlong, "the line is marked over-long across the drop");
        assert!(
            buf.is_empty() && buf.capacity() <= CAP,
            "{}",
            buf.capacity()
        );

        // The rest of the line, a newline, then a good line.
        tx.write_all(b"tail\nok line\n").await.unwrap();
        let r = read_line_capped(&mut reader, &mut buf, &mut overlong, CAP).await;
        assert_eq!(r.unwrap(), LineRead::TooLong);
        assert!(!overlong && buf.is_empty());
        let r = read_line_capped(&mut reader, &mut buf, &mut overlong, CAP).await;
        assert_eq!(r.unwrap(), LineRead::Line);
        assert_eq!(buf, b"ok line");
        buf.clear();

        // Exactly the cap is a line; a last line without a newline is a line; then EOF.
        tx.write_all(&[b'y'; CAP]).await.unwrap();
        tx.write_all(b"\nlast").await.unwrap();
        drop(tx);
        let r = read_line_capped(&mut reader, &mut buf, &mut overlong, CAP).await;
        assert_eq!(r.unwrap(), LineRead::Line);
        assert_eq!(buf.len(), CAP);
        buf.clear();
        let r = read_line_capped(&mut reader, &mut buf, &mut overlong, CAP).await;
        assert_eq!(r.unwrap(), LineRead::Line);
        assert_eq!(buf, b"last");
        buf.clear();
        let r = read_line_capped(&mut reader, &mut buf, &mut overlong, CAP).await;
        assert_eq!(r.unwrap(), LineRead::Eof);
    }

    #[test]
    fn stop_text_names_class_and_element() {
        let out = ToolOutput {
            text: "partial".into(),
            stopped: Some(StopReason {
                class: Class::Challenge,
                why: "turnstile".into(),
                element: Some("#31".into()),
            }),
            ..ToolOutput::default()
        };
        assert_eq!(
            model_text(&out),
            "partial\nstopped: challenge at #31: turnstile"
        );
    }
}
