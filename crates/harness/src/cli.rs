//! `harness` subcommands (H-7, plus `ask` from D-105):
//!
//! ```text
//! harness run "prompt" [--strict]             start a run interactively
//! harness schedule add <cron> <prompt> [--kind cron|watch|reflect] [--watch-target T] [--disabled]
//! harness schedule list
//! harness schedule rm <id>
//! harness vault set <name> [--personal]       value read from the terminal, never argv
//! harness gate set <domain> <class> <mode> [--strict] [--note N]
//! harness tools list|health [domain]
//! harness tools pin|unpin <domain>/<name>
//! harness tools retire <domain>/<name> [--why W] [--no-commit]
//! harness ledger <run>                        print the ledger as a table
//! harness db migrate|status
//! harness ask <run> <n> yes|no                answer an ask card (D-105)
//! ```
//!
//! [`execute`] runs one parsed command against a [`Ctx`] and writes its output to `out`.
//! Everything printed from the database or a run folder goes through the vault scrub and
//! has control characters replaced, so page text cannot drive the terminal.

use crate::{ClaudeLauncher, CliError, Env};
use clap::{Parser, Subcommand, ValueEnum};
use harness_core::class::Mode;
use harness_core::ledger::{self, Ledger, CORE_SERVER};
use harness_core::memory::{
    ts_now, GateRule, LedgerEvent, MemoryStore, Run, RunStatus, Schedule, ScheduleKind, ToolHealth,
    ToolStatus, Ts,
};
use harness_core::run::{is_run_id, RunFolder, RunId};
use harness_core::sched::{self, next_after, parse_cron, RunSpec, Runner, SystemClock};
use harness_core::vault::{scrub, EntryKind};
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use toolstore::ToolStore;
use zeroize::Zeroizing;

/// `tool` of the core ledger event `harness schedule add` writes in the parent run.
pub const SCHEDULE_ADD_TOOL: &str = "schedule.add";

#[derive(Debug, Parser)]
#[command(name = "harness", version, about = "sheetbrowse harness (docs/01 H-7)")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Start a run interactively (a Claude Code session with a run folder)
    Run {
        prompt: String,
        /// Every ask in this run times out to skip (H-1.3)
        #[arg(long)]
        strict: bool,
    },
    /// Scheduled runs for harnessd (H-5)
    Schedule {
        #[command(subcommand)]
        cmd: ScheduleCmd,
    },
    /// The vault (H-2)
    Vault {
        #[command(subcommand)]
        cmd: VaultCmd,
    },
    /// Gate rules (H-1.2)
    Gate {
        #[command(subcommand)]
        cmd: GateCmd,
    },
    /// Site tools (docs/03 T-7)
    Tools {
        #[command(subcommand)]
        cmd: ToolsCmd,
    },
    /// Print a run's ledger as a table (H-3)
    Ledger { run: String },
    /// Database maintenance
    Db {
        #[command(subcommand)]
        cmd: DbCmd,
    },
    /// Answer an ask card from a running tool server (D-105)
    Ask {
        run: String,
        n: u64,
        #[arg(value_enum)]
        answer: YesNo,
    },
}

#[derive(Debug, Subcommand)]
pub enum ScheduleCmd {
    /// Add a schedule: 5-field cron in UTC (D-107) and the prompt to run
    Add {
        cron: String,
        prompt: String,
        #[arg(long, value_enum, default_value_t = KindArg::Cron)]
        kind: KindArg,
        /// For kind watch: the memory.baselines key
        #[arg(long)]
        watch_target: Option<String>,
        /// Store it disabled
        #[arg(long)]
        disabled: bool,
    },
    List,
    Rm {
        id: i32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum KindArg {
    Cron,
    Watch,
    Reflect,
}

impl From<KindArg> for ScheduleKind {
    fn from(k: KindArg) -> Self {
        match k {
            KindArg::Cron => ScheduleKind::Cron,
            KindArg::Watch => ScheduleKind::Watch,
            KindArg::Reflect => ScheduleKind::Reflect,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum VaultCmd {
    /// Add or replace an entry; the value is read from the terminal without echo
    Set {
        name: String,
        /// Form data (name, address) rather than a credential
        #[arg(long)]
        personal: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum GateCmd {
    /// Set a rule: <domain|*> <Class or Class:hint> <allow|ask|stop>
    Set {
        domain: String,
        class: String,
        mode: String,
        /// Every ask under this rule times out to skip (H-1.3)
        #[arg(long)]
        strict: bool,
        #[arg(long, default_value = "")]
        note: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ToolsCmd {
    /// Tools on disk with their status
    List {
        domain: Option<String>,
    },
    /// Every tools.health row with its why (T-7)
    Health {
        domain: Option<String>,
    },
    /// Never auto-retired or deleted (G-5)
    Pin {
        tool: String,
    },
    Unpin {
        tool: String,
    },
    /// Retire and move to tools/_archive/ (T-7)
    Retire {
        tool: String,
        #[arg(long)]
        why: Option<String>,
        /// Move the file but do not git commit it
        #[arg(long)]
        no_commit: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum DbCmd {
    /// Apply pending migrations
    Migrate,
    /// Show applied and pending migrations without applying any
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum YesNo {
    Yes,
    No,
}

/// Reads a secret without echo. The binary uses rpassword; tests pass a closure.
pub type SecretReader = Box<dyn Fn(&str) -> std::io::Result<Zeroizing<String>> + Send + Sync>;

/// What a command runs against.
pub struct Ctx {
    pub env: Env,
    /// Preset store (tests); `None` connects to `env.database_url` on first use.
    pub store: Option<Arc<dyn MemoryStore>>,
    pub read_secret: SecretReader,
    /// Launcher for `harness run`; `None` means interactive Claude Code.
    pub launcher: Option<Arc<dyn sched::Launcher>>,
}

impl Ctx {
    /// The binary's context: Postgres from `env`, rpassword for secrets, interactive claude.
    pub fn new(env: Env) -> Ctx {
        Ctx {
            env,
            store: None,
            read_secret: Box::new(|prompt| rpassword::prompt_password(prompt).map(Zeroizing::new)),
            launcher: None,
        }
    }

    async fn store(&self) -> Result<Arc<dyn MemoryStore>, CliError> {
        match &self.store {
            Some(s) => Ok(s.clone()),
            None => Ok(Arc::new(self.env.connect().await?)),
        }
    }
}

/// Runs one command.
pub async fn execute(cli: Cli, ctx: &Ctx, out: &mut dyn Write) -> Result<(), CliError> {
    match cli.cmd {
        Cmd::Run { prompt, strict } => run(ctx, &prompt, strict, out).await,
        Cmd::Schedule { cmd } => match cmd {
            ScheduleCmd::Add {
                cron,
                prompt,
                kind,
                watch_target,
                disabled,
            } => {
                schedule_add(
                    ctx,
                    &cron,
                    &prompt,
                    kind.into(),
                    watch_target,
                    !disabled,
                    out,
                )
                .await
            }
            ScheduleCmd::List => schedule_list(ctx, out).await,
            ScheduleCmd::Rm { id } => {
                ctx.store().await?.schedule_rm(id).await?;
                sched::notify_changed(&ctx.env.runs_root)?;
                writeln!(out, "schedule {id} removed")?;
                Ok(())
            }
        },
        Cmd::Vault {
            cmd: VaultCmd::Set { name, personal },
        } => vault_set(ctx, &name, personal, out),
        Cmd::Gate {
            cmd:
                GateCmd::Set {
                    domain,
                    class,
                    mode,
                    strict,
                    note,
                },
        } => {
            let mode: Mode = mode
                .parse()
                .map_err(|e: harness_core::class::UnknownMode| CliError::Usage(e.to_string()))?;
            ctx.store()
                .await?
                .gate_rule_set(&domain, &class, GateRule { mode, strict }, &note)
                .await?;
            writeln!(
                out,
                "gate: {} {} -> {mode}{}",
                clean(&domain),
                clean(&class),
                if strict { " (strict)" } else { "" }
            )?;
            Ok(())
        }
        Cmd::Tools { cmd } => tools(ctx, cmd, out).await,
        Cmd::Ledger { run } => print_ledger(ctx, &run, out).await,
        Cmd::Db { cmd } => db(ctx, cmd, out).await,
        Cmd::Ask { run, n, answer } => {
            let yes = answer == YesNo::Yes;
            harness_core::ask::answer(&ctx.env.runs_root, &RunId(run.clone()), n, yes)?;
            writeln!(
                out,
                "ask {n} in run {}: {}",
                clean(&run),
                if yes { "yes" } else { "no" }
            )?;
            Ok(())
        }
    }
}

/// Scrubbed, control characters replaced by spaces, cut to `max` chars (0 = no cut).
fn cell(s: &str, max: usize) -> String {
    let s: String = scrub(s)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if max > 0 && s.chars().count() > max {
        let mut t: String = s.chars().take(max.saturating_sub(3)).collect();
        t.push_str("...");
        t
    } else {
        s
    }
}

fn clean(s: &str) -> String {
    cell(s, 0)
}

fn ts_cell(t: Option<Ts>) -> String {
    t.map_or_else(
        || "-".to_owned(),
        |t| t.format("%Y-%m-%d %H:%M:%S").to_string(),
    )
}

/// Left-aligned columns separated by two spaces.
fn table(out: &mut dyn Write, head: &[&str], rows: &[Vec<String>]) -> std::io::Result<()> {
    let mut w: Vec<usize> = head.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(c.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let mut s = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                s.push_str(c);
            } else {
                s.push_str(c);
                s.push_str(&" ".repeat(w[i] - c.chars().count() + 2));
            }
        }
        s.trim_end().to_owned()
    };
    writeln!(out, "{}", line(head.to_vec()))?;
    for r in rows {
        writeln!(out, "{}", line(r.iter().map(String::as_str).collect()))?;
    }
    Ok(())
}

fn status_str(s: RunStatus) -> &'static str {
    match s {
        RunStatus::Running => "running",
        RunStatus::Done => "done",
        RunStatus::Partial => "partial",
        RunStatus::Stopped => "stopped",
        RunStatus::Failed => "failed",
    }
}

fn kind_str(k: ScheduleKind) -> &'static str {
    match k {
        ScheduleKind::Cron => "cron",
        ScheduleKind::Watch => "watch",
        ScheduleKind::Reflect => "reflect",
    }
}

// ---------------------------------------------------------------- run

async fn run(ctx: &Ctx, prompt: &str, strict: bool, out: &mut dyn Write) -> Result<(), CliError> {
    ctx.env.install_vault()?;
    let store = ctx.store().await?;
    let launcher = ctx
        .launcher
        .clone()
        .unwrap_or_else(|| Arc::new(ClaudeLauncher::interactive(ctx.env.claude.clone())));
    let runner = Runner {
        store,
        launcher,
        clock: Arc::new(SystemClock),
        runs_root: ctx.env.runs_root.clone(),
    };
    let f = runner
        .execute(RunSpec {
            prompt,
            parent_run: ctx.env.run_id.as_ref(),
            strict,
            schedule: None,
        })
        .await?;
    writeln!(
        out,
        "run {}: {}{}\nreport: {}",
        f.run,
        status_str(f.status),
        f.launch_problem
            .as_deref()
            .map(|p| format!(" ({p})"))
            .unwrap_or_default(),
        f.folder.join("report.md").display()
    )?;
    Ok(())
}

// ---------------------------------------------------------------- schedule

fn ledger_event(run: &RunId, tool: &str, args: serde_json::Value, summary: String) -> LedgerEvent {
    LedgerEvent {
        run_id: Some(run.clone()),
        ts: ts_now(),
        server: CORE_SERVER.into(),
        tool: tool.into(),
        class: None,
        mode: None,
        args,
        result_summary: summary,
        consequences: json!({}),
        duration_ms: 0,
        ok: true,
        why: None,
    }
}

/// `harness schedule add` (H-5, D-107). Inside a session (`SB_RUN_ID`) the schedule's run is
/// that run; otherwise the command creates its own run `<ts>-schedule-add` (status done, with
/// report.md), so every firing has `parent_run` set. Wakes harnessd through the marker.
pub async fn schedule_add(
    ctx: &Ctx,
    cron: &str,
    prompt: &str,
    kind: ScheduleKind,
    watch_target: Option<String>,
    enabled: bool,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let parsed = parse_cron(cron)?;
    if kind == ScheduleKind::Watch && watch_target.is_none() {
        return Err(CliError::Usage("--kind watch needs --watch-target".into()));
    }
    if prompt.trim().is_empty() {
        return Err(CliError::Usage("the prompt is empty".into()));
    }
    ctx.env.install_vault()?;
    let store = ctx.store().await?;
    let prompt = scrub(prompt);
    let cron = cron.split_whitespace().collect::<Vec<_>>().join(" ");
    let args = |id: i32| {
        json!({"schedule": id, "cron": cron, "kind": kind_str(kind), "prompt": prompt,
               "watch_target": watch_target})
    };

    let (parent, own) = match &ctx.env.run_id {
        Some(id) => {
            let folder = RunFolder::open(&ctx.env.runs_root, id)?;
            if store.run_get(id).await?.is_none() {
                return Err(CliError::NotFound(format!(
                    "SB_RUN_ID {} has no runs row",
                    clean(&id.0)
                )));
            }
            (folder, false)
        }
        None => {
            let task = format!("schedule add `{cron}`: {prompt}\n");
            let started = ts_now();
            let folder = RunFolder::create_at(&ctx.env.runs_root, "schedule-add", &task, started)?;
            store
                .run_start(Run {
                    id: folder.id.clone(),
                    started,
                    ended: None,
                    prompt: task,
                    status: RunStatus::Running,
                    folder: folder.dir.to_string_lossy().replace('\\', "/"),
                    parent_run: None,
                    strict: false,
                })
                .await?;
            (folder, true)
        }
    };

    let row = Schedule {
        id: 0,
        cron: cron.clone(),
        prompt: prompt.clone(),
        kind,
        watch_target: watch_target.clone(),
        enabled,
        last_run: None,
        run_id: Some(parent.id.clone()),
    };
    let added = store.schedule_add(row).await;
    let id = match added {
        Ok(id) => id,
        Err(e) => {
            if own {
                let _ = store.run_end(&parent.id, RunStatus::Failed, ts_now()).await;
            }
            return Err(e.into());
        }
    };
    let ledger = Ledger::new(store.clone(), parent.clone());
    ledger
        .write(ledger_event(
            &parent.id,
            SCHEDULE_ADD_TOOL,
            args(id),
            format!("schedule {id} added"),
        ))
        .await?;
    if own {
        let events: Vec<LedgerEvent> = store
            .ledger_events(&parent.id)
            .await?
            .into_iter()
            .map(|(_, e)| e)
            .collect();
        let task = std::fs::read_to_string(parent.prompt_md())?;
        ledger::write_report(&parent, &task, &events)?;
        store.run_end(&parent.id, RunStatus::Done, ts_now()).await?;
    }
    sched::notify_changed(&ctx.env.runs_root)?;

    let next = next_after(&parsed, ts_now());
    writeln!(
        out,
        "schedule {id} added: `{cron}` UTC, next {}; parent run {}",
        ts_cell(next),
        parent.id
    )?;
    if kind != ScheduleKind::Cron {
        writeln!(
            out,
            "note: kind {} is stored; harnessd runs it from T-16 (not in Phase 1)",
            kind_str(kind)
        )?;
    }
    Ok(())
}

async fn schedule_list(ctx: &Ctx, out: &mut dyn Write) -> Result<(), CliError> {
    let list = ctx.store().await?.schedule_list().await?;
    let now = ts_now();
    let rows: Vec<Vec<String>> = list
        .iter()
        .map(|s| {
            let next = if !s.enabled {
                "-".to_owned()
            } else if s.kind != ScheduleKind::Cron {
                "T-16".to_owned()
            } else {
                match parse_cron(&s.cron) {
                    Ok(c) => ts_cell(next_after(&c, now)),
                    Err(_) => "bad cron".to_owned(),
                }
            };
            vec![
                s.id.to_string(),
                kind_str(s.kind).to_owned(),
                cell(&s.cron, 40),
                if s.enabled { "yes" } else { "no" }.to_owned(),
                ts_cell(s.last_run),
                next,
                s.run_id
                    .as_ref()
                    .map_or_else(|| "-".to_owned(), |r| cell(&r.0, 0)),
                cell(&s.prompt, 60),
            ]
        })
        .collect();
    table(
        out,
        &[
            "id",
            "kind",
            "cron",
            "on",
            "last_run",
            "next (UTC)",
            "parent",
            "prompt",
        ],
        &rows,
    )?;
    Ok(())
}

// ---------------------------------------------------------------- vault

/// `harness vault set`: the value comes from [`Ctx::read_secret`] (rpassword, no echo), is
/// held in `Zeroizing`, and is never printed, logged or put in an error.
fn vault_set(ctx: &Ctx, name: &str, personal: bool, out: &mut dyn Write) -> Result<(), CliError> {
    let kind = if personal {
        EntryKind::Personal
    } else {
        EntryKind::Secret
    };
    let mut vault = harness_core::vault::Vault::open(&ctx.env.vault_path, &ctx.env.vault_key)?;
    // Vault::set checks the name too; checking first avoids asking for a value it would refuse.
    let name_ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
    if !name_ok {
        return Err(CliError::Usage(format!(
            "vault name {:?} must be [A-Za-z0-9_.-]+",
            clean(name)
        )));
    }
    let value = (ctx.read_secret)(&format!("value for {}: ", clean(name)))?;
    if value.is_empty() {
        return Err(CliError::Usage("empty value; nothing stored".into()));
    }
    vault.set(name, &value, kind)?;
    drop(value);
    writeln!(
        out,
        "vault: {} set ({})",
        clean(name),
        if personal { "personal" } else { "secret" }
    )?;
    Ok(())
}

// ---------------------------------------------------------------- ledger

async fn print_ledger(ctx: &Ctx, run: &str, out: &mut dyn Write) -> Result<(), CliError> {
    if !is_run_id(run) {
        return Err(CliError::Usage(format!(
            "{:?} is not a run id (<yyyy-mm-ddThh-mm-ss>-<slug>)",
            clean(run)
        )));
    }
    ctx.env.install_vault()?;
    let store = ctx.store().await?;
    let id = RunId(run.to_owned());
    let Some(r) = store.run_get(&id).await? else {
        return Err(CliError::NotFound(format!("no run {run}")));
    };
    writeln!(
        out,
        "run {}  {}  started {}  ended {}  parent {}{}",
        r.id,
        status_str(r.status),
        ts_cell(Some(r.started)),
        ts_cell(r.ended),
        r.parent_run.as_ref().map_or("-", |p| p.0.as_str()),
        if r.strict { "  strict" } else { "" }
    )?;
    let events = store.ledger_events(&id).await?;
    let rows: Vec<Vec<String>> = events
        .iter()
        .map(|(lid, e)| {
            let mode = e
                .mode
                .and_then(|m| serde_json::to_value(m).ok())
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| "-".to_owned());
            vec![
                lid.0.to_string(),
                e.ts.format("%H:%M:%S%.3f").to_string(),
                cell(&e.server, 20),
                cell(&e.tool, 40),
                e.class.map_or_else(|| "-".to_owned(), |c| c.to_string()),
                mode,
                if e.ok { "ok" } else { "FAIL" }.to_owned(),
                e.duration_ms.to_string(),
                cell(&e.result_summary, 60),
                cell(e.why.as_deref().unwrap_or(""), 60),
            ]
        })
        .collect();
    table(
        out,
        &[
            "id", "time", "server", "tool", "class", "mode", "ok", "ms", "summary", "why",
        ],
        &rows,
    )?;
    writeln!(out, "{} events", events.len())?;
    Ok(())
}

// ---------------------------------------------------------------- tools

fn split_tool(tool: &str) -> Result<(&str, &str), CliError> {
    let (d, n) = tool
        .split_once('/')
        .ok_or_else(|| CliError::Usage(format!("{:?}: expected <domain>/<name>", clean(tool))))?;
    let n = n.strip_suffix(".js").unwrap_or(n);
    toolstore::check_tool_id(d, n)?;
    Ok((d, n))
}

/// Tool folders on disk (site folders, `_generic`, and domains under `_archive/`), or `[d]`.
fn domains(ts: &ToolStore, only: Option<&str>) -> Result<Vec<String>, CliError> {
    if let Some(d) = only {
        if !toolstore::valid_domain(d) {
            return Err(CliError::Usage(format!(
                "{:?} is not a tool folder",
                clean(d)
            )));
        }
        return Ok(vec![d.to_owned()]);
    }
    let mut out = Vec::new();
    for dir in [ts.root.clone(), ts.root.join(toolstore::ARCHIVE)] {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            if let Ok(n) = e.file_name().into_string() {
                if toolstore::valid_domain(&n) && !out.contains(&n) {
                    out.push(n);
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

fn tool_status(s: ToolStatus) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

async fn tools(ctx: &Ctx, cmd: ToolsCmd, out: &mut dyn Write) -> Result<(), CliError> {
    let ts = ToolStore::new(&ctx.env.tools_root);
    let store = ctx.store().await?;
    match cmd {
        ToolsCmd::List { domain } => {
            let mut rows = Vec::new();
            for d in domains(&ts, domain.as_deref())? {
                let health = store.tool_health(&d).await?;
                let mut names: Vec<String> = std::fs::read_dir(ts.root.join(&d))
                    .map(|rd| {
                        rd.flatten()
                            .filter_map(|e| e.file_name().into_string().ok())
                            .filter_map(|f| f.strip_suffix(".js").map(str::to_owned))
                            .filter(|n| toolstore::valid_name(n))
                            .collect()
                    })
                    .unwrap_or_default();
                for h in &health {
                    if !names.contains(&h.name) {
                        names.push(h.name.clone());
                    }
                }
                names.sort();
                for n in names {
                    let h = health.iter().find(|h| h.name == n);
                    let on_disk = ts
                        .try_tool_path(&d, &n)
                        .map(|p| p.is_file())
                        .unwrap_or(false);
                    rows.push(vec![
                        format!("{d}/{}", cell(&n, 0)),
                        h.map_or_else(|| "-".to_owned(), |h| h.version.to_string()),
                        h.map_or_else(|| "no row".to_owned(), |h| tool_status(h.status)),
                        if h.is_some_and(|h| h.pinned) {
                            "yes"
                        } else {
                            ""
                        }
                        .to_owned(),
                        if on_disk { "file" } else { "-" }.to_owned(),
                    ]);
                }
            }
            table(out, &["tool", "v", "status", "pinned", "on disk"], &rows)?;
        }
        ToolsCmd::Health { domain } => {
            let mut rows = Vec::new();
            for d in domains(&ts, domain.as_deref())? {
                for h in store.tool_health(&d).await? {
                    rows.push(vec![
                        format!("{}/{}", cell(&h.domain, 0), cell(&h.name, 0)),
                        tool_status(h.status),
                        h.version.to_string(),
                        h.runs.to_string(),
                        h.fails.to_string(),
                        ts_cell(h.last_ok),
                        ts_cell(h.last_fail),
                        if h.pinned { "yes" } else { "" }.to_owned(),
                        cell(h.why.as_deref().unwrap_or(""), 80),
                    ]);
                }
            }
            table(
                out,
                &[
                    "tool",
                    "status",
                    "v",
                    "runs",
                    "fails",
                    "last_ok",
                    "last_fail",
                    "pinned",
                    "why",
                ],
                &rows,
            )?;
        }
        ToolsCmd::Pin { tool } => {
            let (d, n) = split_tool(&tool)?;
            let h = ts.set_pinned(store.as_ref(), d, n, true).await?;
            writeln!(out, "{d}/{n}: pinned ({})", tool_status(h.status))?;
        }
        ToolsCmd::Unpin { tool } => {
            let (d, n) = split_tool(&tool)?;
            let h = ts.set_pinned(store.as_ref(), d, n, false).await?;
            writeln!(out, "{d}/{n}: unpinned ({})", tool_status(h.status))?;
        }
        ToolsCmd::Retire {
            tool,
            why,
            no_commit,
        } => retire(&ts, store.as_ref(), &tool, why, no_commit, out).await?,
    }
    Ok(())
}

/// T-7 retire by the user: status retired with its why, file moved to `_archive/`, and the
/// move committed with a task id (unless `--no-commit`). A pinned tool must be unpinned first.
async fn retire(
    ts: &ToolStore,
    store: &dyn MemoryStore,
    tool: &str,
    why: Option<String>,
    no_commit: bool,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let (d, n) = split_tool(tool)?;
    let path = ts.try_tool_path(d, n)?;
    let row = ts.health(store, d, n).await?;
    if row.as_ref().is_some_and(|h| h.pinned) {
        return Err(CliError::Usage(format!(
            "{d}/{n} is pinned; `harness tools unpin {d}/{n}` first"
        )));
    }
    if row.is_none() && !path.is_file() {
        return Err(CliError::NotFound(format!("no tool {d}/{n}")));
    }
    let mut h = row.unwrap_or(ToolHealth {
        domain: d.to_owned(),
        name: n.to_owned(),
        version: 1,
        status: ToolStatus::Retired,
        runs: 0,
        fails: 0,
        last_ok: None,
        last_fail: None,
        why: None,
        pinned: false,
    });
    h.status = ToolStatus::Retired;
    h.why = Some(match why {
        Some(w) => clean(&w),
        None => "retired by the user".to_owned(),
    });
    store.tool_health_set(h.clone()).await?;
    let moved = ts.archive(d, n, h.version)?;
    match &moved {
        Some(to) => writeln!(out, "{d}/{n}: retired, archived to {}", to.display())?,
        None => writeln!(out, "{d}/{n}: retired (no file to archive)")?,
    }
    if let (Some(to), false) = (moved, no_commit) {
        let msg = format!("tools: retire {d}/{n} by the user (T-7)");
        let paths: Vec<PathBuf> = vec![path, to];
        let hash = ts.git_commit(&paths, &msg)?;
        writeln!(out, "committed {hash}")?;
    }
    Ok(())
}

// ---------------------------------------------------------------- db

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

async fn db(ctx: &Ctx, cmd: DbCmd, out: &mut dyn Write) -> Result<(), CliError> {
    if let DbCmd::Migrate = cmd {
        // PgStore::connect runs every pending migration (D-114).
        let store = ctx.env.connect().await?;
        drop(store);
        writeln!(out, "migrations applied")?;
    }
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&ctx.env.database_url)
        .await
        .map_err(|e| {
            CliError::Db(format!(
                "connecting failed ({}); is Postgres up? run `docker compose up -d --wait postgres`",
                db_error_kind(&e)
            ))
        })?;
    let applied: Vec<(i64, bool)> = match sqlx::query_as::<_, (i64, bool)>(
        "select version, success from _sqlx_migrations order by version",
    )
    .fetch_all(&pool)
    .await
    {
        Ok(rows) => rows,
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("42P01") => Vec::new(),
        Err(e) => {
            return Err(CliError::Db(format!(
                "reading _sqlx_migrations: {}",
                db_error_kind(&e)
            )))
        }
    };
    pool.close().await;
    let mut rows = Vec::new();
    for m in MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
    {
        let state = match applied.iter().find(|(v, _)| *v == m.version) {
            Some((_, true)) => "applied",
            Some((_, false)) => "failed",
            None => "pending",
        };
        rows.push(vec![
            m.version.to_string(),
            m.description.to_string(),
            state.to_owned(),
        ]);
    }
    for (v, ok) in &applied {
        if !MIGRATOR.iter().any(|m| m.version == *v) {
            rows.push(vec![
                v.to_string(),
                "(not in this build)".to_owned(),
                if *ok { "applied" } else { "failed" }.to_owned(),
            ]);
        }
    }
    table(out, &["version", "migration", "state"], &rows)?;
    Ok(())
}

/// The error class and SQLSTATE, never the server's message text (it can quote a value).
fn db_error_kind(e: &sqlx::Error) -> String {
    match e {
        sqlx::Error::Database(d) => format!("SQLSTATE {}", d.code().unwrap_or_default()),
        sqlx::Error::Io(io) => format!("io: {}", io.kind()),
        sqlx::Error::PoolTimedOut => "timed out".to_owned(),
        other => clean(&other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_surface_parses() {
        let c =
            Cli::try_parse_from(["harness", "schedule", "add", "*/1 * * * *", "say hi"]).unwrap();
        assert!(matches!(
            c.cmd,
            Cmd::Schedule {
                cmd: ScheduleCmd::Add {
                    kind: KindArg::Cron,
                    ..
                }
            }
        ));
        let c =
            Cli::try_parse_from(["harness", "ask", "2026-10-23T09-05-07-x", "3", "yes"]).unwrap();
        assert!(matches!(
            c.cmd,
            Cmd::Ask {
                n: 3,
                answer: YesNo::Yes,
                ..
            }
        ));
        // The vault value is never an argument.
        assert!(Cli::try_parse_from(["harness", "vault", "set", "pw", "hunter2"]).is_err());
        assert!(Cli::try_parse_from(["harness", "vault", "set", "pw", "--personal"]).is_ok());
        for args in [
            vec!["harness", "run", "say hi"],
            vec!["harness", "schedule", "list"],
            vec!["harness", "schedule", "rm", "4"],
            vec!["harness", "gate", "set", "*", "Send", "ask", "--strict"],
            vec!["harness", "tools", "list"],
            vec!["harness", "tools", "health", "luma.com"],
            vec!["harness", "tools", "pin", "luma.com/register"],
            vec!["harness", "tools", "unpin", "luma.com/register"],
            vec![
                "harness",
                "tools",
                "retire",
                "luma.com/register",
                "--no-commit",
            ],
            vec!["harness", "ledger", "2026-10-23T09-05-07-x"],
            vec!["harness", "db", "migrate"],
            vec!["harness", "db", "status"],
        ] {
            Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
    }

    #[test]
    fn cells_strip_control_characters() {
        assert_eq!(cell("a\x1b[31mb\r\nc", 0), "a [31mb  c");
        assert_eq!(cell("abcdefghij", 6), "abc...");
    }
}
