//! `srv-files` MCP server binary (stdio).

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let harness = core_pg::bootstrap().await?;
    harness_core::serve("files", srv_files::tools(), harness).await?;
    Ok(())
}
