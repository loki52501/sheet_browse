//! toolstore (docs/03). DRAFT signatures for I-7 (`load`, `fingerprint`, `record`) and I-12
//! (tool file header), owned by area store (T-11).
//!
//! Tool runtime (D-53): tool files run in an embedded JS engine (default rquickjs) in a
//! `runner` module shared by srv-web and sb-mcp, with no `document`, `window`, `fetch`,
//! `eval`, `import` or `require`. The page API is host functions: `find(q)` / `findAll(q)`
//! return element refs without the `#` (`12`, `f2.15`), so planners write `c#${id}`;
//! `text(region)`, `table(id)`, `url`, `title`, `q(s)` (act value quoting, D-52), and for
//! `@phase 1` readers `api(url)`, which goes through srv-web's `api` tool and the gate as Read.

pub mod runner;
pub use runner::{
    run_planner, run_planner_with, run_reader, run_reader_with, ApiFetch, PageHost, RunError,
    RunOpts, SheetPage,
};

use harness_core::memory::{MemoryStore, ToolHealth, Ts};
use harness_core::Class;
use sb_sheet::IndexRow;
use std::path::{Path, PathBuf};

/// `@kind` (T-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolKind {
    /// Returns JSON; never calls act.
    Reader,
    /// Returns an act program; never gets hands.
    Planner,
}

/// One `@args` entry: `name:string`, `plan?:string`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgSpec {
    pub name: String,
    pub ty: String,
    pub optional: bool,
}

/// One `@fingerprint` item: `form#f2`, `button"Register"`, `input"Email"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpItem {
    pub role: String,
    pub name: Option<String>,
    pub id: Option<String>,
}

/// I-12: the parsed header of a tool file (T-2).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolHeader {
    pub name: String,
    pub kind: ToolKind,
    /// `@match` path globs within the site.
    pub matches: Vec<String>,
    pub args: Vec<ArgSpec>,
    /// `@gate`: the class floor (H-1.5).
    pub gate: Class,
    pub fingerprint: Vec<FpItem>,
    /// `@expects`, unparsed (`navigated:/thanks|/confirm OR text"You're registered"`).
    pub expects: Option<String>,
    /// `@forged`, unparsed.
    pub forged: Option<String>,
    /// `@phase`: 1 = works on fetched HTML, 2 = needs the browser.
    pub phase: u8,
}

/// A loaded tool.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    /// Folder: eTLD+1 or `_generic`.
    pub domain: String,
    /// MCP name, namespaced: `luma.register`.
    pub namespaced: String,
    pub path: PathBuf,
    pub header: ToolHeader,
    pub source: String,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}: bad tool header: {msg}")]
    Header { path: PathBuf, msg: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Memory(#[from] harness_core::memory::MemoryError),
    /// T-9 / G-4: a repair or forge tried to lower `@gate`, drop `?s`, add a vault key or widen `@match`.
    #[error("invariant violated: {0}")]
    Invariant(String),
}

/// I-12. Parses the `// @key value` header of a tool file.
pub fn parse_header(src: &str) -> Result<ToolHeader, String> {
    let _ = src;
    todo!("T-11: T-2 header parser")
}

/// T-4 outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FingerprintResult {
    Match,
    /// Relocate (T-4, <= 200 ms) re-finds these by role + name synonyms.
    Partial {
        missing: Vec<FpItem>,
    },
    Miss,
}

/// What `record` stores after a run (T-4, T-5, T-7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Ok,
    /// T-4: a partial fingerprint repaired by relocate; record() patches @fingerprint,
    /// bumps the version and sets status `repaired`.
    Relocated {
        fingerprint: Vec<FpItem>,
    },
    Failed {
        why: String,
    },
    FingerprintMiss {
        why: String,
    },
    ExpectsMiss {
        why: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRunResult {
    pub domain: String,
    pub name: String,
    pub outcome: RunOutcome,
    pub at: Ts,
}

/// The store rooted at a `tools/` directory.
#[derive(Debug, Clone)]
pub struct ToolStore {
    pub root: PathBuf,
}

impl ToolStore {
    pub fn new(root: impl AsRef<Path>) -> ToolStore {
        ToolStore {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Host → folder (eTLD+1 via the public suffix list, then `_site.yaml` aliases) (T-1).
    pub fn folder_for(&self, host: &str) -> String {
        let _ = host;
        todo!("T-11: T-1 eTLD+1 + aliases")
    }

    /// I-7 `load(domain, path)`: site tools whose `@match` fits `path`, then `_generic/` at lower precedence (T-3).
    pub fn load(&self, domain: &str, path: &str) -> Result<Vec<Tool>, StoreError> {
        let _ = (domain, path);
        todo!("T-11: T-3 loader")
    }

    /// I-7 `record(result)`: lifecycle transition (T-7) written through `MemoryStore` (I-1).
    pub async fn record(
        &self,
        store: &dyn MemoryStore,
        result: ToolRunResult,
    ) -> Result<ToolHealth, StoreError> {
        let _ = (store, result);
        todo!("T-11: T-7 lifecycle")
    }
}

/// I-7 `fingerprint(tool, index)` (T-4), over a live or fetched index.
pub fn fingerprint(tool: &Tool, index: &[IndexRow]) -> FingerprintResult {
    let _ = (tool, index);
    todo!("T-11: T-4 fingerprint")
}

/// The T-3 skeleton line: `tools for this site: luma.register(name,email) · luma.list_events(city)`.
pub fn skeleton_line(tools: &[Tool]) -> Option<String> {
    let _ = tools;
    todo!("T-11: T-3 skeleton line")
}
