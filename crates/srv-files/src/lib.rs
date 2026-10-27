//! srv-files: Phase 1 files tool server (docs/04 F-1..F-6): `fs.*`, `shell`, the document
//! wrappers, `csv.clean`, `diff`, `history.search`. Every tool registers through
//! `harness_core::mcp::serve` (I-2) and nothing else; the wrapper classifies, gates,
//! scrubs and writes the ledger.
//!
//! Boundaries every tool here keeps (D-520):
//! * relative paths resolve against the folder that holds `runs/`; every path is resolved
//!   as the OS will resolve it before its class is decided ([`paths`]);
//! * changing anything outside `runs/`, or a run's own records, raises the class (to Delete
//!   when something is deleted or replaced); inside `runs/` it just runs;
//! * every call rechecks its class against the approved one before acting;
//! * text for the model is short and escaped ([`text`]); untrusted content cannot forge
//!   the harness's own trailers.

pub mod csv;
pub mod diff;
pub mod docs;
pub mod fs;
pub mod history;
pub mod paths;
pub mod proc;
pub mod safeio;
pub mod shell;
pub mod text;
pub mod util;

use harness_core::ToolSpec;

/// Every tool this server exposes.
pub fn tools() -> Vec<ToolSpec> {
    let mut all = Vec::new();
    all.extend(fs::tools());
    all.extend(shell::tools());
    all.extend(docs::tools());
    all.extend(csv::tools());
    all.extend(diff::tools());
    all.extend(history::tools());
    all
}
