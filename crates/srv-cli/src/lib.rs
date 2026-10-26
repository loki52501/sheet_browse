//! srv-cli: Phase 1 CLI tool server (docs/04 C-1..C-4): `gh`, `dns`, `cloudflare`, `flags`,
//! and the allowlisted `cli`. Every tool registers through `harness_core::mcp::serve` (I-2)
//! and nothing else: the wrapper classifies, gates, scrubs and writes the ledger.
//!
//! Rules every tool here keeps:
//! * programs run as argument vectors, never through a shell, with stdin closed, a time
//!   cap, capped output and kill on drop ([`proc`]);
//! * text from CI, APIs and programs is untrusted: it passes through [`text::neutral`]
//!   before the model reads it, so it cannot forge harness syntax;
//! * `@vault(name)` is resolved only inside the HTTP header write ([`http`]); a file this
//!   server writes is scrubbed with `harness_core::vault::scrub` first (the wrapper scrubs
//!   returned text, not files);
//! * classification fails closed: an unknown command or method is Account, never Read.
//!
//! No tool here adds or removes schedules. One that does must call
//! `harness_core::sched::notify_changed` (I-13).

pub mod argv;
pub mod cli;
pub mod cloudflare;
mod config;
pub mod dns;
pub mod flags;
pub mod gh;
mod http;
mod proc;
pub mod text;

pub use config::{Config, CLOUDFLARE_API, CLOUDFLARE_AUTH};

use harness_core::ToolSpec;
use std::sync::Arc;

/// Every tool this server exposes, configured from the environment ([`Config::from_env`]).
pub fn tools() -> Vec<ToolSpec> {
    tools_with(Config::from_env())
}

/// Every tool this server exposes, with an explicit configuration (tests).
pub fn tools_with(cfg: Config) -> Vec<ToolSpec> {
    let cfg = Arc::new(cfg);
    let mut all = Vec::new();
    all.extend(gh::tools_with(&cfg));
    all.extend(dns::tools_with(&cfg));
    all.extend(cloudflare::tools_with(&cfg));
    all.extend(flags::tools_with(&cfg));
    all.extend(cli::tools_with(&cfg));
    all
}
