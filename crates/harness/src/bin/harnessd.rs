//! `harnessd` (H-5 Phase 1): the long-running scheduler. Fires due schedules as fresh
//! `claude -p` sessions with a run folder (D-106, D-107, D-116). Ctrl-C stops scheduling and
//! waits for running firings to write their report (each `claude -p` session is capped at
//! `SB_SESSION_CAP_SECS`, default one hour, so the wait is bounded); a second Ctrl-C exits at
//! once.
//!
//! `reflect` schedules run a reflect pass (H-5, G-3, T-16) through
//! `harness::reflect::ReflectLauncher`: sweep, proposals (to `SB_PROPOSALS_FILE`, default
//! `<SB_TOOLS_ROOT>/../agent-memory/blockers.yaml`) and, when readers have a call to replay,
//! a `claude -p` session allowed only `mcp__harness-web`. No reflect schedule is created
//! automatically: add one with `harness schedule add` (kind reflect) (D-142).

use clap::Parser;
use harness::daemon::{Daemon, ReopenVault};
use harness::launcher::session_cap_from_env;
use harness::proposals::proposals_file_from_env;
use harness::reflect::{ReflectLauncher, READERS_ALLOWED_TOOLS};
use harness::{ClaudeLauncher, Env};
use harness_core::memory::MemoryStore;
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
    // Fail closed at startup; every firing re-opens the vault again (ReopenVault, D-124).
    env.install_vault()?;
    let mut claude = ClaudeLauncher::print(env.claude.clone());
    claude.max_session = session_cap_from_env()?;
    let store: Arc<dyn MemoryStore> = Arc::new(env.connect().await?);
    let mut readers = claude.clone();
    readers.allowed_tools = READERS_ALLOWED_TOOLS.map(String::from).to_vec();
    let reflect = ReflectLauncher {
        store: store.clone(),
        tools_root: env.tools_root.clone(),
        proposals_file: Some(proposals_file_from_env(&env.tools_root)),
        clock: Arc::new(SystemClock),
        commit: true,
        inner: Arc::new(claude),
        readers: Arc::new(readers),
    };
    let launcher = Arc::new(ReopenVault {
        env: env.clone(),
        inner: Arc::new(reflect),
    });
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
