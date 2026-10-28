//! `harness-forge` (T-28 (b), D-151): the `forge` MCP tool server over recipes
//! (`tools.list`, `tools.run`, `tools.save`), served over stdio by `harness_core::serve` and
//! wired by `core_pg::bootstrap` (`DATABASE_URL`, the vault, `SB_RUNS_ROOT`, `SB_RUN_ID`).
//! Recipes live under `SB_TOOLS_ROOT` (default `./tools`). The logic is in `harness::forge`.

#[tokio::main]
async fn main() {
    if let Err(e) = harness::forge::serve().await {
        // Display only, scrubbed against the vault (H-2); stdout is the JSON-RPC channel.
        eprintln!("harness-forge: {}", harness_core::scrub(&e.to_string()));
        std::process::exit(1);
    }
}
