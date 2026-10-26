//! Ledger (H-3) and `report.md` (Q-4, Q-5). Owned by area core (T-06).
//!
//! The row type is `memory::LedgerEvent` (contract). [`Ledger::write`] writes it twice:
//! the DB first through `MemoryStore::append`, then one line in `runs/<run>/ledger.jsonl`
//! carrying the returned id. Several tool server processes each hold their own `Ledger`
//! for one run, so their appends can land out of id order; "the mirror matches the table
//! row-for-row" therefore means *keyed by id*: [`read_jsonl`] returns the lines sorted by
//! id, which is the order `MemoryStore::ledger_events` returns rows in.
//!
//! # Event shapes [`report`] reads
//!
//! The wrapper (T-07) writes the events; the report derives everything from these fields:
//!
//! * a tool call: `server` = the tool server (`sheetbrowse`, `web`, `files`, `cli`, ...),
//!   `tool` (a site tool is named after its site, e.g. `calendar.google.com/events`),
//!   `class`, `mode` = how the gate let it through (`allow`, `answered_proceed`,
//!   `timeout_proceed`; `None` counts as allowed), `ok`, `args` (the target is the first of
//!   `url href to path file id message_id thread query q domain`), `result_summary`,
//!   `consequences` (paths in it that resolve inside the run's `files/` end up on the
//!   `Files:` line).
//!   A call stopped by a challenge is written with `mode = stop` next to the core stop
//!   event below; it never counts under `Did:` and is listed once under `Not done`.
//!   Calls whose mode is a skip never count under `Did:` either.
//! * `Read:` counts every `ok` call with `class = Read` outside `core`, one per distinct
//!   target (one per call when there is no target), so the counts match the refs listed:
//!   mails = server, tool or target host mentions `mail` or `outlook`; files = server
//!   `files`, a `files`/`file`/`fs` token or an `fs.*` tool; pages = server `web` or
//!   `sheetbrowse`, a site tool, or a web/fetch/page/browser name; anything else (`cli`
//!   tools such as gh or dns, memory...) is `, n other`, printed only when n > 0.
//! * a failed call (`ok = false`, not a stop or skip) is resolved when a later `ok` call has
//!   the same server, tool and target: it then makes the run neither partial nor listed.
//!   An unresolved one is listed (Q-5) with its class, why, consequence and the fallback:
//!   the next non-core event's tool and target, or `none`.
//! * how an ask ended: `server = core` ([`CORE_SERVER`]), `mode` = one of the four
//!   outcomes, `class` and `tool` of the asked call, `why` (or `result_summary`) = the
//!   card's what line;
//! * a stop (gate Stop, Challenge mid-call): `server = core`, `mode = stop`, `why` = the
//!   reason, `tool` and `args` of the call. Each stopped *call* is listed once under
//!   `Not done` (D-121). A gate Stop is one core stop. A call the handler stops writes the
//!   tool event with `mode = stop` and then a trailing core stop. A stop that a mid-call
//!   `ToolCtx::ask` hits (Challenge card or gate Stop) is held while the call runs (D-127):
//!   on a call that stopped it becomes that trailing core stop, and on a call that did not
//!   stop it is written as a lone core stop just before the tool event. A tool stop
//!   event is grouped with the next event of the same (tool, args) when that is a core
//!   stop (the trailing one), and with the core stops of the same (tool, args) right
//!   before it whose `ts` lies inside the call (`ts - duration_ms`, plus
//!   [`CALL_SPAN_SLACK_MS`]). Every other core stop is a call of its own, so a resumed run
//!   (H-1.4) that is stopped again on the same tool and args lists a second stop. A group
//!   shows its first core stop's why (the tool event's when it has none), and
//!   `Result: stopped (...)` shows the why of the last group.
//! * core events with any other mode (`allow`, `ask` gate decisions) are not read.
//!
//! Known limits (no fix needed): an integral f64 of 1e16 or more comes back from jsonb as
//! an integer, so such a row and its line compare unequal; a `write` future dropped
//! between the insert and the append leaves a row without a jsonl line; the D-117
//! fail-closed check reads the data fields only, so a vault value that coincides with a
//! harness field (`ts`, a duration) can appear there, and one containing the JSON glue
//! between two top-level fields is not seen across them; when a call's trailing core stop
//! was never written and the next event of the same (tool, args) is the next call's
//! `ToolCtx::ask` stop, that stop is taken as the trailing one (both calls are still
//! listed, the first with that ask stop's why).

use crate::class::Class;
use crate::memory::{LedgerEvent, LedgerId, LedgerMode, MemoryError, MemoryStore, RunStatus, Ts};
use crate::run::{RunFolder, RunId};
use crate::vault::scrub;
use chrono::DateTime;
use serde::Serialize;
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `server` of events the harness itself writes (gate decisions, ask outcomes, stops).
pub const CORE_SERVER: &str = "core";

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
    /// After scrubbing, a data field as serialized still matches a vault value (D-117; for
    /// example a value spanning an object key and its value in `args`). Nothing was
    /// inserted and no line was written. `fields` names the fields, never the value.
    #[error("ledger event still matches a vault value after scrubbing (in {fields}); not written")]
    Unscrubbable { fields: String },
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
    /// Every string in the event (tool, server, args recursively including object keys,
    /// result_summary, consequences, why) and every JSON number goes through the global
    /// [`scrub`] first, repeated until nothing changes; a number that matches becomes its
    /// scrubbed text as a string (`"@vault(name)"`). U+0000 becomes U+FFFD (Postgres
    /// rejects 0x00 in text and jsonb). The contract says events arrive scrubbed; this is
    /// defence in depth (D-102). Then every data field as serialized is scrubbed once more
    /// and must come back unchanged, else [`LedgerError::Unscrubbable`] and nothing is
    /// written (fail closed). Fields the harness generates (`ts`, `run_id`, `class`, `mode`,
    /// `duration_ms`, `ok`, the id) are not checked (D-117).
    ///
    /// `run_id: None` is filled with this ledger's run; another run is
    /// [`LedgerError::WrongRun`]. `ts` is not re-stamped, only truncated to microseconds (a
    /// no-op for [`crate::memory::ts_now`]), so the line and the row carry the same
    /// instant. If the insert succeeds and the append fails the error is
    /// [`LedgerError::Io`] and the row stays (the table is append-only).
    pub async fn write(&self, mut e: LedgerEvent) -> Result<LedgerId> {
        match &e.run_id {
            None => e.run_id = Some(self.run.id.clone()),
            Some(r) if *r != self.run.id => {
                return Err(LedgerError::WrongRun {
                    event: RunId(scrub(&r.0)),
                    ledger: self.run.id.clone(),
                })
            }
            Some(_) => {}
        }
        e.ts = truncate_micros(e.ts);
        let e = scrub_event(e);
        check_clean(&e)?;

        let _order = self.order.lock().await;
        let id = self.store.append(e.clone()).await?;
        let mut line = serde_json::to_vec(&Line { id, event: &e })?;
        line.push(b'\n');
        let path = self.run.ledger_jsonl();
        // Blocking std I/O on purpose: one small append, and the ordering lock must cover it.
        append_line(&path, &line).map_err(|source| LedgerError::Io { path, source })?;
        Ok(id)
    }

    /// This run's `ledger.jsonl`, parsed back and sorted by id (see [`read_jsonl`]).
    pub fn read_back(&self) -> Result<Vec<(LedgerId, LedgerEvent)>> {
        read_jsonl(&self.run.ledger_jsonl())
    }
}

/// Appends `line` (which ends in `\n`) with a single `write_all` of one buffer on a file
/// opened for append, so concurrent appenders never interleave inside a line. If the file
/// does not end in `\n` (a torn write from a crashed process), a `\n` goes first so this
/// line stays whole.
fn append_line(path: &Path, line: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)?;
    let len = f.metadata()?.len();
    let mut buf = Vec::with_capacity(line.len() + 1);
    if len > 0 {
        f.seek(SeekFrom::Start(len - 1))?;
        let mut last = [0u8; 1];
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            buf.push(b'\n');
        }
    }
    buf.extend_from_slice(line);
    f.write_all(&buf)
}

fn truncate_micros(ts: Ts) -> Ts {
    DateTime::from_timestamp_micros(ts.timestamp_micros()).unwrap_or(ts)
}

/// How many scrub passes [`scrub_event`] makes at most before it stops at a fixpoint.
const SCRUB_ROUNDS: usize = 4;

/// Passes every string field, every JSON string and object key, and every JSON number's
/// decimal text through [`scrub`], and replaces U+0000 with U+FFFD, until nothing changes
/// (at most a few passes). `run_id` is left alone and, since D-117, the fail-closed check
/// does not read it either: [`Ledger::write`] only accepts its own run, whose slug was
/// scrubbed against the vault when the run was created (`run::slugify`). A vault value
/// added later is not re-checked against it; the run folder's name holds the slug anyway.
pub fn scrub_event(e: LedgerEvent) -> LedgerEvent {
    let mut cur = scrub_event_once(e);
    for _ in 1..SCRUB_ROUNDS {
        let next = scrub_event_once(cur.clone());
        if next == cur {
            break;
        }
        cur = next;
    }
    cur
}

fn scrub_event_once(mut e: LedgerEvent) -> LedgerEvent {
    e.server = clean_str(&e.server);
    e.tool = clean_str(&e.tool);
    e.result_summary = clean_str(&e.result_summary);
    e.why = e.why.map(|w| clean_str(&w));
    e.args = scrub_value(e.args);
    e.consequences = scrub_value(e.consequences);
    e
}

/// Scrub, then U+0000 → U+FFFD.
fn clean_str(s: &str) -> String {
    let s = scrub(s);
    if s.contains('\0') {
        s.replace('\0', "\u{FFFD}")
    } else {
        s
    }
}

fn scrub_value(v: Value) -> Value {
    match v {
        Value::String(s) => Value::String(clean_str(&s)),
        Value::Number(n) => {
            let text = n.to_string();
            let s = scrub(&text);
            if s == text {
                Value::Number(n)
            } else {
                Value::String(s)
            }
        }
        Value::Array(a) => Value::Array(a.into_iter().map(scrub_value).collect()),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| (clean_str(&k), scrub_value(v)))
                .collect(),
        ),
        other => other,
    }
}

/// D-117: every data field (the ones that can carry outside data: `server`, `tool`, `args`,
/// `result_summary`, `consequences`, `why`), serialized exactly as it appears in the jsonl
/// line, must survive a scrub unchanged. Names the offending fields on failure.
///
/// The fields the harness generates itself (`id`, `run_id` (always this ledger's own run),
/// `ts`, `class`, `mode`, `duration_ms` as the wrapper measured it, `ok`) and the line's key
/// names and JSON punctuation are not checked: a vault value equal to a year, a duration, a
/// mode or a key name is a coincidence there, not a leak, and failing on it would refuse
/// every write of the run. Known limit: a vault value that itself contains JSON glue
/// between two top-level fields (`","why":"`) is not seen across that boundary.
fn check_clean(e: &LedgerEvent) -> Result<()> {
    // A field that is null (no data) is not checked: `null` is the line's syntax.
    let json = |v: &Value| -> Result<Option<String>> {
        Ok(match v {
            Value::Null => None,
            v => Some(serde_json::to_string(v)?),
        })
    };
    let data: [(&str, Option<String>); 6] = [
        ("server", Some(serde_json::to_string(&e.server)?)),
        ("tool", Some(serde_json::to_string(&e.tool)?)),
        ("args", json(&e.args)?),
        (
            "result_summary",
            Some(serde_json::to_string(&e.result_summary)?),
        ),
        ("consequences", json(&e.consequences)?),
        (
            "why",
            e.why.as_ref().map(serde_json::to_string).transpose()?,
        ),
    ];
    let fields: Vec<&str> = data
        .iter()
        .filter(|(_, text)| text.as_ref().is_some_and(|t| scrub(t) != *t))
        .map(|(name, _)| *name)
        .collect();
    if fields.is_empty() {
        Ok(())
    } else {
        Err(LedgerError::Unscrubbable {
            fields: fields.join(", "),
        })
    }
}

/// A parsed `ledger.jsonl`.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonlRead {
    /// `(id, event)` pairs sorted by id (stable for equal ids).
    pub events: Vec<(LedgerId, LedgerEvent)>,
    /// 1-based numbers of torn lines that were skipped: an unparsable last line without a
    /// trailing `\n`, or a line cut short by a crash mid-write (JSON that ends early or
    /// UTF-8 cut inside a character) that a later append terminated. Their rows can be in
    /// the table without a line here.
    pub torn: Vec<usize>,
}

/// Reads a `ledger.jsonl` back as `(id, event)` pairs sorted by id, for comparison with
/// `MemoryStore::ledger_events` (T-09) and `harness ledger <run>`: several processes
/// append to one run's file, so file order is not id order. Torn lines are skipped (see
/// [`read_jsonl_checked`] to learn which); a missing file is an empty ledger; blank lines
/// are skipped; any other unparsable line is [`LedgerError::Corrupt`].
pub fn read_jsonl(path: &Path) -> Result<Vec<(LedgerId, LedgerEvent)>> {
    Ok(read_jsonl_checked(path)?.events)
}

enum BadLine {
    Torn,
    Corrupt(String),
}

fn parse_line(raw: &[u8]) -> Result<(LedgerId, LedgerEvent), BadLine> {
    let text = match std::str::from_utf8(raw) {
        Ok(t) => t,
        // error_len None: the bytes end inside a character, i.e. the line was cut.
        Err(e) if e.error_len().is_none() => return Err(BadLine::Torn),
        Err(e) => return Err(BadLine::Corrupt(e.to_string())),
    };
    let mut v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) if e.is_eof() => return Err(BadLine::Torn),
        Err(e) => return Err(BadLine::Corrupt(scrub(&e.to_string()))),
    };
    let id = v
        .as_object_mut()
        .and_then(|o| o.remove("id"))
        .ok_or_else(|| BadLine::Corrupt("no id".into()))?;
    let id: LedgerId =
        serde_json::from_value(id).map_err(|e| BadLine::Corrupt(scrub(&e.to_string())))?;
    let e: LedgerEvent =
        serde_json::from_value(v).map_err(|e| BadLine::Corrupt(scrub(&e.to_string())))?;
    Ok((id, e))
}

/// [`read_jsonl`] that also reports the torn lines it skipped.
pub fn read_jsonl_checked(path: &Path) -> Result<JsonlRead> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(JsonlRead {
                events: Vec::new(),
                torn: Vec::new(),
            })
        }
        Err(source) => {
            return Err(LedgerError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let terminated = bytes.last() == Some(&b'\n');
    let pieces: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    let last = pieces.len();
    let mut events = Vec::new();
    let mut torn = Vec::new();
    for (i, raw) in pieces.into_iter().enumerate() {
        let n = i + 1;
        let raw = raw.trim_ascii();
        if raw.is_empty() {
            continue;
        }
        match parse_line(raw) {
            Ok(pair) => events.push(pair),
            Err(BadLine::Torn) => torn.push(n),
            // The last line without its newline: a write that never finished.
            Err(BadLine::Corrupt(_)) if n == last && !terminated => torn.push(n),
            Err(BadLine::Corrupt(reason)) => {
                return Err(LedgerError::Corrupt {
                    path: path.to_path_buf(),
                    line: n,
                    reason,
                })
            }
        }
    }
    events.sort_by_key(|(id, _)| *id);
    Ok(JsonlRead { events, torn })
}

// ---------------------------------------------------------------------------------------
// report.md (Q-4, Q-5)

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
    e.server == CORE_SERVER
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

/// A failed tool call that is not a stop or a skip (those are reported as such).
fn is_failure(e: &LedgerEvent) -> bool {
    !e.ok && !is_core(e) && !is_stop(e) && !is_skip(e.mode)
}

/// A failure at `i` is resolved when a later ok call (not core, not a stop or skip) has
/// the same server, tool and target.
fn is_resolved(events: &[LedgerEvent], i: usize) -> bool {
    let e = &events[i];
    let t = target_raw(e);
    events[i + 1..].iter().any(|l| {
        l.ok && !is_core(l)
            && !is_stop(l)
            && !is_skip(l.mode)
            && l.server == e.server
            && l.tool == e.tool
            && target_raw(l) == t
    })
}

/// `Result:` of a run, as stored in `runs.status` at the end: any stop → Stopped; any
/// skip or unresolved failed step → Partial; otherwise Done.
pub fn run_status(events: &[LedgerEvent]) -> RunStatus {
    if events.iter().any(is_stop) {
        RunStatus::Stopped
    } else if events.iter().any(|e| is_skip(e.mode))
        || (0..events.len()).any(|i| is_failure(&events[i]) && !is_resolved(events, i))
    {
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
    Other,
}

/// Lower-cased host of a URL-ish string (`https://user@host:443/x` → `host`).
fn host_of(s: &str) -> String {
    let rest = s.split_once("://").map(|(_, r)| r).unwrap_or(s);
    let auth = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = auth.rsplit('@').next().unwrap_or_default();
    host.split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn read_kind(e: &LedgerEvent) -> ReadKind {
    let server = e.server.to_ascii_lowercase();
    let tool = e.tool.to_ascii_lowercase();
    let hosts: Vec<String> = e
        .args
        .as_object()
        .map(|o| {
            ["url", "href", "domain"]
                .iter()
                .filter_map(|k| o.get(*k).and_then(Value::as_str))
                .map(host_of)
                .collect()
        })
        .unwrap_or_default();
    let names = [server.as_str(), tool.as_str()];
    let mentions =
        |w: &str| names.iter().any(|n| n.contains(w)) || hosts.iter().any(|h| h.contains(w));
    let tokens: Vec<&str> = names
        .iter()
        .flat_map(|n| n.split(|c: char| !c.is_ascii_alphanumeric()))
        .collect();
    let token = |w: &str| tokens.contains(&w);
    // A site tool is named after its site: `calendar.google.com/events`.
    let site_tool = tool
        .split_once('/')
        .is_some_and(|(site, _)| site.contains('.'));
    if mentions("mail") || mentions("outlook") {
        ReadKind::Mail
    } else if server == "files" || token("files") || token("file") || token("fs") {
        ReadKind::File
    } else if server == "web"
        || server == "sheetbrowse"
        || site_tool
        || mentions("web")
        || mentions("fetch")
        || mentions("page")
        || mentions("browse")
        || token("sb")
    {
        ReadKind::Page
    } else {
        ReadKind::Other
    }
}

/// Scrub, collapse whitespace, scrub again (collapsing can form a value), then cut to
/// `max` chars. Cutting a clean string cannot expose part of a value: any value it
/// crossed was already replaced.
fn clip(s: &str, max: usize) -> String {
    let s = scrub(s);
    let s = scrub(&s.split_whitespace().collect::<Vec<_>>().join(" "));
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
        Value::String(s) if !s.trim().is_empty() => Some(scrub(s)),
        Value::Number(n) => Some(scrub(&n.to_string())),
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().filter_map(value_text).collect();
            (!parts.is_empty()).then(|| scrub(&parts.join(", ")))
        }
        _ => None,
    }
}

/// The call's target from its args (url, recipient, path, id...), scrubbed, unclipped.
fn target_raw(e: &LedgerEvent) -> Option<String> {
    let o = e.args.as_object()?;
    TARGET_KEYS
        .iter()
        .find_map(|k| o.get(*k).and_then(value_text))
}

/// [`target_raw`] for display.
fn target(e: &LedgerEvent) -> Option<String> {
    target_raw(e).map(|t| clip(&t, ITEM_MAX))
}

/// `tool target` (or just `tool`) for display.
fn step(e: &LedgerEvent) -> String {
    let tool = clip(&e.tool, ITEM_MAX);
    match target(e) {
        Some(t) => format!("{tool} {t}"),
        None => tool,
    }
}

/// First consequence, else the result summary, else `empty`.
fn consequence(e: &LedgerEvent, empty: &str) -> String {
    let from_list = match &e.consequences {
        Value::Array(a) => a.first().map(|c| match c {
            Value::String(s) => scrub(s),
            other => scrub(&other.to_string()),
        }),
        Value::Null => None,
        Value::Object(o) if o.is_empty() => None,
        other => Some(scrub(&other.to_string())),
    };
    let text = from_list
        .filter(|s| !s.trim().is_empty())
        .or_else(|| (!e.result_summary.trim().is_empty()).then(|| scrub(&e.result_summary)))
        .unwrap_or_else(|| empty.into());
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

/// What the `Files:` line resolves paths against.
struct FilesCtx<'a> {
    /// The run id, for the `runs/<id>/files/<rel>` output and the relative and
    /// marker forms.
    id: Option<&'a str>,
    /// `/`-separated absolute `<run_dir>/files`, when the run folder is known.
    files_abs: Option<String>,
}

/// `\` → `/`, and the `\\?\` verbatim prefix dropped.
fn norm_path(s: &str) -> String {
    let s = s.replace('\\', "/");
    match s.strip_prefix("//?/") {
        Some(rest) => rest.to_owned(),
        None => s,
    }
}

fn has_drive(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// `rest` of `s` = `base/rest` (ASCII case-insensitive on Windows).
fn strip_dir<'s>(s: &'s str, base: &str) -> Option<&'s str> {
    let head = s.get(..base.len())?;
    let same = if cfg!(windows) {
        head.eq_ignore_ascii_case(base)
    } else {
        head == base
    };
    if !same {
        return None;
    }
    s.get(base.len()..)?.strip_prefix('/')
}

/// `runs/<id>/files/<rel>` for a string that names a file inside this run's `files/`:
/// a relative `files/<rel>` (relative to the run dir) or `runs/<id>/files/<rel>` (the form
/// this report prints); an absolute path under `<run_dir>/files` when the run folder is
/// known, else one containing `/runs/<id>/files/`. Anything with a `..` component, a drive
/// letter or `:` outside the known base, or an absolute path elsewhere is not a run file.
fn run_file(s: &str, ctx: &FilesCtx<'_>) -> Option<String> {
    let s = norm_path(s.trim());
    if s.split('/').any(|c| c == "..") {
        return None;
    }
    let rel: String = if s.starts_with('/') || has_drive(&s) {
        match (&ctx.files_abs, ctx.id) {
            (Some(base), _) => strip_dir(&s, base)?.to_owned(),
            (None, Some(id)) => {
                let marker = format!("/runs/{id}/files/");
                let at = s.find(&marker)?;
                s[at + marker.len()..].to_owned()
            }
            (None, None) => return None,
        }
    } else if let Some(r) = s.strip_prefix("files/") {
        r.to_owned()
    } else {
        let id = ctx.id?;
        s.strip_prefix(&format!("runs/{id}/files/"))?.to_owned()
    };
    let parts: Vec<&str> = rel
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    if parts.is_empty() || parts.iter().any(|c| c.contains(':')) {
        return None;
    }
    Some(format!(
        "runs/{}/files/{}",
        ctx.id.unwrap_or("<run>"),
        parts.join("/")
    ))
}

fn collect_files(v: &Value, ctx: &FilesCtx<'_>, out: &mut Vec<String>) {
    match v {
        Value::String(s) => {
            if let Some(f) = run_file(s, ctx) {
                push_unique(out, f);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_files(x, ctx, out)),
        Value::Object(o) => o.values().for_each(|x| collect_files(x, ctx, out)),
        _ => {}
    }
}

/// `report.md` from a run's events (H-3, Q-4 format, Q-5 failures), scrubbed.
/// Deterministic: the same events give the same text.
pub fn report(prompt: &str, events: &[LedgerEvent]) -> String {
    let run = events
        .iter()
        .find_map(|e| e.run_id.as_ref().map(|r| r.0.clone()));
    let ctx = FilesCtx {
        id: run.as_deref(),
        files_abs: None,
    };
    build_report(prompt, events, &ctx, &[])
}

/// One `Read:` ref: a distinct target, or a target-less tool with its call count.
struct ReadRef {
    kind: ReadKind,
    target: Option<String>,
    shown: String,
    calls: usize,
}

fn read_line(events: &[LedgerEvent]) -> String {
    let mut refs: Vec<ReadRef> = Vec::new();
    for e in events
        .iter()
        .filter(|e| !is_core(e) && e.ok && e.class == Some(Class::Read))
    {
        let kind = read_kind(e);
        // Distinct by the full target; two long URLs sharing a clipped prefix are two reads.
        let t = target_raw(e);
        let shown = match &t {
            Some(t) => clip(t, ITEM_MAX),
            None => clip(&e.tool, ITEM_MAX),
        };
        match refs
            .iter_mut()
            .find(|r| r.kind == kind && r.target == t && r.shown == shown)
        {
            Some(r) if t.is_none() => r.calls += 1,
            Some(_) => {}
            None => refs.push(ReadRef {
                kind,
                target: t,
                shown,
                calls: 1,
            }),
        }
    }
    let count = |k: ReadKind| {
        refs.iter()
            .filter(|r| r.kind == k)
            .map(|r| r.calls)
            .sum::<usize>()
    };
    let mut read = format!(
        "Read: {} pages, {} mails, {} files",
        count(ReadKind::Page),
        count(ReadKind::Mail),
        count(ReadKind::File)
    );
    let other = count(ReadKind::Other);
    if other > 0 {
        read.push_str(&format!(", {other} other"));
    }
    if !refs.is_empty() {
        let items: Vec<String> = refs
            .iter()
            .map(|r| match r.calls {
                1 => r.shown.clone(),
                n => format!("{} (x{n})", r.shown),
            })
            .collect();
        read.push_str(&format!(" ({})", list(&items)));
    }
    read
}

/// Slack on a tool call's span (`ts - duration_ms .. ts`) when deciding that a core stop
/// was written while the call ran: `duration_ms` is rounded down to whole milliseconds and
/// `ts` comes from the wall clock, not the call's `Instant`. A core stop of an earlier call
/// with the same tool and args is at least a model turn earlier, far outside it.
pub const CALL_SPAN_SLACK_MS: i64 = 100;

fn same_call_key(a: &LedgerEvent, b: &LedgerEvent) -> bool {
    a.tool == b.tool && a.args == b.args
}

/// Whether core stop `c` was written while tool call `t` ran (a `ToolCtx::ask` stop).
fn inside_call(c: &LedgerEvent, t: &LedgerEvent) -> bool {
    let span =
        chrono::TimeDelta::milliseconds(i64::from(t.duration_ms.max(0)) + CALL_SPAN_SLACK_MS);
    match t.ts.checked_sub_signed(span) {
        Some(start) => c.ts >= start,
        None => true,
    }
}

/// One listing per stopped call (H-1.4, Q-4, D-121; grouping rules in the module header).
/// `at[i] = Some(l)` when a stopped call's listing goes at event `i` and shows event `l`.
fn stop_listings(events: &[LedgerEvent]) -> Vec<Option<usize>> {
    let n = events.len();
    let is_core_stop = |j: usize| is_core(&events[j]) && is_stop(&events[j]);
    let tool_stops: Vec<usize> = (0..n)
        .filter(|&i| is_stop(&events[i]) && !is_core(&events[i]))
        .collect();
    // owner[j]: the tool stop event that core stop j belongs to.
    let mut owner: Vec<Option<usize>> = vec![None; n];
    // The trailing core stop is the next event of the same call key.
    for &t in &tool_stops {
        if let Some(j) = (t + 1..n).find(|&j| same_call_key(&events[t], &events[j])) {
            if is_core_stop(j) && owner[j].is_none() {
                owner[j] = Some(t);
            }
        }
    }
    // Core stops written while the call ran, right before its tool event.
    for &t in &tool_stops {
        for j in (0..t).rev() {
            if !same_call_key(&events[t], &events[j]) {
                continue;
            }
            if is_core_stop(j) && owner[j].is_none() && inside_call(&events[j], &events[t]) {
                owner[j] = Some(t);
            } else {
                break;
            }
        }
    }
    // Per group (keyed by its tool stop, or by a lone core stop): first index and the
    // event shown (first core stop, else the tool stop).
    let mut pos = vec![usize::MAX; n];
    let mut shown: Vec<Option<usize>> = vec![None; n];
    for i in (0..n).filter(|&i| is_stop(&events[i])) {
        let g = if is_core(&events[i]) {
            owner[i].unwrap_or(i)
        } else {
            i
        };
        pos[g] = pos[g].min(i);
        if is_core(&events[i]) && shown[g].is_none() {
            shown[g] = Some(i);
        }
    }
    let mut at = vec![None; n];
    for g in (0..n).filter(|&g| pos[g] != usize::MAX) {
        at[pos[g]] = Some(shown[g].unwrap_or(g));
    }
    at
}

fn needs_line(events: &[LedgerEvent]) -> String {
    let stops = stop_listings(events);
    let mut first = Vec::new();
    let mut fails = Vec::new();
    for (i, e) in events.iter().enumerate() {
        if is_stop(e) {
            let Some(l) = stops[i] else {
                continue;
            };
            let s = &events[l];
            first.push(format!(
                "stopped at {} ({}): {}; handed back to you",
                clip(&s.tool, ITEM_MAX),
                class_str(s.class),
                why_text(s)
            ));
        } else if is_skip(e.mode) && is_core(e) {
            let how = if e.mode == Some(LedgerMode::TimeoutSkip) {
                "timed out, skipped"
            } else {
                "you said skip"
            };
            first.push(format!(
                "{} ({}): {}: {}",
                clip(&e.tool, ITEM_MAX),
                class_str(e.class),
                why_text(e),
                how
            ));
        } else if is_failure(e) && !is_resolved(events, i) {
            let fallback = events[i + 1..]
                .iter()
                .find(|l| !is_core(l))
                .map(step)
                .unwrap_or_else(|| "none".into());
            fails.push(format!(
                "failed {} ({}): {}; consequence: {}; fallback: {}",
                step(e),
                class_str(e.class),
                why_text(e),
                consequence(e, "none captured"),
                fallback
            ));
        }
    }
    if first.is_empty() && fails.is_empty() {
        return "Not done / needs you: nothing".to_string();
    }
    // Stops and skipped asks are never cut; failures after them are capped.
    let mut parts = first;
    if !fails.is_empty() {
        parts.push(list(&fails));
    }
    format!("Not done / needs you: {}", parts.join("; "))
}

fn build_report(
    prompt: &str,
    events: &[LedgerEvent],
    ctx: &FilesCtx<'_>,
    disk: &[String],
) -> String {
    // Scrub the whole prompt before taking its first line: a value spanning lines would
    // otherwise leave its first part behind.
    let prompt = scrub(prompt);
    let task = prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| clip(l.trim_start_matches('#').trim(), 120))
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "run".into());

    let result = match run_status(events) {
        RunStatus::Stopped => {
            // The latest stopped call is why the run is stopped now (H-1.4 resumes).
            let stop = stop_listings(events)
                .into_iter()
                .rev()
                .flatten()
                .next()
                .map(|l| &events[l]);
            format!("stopped ({})", stop.map(why_text).unwrap_or_default())
        }
        RunStatus::Partial => "partial".into(),
        _ => "done".into(),
    };

    let read = read_line(events);

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

    // Did: ok side effects the gate let through; never stops or skips.
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
            format!(
                "{} {}: {}",
                class_str(e.class),
                step(e),
                consequence(e, "ok")
            )
        })
        .collect();
    let mut did = format!("Did: {} actions", did_items.len());
    if !did_items.is_empty() {
        did.push_str(&format!(" ({})", list(&did_items)));
    }

    // Files
    let mut out_files = Vec::new();
    for e in events {
        collect_files(&e.consequences, ctx, &mut out_files);
    }
    for f in disk {
        push_unique(&mut out_files, f.clone());
    }
    let files_line = if out_files.is_empty() {
        "Files: none".to_string()
    } else {
        format!("Files: {}", list(&out_files))
    };

    let needs = needs_line(events);

    let text =
        format!("# {task}\nResult: {result}\n{read}\n{asked}\n{did}\n{files_line}\n{needs}\n");
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
/// Absolute paths in consequences count only under this run's `files/`.
/// Returns the text written.
pub fn write_report(run: &RunFolder, prompt: &str, events: &[LedgerEvent]) -> Result<String> {
    let io = |path: PathBuf| move |source| LedgerError::Io { path, source };
    let disk: Vec<String> = walk_files(&run.files())
        .map_err(io(run.files()))?
        .into_iter()
        .map(|rel| format!("runs/{}/files/{rel}", run.id))
        .collect();
    let files_abs = std::path::absolute(run.files()).ok().map(|p| {
        norm_path(&p.to_string_lossy())
            .trim_end_matches('/')
            .to_owned()
    });
    let ctx = FilesCtx {
        id: Some(&run.id.0),
        files_abs,
    };
    let text = build_report(prompt, events, &ctx, &disk);
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

    fn read(server: &str, tool: &str, args: Value) -> LedgerEvent {
        let mut e = ev(server, tool, Class::Read, Some(LedgerMode::Allow));
        e.args = args;
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

    fn line(r: &str, prefix: &str) -> String {
        r.lines()
            .find(|l| l.starts_with(prefix))
            .unwrap_or_default()
            .to_owned()
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
             Read: 2 pages, 1 mails, 1 files (https://a.example/; https://b.example/; mail.read; notes.txt)\n\
             Asked: 1 (1 answered / 0 timed out) (send mail to new@x.com at 09:00:05: answered, proceeded)\n\
             Did: 2 actions (Send echo.send new@x.com: sent to new@x.com; Fill fs.write files/out.md: {{\"files\":[\"F:\\\\x\\\\runs\\\\{RUN}\\\\files\\\\out.md\"]}})\n\
             Files: runs/{RUN}/files/out.md\n\
             Not done / needs you: nothing\n"
        );
        assert_eq!(r, expected);
        assert_eq!(run_status(&events), RunStatus::Done);
        assert_eq!(report("Say hi to new@x.com\nmore detail", &events), r);
    }

    /// Item 3: the real server names (sheetbrowse, web, files, cli) and site tools are
    /// counted; counts are distinct targets and match the refs.
    #[test]
    fn read_counts_real_servers_by_distinct_target() {
        let events = vec![
            read(
                "sheetbrowse",
                "read",
                json!({ "url": "https://news.example/a" }),
            ),
            read(
                "sheetbrowse",
                "read",
                json!({ "url": "https://news.example/a" }),
            ),
            read("web", "fetch", json!({ "url": "https://docs.example/" })),
            read(
                "sheetbrowse",
                "calendar.google.com/events",
                json!({ "id": "evt-1" }),
            ),
            read(
                "sheetbrowse",
                "read",
                json!({ "url": "https://mail.google.com/mail/u/0/#inbox/abc" }),
            ),
            read("sheetbrowse", "outlook.office.com/inbox", json!({})),
            read("files", "read", json!({ "path": "notes.txt" })),
            read("files", "list", json!({})),
            read("files", "list", json!({})),
            read("cli", "gh", json!({ "q": "repo issues" })),
            read("cli", "dns", json!({})),
        ];
        let r = report("t", &events);
        assert_eq!(
            line(&r, "Read: "),
            "Read: 3 pages, 2 mails, 3 files, 2 other (https://news.example/a; \
             https://docs.example/; evt-1; https://mail.google.com/mail/u/0/#inbox/abc; \
             outlook.office.com/inbox; notes.txt; list (x2); repo issues; +1 more)"
        );
        // Long targets sharing the clipped prefix still count apart.
        let long = format!("https://e.example/{}", "p".repeat(120));
        let r = report(
            "t",
            &[fetch(&format!("{long}/a")), fetch(&format!("{long}/b"))],
        );
        assert!(line(&r, "Read: ").starts_with("Read: 2 pages,"), "{r}");
        // No other reads: no `other` part at all.
        let r = report("t", &events[..3]);
        assert_eq!(
            line(&r, "Read: "),
            "Read: 2 pages, 0 mails, 0 files (https://news.example/a; https://docs.example/)"
        );
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

    /// Items 11 + 12: Q-5 failure line (class, why, consequence, fallback); no captured
    /// consequence is `none captured`, never `ok`.
    #[test]
    fn report_failure_q5_fields() {
        let mut bad = fetch("https://down.example/");
        bad.ok = false;
        bad.why = Some("connection refused".into());
        let r = report("read it", std::slice::from_ref(&bad));
        assert!(r.contains("Result: partial"), "{r}");
        assert!(r.contains("Read: 0 pages, 0 mails, 0 files\n"), "{r}");
        assert_eq!(
            line(&r, "Not done"),
            "Not done / needs you: failed fetch https://down.example/ (Read): connection refused; \
             consequence: none captured; fallback: none"
        );

        // The next step is the fallback.
        let events = vec![bad.clone(), fetch("https://mirror.example/")];
        let r = report("read it", &events);
        assert!(
            line(&r, "Not done")
                .ends_with("consequence: none captured; fallback: fetch https://mirror.example/"),
            "{r}"
        );
        assert_eq!(run_status(&events), RunStatus::Partial);
    }

    /// Item 11: a failure followed by an ok retry of the same server+tool+target is resolved.
    #[test]
    fn failure_resolved_by_later_ok_retry() {
        let mut bad = fetch("https://flaky.example/");
        bad.ok = false;
        bad.why = Some("timeout".into());
        let events = vec![bad.clone(), fetch("https://flaky.example/")];
        let r = report("read it", &events);
        assert!(r.contains("\nResult: done\n"), "{r}");
        assert!(r.ends_with("Not done / needs you: nothing\n"), "{r}");
        assert_eq!(run_status(&events), RunStatus::Done);

        // Another target or another tool does not resolve it.
        let mut other_tool = fetch("https://flaky.example/");
        other_tool.tool = "fetch2".into();
        for later in [fetch("https://else.example/"), other_tool] {
            let events = vec![bad.clone(), later];
            assert_eq!(run_status(&events), RunStatus::Partial);
            assert!(report("x", &events).contains("failed fetch https://flaky.example/"));
        }
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

    /// Item 13: the tool event of a challenge-stopped call (mode=stop) is not a Did and the
    /// stop is listed once; skipped calls are not Did either.
    #[test]
    fn stopped_tool_event_not_did_and_listed_once() {
        let mut tool_ev = ev(
            "sheetbrowse",
            "act.click",
            Class::Send,
            Some(LedgerMode::Stop),
        );
        tool_ev.ok = true;
        tool_ev.args = json!({ "url": "https://lu.ma/e" });
        tool_ev.why = Some("hCaptcha".into());
        let mut core_stop = ev(
            "core",
            "act.click",
            Class::Challenge,
            Some(LedgerMode::Stop),
        );
        core_stop.ok = false;
        // The wrapper writes both with the call's args (a tool stop is only folded into a
        // core stop of the same tool and args).
        core_stop.args = tool_ev.args.clone();
        core_stop.why = Some("hCaptcha on lu.ma".into());
        let mut skipped = send("a@x.com", LedgerMode::AnsweredSkip);
        skipped.ok = true;
        let events = vec![skipped, tool_ev, core_stop];
        let r = report("Register", &events);
        assert!(r.contains("\nResult: stopped (hCaptcha on lu.ma)\n"), "{r}");
        assert!(r.contains("\nDid: 0 actions\n"), "{r}");
        assert_eq!(r.matches("stopped at").count(), 1, "{r}");
        assert!(
            r.contains("stopped at act.click (Challenge): hCaptcha on lu.ma; handed back to you"),
            "{r}"
        );
    }

    /// Round 2: a mid-call challenge writes a core stop from `ToolCtx::ask`, then the tool
    /// event with mode=stop and a second core stop with another why. One listing, the first
    /// why; a stop of another call (other args) is still listed.
    #[test]
    fn mid_call_challenge_stop_listed_once() {
        let stop = |server: &str, why: &str| {
            let mut e = ev(
                server,
                "act.click",
                Class::Challenge,
                Some(LedgerMode::Stop),
            );
            e.args = json!({ "ref": "9" });
            e.why = Some(why.into());
            e
        };
        let events = vec![
            stop("core", "challenge during act.click: hCaptcha appeared"),
            stop("sheetbrowse", "hCaptcha on lu.ma"),
            stop("core", "hCaptcha on lu.ma"),
        ];
        let r = report("Register", &events);
        assert_eq!(r.matches("stopped at").count(), 1, "{r}");
        assert!(
            r.contains(
                "
Result: stopped (challenge during act.click: hCaptcha appeared)
"
            ),
            "{r}"
        );
        assert!(
            r.contains(
                "
Did: 0 actions
"
            ),
            "{r}"
        );
        let mut other = stop("core", "Turnstile");
        other.args = json!({ "ref": "12" });
        let mut more = events.clone();
        more.push(other);
        assert_eq!(report("Register", &more).matches("stopped at").count(), 2);
    }

    /// Round 3: a stop event of `act.click {"ref":"9"}` at `ms` after 09:00:00, with the
    /// call's real timing (`duration_ms` only matters on the tool event).
    fn stop_at(server: &str, ms: i64, dur: i32, why: &str) -> LedgerEvent {
        let mut e = ev(
            server,
            "act.click",
            Class::Challenge,
            Some(LedgerMode::Stop),
        );
        e.ts = chrono::Utc.with_ymd_and_hms(2026, 10, 23, 9, 0, 0).unwrap()
            + chrono::TimeDelta::milliseconds(ms);
        e.duration_ms = dur;
        e.args = json!({ "ref": "9" });
        e.why = Some(why.into());
        if server == "core" {
            e.ok = false;
        }
        e
    }

    fn stopped_lines(r: &str) -> Vec<&str> {
        line_of(r, "Not done")
            .trim_start_matches("Not done / needs you: ")
            .split("; handed back to you")
            .map(|s| s.trim_start_matches("; "))
            .filter(|s| s.starts_with("stopped at"))
            .collect()
    }

    fn line_of<'a>(r: &'a str, prefix: &str) -> &'a str {
        r.lines()
            .find(|l| l.starts_with(prefix))
            .unwrap_or_default()
    }

    /// Review round 2, item 1 (H-1.4): the user clears an hCaptcha, resumes, and the same
    /// call (same tool, same args) is stopped again by Turnstile. Two calls, two listings,
    /// each with its own why; `Result:` carries the latest one.
    #[test]
    fn resumed_run_same_call_stopped_twice_lists_both() {
        let events = vec![
            // Call 1: mid-call challenge (ToolCtx::ask stop, tool stop, trailing stop).
            stop_at(
                "core",
                30,
                0,
                "challenge during act.click: hCaptcha appeared",
            ),
            stop_at("sheetbrowse", 60, 50, "hCaptcha on lu.ma"),
            stop_at("core", 62, 0, "hCaptcha on lu.ma"),
            fetch("https://lu.ma/e"),
            // Call 2, after the resume: the same tool and args.
            stop_at(
                "core",
                180_030,
                0,
                "challenge during act.click: Turnstile appeared",
            ),
            stop_at("sheetbrowse", 180_070, 60, "Turnstile on lu.ma"),
            stop_at("core", 180_071, 0, "Turnstile on lu.ma"),
        ];
        let r = report("Register", &events);
        assert_eq!(
            stopped_lines(&r),
            vec![
                "stopped at act.click (Challenge): challenge during act.click: hCaptcha appeared",
                "stopped at act.click (Challenge): challenge during act.click: Turnstile appeared",
            ],
            "{r}"
        );
        assert_eq!(
            line_of(&r, "Result:"),
            "Result: stopped (challenge during act.click: Turnstile appeared)",
            "{r}"
        );

        // Two gate Stops of the same call (no tool events): two listings, latest why.
        let gate = vec![
            stop_at("core", 0, 0, "blocked: hCaptcha on lu.ma"),
            stop_at("core", 120_000, 0, "blocked: Turnstile on lu.ma"),
        ];
        let r = report("Register", &gate);
        assert_eq!(stopped_lines(&r).len(), 2, "{r}");
        assert_eq!(
            line_of(&r, "Result:"),
            "Result: stopped (blocked: Turnstile on lu.ma)"
        );

        // A gate Stop, then a retry the handler stops without an ask: the earlier core stop
        // lies outside the retry's span, so it is not folded into it.
        let retry = vec![
            stop_at("core", 0, 0, "blocked: hCaptcha on lu.ma"),
            stop_at("sheetbrowse", 90_040, 40, "Turnstile on lu.ma"),
            stop_at("core", 90_041, 0, "Turnstile on lu.ma"),
        ];
        let r = report("Register", &retry);
        assert_eq!(
            stopped_lines(&r),
            vec![
                "stopped at act.click (Challenge): blocked: hCaptcha on lu.ma",
                "stopped at act.click (Challenge): Turnstile on lu.ma",
            ],
            "{r}"
        );
        assert_eq!(
            line_of(&r, "Result:"),
            "Result: stopped (Turnstile on lu.ma)"
        );

        // One call is still one listing with real timing (ask stop inside the span).
        let r = report("Register", &events[..3]);
        assert_eq!(stopped_lines(&r).len(), 1, "{r}");
    }

    /// Review round 2, item 2: a tool stop event is dropped only for a core stop of the
    /// same (tool, args). Here `act.click {ref:9}` got a gate Stop and later
    /// `act.click {ref:12}` was challenge-stopped, but its trailing core stop was never
    /// written (write_or_diag only logs a failed write).
    #[test]
    fn tool_stop_kept_when_core_stop_has_other_args() {
        let mut later = stop_at("sheetbrowse", 60_050, 40, "Turnstile at #12");
        later.args = json!({ "ref": "12" });
        let events = vec![stop_at("core", 0, 0, "blocked: hCaptcha on lu.ma"), later];
        let r = report("Register", &events);
        assert_eq!(
            stopped_lines(&r),
            vec![
                "stopped at act.click (Challenge): blocked: hCaptcha on lu.ma",
                "stopped at act.click (Challenge): Turnstile at #12",
            ],
            "{r}"
        );
        assert_eq!(line_of(&r, "Result:"), "Result: stopped (Turnstile at #12)");

        // Same args, trailing stop missing, and the earlier core stop is another call: the
        // tool stop is still its own listing.
        let mut same = stop_at("sheetbrowse", 60_050, 40, "Turnstile at #9");
        same.ok = true;
        let events = vec![stop_at("core", 0, 0, "blocked: hCaptcha on lu.ma"), same];
        let r = report("Register", &events);
        assert_eq!(stopped_lines(&r).len(), 2, "{r}");
    }

    /// Item 10: stops and skipped asks come first and are never cut by the list cap.
    #[test]
    fn stops_and_skips_never_truncated() {
        let mut events: Vec<LedgerEvent> = (0..12)
            .map(|i| {
                let mut e = fetch(&format!("https://down.example/{i}"));
                e.ok = false;
                e.why = Some("refused".into());
                e
            })
            .collect();
        events.push(ask(LedgerMode::AnsweredSkip, "send mail to z@x.com"));
        let mut stop = ev("core", "act", Class::Challenge, Some(LedgerMode::Stop));
        stop.ok = false;
        stop.why = Some("Turnstile".into());
        events.push(stop);
        let r = report("t", &events);
        let needs = line(&r, "Not done");
        assert!(
            needs.starts_with(
                "Not done / needs you: echo.send (Send): send mail to z@x.com: you said skip; \
                 stopped at act (Challenge): Turnstile; handed back to you; failed fetch https://down.example/0 "
            ),
            "{needs}"
        );
        assert!(needs.ends_with("; +4 more"), "{needs}");
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

    /// Item 9: only paths inside `<run_dir>/files` are run files.
    #[test]
    fn run_file_rules() {
        let no_dir = FilesCtx {
            id: Some(RUN),
            files_abs: None,
        };
        let f = |s: &str, ctx: &FilesCtx<'_>| run_file(s, ctx);
        let want = |rel: &str| Some(format!("runs/{RUN}/files/{rel}"));
        assert_eq!(f("files/a/b.pdf", &no_dir), want("a/b.pdf"));
        assert_eq!(f("files\\a\\.\\b.pdf", &no_dir), want("a/b.pdf"));
        assert_eq!(
            f(&format!("runs/{RUN}/files/c.txt"), &no_dir),
            want("c.txt")
        );
        assert_eq!(
            f(&format!("/r/runs/{RUN}/files/x.png"), &no_dir),
            want("x.png")
        );
        for bad in [
            "/other/files/x.png".to_owned(),
            "files/../../etc".to_owned(),
            "files/a/../b".to_owned(),
            format!("x{RUN}/files/a"),
            format!("/r/x{RUN}/files/a"),
            format!("/r/{RUN}/files/a"),
            "files/C:/x".to_owned(),
            "C:files/x".to_owned(),
            "C:\\files\\x".to_owned(),
            "files/".to_owned(),
            "files/a:stream".to_owned(),
            format!("runs/{RUN}-2/files/a"),
            "other/files/a".to_owned(),
        ] {
            assert_eq!(f(&bad, &no_dir), None, "{bad}");
        }

        let base = "F:/work/runs/2026-10-23T09-00-00-test/files".to_owned();
        let with_dir = FilesCtx {
            id: Some(RUN),
            files_abs: Some(base.clone()),
        };
        assert_eq!(
            f(&format!("{base}/sub/a.pdf"), &with_dir),
            want("sub/a.pdf")
        );
        assert_eq!(
            f(
                "F:\\work\\runs\\2026-10-23T09-00-00-test\\files\\a.pdf",
                &with_dir
            ),
            want("a.pdf")
        );
        assert_eq!(f("files/a.pdf", &with_dir), want("a.pdf"));
        // Same run id under another root: elsewhere.
        assert_eq!(
            f(&format!("D:/evil/runs/{RUN}/files/a.pdf"), &with_dir),
            None
        );
        assert_eq!(f(&format!("{base}x/a.pdf"), &with_dir), None);
        assert_eq!(f(&format!("{base}/../files/a.pdf"), &with_dir), None);
        assert_eq!(f(&format!("{base}/C:/a.pdf"), &with_dir), None);

        let no_id = FilesCtx {
            id: None,
            files_abs: None,
        };
        assert_eq!(f("files/a", &no_id).as_deref(), Some("runs/<run>/files/a"));
        assert_eq!(f("/x/runs/r/files/a", &no_id), None);
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

    /// Item 6: U+0000 never reaches the store or the jsonl (Postgres rejects it).
    #[tokio::test]
    async fn nul_becomes_replacement_char() {
        let tmp = Tmp::new();
        let run = RunFolder::create(&tmp.0, "nul", "p").unwrap();
        let store = Arc::new(MemStore::new());
        let ledger = Ledger::new(store.clone(), run.clone());
        let mut e = fetch("https://a/\0");
        e.run_id = None;
        e.ts = ts_now();
        e.server = "web\0".into();
        e.tool = "fe\0tch".into();
        e.result_summary = "a\0b".into();
        e.why = Some("\0".into());
        e.args = json!({ "k\0": ["v\0", { "deep\0": "x\0" }] });
        e.consequences = json!(["c\0"]);
        ledger.write(e).await.unwrap();
        let rows = store.ledger_events(&run.id).await.unwrap();
        let rows_json = serde_json::to_string(&rows).unwrap();
        assert!(!rows_json.contains("\\u0000"), "{rows_json}");
        let (_, row) = &rows[0];
        assert_eq!(row.server, "web\u{FFFD}");
        assert_eq!(row.tool, "fe\u{FFFD}tch");
        assert_eq!(row.result_summary, "a\u{FFFD}b");
        assert_eq!(row.why.as_deref(), Some("\u{FFFD}"));
        assert_eq!(row.args["k\u{FFFD}"][1]["deep\u{FFFD}"], json!("x\u{FFFD}"));
        let text = std::fs::read_to_string(run.ledger_jsonl()).unwrap();
        assert!(!text.contains("\\u0000") && !text.contains('\0'), "{text}");
        assert_eq!(ledger.read_back().unwrap(), rows);
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

    fn jsonl_line(id: i64, e: &LedgerEvent) -> String {
        serde_json::to_string(&Line {
            id: LedgerId(id),
            event: e,
        })
        .unwrap()
    }

    /// Item 4: lines appended out of id order (two processes) read back sorted by id.
    #[test]
    fn read_jsonl_sorts_by_id() {
        let tmp = Tmp::new();
        std::fs::create_dir_all(&tmp.0).unwrap();
        let p = tmp.0.join("ledger.jsonl");
        let (a, b, c) = (
            fetch("https://a/"),
            fetch("https://b/"),
            fetch("https://c/"),
        );
        std::fs::write(
            &p,
            format!(
                "{}\n{}\n{}\n",
                jsonl_line(6, &b),
                jsonl_line(5, &a),
                jsonl_line(7, &c)
            ),
        )
        .unwrap();
        let got = read_jsonl(&p).unwrap();
        let ids: Vec<i64> = got.iter().map(|(id, _)| id.0).collect();
        assert_eq!(ids, vec![5, 6, 7]);
        assert_eq!(got[0].1, a);
    }

    /// Item 5: a torn last line is skipped and reported, the next append starts a fresh
    /// line, and a garbage line in the middle still fails the read.
    #[tokio::test]
    async fn torn_last_line_is_skipped_and_next_append_stays_whole() {
        let tmp = Tmp::new();
        let run = RunFolder::create(&tmp.0, "torn", "p").unwrap();
        let store = Arc::new(MemStore::new());
        let ledger = Ledger::new(store.clone(), run.clone());
        let p = run.ledger_jsonl();
        let mut e = fetch("https://a/");
        e.run_id = None;
        e.ts = ts_now();
        ledger.write(e.clone()).await.unwrap();

        // A crash mid-write: half a line, cut inside a UTF-8 character, no newline.
        let mut torn = jsonl_line(99, &e).into_bytes();
        torn.truncate(torn.len() / 2);
        torn.extend_from_slice(&[b'"', b'x', 0xC3]);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(&torn)
            .unwrap();
        let got = read_jsonl_checked(&p).unwrap();
        assert_eq!(got.events.len(), 1);
        assert_eq!(got.torn, vec![2]);

        // The next write starts on its own line; the cut line stays skipped.
        let id = ledger.write(e.clone()).await.unwrap();
        let got = read_jsonl_checked(&p).unwrap();
        assert_eq!(got.events.len(), 2);
        assert_eq!(got.events[1].0, id);
        assert_eq!(got.torn, vec![2]);
        assert_eq!(got.events, store.ledger_events(&run.id).await.unwrap());
        let text = std::fs::read(&p).unwrap();
        assert_eq!(text.last(), Some(&b'\n'));

        // An unterminated last line that is complete but invalid is torn too...
        std::fs::write(&p, format!("{}\nnot json", jsonl_line(1, &e))).unwrap();
        assert_eq!(read_jsonl_checked(&p).unwrap().torn, vec![2]);
        // ...but garbage in the middle is corruption.
        std::fs::write(
            &p,
            format!("{}\nnot json\n{}\n", jsonl_line(1, &e), jsonl_line(2, &e)),
        )
        .unwrap();
        assert!(matches!(
            read_jsonl(&p),
            Err(LedgerError::Corrupt { line: 2, .. })
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

    /// Item 9 through write_report: an absolute path under this run's files/ counts, the
    /// same run id under another root does not.
    #[tokio::test]
    async fn write_report_files_only_inside_run_dir() {
        let tmp = Tmp::new();
        let run = RunFolder::create(&tmp.0, "abs", "p").unwrap();
        let inside = std::path::absolute(run.files().join("out.md")).unwrap();
        let elsewhere = format!("D:\\evil\\runs\\{}\\files\\x.md", run.id);
        let mut save = ev("files", "write", Class::Fill, Some(LedgerMode::Allow));
        save.run_id = Some(run.id.clone());
        save.consequences = json!([inside.to_string_lossy(), elsewhere]);
        let text = write_report(&run, "p", &[save]).unwrap();
        assert_eq!(
            line(&text, "Files: "),
            format!("Files: runs/{}/files/out.md", run.id)
        );
    }
}
