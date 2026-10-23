//! `harness` CLI (H-7). SKELETON owned by area core (T-08):
//!
//! ```text
//! harness run "prompt"
//! harness schedule add|list|rm
//! harness vault set <name> [--personal]
//! harness gate set <domain> <class> <mode>
//! harness tools list|health|pin|unpin|retire <domain>/<name>
//! harness ledger <run>
//! harness db migrate|status
//! ```

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = core_pg::DEFAULT_DATABASE_URL;
    todo!("T-08: H-7 CLI")
}
