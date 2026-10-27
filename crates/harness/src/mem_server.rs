//! `harness mcp`: the `mem` tool server (T-16; docs/02 M-2, M-3; docs/01 H-1.1, H-5, H-6;
//! docs/08 Q-1 rungs 6 to 8; D-141).
//!
//! A Claude Code session reaches memory, schedules and watch state only through these tools,
//! served by `harness_core::serve` like every other tool server (H-6): the wrapper classifies,
//! gates, scrubs the reply and writes the ledger (rule 4). The process is wired by
//! `core_pg::bootstrap` (`DATABASE_URL`, the vault installed as the process-wide scrubber,
//! `SB_RUNS_ROOT`, `SB_RUN_ID`).
//!
//! | tool            | class  | does                                                        |
//! |-----------------|--------|-------------------------------------------------------------|
//! | `review.add`    | Fill   | new SM-2 item, due now (`review_add`)                       |
//! | `review.grade`  | Fill   | SM-2 update (`grade_review`), reports the next due date     |
//! | `review.due`    | Read   | items due by now + `days`, with question and answer         |
//! | `schedule.list` | Read   | every schedule with its next firing (UTC)                   |
//! | `contact.get`   | Read   | one `memory.contacts` row (H-1.1)                           |
//! | `schedule.add`  | Submit | `schedule_add` with the session's run as parent, wakes harnessd |
//! | `schedule.rm`   | Delete | `schedule_rm` (asks by default), wakes harnessd             |
//! | `watch.observe` | Fill   | writes the watched state to `runs/<run>/files/watch.txt`    |
//!
//! Text stored in memory (review item and answer, schedule prompt and watch target) is
//! scrubbed against the vault first: a vault value is stored only as `@vault(name)`. The
//! watch state file is written as given, since the harness hashes it after the run (T-16
//! package A); the tool's reply is scrubbed by the wrapper. Replies are short text (rule 5):
//! user text is cut to one line of at most [`SHOW_MAX`] characters per field.

use crate::CliError;
use async_trait::async_trait;
use chrono::Duration as ChronoDuration;
use harness_core::class::Class;
use harness_core::memory::{contact_key, ts_now, MemoryError, ReviewId, ScheduleKind, Ts};
use harness_core::memory::{Review, Schedule};
use harness_core::sched::{self, next_after, parse_cron};
use harness_core::vault::Vault;
use harness_core::{Classified, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `ledger.events.server` of this server's tool events, and the MCP server name.
pub const SERVER: &str = "mem";

/// The file `watch.observe` writes under `runs/<run>/files/`: the one the harness hashes
/// after a watch firing (`harness_core::watch::WATCH_FILE`, D-140 (1)).
pub const WATCH_FILE: &str = harness_core::watch::WATCH_FILE;

/// Longest review item, review answer or schedule prompt accepted, in characters.
pub const TEXT_MAX: usize = 4000;
/// Longest watch target accepted, in characters.
pub const TARGET_MAX: usize = 200;
/// Largest watch state accepted, in bytes.
pub const STATE_MAX_BYTES: usize = 256 * 1024;
/// Characters of one user-text field shown in a reply.
pub const SHOW_MAX: usize = 200;
/// `review.due` default and largest `k`.
pub const DUE_K_DEFAULT: usize = 10;
pub const DUE_K_MAX: usize = 50;
/// `review.due` largest `days`.
pub const DUE_DAYS_MAX: i64 = 3650;
/// How many reviews `review.grade` scans to report the new due date (I-1 has no
/// `review_get`; B-198 notes).
const GRADE_SCAN: usize = 1000;
/// `schedule.list` shows at most this many rows.
const LIST_MAX: usize = 100;

/// Serves the `mem` tools over stdio until stdin closes (`harness mcp`).
pub async fn serve() -> Result<(), CliError> {
    let harness = core_pg::bootstrap().await?;
    let tools = tools(&harness);
    harness_core::serve(SERVER, tools, harness).await?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    ReviewAdd,
    ReviewGrade,
    ReviewDue,
    ScheduleList,
    ContactGet,
    ScheduleAdd,
    ScheduleRm,
    WatchObserve,
}

struct Mem {
    op: Op,
    /// Scrubs text before it is stored (the wrapper scrubs replies and the ledger itself).
    vault: Arc<Vault>,
    /// For the schedules-changed marker ([`sched::notify_changed`]).
    runs_root: PathBuf,
}

/// Every tool of the `mem` server, bound to `harness`'s vault and runs root.
pub fn tools(harness: &Harness) -> Vec<ToolSpec> {
    let spec = |op: Op, name: &str, class: Class, description: &str, args: Value| ToolSpec {
        name: name.into(),
        description: description.into(),
        class,
        args,
        handler: Arc::new(Mem {
            op,
            vault: harness.vault.clone(),
            runs_root: harness.runs_root.clone(),
        }),
    };
    vec![
        spec(
            Op::ReviewAdd,
            "review.add",
            Class::Fill,
            "Add a spaced-repetition item (question and answer), due now. Returns its id.",
            schema(
                json!({
                    "item": {"type": "string", "description": "the question"},
                    "answer": {"type": "string", "description": "the expected answer"},
                }),
                &["item", "answer"],
            ),
        ),
        spec(
            Op::ReviewGrade,
            "review.grade",
            Class::Fill,
            "Grade a review 0-5 after the user answered (SM-2: 3 or more passes, below 3 \
             resets to 1 day). Returns the next due date.",
            schema(
                json!({
                    "id": {"type": "integer", "minimum": 1},
                    "grade": {"type": "integer", "minimum": 0, "maximum": 5},
                }),
                &["id", "grade"],
            ),
        ),
        spec(
            Op::ReviewDue,
            "review.due",
            Class::Read,
            "List reviews due by now plus `days` (default 0: due now), oldest first, with \
             question, answer and SM-2 state.",
            schema(
                json!({
                    "days": {"type": "integer", "minimum": 0, "maximum": DUE_DAYS_MAX},
                    "k": {"type": "integer", "minimum": 1, "maximum": DUE_K_MAX},
                }),
                &[],
            ),
        ),
        spec(
            Op::ScheduleList,
            "schedule.list",
            Class::Read,
            "List scheduled runs: id, kind, cron (UTC), next firing, parent run, prompt.",
            schema(json!({}), &[]),
        ),
        spec(
            Op::ContactGet,
            "contact.get",
            Class::Read,
            "Look up a person (address or handle) in memory.contacts: how often and when \
             they were last contacted. A Send to someone not here asks the user first.",
            schema(json!({"person": {"type": "string"}}), &["person"]),
        ),
        spec(
            Op::ScheduleAdd,
            "schedule.add",
            Class::Submit,
            "Schedule a prompt to run later in its own fresh run, e.g. `0 8 * * 1-5` (5-field \
             cron, UTC). kind cron (default), watch (needs watch_target; the run writes the \
             watched state with watch.observe and the harness notifies once on change) or \
             reflect. This run becomes the parent.",
            schema(
                json!({
                    "cron": {"type": "string"},
                    "prompt": {"type": "string"},
                    "kind": {"type": "string", "enum": ["cron", "watch", "reflect"]},
                    "watch_target": {"type": "string"},
                    "enabled": {"type": "boolean"},
                }),
                &["cron", "prompt"],
            ),
        ),
        spec(
            Op::ScheduleRm,
            "schedule.rm",
            Class::Delete,
            "Remove a schedule by id (asks the user first).",
            schema(json!({"id": {"type": "integer", "minimum": 1}}), &["id"]),
        ),
        spec(
            Op::WatchObserve,
            "watch.observe",
            Class::Fill,
            "In a watch run: record the watched state (short text that changes exactly when \
             the thing watched changes, e.g. `registration: closed`). The harness compares it \
             with the last run's after this run ends.",
            schema(json!({"state": {"type": "string"}}), &["state"]),
        ),
    ]
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

#[async_trait]
impl ToolHandler for Mem {
    async fn classify(&self, ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let what = match self.op {
            Op::ScheduleAdd => {
                let cron = opt_str(args, "cron")?.unwrap_or_default();
                let prompt = opt_str(args, "prompt")?.unwrap_or_default();
                Some(format!(
                    "add a schedule `{}`: {}",
                    show(&cron, 60),
                    show(&prompt, 120)
                ))
            }
            Op::ScheduleRm => {
                let id = schedule_id(args)?;
                // Read-only lookup so the ask card says what goes (H-1.3).
                let found = ctx
                    .store()
                    .schedule_list()
                    .await
                    .map_err(mem_err)?
                    .into_iter()
                    .find(|s| s.id == id);
                Some(match found {
                    Some(s) => format!(
                        "remove schedule {id} ({} `{}`): {}",
                        kind_str(s.kind),
                        show(&s.cron, 60),
                        show(&s.prompt, 120)
                    ),
                    None => format!("remove schedule {id} (no such schedule)"),
                })
            }
            _ => None,
        };
        Ok(Classified {
            what: what.map(|w| self.vault.scrub(&w)),
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        match self.op {
            Op::ReviewAdd => self.review_add(ctx, &args).await,
            Op::ReviewGrade => self.review_grade(ctx, &args).await,
            Op::ReviewDue => self.review_due(ctx, &args).await,
            Op::ScheduleList => self.schedule_list(ctx).await,
            Op::ContactGet => self.contact_get(ctx, &args).await,
            Op::ScheduleAdd => self.schedule_add(ctx, &args).await,
            Op::ScheduleRm => self.schedule_rm(ctx, &args).await,
            Op::WatchObserve => self.watch_observe(ctx, &args),
        }
    }
}

impl Mem {
    /// Non-blank, at most `max` characters, scrubbed for storage.
    fn stored_text(&self, args: &Value, key: &str, max: usize) -> Result<String, ToolError> {
        let s = req_str(args, key)?;
        if s.trim().is_empty() {
            return Err(ToolError::Args(format!("{key} is empty")));
        }
        if s.chars().count() > max {
            return Err(ToolError::Args(format!(
                "{key} is longer than {max} characters"
            )));
        }
        Ok(self.vault.scrub(s.trim()))
    }

    async fn review_add(&self, ctx: &ToolCtx, args: &Value) -> Result<ToolOutput, ToolError> {
        let item = self.stored_text(args, "item", TEXT_MAX)?;
        let answer = self.stored_text(args, "answer", TEXT_MAX)?;
        let id = ctx
            .store()
            .review_add(&item, &answer)
            .await
            .map_err(mem_err)?;
        Ok(text(format!(
            "review {} added, due now: {}",
            id.0,
            show(&item, SHOW_MAX)
        )))
    }

    async fn review_grade(&self, ctx: &ToolCtx, args: &Value) -> Result<ToolOutput, ToolError> {
        let id = positive_int(args, "id")?;
        let grade = match args.get("grade").and_then(Value::as_u64) {
            Some(g) if g <= 5 => g as u8,
            _ => return Err(ToolError::Args("grade must be an integer 0 to 5".into())),
        };
        let store = ctx.store();
        store
            .grade_review(ReviewId(id), grade)
            .await
            .map_err(mem_err)?;
        // I-1 has no review_get: find the row among everything due before a far horizon.
        let far = ts_now() + ChronoDuration::days(365 * 200);
        let row = store
            .due_reviews(far, GRADE_SCAN)
            .await
            .map_err(mem_err)?
            .into_iter()
            .find(|r| r.id.0 == id);
        let verdict = if grade >= 3 { "pass" } else { "fail, reset" };
        Ok(text(match row {
            Some(r) => format!(
                "review {id} graded {grade} ({verdict}): next due {} UTC ({})",
                ts_str(r.due),
                sm2_state(&r)
            ),
            None => format!("review {id} graded {grade} ({verdict})"),
        }))
    }

    async fn review_due(&self, ctx: &ToolCtx, args: &Value) -> Result<ToolOutput, ToolError> {
        let days = match args.get("days") {
            None | Some(Value::Null) => 0,
            Some(v) => match v.as_i64() {
                Some(d) if (0..=DUE_DAYS_MAX).contains(&d) => d,
                _ => {
                    return Err(ToolError::Args(format!(
                        "days must be an integer 0 to {DUE_DAYS_MAX}"
                    )))
                }
            },
        };
        let k = match args.get("k") {
            None | Some(Value::Null) => DUE_K_DEFAULT,
            Some(v) => match v.as_u64() {
                Some(k) if (1..=DUE_K_MAX as u64).contains(&k) => k as usize,
                _ => {
                    return Err(ToolError::Args(format!(
                        "k must be an integer 1 to {DUE_K_MAX}"
                    )))
                }
            },
        };
        let until = ts_now() + ChronoDuration::days(days);
        let due = ctx.store().due_reviews(until, k).await.map_err(mem_err)?;
        let by = if days == 0 {
            "now".to_owned()
        } else {
            format!("{} UTC", ts_str(until))
        };
        if due.is_empty() {
            return Ok(text(format!("no reviews due by {by}")));
        }
        let mut out = format!("{} due by {by}:", due.len());
        for r in &due {
            out.push_str(&format!(
                "\n- review {} due {} UTC ({})\n  Q: {}\n  A: {}",
                r.id.0,
                ts_str(r.due),
                sm2_state(r),
                show(&r.item, SHOW_MAX),
                show(&r.answer, SHOW_MAX)
            ));
        }
        Ok(ToolOutput {
            summary: Some(format!("{} due by {by}", due.len())),
            ..text(out)
        })
    }

    async fn schedule_list(&self, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let list = ctx.store().schedule_list().await.map_err(mem_err)?;
        if list.is_empty() {
            return Ok(text("no schedules".to_owned()));
        }
        let now = ts_now();
        let mut out = format!("{} schedules (times UTC):", list.len());
        for s in list.iter().take(LIST_MAX) {
            out.push('\n');
            out.push_str(&schedule_line(s, now));
        }
        if list.len() > LIST_MAX {
            out.push_str(&format!("\n({} more not shown)", list.len() - LIST_MAX));
        }
        Ok(ToolOutput {
            summary: Some(format!("{} schedules", list.len())),
            ..text(out)
        })
    }

    async fn contact_get(&self, ctx: &ToolCtx, args: &Value) -> Result<ToolOutput, ToolError> {
        let person = req_str(args, "person")?;
        let key = contact_key(person);
        if key.is_empty() {
            return Err(ToolError::Args("person is empty".into()));
        }
        let shown = show(&key, SHOW_MAX);
        Ok(text(
            match ctx.store().contact(&key).await.map_err(mem_err)? {
                Some(c) => format!(
                    "contact {shown}: contacted {} time{}, last {}{}{}",
                    c.count,
                    if c.count == 1 { "" } else { "s" },
                    c.last
                        .map_or_else(|| "-".to_owned(), |t| format!("{} UTC", ts_str(t))),
                    c.channel
                        .as_deref()
                        .map(|ch| format!(", via {}", show(ch, 80)))
                        .unwrap_or_default(),
                    c.note
                        .as_deref()
                        .map(|n| format!(", note: {}", show(n, SHOW_MAX)))
                        .unwrap_or_default(),
                ),
                None => format!("{shown} is not a contact: a Send to them asks the user first"),
            },
        ))
    }

    async fn schedule_add(&self, ctx: &ToolCtx, args: &Value) -> Result<ToolOutput, ToolError> {
        let cron_raw = req_str(args, "cron")?;
        let parsed = parse_cron(cron_raw).map_err(|e| ToolError::Args(e.to_string()))?;
        let cron = cron_raw.split_whitespace().collect::<Vec<_>>().join(" ");
        let prompt = self.stored_text(args, "prompt", TEXT_MAX)?;
        let kind = match opt_str(args, "kind")?.as_deref() {
            None | Some("cron") => ScheduleKind::Cron,
            Some("watch") => ScheduleKind::Watch,
            Some("reflect") => ScheduleKind::Reflect,
            Some(_) => {
                return Err(ToolError::Args(
                    "kind must be cron, watch or reflect".into(),
                ))
            }
        };
        let watch_target = match (kind, args.get("watch_target")) {
            (ScheduleKind::Watch, Some(Value::String(_))) => {
                Some(self.stored_text(args, "watch_target", TARGET_MAX)?)
            }
            (ScheduleKind::Watch, _) => {
                return Err(ToolError::Args("kind watch needs watch_target".into()))
            }
            (_, None | Some(Value::Null)) => None,
            (_, Some(_)) => {
                return Err(ToolError::Args(
                    "watch_target is only for kind watch".into(),
                ))
            }
        };
        let enabled = match args.get("enabled") {
            None | Some(Value::Null) => true,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(ToolError::Args("enabled must be true or false".into())),
        };
        let row = Schedule {
            id: 0,
            cron: cron.clone(),
            prompt: prompt.clone(),
            kind,
            watch_target: watch_target.clone(),
            enabled,
            last_run: None,
            // The session's run (SB_RUN_ID, through bootstrap) is every firing's parent_run.
            run_id: Some(ctx.run_id.clone()),
        };
        let id = ctx.store().schedule_add(row).await.map_err(mem_err)?;
        // The row exists either way; a marker that cannot be written is reported, not an
        // error the model would retry into a duplicate schedule.
        let woke = match sched::notify_changed(&self.runs_root) {
            Ok(()) => String::new(),
            Err(e) => format!(
                "; warning: harnessd was not woken ({}), it reads the schedule when it next starts",
                show(&e.to_string(), 120)
            ),
        };
        let next = if enabled {
            next_after(&parsed, ts_now())
                .map_or_else(|| "never".to_owned(), |t| format!("{} UTC", ts_str(t)))
        } else {
            "- (disabled)".to_owned()
        };
        let target = watch_target
            .as_deref()
            .map(|t| format!(", watching {}", show(t, 80)))
            .unwrap_or_default();
        Ok(ToolOutput {
            summary: Some(format!("schedule {id} added")),
            consequences: json!({"schedule": id, "kind": kind_str(kind), "parent_run": ctx.run_id.0}),
            ..text(format!(
                "schedule {id} added: {} `{cron}` UTC{target}, next {next}; parent run {}{woke}",
                kind_str(kind),
                ctx.run_id.0
            ))
        })
    }

    async fn schedule_rm(&self, ctx: &ToolCtx, args: &Value) -> Result<ToolOutput, ToolError> {
        let id = schedule_id(args)?;
        ctx.store().schedule_rm(id).await.map_err(mem_err)?;
        let woke = match sched::notify_changed(&self.runs_root) {
            Ok(()) => String::new(),
            Err(e) => format!(
                "; warning: harnessd was not woken ({})",
                show(&e.to_string(), 120)
            ),
        };
        Ok(ToolOutput {
            consequences: json!({"schedule_removed": id}),
            ..text(format!("schedule {id} removed{woke}"))
        })
    }

    fn watch_observe(&self, ctx: &ToolCtx, args: &Value) -> Result<ToolOutput, ToolError> {
        let state = req_str(args, "state")?;
        if state.trim().is_empty() {
            return Err(ToolError::Args("state is empty".into()));
        }
        if state.len() > STATE_MAX_BYTES {
            return Err(ToolError::Args(format!(
                "state is larger than {} KiB; record only what decides a change",
                STATE_MAX_BYTES / 1024
            )));
        }
        let files = ctx.run_dir.join("files");
        let path = write_atomic(&files, WATCH_FILE, state.as_bytes())
            .map_err(|e| ToolError::Failed(format!("writing files/{WATCH_FILE}: {e}")))?;
        Ok(ToolOutput {
            files: vec![path],
            ..text(format!(
                "watch state recorded in files/{WATCH_FILE} ({} chars); the harness compares \
                 it with the baseline when this run ends",
                state.chars().count()
            ))
        })
    }
}

/// Writes `dir/name` through a temp file and a rename, so a reader never sees half a file.
fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, bytes)?;
    let path = dir.join(name);
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

fn text(s: String) -> ToolOutput {
    ToolOutput {
        text: s,
        ..ToolOutput::default()
    }
}

fn mem_err(e: MemoryError) -> ToolError {
    match e {
        MemoryError::NotFound(m) => ToolError::Args(format!("not found: {m}")),
        MemoryError::Invalid(m) => ToolError::Args(m),
        other => ToolError::Failed(other.to_string()),
    }
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::Args(format!("{key} (string) is required")))
}

fn opt_str(args: &Value, key: &str) -> Result<Option<String>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(ToolError::Args(format!("{key} must be a string"))),
    }
}

fn positive_int(args: &Value, key: &str) -> Result<i64, ToolError> {
    match args.get(key).and_then(Value::as_i64) {
        Some(n) if n >= 1 => Ok(n),
        _ => Err(ToolError::Args(format!("{key} must be a positive integer"))),
    }
}

fn schedule_id(args: &Value) -> Result<i32, ToolError> {
    i32::try_from(positive_int(args, "id")?)
        .map_err(|_| ToolError::Args("id is out of range".into()))
}

/// One line: control characters (line breaks included) become spaces, runs of spaces
/// collapse, cut to `max` characters with `...`.
fn show(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in s.chars() {
        if c.is_control() || c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    if out.chars().count() > max {
        let mut cut: String = out.chars().take(max.saturating_sub(3)).collect();
        cut.push_str("...");
        cut
    } else {
        out
    }
}

fn ts_str(t: Ts) -> String {
    t.format("%Y-%m-%d %H:%M").to_string()
}

fn sm2_state(r: &Review) -> String {
    format!(
        "interval {} d, ease {:.2}, reps {}",
        r.interval_days, r.ease, r.reps
    )
}

fn kind_str(k: ScheduleKind) -> &'static str {
    match k {
        ScheduleKind::Cron => "cron",
        ScheduleKind::Watch => "watch",
        ScheduleKind::Reflect => "reflect",
    }
}

fn schedule_line(s: &Schedule, now: Ts) -> String {
    let next = if !s.enabled {
        "off".to_owned()
    } else {
        match parse_cron(&s.cron) {
            Ok(c) => next_after(&c, now).map_or_else(|| "never".to_owned(), ts_str),
            Err(_) => "bad cron".to_owned(),
        }
    };
    format!(
        "- schedule {} {} `{}`{}, next {next}, last {}, parent {}: {}",
        s.id,
        kind_str(s.kind),
        show(&s.cron, 60),
        s.watch_target
            .as_deref()
            .map(|t| format!(" watching {}", show(t, 80)))
            .unwrap_or_default(),
        s.last_run.map_or_else(|| "-".to_owned(), ts_str),
        s.run_id
            .as_ref()
            .map_or_else(|| "-".to_owned(), |r| show(&r.0, 80)),
        show(&s.prompt, 120)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_is_one_capped_line() {
        assert_eq!(show("a\n\n b\t\u{1b}[2Jc ", 100), "a b [2Jc");
        assert_eq!(show("abcdefghij", 6), "abc...");
        assert_eq!(show("   ", 10), "");
    }

    #[test]
    fn arg_helpers_reject_wrong_types() {
        let a = json!({"id": 0, "s": 3, "n": "x"});
        assert!(positive_int(&a, "id").is_err());
        assert!(opt_str(&a, "s").is_err());
        assert!(req_str(&a, "missing").is_err());
        assert!(schedule_id(&json!({"id": 1_i64 << 40})).is_err());
        assert_eq!(schedule_id(&json!({"id": 7})).unwrap(), 7);
    }
}
