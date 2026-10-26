//! `harness` CLI (H-7). The subcommands live in `harness::cli`; this binary reads the
//! environment, runs one command, and prints a scrubbed error with exit code 1.

use clap::Parser;
use harness::cli::{execute, Cli, Ctx};

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let env = harness::Env::from_env()?;
    let ctx = Ctx::new(env);
    let mut out = std::io::stdout();
    execute(cli, &ctx, &mut out).await?;
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        // Display chain only, scrubbed against the vault (H-2); never Debug.
        eprintln!("harness: {}", harness_core::scrub(&format!("{e:#}")));
        std::process::exit(1);
    }
}
