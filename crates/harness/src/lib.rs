//! `harness` CLI and `harnessd` (docs/01 H-7, H-5; D-106, D-107, D-116). Owned by area core (T-08).
//!
//! * [`cli`]: the H-7 subcommands plus `harness ask <run> <n> yes|no` (D-105).
//! * [`daemon`]: harnessd, the `harness_core::sched` loop with a watcher on the
//!   schedules-changed marker.
//! * [`launcher`]: [`ClaudeLauncher`], the default `harness_core::sched::Launcher` (D-106).
//! * [`reflect`]: [`reflect::ReflectLauncher`], which runs `reflect` schedules (H-5, G-3, T-16).
//! * [`proposals`]: the T-7 / T-8 proposals reflect writes (D-142).
//!
//! The binaries are thin: they read [`Env`] from the process environment, call into this
//! library, and turn errors into a scrubbed message and exit code 1 (`anyhow` only there).

pub mod cli;
pub mod daemon;
pub mod launcher;
pub mod proposals;
pub mod reflect;

pub use harness_core::sched::{LaunchExit, LaunchRequest, Launcher};
pub use launcher::ClaudeLauncher;

use harness_core::run::RunId;
use harness_core::vault::{self, KeySource, Vault, VaultError};
use std::path::PathBuf;
use std::sync::Arc;

/// Everything a command reads from the environment, so tests can build one directly.
#[derive(Debug, Clone)]
pub struct Env {
    /// `DATABASE_URL`, default `core_pg::DEFAULT_DATABASE_URL`. Never printed.
    pub database_url: String,
    /// `SB_RUNS_ROOT`, default `runs`.
    pub runs_root: PathBuf,
    /// `SB_TOOLS_ROOT`, default `tools`.
    pub tools_root: PathBuf,
    /// `SB_VAULT_PATH` or the config dir (D-101).
    pub vault_path: PathBuf,
    /// The OS keychain (D-101); tests use an identity file.
    pub vault_key: KeySource,
    /// `SB_RUN_ID`: the run this command runs inside (a Claude Code session's tool call).
    pub run_id: Option<RunId>,
    /// `SB_CLAUDE`: the Claude Code executable, default `claude`.
    pub claude: PathBuf,
}

impl Env {
    pub fn from_env() -> Result<Env, CliError> {
        let var = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        Ok(Env {
            database_url: var("DATABASE_URL")
                .unwrap_or_else(|| core_pg::DEFAULT_DATABASE_URL.to_owned()),
            runs_root: var("SB_RUNS_ROOT").map_or_else(|| PathBuf::from("runs"), PathBuf::from),
            tools_root: var("SB_TOOLS_ROOT").map_or_else(|| PathBuf::from("tools"), PathBuf::from),
            vault_path: vault::default_path()?,
            vault_key: vault::default_key(),
            run_id: var("SB_RUN_ID").map(RunId),
            claude: var("SB_CLAUDE").map_or_else(|| PathBuf::from("claude"), PathBuf::from),
        })
    }

    /// Opens the vault and installs it as the process-wide scrubber (H-2). Commands that
    /// write run files or print ledger text call this first and fail closed if it fails.
    pub fn install_vault(&self) -> Result<Arc<Vault>, CliError> {
        let v = Vault::open(&self.vault_path, &self.vault_key)?;
        v.install_global();
        Ok(Arc::new(v))
    }

    pub async fn connect(&self) -> Result<core_pg::PgStore, CliError> {
        Ok(core_pg::PgStore::connect(&self.database_url).await?)
    }
}

/// Typed errors of the library (the binaries wrap them in `anyhow`). No variant ever
/// carries a vault value; messages are scrubbed again before printing.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(transparent)]
    Memory(#[from] harness_core::memory::MemoryError),
    #[error(transparent)]
    Sched(#[from] harness_core::sched::SchedError),
    #[error("vault: {0}")]
    Vault(#[from] VaultError),
    #[error("tools: {0}")]
    Store(#[from] toolstore::StoreError),
    #[error("ledger: {0}")]
    Ledger(#[from] harness_core::ledger::LedgerError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("watching for schedule changes: {0}")]
    Watch(#[from] notify::Error),
    #[error("database: {0}")]
    Db(String),
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    NotFound(String),
}
