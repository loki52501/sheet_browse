//! toolstore (docs/03). DRAFT signatures for I-7 (`load`, `fingerprint`, `record`) and I-12
//! (tool file header), owned by area store (T-11).
//!
//! Tool runtime (D-53): tool files run in an embedded JS engine (default rquickjs) in a
//! `runner` module shared by srv-web and sb-mcp, with no `document`, `window`, `fetch`,
//! `eval`, `import` or `require`. The page API is host functions: `find(q)` / `findAll(q)`
//! return element refs without the `#` (`12`, `f2.15`), so planners write `c#${id}`;
//! `text(region)`, `table(id)`, `url`, `title`, `q(s)` (act value quoting, D-52), and for
//! `@phase 1` readers `api(url)`, which goes through srv-web's `api` tool and the gate as Read.
//!
//! Modules: `header` (T-2, I-12), `domain` (T-1), `loader` (T-3), `fingerprint` (T-4),
//! `lifecycle` (T-5, T-7, D-302, D-303), `invariants` (T-9, G-4, D-307), `archive` (archive
//! moves and the git commit helper), `recipe` (the WAVE3 recipe kind, `<name>.recipe.yaml`,
//! T-28, D-150).

pub mod runner;
pub use runner::{
    run_planner, run_planner_with, run_reader, run_reader_with, ApiFetch, PageHost, RunError,
    RunOpts, SheetPage,
};

pub mod archive;
pub mod domain;
pub mod fingerprint;
pub mod header;
pub mod invariants;
pub mod lifecycle;
pub mod loader;
pub mod recipe;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_recipe;
#[cfg(test)]
mod tests_review;

pub use archive::has_task_id;
pub use domain::{
    check_tool_id, normalize_host, registrable, valid_domain, valid_name, SiteConfig, ARCHIVE,
    GENERIC, SITE_FILE,
};
pub use fingerprint::{fingerprint, fingerprint_items, relocate, role_matches, FingerprintResult};
pub use header::{
    parse_fingerprint, parse_header, patch_fingerprint, render_fingerprint, ArgSpec, FpItem,
    ToolHeader, ToolKind,
};
pub use invariants::{
    check_repair, planner_act_class, planner_class, stop_count, vault_keys, PlannerCheck,
    RepairKind, SbAct,
};
pub use lifecycle::{
    SweepAction, ARCHIVE_DELETE_DAYS, CLEAN_RUNS_TO_SAVE, EPHEMERAL_IDLE_DAYS,
    EPHEMERAL_IDLE_VISITS, MISSES_TO_RETIRE,
};
pub use loader::{glob_match, namespace, skeleton_line, url_path, LoadReport};
pub use recipe::{
    fill_placeholders, key_shaped, parse_recipe, placeholders, render_recipe, ArgType, Expects,
    Recipe, RecipeArg, RecipeReport, RecipeStep, Target, RECIPE_KIND, RECIPE_SUFFIX, RECIPE_TOOLS,
};

use harness_core::memory::Ts;
use std::path::{Path, PathBuf};

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
    /// T-9 / G-4: a repair or forge tried to lower `@gate`, drop `?s`, add a vault key, widen
    /// `@match`, rename the tool, change `@kind` or lower `@phase` (D-312, D-313).
    #[error("invariant violated: {0}")]
    Invariant(String),
    /// The git commit helper refused (no task id, path outside the tools root) or git failed.
    #[error("git: {0}")]
    Git(String),
    /// D-310: a domain or tool name that is not a single safe path component.
    #[error("bad path: {0}")]
    BadPath(String),
    /// T-28, D-150: a `<name>.recipe.yaml` that breaks a recipe rule (or is unreadable).
    /// `msg` never holds a vault value or a key-shaped string.
    #[error("{path}: bad recipe: {msg}")]
    Recipe { path: PathBuf, msg: String },
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

    /// Host → folder (T-1, D-300): the most specific existing folder among the host and its
    /// parents down to eTLD+1 (so `mail.google.com/` wins over `google.com/`), else a folder
    /// whose `_site.yaml` lists the host (or a parent) under `aliases`, else eTLD+1.
    /// Idempotent on folder names, including `_generic`. Never returns a separator, `..`, a
    /// drive or UNC prefix, or a name starting with `.`/`_` other than `_generic`: input that
    /// is not a plain ASCII hostname gives `""`, the "no folder" sentinel (D-310).
    pub fn folder_for(&self, host: &str) -> String {
        domain::folder_for(&self.root, host)
    }

    /// I-7 `load(domain, path)`: site tools whose `@match` fits `path`, then `_generic/` at
    /// lower precedence (T-3). Invalid files are skipped; use [`ToolStore::scan`] to see them.
    ///
    /// Files only: this does not consult `tools.health`, so a retired tool whose file is still
    /// in place is loaded. Runtime callers use [`ToolStore::load_live`] (D-315).
    pub fn load(&self, domain: &str, path: &str) -> Result<Vec<Tool>, StoreError> {
        Ok(self.scan(domain, path).tools)
    }
}
