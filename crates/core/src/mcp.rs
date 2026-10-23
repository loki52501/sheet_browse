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

use crate::class::Class;
use crate::gate::{AskCard, AskOutcome, Asker, StopReason};
use crate::memory::MemoryStore;
use crate::run::RunId;
use crate::vault::{Resolved, Vault, VaultError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

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
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Arc<RwLock<Vec<ToolSpec>>>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<ToolSpec>) -> ToolRegistry {
        ToolRegistry {
            tools: Arc::new(RwLock::new(tools)),
        }
    }

    /// Adds or replaces by name.
    pub fn add(&self, spec: ToolSpec) {
        let mut t = self.tools.write().unwrap_or_else(|e| e.into_inner());
        t.retain(|s| s.name != spec.name);
        t.push(spec);
    }

    pub fn remove(&self, name: &str) {
        let mut t = self.tools.write().unwrap_or_else(|e| e.into_inner());
        t.retain(|s| s.name != name);
    }

    pub fn get(&self, name: &str) -> Option<ToolSpec> {
        let t = self.tools.read().unwrap_or_else(|e| e.into_inner());
        t.iter().find(|s| s.name == name).cloned()
    }

    pub fn list(&self) -> Vec<ToolSpec> {
        self.tools.read().unwrap_or_else(|e| e.into_inner()).clone()
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
}

impl ToolCtx {
    pub fn new(
        run_id: RunId,
        run_dir: PathBuf,
        class: Class,
        harness: &Harness,
        registry: ToolRegistry,
    ) -> ToolCtx {
        ToolCtx {
            run_id,
            run_dir,
            class,
            store: harness.store.clone(),
            vault: harness.vault.clone(),
            asker: harness.asker.clone(),
            registry,
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
    pub async fn ask(&self, card: AskCard) -> AskOutcome {
        let _ = (&self.asker, card);
        todo!("T-07: mid-call ask, logged by the wrapper")
    }

    /// Register or drop site tools for this server (T-3).
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }
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
    let _ = (server, tools, harness);
    todo!("T-07: H-6 over MCP stdio")
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
    let _ = (server, registry, harness, tool, args);
    todo!("T-07: H-6 pipeline")
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
}
