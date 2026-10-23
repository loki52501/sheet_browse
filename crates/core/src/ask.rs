//! File-channel asker for tool server processes (D-105). Owned by area core (T-07).
//!
//! A server launched by Claude Code owns stdio for MCP, so it cannot prompt. [`FileAsker`]
//! writes the card (scrubbed) to `runs/<run>/asks/<n>.json`, prints one notice line to
//! stderr, and waits for `runs/<run>/asks/<n>.answer` (`yes` or `no`), written by
//! `harness ask <run> <n> yes|no` through [`answer`]. The wait is a `notify` watch on the
//! `asks/` folder capped at `card.timeout`; no polling (rule 6). On timeout the card's
//! `on_timeout` applies.

use crate::gate::{AskCard, AskOutcome, Asker, OnTimeout};
use crate::run::{RunFolder, RunId};
use crate::vault::Vault;
use async_trait::async_trait;
use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// `runs/<run>/asks/`.
pub fn asks_dir(run_dir: &Path) -> PathBuf {
    run_dir.join("asks")
}

/// `runs/<run>/asks/<n>.json`.
pub fn card_path(run_dir: &Path, n: u64) -> PathBuf {
    asks_dir(run_dir).join(format!("{n}.json"))
}

/// `runs/<run>/asks/<n>.answer`.
pub fn answer_path(run_dir: &Path, n: u64) -> PathBuf {
    asks_dir(run_dir).join(format!("{n}.answer"))
}

/// The `asks/<n>.json` document.
#[derive(Serialize)]
struct CardFile<'a> {
    n: u64,
    run: &'a str,
    class: crate::class::Class,
    what: String,
    site: Option<String>,
    values: Vec<(String, String)>,
    timeout_s: u64,
    on_timeout: OnTimeout,
    answer_with: String,
}

/// The D-105 asker: one per server process, bound to the run the process serves.
pub struct FileAsker {
    run_dir: PathBuf,
    run: RunId,
    vault: Arc<Vault>,
    /// Next `n` to try; `create_new` decides, so several processes on one run never share one.
    next: AtomicU64,
}

impl FileAsker {
    /// `runs_root/<run>/` need not exist yet; `asks/` is created on the first ask.
    /// `vault` scrubs the card before it is written or printed.
    pub fn new(runs_root: &Path, run: RunId, vault: Arc<Vault>) -> FileAsker {
        FileAsker {
            run_dir: runs_root.join(&run.0),
            run,
            vault,
            next: AtomicU64::new(1),
        }
    }

    /// Claims the next free `n` by creating `<n>.json` with `create_new`, then writes it.
    fn write_card(&self, card: &AskCard) -> io::Result<u64> {
        let dir = asks_dir(&self.run_dir);
        std::fs::create_dir_all(&dir)?;
        let scrub = |s: &str| self.vault.scrub(s);
        let mut n = self.next.load(Ordering::Relaxed).max(1);
        loop {
            let path = card_path(&self.run_dir, n);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    self.next.store(n + 1, Ordering::Relaxed);
                    let doc = CardFile {
                        n,
                        run: &self.run.0,
                        class: card.class,
                        what: scrub(&card.what),
                        site: card.site.as_deref().map(scrub),
                        values: card
                            .values
                            .iter()
                            .map(|(k, v)| (scrub(k), scrub(v)))
                            .collect(),
                        timeout_s: card.timeout.as_secs(),
                        on_timeout: card.on_timeout,
                        answer_with: format!("harness ask {} {n} yes|no", self.run.0),
                    };
                    let mut bytes = serde_json::to_vec_pretty(&doc).map_err(io::Error::other)?;
                    bytes.push(b'\n');
                    f.write_all(&bytes)?;
                    return Ok(n);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e),
            }
        }
    }
}

/// `yes`/`no` (also `y`/`n`, any case, surrounding whitespace ignored); anything else is
/// not an answer yet.
fn parse_answer(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "yes" | "y" => Some(true),
        "no" | "n" => Some(false),
        _ => None,
    }
}

fn read_answer(path: &Path) -> Option<bool> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| parse_answer(&t))
}

fn timed_out(card: &AskCard) -> AskOutcome {
    match card.on_timeout {
        OnTimeout::Proceed => AskOutcome::TimeoutProceed,
        OnTimeout::Skip => AskOutcome::TimeoutSkip,
    }
}

fn answered(yes: bool) -> AskOutcome {
    if yes {
        AskOutcome::AnsweredProceed
    } else {
        AskOutcome::AnsweredSkip
    }
}

#[async_trait]
impl Asker for FileAsker {
    async fn ask(&self, card: &AskCard) -> AskOutcome {
        let deadline = tokio::time::Instant::now() + card.timeout;

        // Arm the watch before the card exists, so an answer written right after the card
        // cannot slip between "card written" and "watch armed".
        let dir = asks_dir(&self.run_dir);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!(
                "{}",
                self.vault
                    .scrub(&format!("ask: cannot create {}: {e}", dir.display()))
            );
            return timed_out(card);
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let watcher = notify::recommended_watcher(move |_res: notify::Result<notify::Event>| {
            let _ = tx.send(());
        })
        .and_then(|mut w| w.watch(&dir, RecursiveMode::NonRecursive).map(|()| w));

        let n = match self.write_card(card) {
            Ok(n) => n,
            Err(e) => {
                eprintln!(
                    "{}",
                    self.vault
                        .scrub(&format!("ask: cannot write the card: {e}"))
                );
                return timed_out(card);
            }
        };
        let path = answer_path(&self.run_dir, n);
        eprintln!(
            "{}",
            self.vault.scrub(&format!(
                "ask {n} ({}): {} | answer: harness ask {} {n} yes|no (on timeout in {}s: {})",
                card.class,
                card.what,
                self.run.0,
                card.timeout.as_secs(),
                match card.on_timeout {
                    OnTimeout::Proceed => "proceed",
                    OnTimeout::Skip => "skip",
                }
            ))
        );

        let _watcher = match watcher {
            Ok(w) => w,
            Err(e) => {
                eprintln!(
                    "{}",
                    self.vault
                        .scrub(&format!("ask {n}: cannot watch for the answer: {e}"))
                );
                // No file events available: wait out the card's timer (the deadline is the
                // condition), then read the answer file once.
                tokio::time::sleep_until(deadline).await;
                return read_answer(&path)
                    .map(answered)
                    .unwrap_or_else(|| timed_out(card));
            }
        };

        // The watch is armed: one check catches an answer that landed before it was.
        if let Some(yes) = read_answer(&path) {
            return answered(yes);
        }
        let wait = async {
            while rx.recv().await.is_some() {
                if let Some(yes) = read_answer(&path) {
                    return Some(yes);
                }
            }
            None
        };
        match tokio::time::timeout_at(deadline, wait).await {
            Ok(Some(yes)) => answered(yes),
            // The watcher stopped delivering events: nothing more can arrive before the
            // deadline that we could see, so the card times out.
            Ok(None) | Err(_) => timed_out(card),
        }
    }
}

/// `harness ask <run> <n> yes|no`: writes `runs_root/<run>/asks/<n>.answer` atomically
/// (temp file, then rename). `NotFound` if the run folder or `asks/<n>.json` does not exist;
/// `InvalidInput` if `run` is not a plain folder name.
pub fn answer(runs_root: &Path, run: &RunId, n: u64, yes: bool) -> io::Result<()> {
    let folder = RunFolder::open(runs_root, run)?;
    let card = card_path(&folder.dir, n);
    if !card.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no ask {n} in run {run}"),
        ));
    }
    let target = answer_path(&folder.dir, n);
    let tmp = asks_dir(&folder.dir).join(format!(".{n}.answer.{}.tmp", std::process::id()));
    std::fs::write(&tmp, if yes { "yes\n" } else { "no\n" })?;
    std::fs::rename(&tmp, &target).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_parse() {
        assert_eq!(parse_answer(" YES\n"), Some(true));
        assert_eq!(parse_answer("n"), Some(false));
        assert_eq!(parse_answer("maybe"), None);
        assert_eq!(parse_answer(""), None);
    }
}
