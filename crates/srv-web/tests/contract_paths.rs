//! pr-ready section 4: every interface path srv-web consumes, named from outside the crate,
//! so a moved or renamed item fails compilation here (D-74). srv-web provides no interface
//! of its own in agent-memory/interfaces.yaml; its public entry points are pinned too.

#![allow(unused_imports)]

// I-2 (harness_core::mcp), consumed by every tool.
use harness_core::mcp::{
    dispatch, scrub_value, serve, serve_io, Classified, Harness, ToolCtx, ToolError, ToolHandler,
    ToolOutput, ToolRegistry, ToolSpec,
};
// I-3 / I-4: the stop a challenge returns, the vault the api sink resolves through.
use harness_core::gate::StopReason;
use harness_core::vault::{Resolved, Vault, VaultError};
// I-5 / I-6: the rows and blocks the HTML walk builds, and the sheet functions it feeds.
use sb_sheet::index::{ElemRef, FieldValue, Flags, IndexRow, State};
use sb_sheet::{
    find, find_lines, read, sheet, sheet_line, skeleton, BlockKind, Cursor, FindOpts, PageMeta,
    ReadOpts, SheetOpts, SkeletonOpts, Span, TextBlock,
};
// I-7 / I-12: the tool store and runner the W-7 site tools use.
use toolstore::runner::{run_reader, ApiFetch, PageHost, RunError, RunOpts, SheetPage};
use toolstore::{
    fingerprint, normalize_host, registrable, skeleton_line, FingerprintResult, RunOutcome, Tool,
    ToolKind, ToolRunResult, ToolStore, GENERIC,
};

// srv-web's own entry points (main.rs and tests).
use srv_web::{tools, tools_with, Config};

#[test]
fn contract_paths_resolve() {
    let store = ToolStore::new("tools");
    // D-72: consumers call try_tool_path, never tool_path.
    assert!(store.try_tool_path("example.com", "list_items").is_ok());
    assert!(store.try_tool_path("../x", "list_items").is_err());
    let _: fn(&Tool, &[IndexRow]) -> FingerprintResult = fingerprint;
    let _: fn(&[Tool]) -> Option<String> = skeleton_line;
    let _: fn(&str) -> String = registrable;
    // ApiFetch::api(&self, url, remaining) per D-64.
    fn _api(a: &dyn ApiFetch) -> Result<serde_json::Value, String> {
        a.api("https://example.com", std::time::Duration::from_secs(1))
    }
    let _: fn(&[IndexRow], &[TextBlock], &ReadOpts) -> sb_sheet::ReadOutput = read;
    let names: Vec<String> = tools_with(Config::default())
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["search", "fetch", "find", "api", "feed"]);
    let _ = tools;
}
