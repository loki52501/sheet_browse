//! Shared test environment for srv-cli: MemStore + ScriptedAsker + an in-memory vault holding
//! the canary + a temp runs root with one run (row and folder), driven through
//! `harness_core::mcp::dispatch` (the H-6 pipeline). Nothing here prints the canary.
#![allow(dead_code)]

use harness_core::class::Class;
use harness_core::gate::AskOutcome;
use harness_core::mcp::{dispatch, Harness, ToolError, ToolOutput, ToolRegistry};
use harness_core::memory::{ts_now, LedgerEvent, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::{EntryKind, Vault};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub const SERVER: &str = "cli";

/// The planted vault value: `SB_VAULT_CANARY`, else a fixed test-only string.
pub fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| c.len() >= 8)
        .unwrap_or_else(|| "srv-cli-t15-canary-not-a-secret".to_owned())
}

/// A temp directory removed on drop.
pub struct Tmp(pub PathBuf);
impl Tmp {
    pub fn new(tag: &str) -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "sb-srv-cli-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct Env {
    pub root: Tmp,
    pub run: RunFolder,
    pub store: Arc<MemStore>,
    pub asker: Arc<ScriptedAsker>,
    pub harness: Harness,
    pub registry: ToolRegistry,
}

impl Env {
    /// `tools` registered; the vault holds the canary under `vault_name`. The vault is also
    /// installed as the process-wide scrubber (files are scrubbed with it); every test in a
    /// binary installs the same canary, so the order does not matter.
    pub async fn new(
        tools: Vec<harness_core::mcp::ToolSpec>,
        vault_name: &str,
        answers: Vec<AskOutcome>,
    ) -> Env {
        let root = Tmp::new("runs");
        let run = RunFolder::create(&root.0, "srv-cli-t15", "srv-cli tests").unwrap();
        let store = Arc::new(MemStore::new());
        store
            .run_start(Run {
                id: run.id.clone(),
                started: ts_now(),
                ended: None,
                prompt: "srv-cli tests".into(),
                status: RunStatus::Running,
                folder: format!("runs/{}", run.id),
                parent_run: None,
                strict: false,
            })
            .await
            .unwrap();
        let vault = Arc::new(Vault::in_memory([(
            vault_name.to_owned(),
            canary(),
            EntryKind::Secret,
        )]));
        vault.install_global();
        let asker = Arc::new(ScriptedAsker::with(answers));
        let harness = Harness {
            store: store.clone(),
            vault,
            asker: asker.clone(),
            runs_root: root.0.clone(),
            run: Some(run.id.clone()),
        };
        Env {
            root,
            run,
            store,
            asker,
            harness,
            registry: ToolRegistry::new(tools),
        }
    }

    pub async fn call(&self, tool: &str, args: Value) -> Result<ToolOutput, ToolError> {
        dispatch(SERVER, &self.registry, &self.harness, tool, args).await
    }

    pub fn events(&self) -> Vec<LedgerEvent> {
        let mut t = self.store.ledger();
        t.sort_by_key(|(id, _)| *id);
        t.into_iter().map(|(_, e)| e).collect()
    }

    /// The last tool event (server = cli).
    pub fn last_tool_event(&self) -> LedgerEvent {
        self.events()
            .into_iter()
            .rev()
            .find(|e| e.server == SERVER)
            .expect("a tool event")
    }

    /// Every ledger row and every file under the runs root, as one string.
    pub fn everything_written(&self) -> String {
        let mut all = String::new();
        for e in self.events() {
            all.push_str(&serde_json::to_string(&e).unwrap());
            all.push('\n');
        }
        walk(&self.root.0, &mut all);
        all
    }
}

fn walk(dir: &Path, out: &mut String) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if let Ok(b) = std::fs::read(&p) {
            out.push_str(&String::from_utf8_lossy(&b));
            out.push('\n');
        }
    }
}

/// The class shown on the last ask card, if any.
pub fn last_card_class(asker: &ScriptedAsker) -> Option<Class> {
    asker.cards().last().map(|c| c.class)
}

/// No assertion message ever formats `hay`: it may hold the canary.
pub fn assert_clean(hay: &str, what: &str) {
    let c = canary();
    assert!(!hay.contains(&c), "vault canary leaked into {what}");
}

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}
