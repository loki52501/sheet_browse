//! `sb-mcp` MCP server binary (stdio).

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let harness = core_pg::bootstrap().await?;
    harness_core::serve("sheetbrowse", sb_mcp::tools(), harness).await?;
    Ok(())
}
