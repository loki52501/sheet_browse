//! Ledger (H-3) and `report.md` (Q-4). Owned by area core (T-06).
//!
//! The row type is `memory::LedgerEvent` (contract). [`Ledger::write`] writes it twice:
//! the DB first through `MemoryStore::append`, then one line in `runs/<run>/ledger.jsonl`
//! carrying the returned id, so the mirror matches the table row-for-row.
//!
//! # Event shapes [`report`] reads
//!
//! The wrapper (T-07) writes the events; the report derives everything from these fields:
//!
//! * a tool call: `server = <tool server>`, `tool`, `class`, `mode` = how the gate let it
//!   through (`allow`, `answered_proceed`, `timeout_proceed`; `None` counts as allowed),
//!   `ok`, `args` (the target is the first of `url href to path file id message_id thread
//!   query q domain`), `result_summary`, `consequences` (file paths anywhere in it end up
//!   on the `Files:` line);
//! * how an ask ended: `server = core`, `mode` = one of the four outcomes, `class` and
//!   `tool` of the asked call, `why` (or `result_summary`) = the card's what line;
//! * a stop (gate Stop, Challenge mid-call): `mode = stop`, `why` = the reason.

use crate::class::Class;
use crate::memory::{LedgerEvent, LedgerId, LedgerMode, MemoryError, MemoryStore, RunStatus, Ts};
use crate::run::{RunFolder, RunId};
use crate::vault::scrub;
use chrono::DateTime;
use serde::Serialize;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error(transparent)]
    Memory(#[from] MemoryError),
    /// The row is in the table but its jsonl line could not be written (or read).
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("event belongs to run {event}, this ledger writes run {ledger}")]
    WrongRun { event: RunId, ledger: RunId },
    #[error("serialize ledger line: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{}:{line}: {reason}", path.display())]
    Corrupt {
        path: PathBuf,
        line: usize,
        reason: String,
    },
}

pub type Result<T, E = LedgerError> = std::result::Result<T, E>;

/// One run's ledger writer. Share it (`Arc<Ledger>`) within a process; each tool server
/// process holds its own for the run it attached to.
pub struct Ledger {
    store: Arc<dyn MemoryStore>,
    run: RunFolder,
    /// Held across insert + append so this process's jsonl lines are in id order.
    order: tokio::sync::Mutex<()>,
}

/// A jsonl line: `{"id": <LedgerId>, ...event fields}`.
#[derive(Serialize)]
struct Line<'a> {
    id: LedgerId,
    #[serde(flatten)]
    event: &'a LedgerEvent,
}

impl Ledger {
    pub fn new(store: Arc<dyn MemoryStore>, run: RunFolder) -> Ledger {
        Ledger {
            store,
            run,
            order: tokio::sync::Mutex::new(()),
        }
    }

    pub fn run(&self) -> &RunFolder {
        &self.run
    }

    /// Insert, then mirror to jsonl; returns the row id.
    ///
    /// Every string in the event (tool, server, args recursively, result_summary,
    /// consequences, why) goes through the global [`scrub`] first: the contract says events
    /// arrive scrubbed, this is defence in depth (D-102). `run_id: None` is filled with this
    /// ledger's run; another run is [`LedgerError::WrongRun`]. `ts` is not re-stamped, only
    /// truncated to microseconds (a no-op for [`crate::memory::ts_now`]), so the line and
    /// the row carry the same instant. If the insert succeeds and the append fails the
    /// error is [`LedgerError::Io`] and the row stays (the table is append-only).
    pub async fn write(&self, e: LedgerEvent) -> Result<LedgerId> {
        let mut e = scrub_event(e);
        match &e.run_id {
            None => e.run_id = Some(self.run.id.clone()),
            Some(r) if *r != self.run.id => {
                return Err(LedgerError::WrongRun {
                    event: r.clone(),
                    ledger: self.run.id.clone(),
                })
            }
            Some(_) => {}
        }
        e.ts = truncate_micros(e.ts);

        let _order = self.order.lock().await;
        let id = self.store.append(e.clone()).await?;
        let mut line = serde_json::to_vec(&Line { id, event: &e })?;
        line.push(b'\n');
        let path = self.run.ledger_jsonl();
        // Blocking std I/O on purpose: one small append, and the ordering lock must cover it.
        append_line(&path, &line).map_err(|source| LedgerError::Io { path, source })?;
        Ok(id)
    }

    /// This run's `ledger.jsonl`, parsed back (see [`read_jsonl`]).
    pub fn read_back(&self) -> Result<Vec<(LedgerId, LedgerEvent)>> {
        read_jsonl(&self.run.ledger_jsonl())
    }
}

/// One `write` call on a file opened for append: concurrent appenders (several tool server
/// processes on one run) never interleave inside a line.
fn append_line(path: &Path, line: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let n = f.write(line)?;
    if n < line.len() {
        // A regular file in append mode takes the whole buffer; finish rather than lose it.
        f.write_all(&line[n..])?;
    }
    Ok(())
}

fn truncate_micros(ts: Ts) -> Ts {
    DateTime::from_timestamp_micros(ts.timestamp_micros()).unwrap_or(ts)
}

/// Passes every string field and every JSON string (and object key) through [`scrub`].
pub fn scrub_event(mut e: LedgerEvent) -> LedgerEvent {
    e.server = scrub(&e.server);
    e.tool = scrub(&e.tool);
    e.result_summary = scrub(&e.result_summary);
    e.why = e.why.map(|w| scrub(&w));
    e.args = scrub_value(e.args);
    e.consequences = scrub_value(e.consequences);
    e
}

fn scrub_value(v: Value) -> Value {
    match v {
        Value::String(s) => Value::String(scrub(&s)),
        Value::Array(a) => Value::Array(a.into_iter().map(scrub_value).collect()),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| (scrub(&k), scrub_value(v)))
                .collect(),
        ),
        other => other,
    }
}

/// Reads a `ledger.jsonl` back as `(id, event)` pairs in file order, for comparison with
/// `MemoryStore::ledger_events` (T-09) and `harness ledger <run>`. A missing file is an
/// empty ledger; blank lines are skipped; anything else unparsable is
/// [`LedgerError::Corrupt`].
pub fn read_jsonl(path: &Path) -> Result<Vec<(LedgerId, LedgerEvent)>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(LedgerError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let corrupt = |line: usize, reason: String| LedgerError::Corrupt {
        path: path.to_path_buf(),
        line,
        reason,
    };
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        let mut v: Value = serde_json::from_str(raw).map_err(|e| corrupt(i + 1, e.to_string()))?;
        let id = v
            .as_object_mut()
            .and_then(|o| o.remove("id"))
            .ok_or_else(|| corrupt(i + 1, "no id".into()))?;
        let id: LedgerId = serde_json::from_value(id).map_err(|e| corrupt(i + 1, e.to_string()))?;
        let e: LedgerEvent =
            serde_json::from_value(v).map_err(|e| corrupt(i + 1, e.to_string()))?;
        out.push((id, e));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// report.md (Q-4)

/// How many items a report line lists before `+n more`.
const LIST_MAX: usize = 8;
/// Longest single item or why, in chars.
const ITEM_MAX: usize = 100;

const TARGET_KEYS: [&str; 11] = [
    "url",
    "href",
    "to",
    "path",
    "file",
    "id",
    "message_id",
    "thread",
    "query",
    "q",
    "domain",
];

fn is_core(e: &LedgerEvent) -> bool {
    e.server == "core"
}

fn is_outcome(m: Option<LedgerMode>) -> bool {
    matches!(
        m,
        Some(
            LedgerMode::AnsweredProceed
                | LedgerMode::AnsweredSkip
                | LedgerMode::TimeoutProceed
                | LedgerMode::TimeoutSkip
        )
    )
}

fn is_skip(m: Option<LedgerMode>) -> bool {
    matches!(m, Some(LedgerMode::AnsweredSkip | LedgerMode::TimeoutSkip))
}

fn is_stop(e: &LedgerEvent) -> bool {
    e.mode == Some(LedgerMode::Stop)
}

/// A failed step that is not a stop or a skip (those are reported as such).
fn is_failure(e: &LedgerEvent) -> bool {
    !e.ok && !is_stop(e) && !is_skip(e.mode)
}

/// `Result:` of a run, as stored in `runs.status` at the end: any stop → Stopped; any
/// skipped ask or failed step → Partial; otherwise Done.
pub fn run_status(events: &[LedgerEvent]) -> RunStatus {
    if events.iter().any(is_stop) {
        RunStatus::Stopped
    } else if events.iter().any(|e| is_skip(e.mode) || !e.ok) {
        RunStatus::Partial
    } else {
        RunStatus::Done
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadKind {
    Page,
    Mail,
    File,
}

fn read_kind(e: &LedgerEvent) -> Option<ReadKind> {
    let both = format!("{} {}", e.server, e.tool).to_ascii_lowercase();
    let has = |w: &str| {
        both.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|t| t == w)
    };
    if both.contains("mail") {
        Some(ReadKind::Mail)
    } else if has("files") || has("file") || has("fs") {
        Some(ReadKind::File)
    } else if both.contains("web")
        || both.contains("fetch")
        || both.contains("page")
        || has("sb")
        || has("browser")
    {
        Some(ReadKind::Page)
    } else {
        None
    }
}

fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }
}

fn value_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().filter_map(value_text).collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        _ => None,
    }
}

/// The call's target from its args (url, recipient, path, id...).
fn target(e: &LedgerEvent) -> Option<String> {
    let o = e.args.as_object()?;
    TARGET_KEYS
        .iter()
        .find_map(|k| o.get(*k).and_then(value_text))
        .map(|t| clip(&t, ITEM_MAX))
}

fn consequence(e: &LedgerEvent) -> String {
    let from_list = match &e.consequences {
        Value::Array(a) => a.first().map(|c| match c {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }),
        Value::Null => None,
        Value::Object(o) if o.is_empty() => None,
        other => Some(other.to_string()),
    };
    let text = from_list
        .filter(|s| !s.trim().is_empty())
        .or_else(|| (!e.result_summary.trim().is_empty()).then(|| e.result_summary.clone()))
        .unwrap_or_else(|| "ok".into());
    clip(&text, ITEM_MAX)
}

fn class_str(c: Option<Class>) -> &'static str {
    c.map(Class::as_str).unwrap_or("-")
}

fn why_text(e: &LedgerEvent) -> String {
    let w = e
        .why
        .as_deref()
        .filter(|w| !w.trim().is_empty())
        .or_else(|| (!e.result_summary.trim().is_empty()).then_some(e.result_summary.as_str()))
        .unwrap_or("no reason given");
    clip(w, ITEM_MAX)
}

fn list(items: &[String]) -> String {
    let mut s = items
        .iter()
        .take(LIST_MAX)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if items.len() > LIST_MAX {
        s.push_str(&format!("; +{} more", items.len() - LIST_MAX));
    }
    s
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !v.contains(&s) {
        v.push(s);
    }
}

/// `runs/<id>/files/<rel>` for a string that names a file under the run's `files/`.
fn run_file(s: &str, run: Option<&str>) -> Option<String> {
    let s = s.replace('\\', "/");
    let rel = match run {
        Some(id) => {
            let marker = format!("{id}/files/");
            s.find(&marker)
                .filter(|&at| at == 0 || s[..at].ends_with('/'))
                .map(|at| s[at + marker.len()..].to_string())
                .or_else(|| s.strip_prefix("files/").map(str::to_string))
        }
        None => s.strip_prefix("files/").map(str::to_string),
    }?;
    let rel = rel.trim_end_matches('/');
    if rel.is_empty() || rel.split('/').any(|c| c == "..") {
        return None;
    }
    Some(format!("runs/{}/files/{rel}", run.unwrap_or("<run>")))
}

fn collect_files(v: &Value, run: Option<&str>, out: &mut Vec<String>) {
    match v {
        Value::String(s) => {
            if let Some(f) = run_file(s, run) {
                push_unique(out, f);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_files(x, run, out)),
        Value::Object(o) => o.values().for_each(|x| collect_files(x, run, out)),
        _ => {}
    }
}

/// `report.md` from a run's events (H-3, Q-4 format), scrubbed. Deterministic: the same
/// events give the same text.
pub fn report(prompt: &str, events: &[LedgerEvent]) -> String {
    let run = events
        .iter()
        .find_map(|e| e.run_id.as_ref().map(|r| r.0.clone()));
    build_report(prompt, events, run.as_deref(), &[])
}

fn build_report(
    prompt: &str,
    events: &[LedgerEvent],
    run: Option<&str>,
    disk: &[String],
) -> String {
    let task = prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| clip(l.trim_start_matches('#').trim(), 120))
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "run".into());

    let result = match run_status(events) {
        RunStatus::Stopped => {
            let why = events.iter().find(|e| is_stop(e)).map(why_text);
            format!("stopped ({})", why.unwrap_or_default())
        }
        RunStatus::Partial => "partial".into(),
        _ => "done".into(),
    };

    // Read
    let (mut pages, mut mails, mut files) = (0usize, 0usize, 0usize);
    let mut read_refs = Vec::new();
    for e in events
        .iter()
        .filter(|e| !is_core(e) && e.ok && e.class == Some(Class::Read))
    {
        let Some(kind) = read_kind(e) else { continue };
        match kind {
            ReadKind::Page => pages += 1,
            ReadKind::Mail => mails += 1,
            ReadKind::File => files += 1,
        }
        push_unique(&mut read_refs, target(e).unwrap_or_else(|| e.tool.clone()));
    }
    let mut read = format!("Read: {pages} pages, {mails} mails, {files} files");
    if !read_refs.is_empty() {
        read.push_str(&format!(" ({})", list(&read_refs)));
    }

    // Asked
    let asks: Vec<&LedgerEvent> = events
        .iter()
        .filter(|e| is_core(e) && is_outcome(e.mode))
        .collect();
    let timed_out = asks
        .iter()
        .filter(|e| {
            matches!(
                e.mode,
                Some(LedgerMode::TimeoutProceed | LedgerMode::TimeoutSkip)
            )
        })
        .count();
    let mut asked = format!(
        "Asked: {} ({} answered / {} timed out)",
        asks.len(),
        asks.len() - timed_out,
        timed_out
    );
    if !asks.is_empty() {
        let items: Vec<String> = asks
            .iter()
            .map(|e| {
                format!(
                    "{} at {}: {}",
                    why_text(e),
                    e.ts.format("%H:%M:%S"),
                    mode_str(e.mode)
                )
            })
            .collect();
        asked.push_str(&format!(" ({})", list(&items)));
    }

    // Did
    let did_items: Vec<String> = events
        .iter()
        .filter(|e| {
            !is_core(e)
                && e.ok
                && matches!(e.class, Some(c) if c != Class::Read)
                && matches!(
                    e.mode,
                    None | Some(
                        LedgerMode::Allow
                            | LedgerMode::AnsweredProceed
                            | LedgerMode::TimeoutProceed
                    )
                )
        })
        .map(|e| {
            let what = match target(e) {
                Some(t) => format!("{} {}", e.tool, t),
                None => e.tool.clone(),
            };
            format!("{} {}: {}", class_str(e.class), what, consequence(e))
        })
        .collect();
    let mut did = format!("Did: {} actions", did_items.len());
    if !did_items.is_empty() {
        did.push_str(&format!(" ({})", list(&did_items)));
    }

    // Files
    let mut out_files = Vec::new();
    for e in events {
        collect_files(&e.consequences, run, &mut out_files);
    }
    for f in disk {
        push_unique(&mut out_files, f.clone());
    }
    let files_line = if out_files.is_empty() {
        "Files: none".to_string()
    } else {
        format!("Files: {}", list(&out_files))
    };

    // Not done / needs you (Q-5: step, class, why, consequence)
    let mut needs = Vec::new();
    for e in events {
        if is_stop(e) {
            needs.push(format!(
                "stopped at {} ({}): {}; handed back to you",
                e.tool,
                class_str(e.class),
                why_text(e)
            ));
        } else if is_skip(e.mode) && is_core(e) {
            let how = if e.mode == Some(LedgerMode::TimeoutSkip) {
                "timed out, skipped"
            } else {
                "you said skip"
            };
            needs.push(format!(
                "{} ({}): {}: {}",
                e.tool,
                class_str(e.class),
                why_text(e),
                how
            ));
        } else if is_failure(e) {
            needs.push(format!(
                "failed {} ({}): {}; consequence: {}",
                e.tool,
                class_str(e.class),
                why_text(e),
                consequence(e)
            ));
        }
    }
    let needs_line = if needs.is_empty() {
        "Not done / needs you: nothing".to_string()
    } else {
        format!("Not done / needs you: {}", list(&needs))
    };

    let text =
        format!("# {task}\nResult: {result}\n{read}\n{asked}\n{did}\n{files_line}\n{needs_line}\n");
    scrub(&text)
}

fn mode_str(m: Option<LedgerMode>) -> &'static str {
    match m {
        Some(LedgerMode::Allow) => "allow",
        Some(LedgerMode::Ask) => "ask",
        Some(LedgerMode::Stop) => "stop",
        Some(LedgerMode::AnsweredProceed) => "answered, proceeded",
        Some(LedgerMode::AnsweredSkip) => "answered, skipped",
        Some(LedgerMode::TimeoutProceed) => "timed out, proceeded",
        Some(LedgerMode::TimeoutSkip) => "timed out, skipped",
        None => "-",
    }
}

/// Every file under `dir`, as `/`-separated paths relative to it, sorted.
fn walk_files(dir: &Path) -> std::io::Result<Vec<String>> {
    fn go(base: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                go(base, &path, out)?;
            } else if let Ok(rel) = path.strip_prefix(base) {
                let rel: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.push(rel.join("/"));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    go(dir, dir, &mut out)?;
    out.sort();
    Ok(out)
}

/// [`report`] plus every file found on disk under `files/`, written to `report.md`.
/// Returns the text written.
pub fn write_report(run: &RunFolder, prompt: &str, events: &[LedgerEvent]) -> Result<String> {
    let io = |path: PathBuf| move |source| LedgerError::Io { path, source };
    let disk: Vec<String> = walk_files(&run.files())
        .map_err(io(run.files()))?
        .into_iter()
        .map(|rel| format!("runs/{}/files/{rel}", run.id))
        .collect();
    let text = build_report(prompt, events, Some(&run.id.0), &disk);
    std::fs::write(run.report_md(), &text).map_err(io(run.report_md()))?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::ts_now;
    use crate::testing::MemStore;
    use chrono::TimeZone;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const RUN: &str = "2026-10-23T09-00-00-test";

    fn ev(server: &str, tool: &str, class: Class, mode: Option<LedgerMode>) -> LedgerEvent {
        LedgerEvent {
            run_id: Some(RunId(RUN.into())),
            ts: chrono::Utc.with_ymd_and_hms(2026, 10, 23, 9, 0, 5).unwrap(),
            server: server.into(),
            tool: tool.into(),
            class: Some(class),
            mode,
            args: json!({}),
            result_summary: String::new(),
            consequences: json!([]),
            duration_ms: 3,
            ok: true,
            why: None,
        }
    }

    fn fetch(url: &str) -> LedgerEvent {
        let mut e = ev("srv-web", "fetch", Class::Read, Some(LedgerMode::Allow));
        e.args = json!({ "url": url });
        e
    }

    fn send(to: &str, mode: LedgerMode) -> LedgerEvent {
        let mut e = ev("echo", "echo.send", Class::Send, Some(mode));
        e.args = json!({ "to": to, "body": "hi" });
        e.consequences = json!(["sent to ".to_string() + to]);
        e
    }

    fn ask(mode: LedgerMode, what: &str) -> LedgerEvent {
        let mut e = ev("core", "echo.send", Class::Send, Some(mode));
        e.why = Some(what.into());
        e
    }

    #[test]
    fn report_done() {
        let mut save = ev(
            "srv-files",
            "fs.write",
            Class::Fill,
            Some(LedgerMode::Allow),
        );
        save.args = json!({ "path": "files/out.md" });
        save.consequences = json!({ "files": [format!("F:\\x\\runs\\{RUN}\\files\\out.md")] });
        let mut read_file = ev("srv-files", "fs.read", Class::Read, None);
        read_file.args = json!({ "path": "notes.txt" });
        let events = vec![
            fetch("https://a.example/"),
            fetch("https://b.example/"),
            fetch("https://a.example/"),
            ev("sb-mcp", "mail.read", Class::Read, None),
            read_file,
            ask(LedgerMode::AnsweredProceed, "send mail to new@x.com"),
            send("new@x.com", LedgerMode::AnsweredProceed),
            save,
        ];
        let r = report("Say hi to new@x.com\nmore detail", &events);
        let expected = format!(
            "# Say hi to new@x.com\n\
             Result: done\n\
             Read: 3 pages, 1 mails, 1 files (https://a.example/; https://b.example/; mail.read; notes.txt)\n\
             Asked: 1 (1 answered / 0 timed out) (send mail to new@x.com at 09:00:05: answered, proceeded)\n\
             Did: 2 actions (Send echo.send new@x.com: sent to new@x.com; Fill fs.write files/out.md: {{\"files\":[\"F:\\\\x\\\\runs\\\\{RUN}\\\\files\\\\out.md\"]}})\n\
             Files: runs/{RUN}/files/out.md\n\
             Not done / needs you: nothing\n"
        );
        assert_eq!(r, expected);
        assert_eq!(run_status(&events), RunStatus::Done);
        assert_eq!(report("Say hi to new@x.com\nmore detail", &events), r);
    }

    #[test]
    fn report_partial_on_timeout_skip() {
        let mut pay = ask(LedgerMode::TimeoutSkip, "pay 20 EUR on shop.example");
        pay.class = Some(Class::Pay);
        pay.tool = "act".into();
        let events = vec![fetch("https://shop.example/"), pay];
        let r = report("buy the thing", &events);
        assert!(r.contains("\nResult: partial\n"), "{r}");
        assert!(
            r.contains("Asked: 1 (0 answered / 1 timed out) (pay 20 EUR on shop.example at 09:00:05: timed out, skipped)"),
            "{r}"
        );
        assert!(r.contains("Did: 0 actions\n"), "{r}");
        assert!(
            r.contains(
                "Not done / needs you: act (Pay): pay 20 EUR on shop.example: timed out, skipped\n"
            ),
            "{r}"
        );
        assert_eq!(run_status(&events), RunStatus::Partial);
    }

    #[test]
    fn report_partial_on_failure() {
        let mut bad = fetch("https://down.example/");
        bad.ok = false;
        bad.why = Some("connection refused".into());
        let r = report("read it", &[bad]);
        assert!(r.contains("Result: partial"), "{r}");
        assert!(r.contains("Read: 0 pages, 0 mails, 0 files\n"), "{r}");
        assert!(
            r.contains("failed fetch (Read): connection refused; consequence: ok"),
            "{r}"
        );
    }

    #[test]
    fn report_stopped_with_why() {
        let mut stop = ev("core", "act", Class::Challenge, Some(LedgerMode::Stop));
        stop.ok = false;
        stop.why = Some("Turnstile challenge at #31".into());
        let events = vec![fetch("https://luma.com/e"), stop];
        let r = report("Register on Luma", &events);
        assert!(
            r.contains("\nResult: stopped (Turnstile challenge at #31)\n"),
            "{r}"
        );
        assert!(
            r.contains(
                "stopped at act (Challenge): Turnstile challenge at #31; handed back to you"
            ),
            "{r}"
        );
        assert_eq!(run_status(&events), RunStatus::Stopped);
    }

    #[test]
    fn report_empty_and_long_lists() {
        let r = report("", &[]);
        assert_eq!(
            r,
            "# run\nResult: done\nRead: 0 pages, 0 mails, 0 files\nAsked: 0 (0 answered / 0 timed out)\nDid: 0 actions\nFiles: none\nNot done / needs you: nothing\n"
        );
        let many: Vec<LedgerEvent> = (0..12).map(|i| fetch(&format!("https://e/{i}"))).collect();
        let r = report("# Many", &many);
        assert!(r.starts_with("# Many\n"), "{r}");
        assert!(r.contains("Read: 12 pages"), "{r}");
        assert!(r.contains("; +4 more)"), "{r}");
    }

    #[test]
    fn run_file_rules() {
        assert_eq!(
            run_file("files/a/b.pdf", Some(RUN)).as_deref(),
            Some(format!("runs/{RUN}/files/a/b.pdf").as_str())
        );
        assert_eq!(
            run_file(&format!("/r/runs/{RUN}/files/x.png"), Some(RUN)).as_deref(),
            Some(format!("runs/{RUN}/files/x.png").as_str())
        );
        assert_eq!(run_file("/other/files/x.png", Some(RUN)), None);
        assert_eq!(run_file("files/../../etc", Some(RUN)), None);
        assert_eq!(run_file(&format!("x{RUN}/files/a"), Some(RUN)), None);
    }

    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Tmp {
            static N: AtomicUsize = AtomicUsize::new(0);
            Tmp(std::env::temp_dir().join(format!(
                "sb-ledger-unit-{}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed),
                chrono::Utc::now().timestamp_micros()
            )))
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn write_fills_run_id_and_rejects_other_run() {
        let tmp = Tmp::new();
        let run = RunFolder::create(&tmp.0, "unit", "p").unwrap();
        let store = Arc::new(MemStore::new());
        let ledger = Ledger::new(store.clone(), run.clone());
        let mut e = fetch("https://a/");
        e.run_id = None;
        e.ts = ts_now();
        let id = ledger.write(e).await.unwrap();
        let back = ledger.read_back().unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].0, id);
        assert_eq!(back[0].1.run_id.as_ref(), Some(&run.id));
        assert_eq!(store.ledger_events(&run.id).await.unwrap(), back);

        let mut other = fetch("https://a/");
        other.run_id = Some(RunId("someone-else".into()));
        assert!(matches!(
            ledger.write(other).await,
            Err(LedgerError::WrongRun { .. })
        ));
        assert_eq!(ledger.read_back().unwrap().len(), 1);
    }

    #[test]
    fn read_jsonl_missing_blank_and_corrupt() {
        let tmp = Tmp::new();
        std::fs::create_dir_all(&tmp.0).unwrap();
        let p = tmp.0.join("ledger.jsonl");
        assert!(read_jsonl(&p).unwrap().is_empty());
        std::fs::write(&p, "\n\n").unwrap();
        assert!(read_jsonl(&p).unwrap().is_empty());
        std::fs::write(&p, "{\"server\":\"x\"}\n").unwrap();
        assert!(matches!(
            read_jsonl(&p),
            Err(LedgerError::Corrupt { line: 1, .. })
        ));
    }

    #[tokio::test]
    async fn write_report_lists_disk_files() {
        let tmp = Tmp::new();
        let run = RunFolder::create(&tmp.0, "files", "Make a pdf").unwrap();
        std::fs::create_dir_all(run.files().join("sub")).unwrap();
        std::fs::write(run.files().join("sub").join("b.txt"), "b").unwrap();
        std::fs::write(run.files().join("a.pdf"), "a").unwrap();
        let text = write_report(&run, "Make a pdf", &[]).unwrap();
        assert_eq!(std::fs::read_to_string(run.report_md()).unwrap(), text);
        let id = &run.id;
        assert!(
            text.contains(&format!(
                "Files: runs/{id}/files/a.pdf; runs/{id}/files/sub/b.txt\n"
            )),
            "{text}"
        );
    }
}
