//! `echo`: the fake tool server of the step-1 acceptance (docs/01 "Acceptance for this doc";
//! H-6, H-2, H-3; D-122). A test tool, so an example and never an installed bin.
//!
//! A stdio MCP server on `harness_core::mcp::serve_io`, exactly as Claude Code would launch a
//! tool server, exposing one tool through the H-6 wrapper:
//!
//! * `echo.send(to, body, amount?)`: declared class Send, recipients = `[to]`, so a known
//!   contact is allowed and a new one is asked (H-1.1). With `amount` present the classifier
//!   raises the class to Pay (the "card field in the form" case of H-1), which asks and skips
//!   on timeout. `body` may hold `@vault(name)` literals; they are resolved only inside the
//!   sink (H-2). The sink appends one JSON line per send to the `--sink` file with the
//!   recipient, the resolved body's length and FNV-1a 64 hash, and whether any literal was
//!   left unresolved. Never the resolved text itself. The model gets one short line.
//!
//! The wiring is the production one apart from the vault: a `PgStore` ledger, the real age
//! `Vault` (installed as the process-wide scrubber), and a `FileAsker` on the run. The vault
//! is always a test vault: echo refuses to start without both `--vault-key` and
//! `SB_VAULT_PATH`, and never falls back to the OS keychain or the default vault path, so the
//! sink's length and hash line is only ever computed over a test value.
//!
//! Environment, as `core_pg::bootstrap` reads it:
//! * `DATABASE_URL` (default `core_pg::DEFAULT_DATABASE_URL`);
//! * `SB_VAULT_PATH` (required): the vault file (a test-owned temp vault);
//! * `SB_RUNS_ROOT` (default `runs`) and `SB_RUN_ID` (required: the run folder and its
//!   `runs` row must exist, as `harness run` leaves them).
//!
//! Arguments:
//! * `--vault-key <file>` (required): an age identity file. Never the OS keychain. The path
//!   only; the key itself never comes from the command line or the environment (D-1);
//! * `--sink <file>`: where the sink appends its lines (required);
//! * `--ask-timeout-ms <n>`: shortens every ask card's 60 s timer (test seam), so a test
//!   sees the H-1.3 timeout behaviour without waiting a minute. The card's `on_timeout` is
//!   unchanged; the wrapper's cap still applies.
//!
//! No model is called (rule 1). Nothing but JSON-RPC goes to stdout.

use async_trait::async_trait;
use clap::Parser;
use harness_core::class::Class;
use harness_core::gate::{AskCard, AskOutcome, Asker};
use harness_core::memory::MemoryStore;
use harness_core::run::{RunFolder, RunId};
use harness_core::vault::{KeySource, Vault};
use harness_core::{
    serve_io, Classified, FileAsker, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput,
    ToolRegistry, ToolSpec,
};
use serde_json::{json, Value};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// `ledger.events.server` of this server's tool events.
const SERVER: &str = "echo";
const TOOL: &str = "echo.send";

#[derive(Debug, Parser)]
#[command(
    name = "echo",
    about = "step-1 acceptance fake tool server (docs/01, D-122)"
)]
struct Args {
    /// Age identity file of the test vault (required; never the OS keychain).
    #[arg(long)]
    vault_key: PathBuf,
    /// File the sink appends one JSON line per send to.
    #[arg(long)]
    sink: PathBuf,
    /// Cap on every ask card's timer, in milliseconds (test seam).
    #[arg(long)]
    ask_timeout_ms: Option<u64>,
}

/// FNV-1a 64: lets a test check what the sink received without the sink writing it.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::Args(format!("`{key}` must be a string")))
}

/// `amount` as text: a string or a number; anything else is a bad argument.
fn amount_arg(args: &Value) -> Result<Option<String>, ToolError> {
    match args.get("amount") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        Some(_) => Err(ToolError::Args(
            "`amount` must be a string or number".into(),
        )),
    }
}

struct Echo {
    sink: PathBuf,
}

#[async_trait]
impl ToolHandler for Echo {
    async fn classify(&self, _ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let to = str_arg(args, "to")?.trim().to_owned();
        let body = str_arg(args, "body")?;
        if to.is_empty() {
            return Err(ToolError::Args("`to` is empty".into()));
        }
        let amount = amount_arg(args)?;
        let (class, what) = match &amount {
            Some(a) => (Some(Class::Pay), format!("pay {a} to {to}")),
            None => (None, format!("send a message to {to}")),
        };
        // The wrapper scrubs every field; `body` keeps its `@vault(name)` literals.
        let mut values = vec![("to".to_owned(), to.clone()), ("body".into(), body.into())];
        if let Some(a) = amount {
            values.push(("amount".into(), a));
        }
        Ok(Classified {
            class,
            domain: None,
            recipients: vec![to],
            hints: Vec::new(),
            what: Some(what),
            values,
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let to = str_arg(&args, "to")?.trim().to_owned();
        let body = str_arg(&args, "body")?;
        let amount = amount_arg(&args)?;
        // The sink: the resolved body exists only inside this closure (H-2).
        let (line, chars) = ctx.resolve_into(body, |r| {
            let v = r.expose();
            let chars = v.chars().count();
            let line = json!({
                "to": harness_core::scrub(&to),
                "class": ctx.class,
                "chars": chars,
                "bytes": v.len(),
                "fnv1a64": format!("{:016x}", fnv1a64(v.as_bytes())),
                "unresolved": v.contains("@vault("),
            });
            (line, chars)
        })?;
        let path = self.sink.clone();
        let mut bytes = serde_json::to_vec(&line).map_err(|e| ToolError::Failed(e.to_string()))?;
        bytes.push(b'\n');
        tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(&bytes))
        })
        .await
        .map_err(|_| ToolError::Failed("sink task failed".into()))?
        .map_err(|e| ToolError::Failed(format!("sink: {e}")))?;

        let text = match amount {
            Some(a) => format!("paid {a} to {to}"),
            None => format!("sent to {to} ({chars} chars)"),
        };
        Ok(ToolOutput {
            text,
            summary: None,
            consequences: json!({ "sent_to": to }),
            files: Vec::new(),
            stopped: None,
        })
    }
}

/// A `FileAsker` whose cards carry a shorter timer (`--ask-timeout-ms`).
struct ShortTimer {
    inner: FileAsker,
    cap: Duration,
}

#[async_trait]
impl Asker for ShortTimer {
    async fn ask(&self, card: &AskCard) -> AskOutcome {
        let mut card = card.clone();
        card.timeout = card.timeout.min(self.cap);
        self.inner.ask(&card).await
    }
}

fn env(k: &str) -> Option<String> {
    std::env::var(k)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

async fn run() -> anyhow::Result<()> {
    let args = Args::parse();

    // The vault first, so every later message is scrubbed (H-2). Only a test vault: an
    // explicit file and an explicit identity file, never the default path or the keychain.
    let path = env("SB_VAULT_PATH").map(PathBuf::from).ok_or_else(|| {
        anyhow::anyhow!("SB_VAULT_PATH is required (a test vault; no default path)")
    })?;
    let key = KeySource::IdentityFile(args.vault_key.clone());
    let vault = Vault::open(&path, &key)?;
    vault.install_global();
    let vault = Arc::new(vault);

    let url = env("DATABASE_URL").unwrap_or_else(|| core_pg::DEFAULT_DATABASE_URL.to_owned());
    let store: Arc<dyn MemoryStore> = Arc::new(core_pg::PgStore::connect(&url).await?);

    let runs_root = env("SB_RUNS_ROOT").map_or_else(|| PathBuf::from("runs"), PathBuf::from);
    let run = RunId(env("SB_RUN_ID").ok_or_else(|| anyhow::anyhow!("SB_RUN_ID is required"))?);
    RunFolder::open(&runs_root, &run)?;
    if store.run_get(&run).await?.is_none() {
        anyhow::bail!("SB_RUN_ID {run}: no runs row; the launching session creates it");
    }

    let file_asker = FileAsker::new(&runs_root, run.clone(), vault.clone());
    let asker: Arc<dyn Asker> = match args.ask_timeout_ms {
        Some(ms) => Arc::new(ShortTimer {
            inner: file_asker,
            cap: Duration::from_millis(ms),
        }),
        None => Arc::new(file_asker),
    };

    let spec = ToolSpec {
        name: TOOL.into(),
        description: "Send `body` to `to` (fake tool for the step-1 acceptance). \
                      `amount` makes it a payment."
            .into(),
        class: Class::Send,
        args: json!({
            "type": "object",
            "properties": {
                "to": {"type": "string"},
                "body": {"type": "string"},
                "amount": {"type": ["string", "number"]},
            },
            "required": ["to", "body"],
        }),
        handler: Arc::new(Echo { sink: args.sink }),
    };
    let harness = Harness {
        store,
        vault,
        asker,
        runs_root,
        run: Some(run),
    };
    serve_io(
        SERVER,
        ToolRegistry::new(vec![spec]),
        harness,
        tokio::io::BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await?;
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("echo: {}", harness_core::scrub(&format!("{e:#}")));
        std::process::exit(1);
    }
}
