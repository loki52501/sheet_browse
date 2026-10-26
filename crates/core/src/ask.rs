//! File-channel asker for tool server processes (D-105, D-119). Owned by area core (T-07).
//!
//! A server launched by Claude Code owns stdio for MCP, so it cannot prompt. [`FileAsker`]
//! writes the card (scrubbed) to `runs/<run>/asks/<n>.json`, prints one notice line to
//! stderr, and waits for `runs/<run>/asks/<n>.answer` (`yes` or `no`), written by
//! `harness ask <run> <n> yes|no` through [`answer`]. The wait is a `notify` watch on the
//! `asks/` folder capped at `card.timeout`; no polling (rule 6). On timeout the card's
//! `on_timeout` applies.
//!
//! The card is published whole (D-123): written to a temp file, then hard-linked to
//! `<n>.json`, which is atomic and no-clobber, so a reader never sees an empty or partial
//! card and several processes on one run never share one `n` (a taken `n` moves on to the
//! next). If the write fails, no card is left behind.
//!
//! Fail closed (D-119 (4)): if the card cannot be written, nobody saw it, so the ask is
//! `TimeoutSkip` whatever the card says. `<n>.answer` is claimed no-clobber (temp file and
//! `hard_link`) by whoever closes the ask first: the user's answer, or the asker's own
//! timeout, which writes `timeout`. So [`answer`] refuses an ask that is already closed
//! instead of writing an answer nobody reads. The asker claims `<n>.answer` on every path
//! where it stops waiting (D-123): its timer, a dropped ask future (the wrapper's cap), a
//! card that landed after the asker gave up, and, if the temp-file claim fails, a bare
//! `create_new` as a last try. If even that fails the ask is `TimeoutSkip`: a late answer
//! might still be taken, so nothing proceeds on the timeout. [`answer`] also refuses a
//! card file that is not a complete JSON object.

use crate::gate::{AskCard, AskOutcome, Asker, OnTimeout};
use crate::mcp::clean_line;
use crate::run::{RunFolder, RunId};
use crate::vault::Vault;
use async_trait::async_trait;
use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// What the asker writes into `<n>.answer` when the card times out unanswered.
const TIMED_OUT: &str = "timeout";

/// A published card whose `<n>.answer` the asker has not settled yet. Dropping it while
/// still open (the ask future was dropped, e.g. by the wrapper's cap, or the card landed
/// after the asker stopped waiting) closes the ask with `timeout`, best effort, so a late
/// `harness ask` is refused instead of reported as taken (D-123).
struct OpenAsk {
    n: u64,
    answer: PathBuf,
    open: bool,
}

impl OpenAsk {
    fn settled(&mut self) {
        self.open = false;
    }
}

impl Drop for OpenAsk {
    fn drop(&mut self) {
        if self.open {
            // Inline rather than on the blocking pool: a few small file calls, and the ask
            // is closed before the dropping caller (the wrapper's cap) reports its skip.
            let _ = close_answer(&self.answer);
        }
    }
}

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

    /// The card document with `n = 0`; [`write_card`] fills in the number it claims.
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

    /// Creates `asks/` and publishes the card off the async executor. `None` = nobody can
    /// see the card (already reported on stderr). The [`OpenAsk`] is made on the blocking
    /// thread the moment the card exists and handed over by a oneshot: if this future is
    /// dropped first, the guard is dropped with the channel and closes the ask.
    async fn publish(&self, card: &AskCard) -> Option<OpenAsk> {
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
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let published = write_card(&run_dir, &next, doc).map(|n| OpenAsk {
                n,
                answer: answer_path(&run_dir, n),
                open: true,
            });
            // A failed send hands the guard back and drops it here: the ask is closed.
            let _ = tx.send(published);
        });
        let err = match rx.await {
            Ok(Ok(open)) => return Some(open),
            Ok(Err(e)) => e,
            Err(_) => io::Error::other("ask: file task failed"),
        };
        self.diag(&format!(
            "ask: cannot write the card: {err}; skipped, nobody saw it"
        ));
        None
    }

    /// The card's timer ran out (or nothing more can arrive): claim `<n>.answer` with
    /// `timeout`. If the user's answer got there first, it counts.
    async fn close(&self, card: &AskCard, open: &mut OpenAsk) -> AskOutcome {
        let (n, path) = (open.n, open.answer.clone());
        let p = path.clone();
        let closed = blocking(move || close_answer(&p)).await;
        open.settled();
        match closed {
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
                    // Not closed: a late answer may still be taken, so never proceed.
                    None => AskOutcome::TimeoutSkip,
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

/// Publishes `doc` as `<n>.json` for the next free `n` through [`claim`]: the whole card
/// appears at once, and a taken `n` (another ask, another process) moves on to the next.
fn write_card(run_dir: &Path, next: &AtomicU64, mut doc: CardFile) -> io::Result<u64> {
    let mut n = next.load(Ordering::Relaxed).max(1);
    loop {
        doc.n = n;
        doc.answer_with = format!("harness ask {} {n} yes|no", doc.run);
        let mut text = serde_json::to_string_pretty(&doc).map_err(io::Error::other)?;
        text.push('\n');
        match claim(&card_path(run_dir, n), &text) {
            Ok(()) => {
                next.fetch_max(n + 1, Ordering::Relaxed);
                return Ok(n);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => return Err(e),
        }
    }
}

/// Closes an ask: claims `answer` with `timeout`. If the temp-file claim fails for an I/O
/// reason, a bare `create_new` still closes it (an empty answer is not a yes or no, and
/// [`answer`] refuses a closed ask). `AlreadyExists` when the ask was already closed.
fn close_answer(answer: &Path) -> io::Result<()> {
    close_answer_with(answer, claim)
}

/// [`close_answer`] with the temp-file claim passed in, so a test can make it fail.
fn close_answer_with(
    answer: &Path,
    claim: impl FnOnce(&Path, &str) -> io::Result<()>,
) -> io::Result<()> {
    let err = match claim(answer, &format!("{TIMED_OUT}\n")) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    if err.kind() == io::ErrorKind::AlreadyExists {
        return Err(err);
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(answer)
    {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
        Err(_) => Err(err),
    }
}

/// Writes `text` to `target` only if `target` does not exist yet: a temp file, then
/// `hard_link` (atomic and no-clobber, so a reader never sees a partial file). Where the
/// filesystem has no hard links, `create_new` claims the name and one rename replaces the
/// empty placeholder with the whole temp file: still no-clobber, and a reader sees an empty
/// file or all of `text`, never a part (an empty answer is no answer yet, an empty card is
/// refused by [`answer`]). On failure the temp file is removed and no placeholder is left.
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
    let claimed =
        std::fs::write(&tmp, text).and_then(|()| match std::fs::hard_link(&tmp, target) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists || target.exists() => {
                Err(io::Error::new(io::ErrorKind::AlreadyExists, e.to_string()))
            }
            Err(_) => claim_without_links(&tmp, target),
        });
    // Gone already after a rename; otherwise the link (if any) keeps the bytes.
    let _ = std::fs::remove_file(&tmp);
    claimed
}

/// [`claim`] where hard links are not available: an empty `create_new` placeholder takes
/// the name, then the full temp file replaces it in one rename. The placeholder is removed
/// if the rename fails.
fn claim_without_links(tmp: &Path, target: &Path) -> io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    std::fs::rename(tmp, target).inspect_err(|_| {
        let _ = std::fs::remove_file(target);
    })
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
        // From here on, `open` closes the ask if this future is dropped (D-123).
        let Some(mut open) = self.publish(card).await else {
            return AskOutcome::TimeoutSkip;
        };
        let n = open.n;
        // The watch is armed after the card exists; the one read right after arming catches
        // an answer that landed in between, so none can slip past. It is armed on the
        // blocking pool, raced against the card's deadline: on Windows `watch` blocks until
        // the watcher's own thread acks, which can take a while on a loaded machine, and
        // must neither hold an executor thread nor stretch the ask past its timer. If the
        // deadline wins, the watcher is dropped on the blocking thread when it is ready.
        let dir = asks_dir(&self.run_dir);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let arm = tokio::task::spawn_blocking(move || {
            notify::recommended_watcher(move |_res: notify::Result<notify::Event>| {
                let _ = tx.send(());
            })
            .and_then(|mut w| w.watch(&dir, RecursiveMode::NonRecursive).map(|()| w))
            .map_err(|e| e.to_string())
        });
        let path = open.answer.clone();
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

        let armed = match tokio::time::timeout_at(deadline, arm).await {
            // The timer ran out before the watch was armed: close the ask (an answer that
            // landed is read there).
            Err(_) => return self.close(card, &mut open).await,
            Ok(Ok(armed)) => armed,
            // A failed join becomes a fixed text (its message could carry a panic payload).
            Ok(Err(_)) => Err("watch task failed".to_owned()),
        };
        let _watcher = match armed {
            Ok(w) => w,
            Err(e) => {
                self.diag(&format!("ask {n}: cannot watch for the answer: {e}"));
                // No file events available: wait out the card's timer (the deadline is the
                // condition), then close the ask, which reads an answer that landed.
                tokio::time::sleep_until(deadline).await;
                return self.close(card, &mut open).await;
            }
        };

        // The watch is armed: one check catches an answer that landed before it was.
        if let Some(yes) = read_answer(&path).await {
            open.settled();
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
            Ok(Some(yes)) => {
                open.settled();
                answered(yes)
            }
            // Timer ran out, or the watcher stopped delivering events (nothing more can
            // arrive that we could see): close the ask.
            Ok(None) | Err(_) => self.close(card, &mut open).await,
        }
    }
}

/// `harness ask <run> <n> yes|no`: claims `runs_root/<run>/asks/<n>.answer` (temp file,
/// then a no-clobber link). `NotFound` if the run folder or `asks/<n>.json` does not exist;
/// `InvalidData` if `asks/<n>.json` is not a complete JSON object (nothing is answered for
/// a card nobody could read); `InvalidInput` if `run` is not a plain folder name;
/// `AlreadyExists` if the ask is already closed (answered, or timed out: then nothing was
/// done with this answer).
pub fn answer(runs_root: &Path, run: &RunId, n: u64, yes: bool) -> io::Result<()> {
    let folder = RunFolder::open(runs_root, run)?;
    let card = card_path(&folder.dir, n);
    if !card.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no ask {n} in run {run}"),
        ));
    }
    let complete = std::fs::read(&card)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .is_some_and(|v| v.is_object());
    if !complete {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("ask {n} in run {run} is not a complete card; nothing was answered"),
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

    /// A fresh folder under the OS temp dir.
    fn tmp_dir(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sb-ask-{tag}-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            chrono::Utc::now().timestamp_micros()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// D-123 (1), the no-hard-link path: the target holds the whole text, the temp file is
    /// gone, and a second claim is `AlreadyExists` and leaves the first text alone.
    #[test]
    fn claim_without_links_publishes_whole_and_no_clobber() {
        let dir = tmp_dir("nolink");
        let target = dir.join("1.json");
        let text = format!("{{\"n\": 1, \"what\": \"{}\"}}\n", "x".repeat(100_000));
        let tmp = dir.join(".1.json.a.tmp");
        std::fs::write(&tmp, &text).unwrap();
        claim_without_links(&tmp, &target).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), text);
        assert!(!tmp.exists(), "the temp file was renamed away");
        assert_eq!(names(&dir), vec!["1.json".to_owned()]);

        let tmp2 = dir.join(".1.json.b.tmp");
        std::fs::write(&tmp2, "{\"n\": 2}\n").unwrap();
        let err = claim_without_links(&tmp2, &target).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), text);
        assert!(
            tmp2.exists(),
            "the caller (claim) removes its own temp file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D-123 (1): a failed rename removes the placeholder, so no empty card or answer is
    /// left behind, and the name can be claimed again.
    #[test]
    fn claim_without_links_failed_rename_leaves_no_placeholder() {
        let dir = tmp_dir("nolink-fail");
        let target = dir.join("1.answer");
        let missing = dir.join(".1.answer.missing.tmp");
        let err = claim_without_links(&missing, &target).unwrap_err();
        assert_ne!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(
            !target.exists(),
            "placeholder removed after the failed rename"
        );
        assert!(names(&dir).is_empty(), "{:?}", names(&dir));

        let tmp = dir.join(".1.answer.ok.tmp");
        std::fs::write(&tmp, "yes\n").unwrap();
        claim_without_links(&tmp, &target).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "yes\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D-123 (3): when the temp-file claim fails for an I/O reason, a bare `create_new`
    /// still closes the ask with an empty answer (no yes or no), and a late reply is refused.
    #[test]
    fn close_answer_falls_back_to_create_new() {
        let dir = tmp_dir("close-fallback");
        let target = dir.join("1.answer");
        let denied = |_: &Path, _: &str| -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "no temp file",
            ))
        };
        close_answer_with(&target, denied).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "");
        assert_eq!(parse_answer(""), None, "an empty answer is not an answer");
        let late = claim(&target, "yes\n").unwrap_err();
        assert_eq!(late.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "");

        // Already closed: the fallback reports AlreadyExists and changes nothing.
        let err = close_answer_with(&target, denied).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "");

        // Both fail (no folder): the claim's own error comes back and nothing is created.
        let gone = dir.join("missing").join("1.answer");
        let err = close_answer_with(&gone, denied).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(!gone.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn proceed_card() -> AskCard {
        AskCard {
            class: crate::class::Class::Account,
            what: "change the email".into(),
            site: None,
            values: Vec::new(),
            timeout: std::time::Duration::from_millis(1),
            on_timeout: OnTimeout::Proceed,
        }
    }

    /// D-123 (3): a close that fails (the ask could not be claimed, so a late answer may
    /// still be taken) is `TimeoutSkip`, even for a card that proceeds on timeout. Controls:
    /// the same card closed normally proceeds, and a closed but unreadable answer skips.
    #[tokio::test]
    async fn a_failed_close_is_timeout_skip() {
        let root = tmp_dir("close-skip");
        let fa = FileAsker::new(
            &root,
            RunId("2026-10-25T00-00-00-close".into()),
            Arc::new(Vault::in_memory([])),
        );
        let card = proceed_card();

        let mut open = OpenAsk {
            n: 1,
            answer: root.join("no-such-folder").join("1.answer"),
            open: true,
        };
        assert_eq!(fa.close(&card, &mut open).await, AskOutcome::TimeoutSkip);
        assert!(!open.open, "settled: the guard does not retry on drop");
        assert!(!open.answer.exists());

        let mut open = OpenAsk {
            n: 2,
            answer: root.join("2.answer"),
            open: true,
        };
        assert_eq!(fa.close(&card, &mut open).await, AskOutcome::TimeoutProceed);
        assert_eq!(
            std::fs::read_to_string(&open.answer).unwrap(),
            format!("{TIMED_OUT}\n")
        );

        std::fs::write(root.join("3.answer"), "").unwrap();
        let mut open = OpenAsk {
            n: 3,
            answer: root.join("3.answer"),
            open: true,
        };
        assert_eq!(fa.close(&card, &mut open).await, AskOutcome::TimeoutSkip);
        let _ = std::fs::remove_dir_all(&root);
    }
}
