//! `srv-cli` MCP server binary (stdio).

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let harness = core_pg::bootstrap().await?;
    harness_core::serve("cli", srv_cli::tools(), harness).await?;
    Ok(())
}
