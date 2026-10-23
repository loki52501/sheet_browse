//! `srv-web` MCP server binary (stdio).

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let harness = core_pg::bootstrap().await?;
    harness_core::serve("web", srv_web::tools(), harness).await?;
    Ok(())
}
