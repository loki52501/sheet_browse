//! harnessd (H-5 Phase 1, D-107, D-116): the `harness_core::sched` loop plus a watcher on
//! `<runs_root>/.sched/`, which `harness schedule add|rm` writes, so a change re-reads the
//! schedules at once instead of on a poll interval (rule 6).

use crate::CliError;
use harness_core::memory::MemoryStore;
use harness_core::sched::{changed_dir, Clock, Fired, Launcher, Runner, Scheduler};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, Notify};

/// Calls `wake.notify_one()` on any event in `<runs_root>/.sched/` (created if missing).
/// Keep the watcher alive for as long as the loop runs.
pub fn watch_changes(runs_root: &Path, wake: Arc<Notify>) -> Result<RecommendedWatcher, CliError> {
    let dir = changed_dir(runs_root);
    std::fs::create_dir_all(&dir)?;
    // Errors from the watcher wake the loop too: a spurious re-read costs one query.
    let mut w = notify::recommended_watcher(move |_res: notify::Result<notify::Event>| {
        wake.notify_one();
    })?;
    w.watch(&dir, RecursiveMode::NonRecursive)?;
    Ok(w)
}

/// A scheduler with its change watcher already installed, so a change made right after
/// [`Daemon::new`] returns is never missed.
pub struct Daemon {
    scheduler: Scheduler,
    _watcher: RecommendedWatcher,
}

impl Daemon {
    pub fn new(
        store: Arc<dyn MemoryStore>,
        launcher: Arc<dyn Launcher>,
        clock: Arc<dyn Clock>,
        runs_root: PathBuf,
        fired: Option<mpsc::UnboundedSender<Fired>>,
    ) -> Result<Daemon, CliError> {
        let wake = Arc::new(Notify::new());
        let watcher = watch_changes(&runs_root, wake.clone())?;
        Ok(Daemon {
            scheduler: Scheduler {
                runner: Runner {
                    store,
                    launcher,
                    clock,
                    runs_root,
                },
                wake,
                fired,
            },
            _watcher: watcher,
        })
    }

    /// Runs until `shutdown` resolves, then waits for running firings to finish.
    pub async fn run(self, shutdown: impl Future<Output = ()> + Send) -> Result<(), CliError> {
        let Daemon {
            scheduler,
            _watcher,
        } = self;
        scheduler.run_loop(shutdown).await?;
        drop(_watcher);
        Ok(())
    }
}
