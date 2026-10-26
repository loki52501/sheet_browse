//! Watch schedules (H-5 watch, M-1(5), M-3 baselines, D-140). Owned by area core (T-16).
//!
//! After a watch firing's session ends, [`crate::sched::Runner::execute`] reads the state the
//! session wrote to `files/watch.txt`, normalises it ([`normalise`]: whitespace runs become
//! one space, ends trimmed), scrubs it against the vault, hashes it (sha256), and compares
//! the hash with `memory.baselines[watch_target]`:
//!
//! * no baseline yet: the baseline is set silently (first observation, nothing is sent);
//! * same hash: `report.md` says "no change" and nothing is sent;
//! * other hash: one notification (`files/notification.md` in the run, one line appended to
//!   `<runs_root>/notifications.md`, one stderr line), then the new baseline is stored;
//! * no or empty `watch.txt`, a failed session or a stopped run: an `ok: false`
//!   `watch.check` event, no notification, the baseline is left alone.
//!
//! Every string written to the notification files, the ledger or the baseline is scrubbed
//! first (rule 2). Comparing the output shape of the tools is deferred (D-140 (5)).

use crate::ledger::{Ledger, CORE_SERVER};
use crate::memory::{Baseline, LedgerEvent, MemoryStore, Schedule, Ts};
use crate::run::RunFolder;
use crate::sched::Result;
use crate::vault::scrub;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

/// File under the run's `files/` where a watch session writes the watched state.
pub const WATCH_FILE: &str = "watch.txt";
/// File under the run's `files/` written when the watched state changed.
pub const NOTIFICATION_FILE: &str = "notification.md";
/// File under the runs root that gets one line per change notification.
pub const NOTIFICATIONS_LOG: &str = "notifications.md";
/// `tool` of the core ledger event of every watch check.
pub const CHECK_TOOL: &str = "watch.check";
/// `tool` of the core ledger event written when a change was notified.
pub const NOTIFY_TOOL: &str = "watch.notify";

/// Characters of the watched state kept in the baseline snapshot and the notification.
const STATE_MAX: usize = 4000;

/// Whitespace runs become one space; leading and trailing whitespace goes.
pub fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lowercase hex sha256 of `state`.
pub fn state_hash(state: &str) -> String {
    let digest = Sha256::digest(state.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `memory.baselines` key of a watch schedule: its `watch_target`, or `schedule-<id>`
/// when the row has none (the CLI requires one; a row written another way may not).
pub fn target_key(s: &Schedule) -> String {
    match s.watch_target.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => t.to_owned(),
        _ => format!("schedule-{}", s.id),
    }
}

/// What a watch check found.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WatchOutcome {
    /// No baseline existed; it was set and nothing was sent.
    First,
    /// Same state as the baseline; nothing was sent.
    Unchanged,
    /// The state changed: notified once, baseline updated. `notification` is the run-relative
    /// path (`runs/<id>/files/notification.md`).
    Changed { notification: String },
    /// The session wrote no (or an empty) `files/watch.txt`.
    Missing,
    /// Not checked (the session failed or the run stopped); the reason.
    Skipped(String),
}

/// The target checked and what was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchCheck {
    pub target: String,
    pub outcome: WatchOutcome,
}

impl WatchCheck {
    /// The `Watch:` line of `report.md` (scrubbed, ends without a newline).
    pub fn report_line(&self) -> String {
        let t = &self.target;
        let what = match &self.outcome {
            WatchOutcome::First => "first observation, baseline set, nothing sent".to_owned(),
            WatchOutcome::Unchanged => "no change, nothing sent".to_owned(),
            WatchOutcome::Changed { notification } => format!("changed, notified ({notification})"),
            WatchOutcome::Missing => {
                format!("no state in files/{WATCH_FILE}, baseline untouched, nothing sent")
            }
            WatchOutcome::Skipped(why) => {
                format!("not checked ({why}), baseline untouched, nothing sent")
            }
        };
        scrub(&format!("Watch: {t}: {what}"))
    }
}

fn event(tool: &str, args: serde_json::Value, ok: bool, summary: String) -> LedgerEvent {
    LedgerEvent {
        run_id: None,
        ts: crate::memory::ts_now(),
        server: CORE_SERVER.into(),
        tool: tool.into(),
        class: None,
        mode: None,
        args,
        result_summary: summary.clone(),
        consequences: json!({}),
        duration_ms: 0,
        ok,
        why: Some(summary),
    }
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}...", &s[..i]),
        None => s.to_owned(),
    }
}

/// Appends one line to `<runs_root>/notifications.md` with a single write.
fn append_log(runs_root: &Path, line: &str) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(runs_root.join(NOTIFICATIONS_LOG))?;
    f.write_all(format!("{line}\n").as_bytes())
}

/// The watch check of one firing (see the module docs). `skip` is why the state must not
/// be trusted (the session failed, the run stopped). Errors: the store, the ledger, or a
/// notification file could not be written; the baseline is then left alone, so the next
/// firing notifies again rather than never.
pub(crate) async fn check(
    store: &dyn MemoryStore,
    ledger: &Ledger,
    runs_root: &Path,
    s: &Schedule,
    now: Ts,
    skip: Option<String>,
) -> Result<WatchCheck> {
    let run: &RunFolder = ledger.run();
    let target = target_key(s);
    let shown = scrub(&target);
    let done = |outcome| WatchCheck {
        target: shown.clone(),
        outcome,
    };
    if let Some(why) = skip {
        let why = scrub(&why);
        let args = json!({ "target": shown, "schedule": s.id });
        ledger
            .write(event(
                CHECK_TOOL,
                args,
                false,
                format!("not checked: {why}"),
            ))
            .await?;
        return Ok(done(WatchOutcome::Skipped(why)));
    }

    let path = run.files().join(WATCH_FILE);
    let raw = match std::fs::read(&path) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    // Scrubbed before hashing: the baseline and the notification only ever hold the
    // `@vault(name)` form (rule 2).
    let state = scrub(&normalise(&raw));
    if state.is_empty() {
        let args = json!({ "target": shown, "schedule": s.id });
        let why = format!("no state in files/{WATCH_FILE}; baseline untouched");
        ledger.write(event(CHECK_TOOL, args, false, why)).await?;
        return Ok(done(WatchOutcome::Missing));
    }
    let hash = state_hash(&state);
    let before = store.baseline_get(&target).await?;
    let outcome = match &before {
        None => WatchOutcome::First,
        Some(b) if b.hash == hash => WatchOutcome::Unchanged,
        Some(_) => WatchOutcome::Changed {
            notification: format!("runs/{}/files/{NOTIFICATION_FILE}", run.id),
        },
    };

    if let WatchOutcome::Changed { notification } = &outcome {
        let prev = before.as_ref().map(|b| b.updated.to_rfc3339());
        let text = format!(
            "# Watch: {shown} changed\n\nRun: {id}\nSchedule: {sid}\nPrevious state seen: {prev}\n\n## Current state\n\n{state}\n",
            id = run.id,
            sid = s.id,
            prev = prev.as_deref().unwrap_or("unknown"),
            state = clip(&state, STATE_MAX),
        );
        std::fs::write(run.files().join(NOTIFICATION_FILE), scrub(&text))?;
        let line = format!(
            "- {} watch {shown} changed: {notification}",
            now.to_rfc3339()
        );
        append_log(runs_root, &scrub(&line))?;
    }

    let summary = match &outcome {
        WatchOutcome::First => "first observation; baseline set",
        WatchOutcome::Unchanged => "no change",
        _ => "changed",
    };
    let args = json!({
        "target": shown,
        "schedule": s.id,
        "hash": hash,
        "baseline": before.as_ref().map(|b| b.hash.clone()),
    });
    ledger
        .write(event(CHECK_TOOL, args, true, summary.to_owned()))
        .await?;
    if let WatchOutcome::Changed { notification } = &outcome {
        let args = json!({ "target": shown, "schedule": s.id, "file": notification });
        let mut ev = event(NOTIFY_TOOL, args, true, format!("notified: {notification}"));
        ev.consequences = json!({ "files": [notification] });
        ledger.write(ev).await?;
        // The message half of "notify once (file + message)" (H-5): harnessd's stderr.
        eprintln!(
            "{}",
            scrub(&format!("watch {shown} changed: {notification}"))
        );
    }
    if before.is_none() || matches!(outcome, WatchOutcome::Changed { .. }) {
        let b = Baseline {
            hash,
            snapshot: json!({ "text": clip(&state, STATE_MAX), "run": run.id.0 }),
            updated: now,
        };
        store.baseline_set(&target, b).await?;
    }
    Ok(done(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_collapses_whitespace() {
        assert_eq!(normalise("  a\n\n b\t c  \r\n"), "a b c");
        assert_eq!(normalise(" \n\t "), "");
    }

    #[test]
    fn hash_is_sha256_hex() {
        let h = state_hash("abc");
        assert_eq!(h.len(), 64);
        assert!(h.starts_with("ba7816bf"), "{h}");
        assert_eq!(state_hash(&normalise("a  b")), state_hash("a b"));
    }

    #[test]
    fn target_key_falls_back_to_schedule_id() {
        let mut s = Schedule {
            id: 7,
            cron: "* * * * *".into(),
            prompt: "p".into(),
            kind: crate::memory::ScheduleKind::Watch,
            watch_target: Some(" jobs ".into()),
            enabled: true,
            last_run: None,
            run_id: None,
        };
        assert_eq!(target_key(&s), "jobs");
        s.watch_target = Some("  ".into());
        assert_eq!(target_key(&s), "schedule-7");
        s.watch_target = None;
        assert_eq!(target_key(&s), "schedule-7");
    }
}
