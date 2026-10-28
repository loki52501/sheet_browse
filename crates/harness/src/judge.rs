//! Reflect judges recipe runs (T-28 (d), D-153; WAVE3 (d)).
//!
//! For every ok `forge` `tools.run` event in another run's `ledger.jsonl` that no core
//! [`JUDGE_TOOL`] event in any run's `ledger.jsonl` marks yet (args `{run, at, domain,
//! name}`), the window is that run's `browseros.jsonl` lines from the event's second until
//! the second of the run's next forge `tools.run` (a line in that second belongs to the next
//! window). The run is `Ok` when the recipe's step tools occur in order among the window's
//! ok lines and, with `expects.text`, that text is in some window line's result summary;
//! else `Failed{why}`. Only runs no longer running are judged.
//!
//! Each verdict is recorded with [`ToolStore::record`] (Ok or Failed) and written as a core
//! `tools.judge` event, so a second pass judges nothing again. A recipe whose version
//! reaches [`RECIPE_FAILS_TO_RETIRE`] failures is retired and its file archived by `record`
//! (not when pinned: then a T-7 retire proposal). The step tools and the filled expects text
//! come from the event's `consequences` (`harness::forge` writes them); an older event without
//! them falls back to the recipe file (expects only when it has no placeholder).

use crate::proposals::{self, Proposal, StaleWhy};
use crate::reflect::ReflectError;
use chrono::{DateTime, SecondsFormat, Timelike, Utc};
use harness_core::ledger::{self, Ledger, BROWSEROS_FILE, CORE_SERVER};
use harness_core::memory::{LedgerEvent, MemoryStore, RunStatus, ToolStatus, Ts};
use harness_core::run::{is_run_id, RunId};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use toolstore::{
    placeholders, RunOutcome, SiteConfig, ToolRunResult, ToolStore, RECIPE_FAILS_TO_RETIRE,
    RECIPE_SUFFIX,
};

/// The core event that records one verdict (and marks the run judged).
pub const JUDGE_TOOL: &str = "tools.judge";

/// One judged (or skipped) recipe run.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub run: RunId,
    /// The `tools.run` event's time, [`at_key`].
    pub at: String,
    pub folder: String,
    pub name: String,
    /// `None`: not judged (the recipe is gone or retired); `note` says why.
    pub outcome: Option<RunOutcome>,
    pub note: String,
    /// The row after `record`.
    pub status: Option<ToolStatus>,
    /// This verdict retired the recipe.
    pub retired: bool,
}

/// What one judge pass did.
#[derive(Debug, Clone, Default)]
pub struct Judged {
    pub verdicts: Vec<Verdict>,
    /// Paths (relative to the tools root) a retire moved: the recipe and its archived copy.
    pub moved: Vec<PathBuf>,
    /// T-7 retire proposals for pinned recipes past the limit.
    pub proposals: Vec<Proposal>,
}

impl Judged {
    pub fn failed(&self) -> usize {
        self.verdicts
            .iter()
            .filter(|v| matches!(v.outcome, Some(RunOutcome::Failed { .. })))
            .count()
    }

    pub fn judged(&self) -> usize {
        self.verdicts.iter().filter(|v| v.outcome.is_some()).count()
    }
}

/// The `at` of a judge mark: the event time, RFC 3339 with microseconds, UTC.
pub fn at_key(ts: Ts) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// One `browseros.jsonl` line, as much as the judge needs.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserLine {
    pub ts: Ts,
    pub tool: String,
    pub ok: bool,
    pub summary: String,
}

/// The hook's `ts` (`%Y-%m-%dT%H:%M:%S%z`, local offset) or RFC 3339.
pub fn parse_hook_ts(s: &str) -> Option<Ts> {
    DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%z")
        .or_else(|_| DateTime::parse_from_rfc3339(s))
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// The readable lines of a run's `browseros.jsonl` (no file: none). A line without a
/// readable time, tool or ok flag is left out.
pub fn browser_lines(path: &Path) -> Vec<BrowserLine> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            Some(BrowserLine {
                ts: parse_hook_ts(v.get("ts")?.as_str()?)?,
                tool: v.get("tool")?.as_str()?.to_owned(),
                ok: v.get("ok")?.as_bool()?,
                summary: v
                    .get("result_summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            })
        })
        .collect()
}

fn floor_sec(t: Ts) -> Ts {
    t.with_nanosecond(0).unwrap_or(t)
}

/// D-153's test over one window: the step tools in order among the ok lines, then the
/// expects text in some line's summary.
pub fn verdict(tools: &[String], expects: Option<&str>, window: &[&BrowserLine]) -> RunOutcome {
    if window.is_empty() {
        return RunOutcome::Failed {
            why: "no browseros call after tools.run".into(),
        };
    }
    let mut ok = window.iter().filter(|l| l.ok);
    for (i, t) in tools.iter().enumerate() {
        if !ok.any(|l| &l.tool == t) {
            return RunOutcome::Failed {
                why: format!(
                    "step {} ({t}) not seen in order among {} ok browseros calls",
                    i + 1,
                    window.iter().filter(|l| l.ok).count()
                ),
            };
        }
    }
    if let Some(text) = expects {
        if !window.iter().any(|l| l.summary.contains(text)) {
            return RunOutcome::Failed {
                why: "expects text not seen in any call's result".into(),
            };
        }
    }
    RunOutcome::Ok
}

/// A run's ledger events (unreadable or missing: none).
fn events(dir: &Path) -> Vec<LedgerEvent> {
    ledger::read_jsonl(&dir.join("ledger.jsonl"))
        .map(|v| v.into_iter().map(|(_, e)| e).collect())
        .unwrap_or_default()
}

type Mark = (String, String, String, String);

fn mark_of(e: &LedgerEvent) -> Option<Mark> {
    if e.server != CORE_SERVER || e.tool != JUDGE_TOOL {
        return None;
    }
    let s = |k: &str| e.args.get(k).and_then(Value::as_str).map(str::to_owned);
    Some((s("run")?, s("at")?, s("domain")?, s("name")?))
}

/// The recipe a `tools.run` event ran, with what to look for.
struct RunRecord {
    folder: String,
    name: String,
    tools: Option<Vec<String>>,
    expects: Option<String>,
}

fn run_record(store: &ToolStore, e: &LedgerEvent) -> Option<RunRecord> {
    let c = &e.consequences;
    let folder = c
        .get("folder")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            e.args
                .get("domain")
                .and_then(Value::as_str)
                .map(|d| store.folder_for(d))
        })
        .filter(|f| !f.is_empty())?;
    let name = c
        .get("name")
        .or_else(|| e.args.get("name"))
        .and_then(Value::as_str)?
        .to_owned();
    let tools = c.get("tools").and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    });
    let expects = c.get("expects").and_then(Value::as_str).map(str::to_owned);
    Some(RunRecord {
        folder,
        name,
        tools,
        expects,
    })
}

/// One judge pass (see the module doc). `current` is the reflect run (never judged); its
/// events (`tools.judge`, and the `tools.retire` that `record` writes through `store`) go
/// to `ledger`.
#[allow(clippy::too_many_arguments)]
pub async fn judge_recipe_runs(
    runs_root: &Path,
    tools: &ToolStore,
    store: &dyn MemoryStore,
    ledger: &Ledger,
    current: &RunId,
    now: Ts,
    day: &str,
) -> Result<Judged, ReflectError> {
    let mut out = Judged::default();
    let Ok(rd) = std::fs::read_dir(runs_root) else {
        return Ok(out);
    };
    let mut dirs: Vec<(String, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            (is_run_id(&n) && e.path().is_dir()).then(|| (n, e.path()))
        })
        .collect();
    dirs.sort();
    let runs: Vec<(String, PathBuf, Vec<LedgerEvent>)> = dirs
        .into_iter()
        .map(|(n, d)| {
            let ev = events(&d);
            (n, d, ev)
        })
        .collect();
    let marks: HashSet<Mark> = runs
        .iter()
        .flat_map(|(_, _, ev)| ev.iter().filter_map(mark_of))
        .collect();
    for (name, dir, ev) in &runs {
        if name == &current.0 {
            continue;
        }
        let forge_runs: Vec<&LedgerEvent> = ev
            .iter()
            .filter(|e| e.server == crate::forge::SERVER && e.tool == crate::forge::RUN_TOOL)
            .collect();
        if !forge_runs.iter().any(|e| e.ok) {
            continue;
        }
        let run_id = RunId(name.clone());
        if store
            .run_get(&run_id)
            .await?
            .is_some_and(|r| r.status == RunStatus::Running)
        {
            continue;
        }
        let lines = browser_lines(&dir.join(BROWSEROS_FILE));
        for (i, e) in forge_runs.iter().enumerate() {
            if !e.ok {
                continue;
            }
            let Some(rec) = run_record(tools, e) else {
                continue;
            };
            let at = at_key(e.ts);
            let key = (
                name.clone(),
                at.clone(),
                rec.folder.clone(),
                rec.name.clone(),
            );
            if marks.contains(&key) {
                continue;
            }
            let from = floor_sec(e.ts);
            let until = forge_runs.get(i + 1).map(|n| floor_sec(n.ts));
            let window: Vec<&BrowserLine> = lines
                .iter()
                .filter(|l| l.ts >= from && until.is_none_or(|u| l.ts < u))
                .collect();
            let (v, moved, proposal) =
                judge_one(tools, store, &run_id, e.ts, &at, &rec, &window, day).await?;
            out.moved.extend(moved);
            out.proposals.extend(proposal);
            write_mark(ledger, &v, now).await?;
            out.verdicts.push(v);
        }
    }
    out.moved.sort();
    out.moved.dedup();
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
async fn judge_one(
    tools: &ToolStore,
    store: &dyn MemoryStore,
    run: &RunId,
    ts: Ts,
    at: &str,
    rec: &RunRecord,
    window: &[&BrowserLine],
    day: &str,
) -> Result<(Verdict, Vec<PathBuf>, Option<Proposal>), ReflectError> {
    let mut v = Verdict {
        run: run.clone(),
        at: at.to_owned(),
        folder: rec.folder.clone(),
        name: rec.name.clone(),
        outcome: None,
        note: String::new(),
        status: None,
        retired: false,
    };
    let before = tools.health(store, &rec.folder, &rec.name).await?;
    if before
        .as_ref()
        .is_some_and(|h| h.status == ToolStatus::Retired)
    {
        v.note = "recipe retired".into();
        v.status = Some(ToolStatus::Retired);
        return Ok((v, Vec::new(), None));
    }
    // record() treats a row as a recipe only while its file is in place.
    let recipe = match tools.recipe(&rec.folder, &rec.name) {
        Ok(Some(r)) => r,
        Ok(None) => {
            v.note = "no recipe file".into();
            return Ok((v, Vec::new(), None));
        }
        Err(e) => {
            v.note = format!("recipe does not load: {e}");
            return Ok((v, Vec::new(), None));
        }
    };
    let step_tools = rec
        .tools
        .clone()
        .unwrap_or_else(|| recipe.step_tools().into_iter().map(str::to_owned).collect());
    let expects = match &rec.tools {
        Some(_) => rec.expects.clone(),
        None => recipe
            .expects
            .as_ref()
            .map(|e| e.text.clone())
            .filter(|t| placeholders(t).is_empty()),
    };
    let outcome = verdict(&step_tools, expects.as_deref(), window);
    let h = tools
        .record(
            store,
            ToolRunResult {
                domain: rec.folder.clone(),
                name: rec.name.clone(),
                outcome: outcome.clone(),
                at: ts,
            },
        )
        .await?;
    v.outcome = Some(outcome);
    v.status = Some(h.status);
    let mut moved = Vec::new();
    if h.status == ToolStatus::Retired {
        v.retired = true;
        moved.push(PathBuf::from(&rec.folder).join(format!("{}{RECIPE_SUFFIX}", rec.name)));
        if let Some(a) = tools.archived_recipe(&rec.folder, &rec.name, h.version) {
            if let Ok(r) = a.strip_prefix(&tools.root) {
                moved.push(r.to_path_buf());
            }
        }
    }
    let pinned = h.pinned
        || SiteConfig::read(&tools.root, &rec.folder)
            .pinned
            .contains(&rec.name);
    let proposal = (pinned && !v.retired && h.fails >= RECIPE_FAILS_TO_RETIRE).then(|| {
        proposals::retire_proposal(
            &rec.folder,
            &rec.name,
            h.version,
            &StaleWhy::RecipeFails(h.fails),
            run,
            day,
        )
    });
    Ok((v, moved, proposal))
}

async fn write_mark(ledger: &Ledger, v: &Verdict, now: Ts) -> Result<(), ReflectError> {
    let (verdict, summary) = match &v.outcome {
        Some(RunOutcome::Ok) => ("ok", format!("{}/{}: ok", v.folder, v.name)),
        Some(RunOutcome::Failed { why }) => {
            ("failed", format!("{}/{}: failed: {why}", v.folder, v.name))
        }
        Some(_) => ("other", format!("{}/{}", v.folder, v.name)),
        None => (
            "skipped",
            format!("{}/{}: skipped: {}", v.folder, v.name, v.note),
        ),
    };
    let status = v.status.map(|s| format!("{s:?}"));
    ledger
        .write(LedgerEvent {
            run_id: None,
            ts: now,
            server: CORE_SERVER.into(),
            tool: JUDGE_TOOL.into(),
            class: None,
            mode: None,
            args: json!({"run": v.run.0, "at": v.at, "domain": v.folder, "name": v.name}),
            result_summary: summary,
            consequences: json!({"verdict": verdict, "status": status, "retired": v.retired}),
            duration_ms: 0,
            ok: true,
            why: None,
        })
        .await?;
    Ok(())
}

#[cfg(test)]
#[path = "judge_tests.rs"]
mod tests;
