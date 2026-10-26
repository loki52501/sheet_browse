//! Contract paths (pr-ready step 4, D-74): the I-1 and I-2 items srv-files consumes, at
//! exactly the paths agent-memory/interfaces.yaml lists, plus this crate's own module
//! entry points the server binary and the tests use. A moved or renamed item fails to
//! compile here.

#![allow(unused_imports)]

// I-2 (harness_core::mcp).
use harness_core::mcp::{
    dispatch, scrub_value, serve, serve_io, Classified, Harness, ToolCtx, ToolError, ToolHandler,
    ToolOutput, ToolRegistry, ToolSpec, PROTOCOL_VERSION,
};
// I-1 (harness_core::memory) items the tests and the ledger checks use.
use harness_core::memory::{ts_now, LedgerEvent, LedgerMode, MemoryStore, Run, RunStatus};
// I-3 classes and ask outcomes.
use harness_core::class::Class;
use harness_core::gate::{AskOutcome, OnTimeout};
// D-108 test doubles.
use harness_core::testing::{MemStore, ScriptedAsker};
// srv-files entry points.
use srv_files::docs::DocsConfig;
use srv_files::paths::{Place, Roots};
use srv_files::shell::Runner;
use srv_files::{csv, diff, docs, fs, history, shell, tools};

#[test]
fn entry_points_exist() {
    let _: fn() -> Vec<ToolSpec> = tools;
    let _: fn() -> Vec<ToolSpec> = fs::tools;
    let _: fn() -> Vec<ToolSpec> = shell::tools;
    let _: fn(Runner) -> Vec<ToolSpec> = shell::tools_with;
    let _: fn() -> Vec<ToolSpec> = docs::tools;
    let _: fn(DocsConfig) -> Vec<ToolSpec> = docs::tools_with;
    let _: fn() -> Vec<ToolSpec> = csv::tools;
    let _: fn() -> Vec<ToolSpec> = diff::tools;
    let _: fn() -> Vec<ToolSpec> = history::tools;
    let _: fn(Option<std::path::PathBuf>) -> Vec<ToolSpec> = history::tools_with;
}

#[test]
fn tool_names_are_unique_and_declared() {
    let mut names: Vec<String> = srv_files::fs::tools()
        .into_iter()
        .chain(shell::tools_with(Runner::Fallback("sh".into())))
        .chain(docs::tools_with(DocsConfig::default()))
        .chain(csv::tools())
        .chain(diff::tools())
        .chain(history::tools_with(None))
        .map(|t| t.name)
        .collect();
    let all = names.clone();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), all.len(), "duplicate tool names: {all:?}");
    for want in [
        "fs.read",
        "fs.write",
        "fs.glob",
        "fs.move",
        "fs.mkdir",
        "fs.delete",
        "shell",
        "pdf.read",
        "pdf.write",
        "xlsx.read",
        "xlsx.write",
        "docx.read",
        "docx.write",
        "pptx.write",
        "md.to_pdf",
        "csv.clean",
        "diff",
        "history.search",
    ] {
        assert!(all.iter().any(|n| n == want), "missing {want}");
    }
}
