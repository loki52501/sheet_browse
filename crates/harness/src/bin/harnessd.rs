//! `harnessd` (H-5 Phase 1): the long-running scheduler. Fires due schedules as fresh
//! `claude -p` sessions with a run folder (D-106, D-107, D-116). Ctrl-C stops scheduling and
//! waits for running firings to write their report; a second Ctrl-C exits at once.

use clap::Parser;
use harness::daemon::Daemon;
use harness::{ClaudeLauncher, Env};
use harness_core::sched::SystemClock;
use std::sync::Arc;

#[derive(Debug, Parser)]
#[command(
    name = "harnessd",
    version,
    about = "sheetbrowse scheduler: runs `harness schedule` entries (docs/01 H-5)"
)]
struct Args {}

async fn run() -> anyhow::Result<()> {
    let _ = Args::parse();
    let env = Env::from_env()?;
    env.install_vault()?;
    let store = Arc::new(env.connect().await?);
    let launcher = Arc::new(ClaudeLauncher::print(env.claude.clone()));
    let daemon = Daemon::new(
        store,
        launcher,
        Arc::new(SystemClock),
        env.runs_root.clone(),
        None,
    )?;
    eprintln!(
        "harnessd: scheduling; runs in {}",
        std::path::absolute(&env.runs_root)?.display()
    );
    let shutdown = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
        eprintln!("harnessd: stopping after running firings finish (Ctrl-C again to exit now)");
        tokio::spawn(async {
            if tokio::signal::ctrl_c().await.is_ok() {
                std::process::exit(130);
            }
        });
    };
    daemon.run(shutdown).await?;
    eprintln!("harnessd: stopped");
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("harnessd: {}", harness_core::scrub(&format!("{e:#}")));
        std::process::exit(1);
    }
}
