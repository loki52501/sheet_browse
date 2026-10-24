//! File-channel asker for tool server processes (D-105, D-119). Owned by area core (T-07).
//!
//! A server launched by Claude Code owns stdio for MCP, so it cannot prompt. [`FileAsker`]
//! writes the card (scrubbed) to `runs/<run>/asks/<n>.json`, prints one notice line to
//! stderr, and waits for `runs/<run>/asks/<n>.answer` (`yes` or `no`), written by
//! `harness ask <run> <n> yes|no` through [`answer`]. The wait is a `notify` watch on the
//! `asks/` folder capped at `card.timeout`; no polling (rule 6). On timeout the card's
//! `on_timeout` applies.
//!
//! Fail closed (D-119 (4)): if the card cannot be written, nobody saw it, so the ask is
//! `TimeoutSkip` whatever the card says. `<n>.answer` is claimed no-clobber (temp file and
//! `hard_link`) by whoever closes the ask first: the user's answer, or the asker's own
//! timeout, which writes `timeout`. So [`answer`] refuses an ask that is already closed
//! instead of writing an answer nobody reads.

use crate::gate::{AskCard, AskOutcome, Asker, OnTimeout};
use crate::mcp::clean_line;
use crate::run::{RunFolder, RunId};
use crate::vault::Vault;
use async_trait::async_trait;
use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// What the asker writes into `<n>.answer` when the card times out unanswered.
const TIMED_OUT: &str = "timeout";

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

/// The `asks/<n>.json` document. Every string is scrubbed before it is built.
#[derive(Serialize)]
struct CardFile {
    n: u64,
    run: String,
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
    next: Arc<AtomicU64>,
}

impl FileAsker {
    /// `runs_root/<run>/` need not exist yet; `asks/` is created on the first ask.
    /// `vault` scrubs the card before it is written or printed.
    pub fn new(runs_root: &Path, run: RunId, vault: Arc<Vault>) -> FileAsker {
        FileAsker {
            run_dir: runs_root.join(&run.0),
            run,
            vault,
            next: Arc::new(AtomicU64::new(1)),
        }
    }

    /// One scrubbed, cleaned line on stderr (stdout carries only JSON-RPC).
    fn diag(&self, msg: &str) {
        eprintln!("{}", clean_line(&self.vault, msg));
    }

    /// The card document with `n = 0`; [`write_card`] fills in the claimed number.
    fn card_doc(&self, card: &AskCard) -> CardFile {
        let scrub = |s: &str| self.vault.scrub(s);
        CardFile {
            n: 0,
            run: self.run.0.clone(),
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
            answer_with: String::new(),
        }
    }

    /// Creates `asks/` and writes the card off the async executor. `None` = nobody can see
    /// the card (already reported on stderr).
    async fn publish(&self, card: &AskCard) -> Option<u64> {
        let dir = asks_dir(&self.run_dir);
        let d = dir.clone();
        if let Err(e) = blocking(move || std::fs::create_dir_all(&d)).await {
            self.diag(&format!(
                "ask: cannot create {}: {e}; skipped, nobody saw the card",
                dir.display()
            ));
            return None;
        }
        let doc = self.card_doc(card);
        let (run_dir, next) = (self.run_dir.clone(), self.next.clone());
        match blocking(move || write_card(&run_dir, &next, doc)).await {
            Ok(n) => Some(n),
            Err(e) => {
                self.diag(&format!(
                    "ask: cannot write the card: {e}; skipped, nobody saw it"
                ));
                None
            }
        }
    }

    /// The card's timer ran out (or nothing more can arrive): claim `<n>.answer` with
    /// `timeout`. If the user's answer got there first, it counts.
    async fn close(&self, card: &AskCard, n: u64) -> AskOutcome {
        let path = answer_path(&self.run_dir, n);
        let p = path.clone();
        match blocking(move || claim(&p, &format!("{TIMED_OUT}\n"))).await {
            Ok(()) => timed_out(card),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => match read_answer(&path).await {
                Some(yes) => answered(yes),
                // Claimed, but not a yes or no: never proceed on something unreadable.
                None => AskOutcome::TimeoutSkip,
            },
            Err(e) => {
                self.diag(&format!("ask {n}: cannot close the ask: {e}"));
                match read_answer(&path).await {
                    Some(yes) => answered(yes),
                    None => timed_out(card),
                }
            }
        }
    }
}

/// Runs blocking file I/O on the blocking pool. A failed join becomes a fixed error (its
/// message could carry a panic payload).
async fn blocking<T, F>(f: F) -> io::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|_| Err(io::Error::other("ask: file task failed")))
}

/// Claims the next free `n` by creating `<n>.json` with `create_new`, then writes `doc`.
fn write_card(run_dir: &Path, next: &AtomicU64, mut doc: CardFile) -> io::Result<u64> {
    let mut n = next.load(Ordering::Relaxed).max(1);
    loop {
        let path = card_path(run_dir, n);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                next.store(n + 1, Ordering::Relaxed);
                doc.n = n;
                doc.answer_with = format!("harness ask {} {n} yes|no", doc.run);
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

/// Writes `text` to `target` only if `target` does not exist yet: a temp file, then
/// `hard_link` (atomic and no-clobber, so a reader never sees a partial file). Where the
/// filesystem has no hard links it falls back to `create_new`, still no-clobber; a reader
/// can then see the file before its bytes, which [`parse_answer`] reads as no answer yet.
/// `AlreadyExists` when `target` exists.
fn claim(target: &Path, text: &str) -> io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "answer path has no folder"))?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, text)?;
    let linked = std::fs::hard_link(&tmp, target);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists || target.exists() => {
            Err(io::Error::new(io::ErrorKind::AlreadyExists, e.to_string()))
        }
        Err(_) => {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)?;
            f.write_all(text.as_bytes())
        }
    }
}

/// `yes`/`no` (also `y`/`n`, any case, surrounding whitespace ignored); anything else is
/// not an answer.
fn parse_answer(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "yes" | "y" => Some(true),
        "no" | "n" => Some(false),
        _ => None,
    }
}

async fn read_answer(path: &Path) -> Option<bool> {
    tokio::fs::read_to_string(path)
        .await
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
        // Nobody saw a card that could not be written: never proceed on that (D-119 (4)).
        let Some(n) = self.publish(card).await else {
            return AskOutcome::TimeoutSkip;
        };
        // The watch is armed after the card exists; the one read right after arming catches
        // an answer that landed in between, so none can slip past.
        let dir = asks_dir(&self.run_dir);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let watcher = notify::recommended_watcher(move |_res: notify::Result<notify::Event>| {
            let _ = tx.send(());
        })
        .and_then(|mut w| w.watch(&dir, RecursiveMode::NonRecursive).map(|()| w));
        let path = answer_path(&self.run_dir, n);
        eprintln!(
            "{}",
            clean_line(
                &self.vault,
                &format!(
                    "ask {n} ({}): {} | answer: harness ask {} {n} yes|no (on timeout in {}s: {})",
                    card.class,
                    card.what,
                    self.run.0,
                    card.timeout.as_secs(),
                    match card.on_timeout {
                        OnTimeout::Proceed => "proceed",
                        OnTimeout::Skip => "skip",
                    }
                ),
            )
        );

        let _watcher = match watcher {
            Ok(w) => w,
            Err(e) => {
                self.diag(&format!("ask {n}: cannot watch for the answer: {e}"));
                // No file events available: wait out the card's timer (the deadline is the
                // condition), then close the ask, which reads an answer that landed.
                tokio::time::sleep_until(deadline).await;
                return self.close(card, n).await;
            }
        };

        // The watch is armed: one check catches an answer that landed before it was.
        if let Some(yes) = read_answer(&path).await {
            return answered(yes);
        }
        let wait = async {
            while rx.recv().await.is_some() {
                if let Some(yes) = read_answer(&path).await {
                    return Some(yes);
                }
            }
            None
        };
        match tokio::time::timeout_at(deadline, wait).await {
            Ok(Some(yes)) => answered(yes),
            // Timer ran out, or the watcher stopped delivering events (nothing more can
            // arrive that we could see): close the ask.
            Ok(None) | Err(_) => self.close(card, n).await,
        }
    }
}

/// `harness ask <run> <n> yes|no`: claims `runs_root/<run>/asks/<n>.answer` (temp file,
/// then a no-clobber link). `NotFound` if the run folder or `asks/<n>.json` does not exist;
/// `InvalidInput` if `run` is not a plain folder name; `AlreadyExists` if the ask is
/// already closed (answered, or timed out: then nothing was done with this answer).
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
    match claim(&target, if yes { "yes\n" } else { "no\n" }) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let prior = std::fs::read_to_string(&target).unwrap_or_default();
            let msg = match parse_answer(&prior) {
                Some(y) => format!(
                    "ask {n} in run {run} is already answered ({}); nothing changed",
                    if y { "yes" } else { "no" }
                ),
                None => format!(
                    "ask {n} in run {run} timed out before this answer; nothing was done with it"
                ),
            };
            Err(io::Error::new(io::ErrorKind::AlreadyExists, msg))
        }
        Err(e) => Err(e),
    }
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
        assert_eq!(parse_answer(TIMED_OUT), None);
    }

    #[test]
    fn claim_is_no_clobber() {
        let dir = std::env::temp_dir().join(format!(
            "sb-ask-claim-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_micros()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("1.answer");
        claim(&target, "yes\n").unwrap();
        let err = claim(&target, "no\n").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "yes\n");
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(left.len(), 1, "temp files are removed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
