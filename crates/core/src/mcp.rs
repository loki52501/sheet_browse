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
//! * stop (gate Stop before the call, or a handler's stopped output after it):
//!   `server = core`, `mode = stop`, `why` = the reason, `consequences = {"element"}`.
//!
//! # Transport (D-104)
//!
//! Newline-delimited JSON-RPC 2.0 on stdin/stdout, protocol `2025-06-18`: `initialize`,
//! `notifications/initialized`, `ping`, `tools/list`, `tools/call`, and the server's
//! `notifications/tools/list_changed` when the registry changes. Nothing but JSON-RPC goes
//! to stdout; diagnostics go to stderr, scrubbed.

use crate::class::Class;
use crate::gate::{AskCard, AskOutcome, Asker, Call, Decision, Gate, OnTimeout, StopReason};
use crate::ledger::Ledger;
use crate::memory::{ts_now, LedgerEvent, LedgerMode, MemoryStore};
use crate::run::{RunFolder, RunId};
use crate::vault::{Resolved, Vault, VaultError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::Instant;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

/// `ledger.events.server` of the events the wrapper writes for the harness itself.
const CORE: &str = "core";

/// MCP protocol revision this wrapper speaks (D-104).
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Cap on `ledger.events.result_summary` when it is taken from the output text.
const SUMMARY_MAX_CHARS: usize = 300;

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
#[derive(Clone)]
pub struct ToolRegistry {
    tools: Arc<RwLock<Vec<ToolSpec>>>,
    /// Bumped by every `add` and every `remove` that removed something; `serve_io` watches it.
    version: Arc<watch::Sender<u64>>,
}

impl Default for ToolRegistry {
    fn default() -> ToolRegistry {
        ToolRegistry::new(Vec::new())
    }
}

impl ToolRegistry {
    pub fn new(tools: Vec<ToolSpec>) -> ToolRegistry {
        ToolRegistry {
            tools: Arc::new(RwLock::new(tools)),
            version: Arc::new(watch::channel(0).0),
        }
    }

    /// Adds or replaces by name.
    pub fn add(&self, spec: ToolSpec) {
        {
            let mut t = self.tools.write().unwrap_or_else(|e| e.into_inner());
            t.retain(|s| s.name != spec.name);
            t.push(spec);
        }
        self.bump();
    }

    pub fn remove(&self, name: &str) {
        let removed = {
            let mut t = self.tools.write().unwrap_or_else(|e| e.into_inner());
            let before = t.len();
            t.retain(|s| s.name != name);
            t.len() != before
        };
        if removed {
            self.bump();
        }
    }

    pub fn get(&self, name: &str) -> Option<ToolSpec> {
        let t = self.tools.read().unwrap_or_else(|e| e.into_inner());
        t.iter().find(|s| s.name == name).cloned()
    }

    pub fn list(&self) -> Vec<ToolSpec> {
        self.tools.read().unwrap_or_else(|e| e.into_inner()).clone()
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
    tool: String,
    /// Scrubbed.
    args: Value,
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
    /// The card is sanitised like a gate card: scrubbed, control-stripped and capped, the
    /// timer is [`crate::gate::ASK_TIMEOUT`], and `on_timeout` can only be tightened (a
    /// card may proceed on timeout only where [`Gate::on_timeout`] allows it for its class
    /// and the run's `strict`). A `Challenge` card is never shown: it is logged as a stop
    /// and returns `TimeoutSkip` (D-5; the handler should return `ToolOutput::stopped`).
    /// If the outcome cannot be written to the ledger, a proceed becomes a skip: nothing
    /// runs that the ledger does not show.
    pub async fn ask(&self, card: AskCard) -> AskOutcome {
        if card.class == Class::Challenge {
            let why = self.vault.scrub(&card_line(
                &format!("challenge during {}: {}", self.call.tool, card.what),
                WHAT_MAX_CHARS,
            ));
            let mut e = self.event(CORE, Some(Class::Challenge), Some(LedgerMode::Stop));
            e.result_summary = why.clone();
            e.why = Some(why);
            if let Err(err) = self.ledger.write(e).await {
                diag(&self.vault, &format!("ledger write failed (stop): {err}"));
            }
            return AskOutcome::TimeoutSkip;
        }
        let strict = match self.store.run_get(&self.run_id).await {
            Ok(run) => run.is_some_and(|r| r.strict),
            // Unknown strictness: the safe reading.
            Err(_) => true,
        };
        let card = clean_card(&self.vault, card, strict);
        let started = Instant::now();
        let outcome = self.asker.ask(&card).await;
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
    let mut lines = reader.lines();
    let mut initialized = false;
    let mut watching = true;

    loop {
        tokio::select! {
            line = lines.next_line(), if tx.is_some() => match line {
                Ok(Some(line)) => {
                    let Some(sender) = tx.as_ref() else { continue };
                    let reply = handle_line(&server, &registry, &harness, &line, sender, &mut initialized);
                    if let Some(reply) = reply {
                        write_message(&mut writer, &harness.vault, reply).await?;
                    }
                }
                // EOF: stop reading; answer the calls in flight, then return.
                Ok(None) => tx = None,
                Err(e) => return Err(ServeError::Transport(harness.vault.scrub(&e.to_string()))),
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

/// One whole line, scrubbed value by value (strings, keys, numbers), then flushed.
async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    vault: &Vault,
    msg: Value,
) -> Result<(), ServeError> {
    let msg = scrub_value(vault, &msg);
    let mut line = serde_json::to_vec(&msg).map_err(|e| ServeError::Transport(e.to_string()))?;
    line.push(b'\n');
    let io = |e: std::io::Error| ServeError::Transport(vault.scrub(&e.to_string()));
    writer.write_all(&line).await.map_err(io)?;
    writer.flush().await.map_err(io)
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
                // Inner task: a panicking handler becomes an error reply, not a lost request.
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
                        "content": [{"type": "text", "text": "the tool failed unexpectedly (panic); see the server log"}],
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
        tool: spec.name.clone(),
        args: args_scrubbed.clone(),
    };

    // Classify: the declared class is the floor (rule 4, H-1.5).
    let classified = match spec.handler.classify(&ctx, &args).await {
        Ok(c) => c,
        Err(err) => {
            let err = scrub_error(vault, err);
            let mut e = ctx.event(server, Some(spec.class), None);
            e.ok = false;
            e.result_summary = clip(&format!("error: {}", first_line(&err.to_string())));
            e.why = Some(format!("classify: {err}"));
            write_or_diag(&ctx, e).await;
            return Err(err);
        }
    };
    let class = classified.class.map_or(spec.class, |c| c.max(spec.class));

    let strict = harness
        .store
        .run_get(&run_id)
        .await
        .map_err(|e| ToolError::Failed(vault.scrub(&format!("run lookup: {e}"))))?
        .is_some_and(|r| r.strict);
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
    let decision = Gate::new(harness.store.clone())
        .check(&call)
        .await
        .map_err(|e| ToolError::Failed(vault.scrub(&format!("gate: {e}"))))?;

    let mode = match decision {
        Decision::Allow => LedgerMode::Allow,
        Decision::Ask(card) => {
            let started = Instant::now();
            let outcome = harness.asker.ask(&card).await;
            let e = ctx.outcome_event(&card, outcome, started);
            // Nothing runs that the ledger does not show.
            ctx.ledger
                .write(e)
                .await
                .map_err(|e| ToolError::Failed(vault.scrub(&format!("ledger: {e}"))))?;
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
            let reason = scrub_stop(vault, reason);
            ctx.ledger
                .write(ctx.stop_event(&reason))
                .await
                .map_err(|e| ToolError::Failed(vault.scrub(&format!("ledger: {e}"))))?;
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
    let started = Instant::now();
    let result = spec.handler.call(&ctx, args).await;
    let duration_ms = millis(started);

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
            if let Some(r) = &out.stopped {
                // H-1.4: a stopped call is not a completed action.
                e.mode = Some(LedgerMode::Stop);
                e.why = Some(r.why.clone());
            }
            write_or_diag(&ctx, e).await;
            if let Some(r) = &out.stopped {
                write_or_diag(&ctx, ctx.stop_event(r)).await;
            }
            Ok(out)
        }
        Err(err) => {
            let err = scrub_error(vault, err);
            e.ok = false;
            e.result_summary = clip(&format!("error: {}", first_line(&err.to_string())));
            e.why = Some(err.to_string());
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
    eprintln!("{}", vault.scrub(&card_line(msg, 1000)));
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

fn scrub_stop(vault: &Vault, r: StopReason) -> StopReason {
    StopReason {
        class: r.class,
        why: vault.scrub(&r.why),
        element: r.element.map(|e| vault.scrub(&e)),
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
        stopped: out.stopped.map(|r| scrub_stop(vault, r)),
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

// ---- mid-call ask cards: sanitised like gate cards (D-110 (5)) ----

const WHAT_MAX_CHARS: usize = 200;
const VALUE_MAX_CHARS: usize = 200;
const KEY_MAX_CHARS: usize = 64;
const SITE_MAX_CHARS: usize = 253;
const MAX_VALUES: usize = 32;

fn clean_card(vault: &Vault, card: AskCard, strict: bool) -> AskCard {
    let what = card_line(&vault.scrub(&card.what), WHAT_MAX_CHARS);
    let what = if what.is_empty() {
        format!("{} step needs your answer", card.class)
    } else {
        what
    };
    let more = card.values.len().saturating_sub(MAX_VALUES);
    let mut values: Vec<(String, String)> = card
        .values
        .iter()
        .take(MAX_VALUES)
        .map(|(k, v)| {
            (
                card_line(&vault.scrub(k), KEY_MAX_CHARS),
                card_line(&vault.scrub(v), VALUE_MAX_CHARS),
            )
        })
        .collect();
    if more > 0 {
        values.push(("...".to_string(), format!("{more} more values not shown")));
    }
    let on_timeout = match (card.on_timeout, Gate::on_timeout(card.class, strict)) {
        (OnTimeout::Proceed, OnTimeout::Proceed) => OnTimeout::Proceed,
        _ => OnTimeout::Skip,
    };
    AskCard {
        class: card.class,
        what,
        site: card
            .site
            .as_deref()
            .map(|s| card_line(&vault.scrub(s), SITE_MAX_CHARS)),
        values,
        timeout: crate::gate::ASK_TIMEOUT,
        on_timeout,
    }
}

/// Invisible characters that can reorder or hide text on a terminal or in a notification.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2069}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
    )
}

/// Skips an escape sequence after ESC (`intro = None`) or an 8-bit introducer. CSI runs to
/// its final byte; string sequences (OSC, DCS, ...) run to BEL or ST and are kept as text
/// when unterminated; any other `ESC x` is two characters.
fn skip_escape(it: &mut std::iter::Peekable<std::str::Chars<'_>>, intro: Option<char>) {
    let kind = match intro {
        Some(k) => k,
        None => match it.peek().copied() {
            Some(k @ ('[' | ']' | 'P' | 'X' | '^' | '_')) => {
                it.next();
                k
            }
            Some(k) if ('\u{20}'..='\u{7e}').contains(&k) => {
                it.next();
                return;
            }
            _ => return,
        },
    };
    if matches!(kind, '[' | '\u{9b}') {
        while let Some(&c) = it.peek() {
            if ('\u{20}'..='\u{3f}').contains(&c) {
                it.next();
            } else {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    it.next();
                }
                return;
            }
        }
        return;
    }
    let mut probe = it.clone();
    while let Some(c) = probe.next() {
        match c {
            '\u{07}' | '\u{9c}' => {
                *it = probe;
                return;
            }
            '\u{1b}' => {
                if probe.peek() == Some(&'\\') {
                    probe.next();
                    *it = probe;
                }
                return;
            }
            _ => {}
        }
    }
}

/// One clean line: escape sequences, control characters and invisible marks become one
/// space, whitespace runs collapse, capped at `max` characters with a trailing `...`.
fn card_line(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max * 4));
    let mut space = false;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        let sep = match c {
            '\u{1b}' => {
                skip_escape(&mut it, None);
                true
            }
            '\u{9b}' | '\u{9d}' | '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => {
                skip_escape(&mut it, Some(if c == '\u{9b}' { c } else { ']' }));
                true
            }
            c => c.is_control() || c.is_whitespace() || invisible(c),
        };
        if sep {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    if out.chars().count() > max {
        let mut cut: String = out.chars().take(max.saturating_sub(3)).collect();
        cut.push_str("...");
        cut
    } else {
        out
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

    #[test]
    fn card_lines_are_clean() {
        assert_eq!(card_line("a\r\n\x1b[31mb\x1b[0m  c", 50), "a b c");
        assert_eq!(card_line("x\u{202e}y", 50), "x y");
        assert_eq!(card_line(&"z".repeat(10), 5), "zz...");
        assert_eq!(card_line("\x1b]0;title\x07ok", 50), "ok");
    }

    #[test]
    fn mid_call_cards_cannot_loosen_the_timeout() {
        let v = Vault::in_memory([]);
        let card = |class, on_timeout| AskCard {
            class,
            what: "w".into(),
            site: None,
            values: Vec::new(),
            timeout: std::time::Duration::from_millis(1),
            on_timeout,
        };
        let pay = clean_card(&v, card(Class::Pay, OnTimeout::Proceed), false);
        assert_eq!(pay.on_timeout, OnTimeout::Skip);
        assert_eq!(pay.timeout, crate::gate::ASK_TIMEOUT);
        let acct = clean_card(&v, card(Class::Account, OnTimeout::Proceed), false);
        assert_eq!(acct.on_timeout, OnTimeout::Proceed);
        let strict = clean_card(&v, card(Class::Account, OnTimeout::Proceed), true);
        assert_eq!(strict.on_timeout, OnTimeout::Skip);
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
