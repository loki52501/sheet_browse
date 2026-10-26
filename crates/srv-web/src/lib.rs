//! srv-web: Phase 1 web tool server (docs/04 W-1..W-7). No browser anywhere.
//!
//! Tools: `search` (W-1, SearXNG per D-31), `fetch` (W-2, with the W-5 reader from
//! sb-sheet), `find` (W-6, sb-sheet's one scorer over the fetched index), `api` (W-3, vault
//! headers resolved at the sink), `feed` (W-4), and site tools (W-7: `@phase 1` readers
//! from `tools/`, registered as `<site>.<name>` when a page on their site is fetched).
//! Every tool registers through `harness_core::mcp::serve` (I-2) and nothing else: the
//! wrapper classifies, gates, scrubs and writes the ledger.

pub mod api;
mod cache;
pub mod config;
pub mod escape;
pub mod feed;
pub mod fetch;
pub mod html;
mod http;
mod pin;
pub mod search;
mod site;

pub use config::Config;

use harness_core::{ToolError, ToolSpec};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// State shared by every tool of one server: config, HTTP clients, the page cache.
pub(crate) struct Web {
    pub cfg: Config,
    clients: Result<http::Clients, String>,
    pub cache: cache::Cache,
    /// Site tools (`<site>.<name>`) disabled by a fingerprint miss (T-4, D-506): registered
    /// again only on a fetched page their fingerprint matches or relocates on, and enabled
    /// again by a clean repair run there (D-561).
    pub disabled: Mutex<HashSet<String>>,
}

impl Web {
    fn new(cfg: Config) -> Web {
        let clients = http::build(&cfg);
        Web {
            cfg,
            clients,
            cache: cache::Cache::default(),
            disabled: Mutex::new(HashSet::new()),
        }
    }

    pub fn is_disabled(&self, name: &str) -> bool {
        self.disabled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(name)
    }

    pub fn disable(&self, name: &str) {
        self.disabled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string());
    }

    /// Lifts a [`Web::disable`] after a clean repair run (D-561).
    pub fn enable(&self, name: &str) {
        self.disabled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name);
    }

    pub fn clients(&self) -> Result<&http::Clients, ToolError> {
        self.clients
            .as_ref()
            .map_err(|e| ToolError::Failed(e.clone()))
    }
}

/// Every tool this server exposes, configured from the environment
/// ([`Config::from_env`]: `SB_TOOLS_ROOT`, `SB_SEARXNG_URL`).
pub fn tools() -> Vec<ToolSpec> {
    tools_with(Config::from_env())
}

/// Every tool with an explicit config (tests: DNS overrides, a temp tools root, a fake
/// SearXNG). Site tools are not listed here; `fetch` adds them to the registry (W-7).
pub fn tools_with(cfg: Config) -> Vec<ToolSpec> {
    let web = Arc::new(Web::new(cfg));
    vec![
        search::spec(web.clone()),
        fetch::spec(web.clone()),
        fetch::find_spec(web.clone()),
        api::spec(web.clone()),
        feed::spec(web),
    ]
}

/// A string argument, `None` when absent or null; an error when present with another type.
pub(crate) fn opt_str<'a>(
    args: &'a serde_json::Value,
    key: &str,
) -> Result<Option<&'a str>, ToolError> {
    match args.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(ToolError::Args(format!("`{key}` must be a string"))),
    }
}

/// A required, non-empty string argument.
pub(crate) fn req_str<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str, ToolError> {
    match opt_str(args, key)? {
        Some(s) if !s.trim().is_empty() => Ok(s),
        _ => Err(ToolError::Args(format!("`{key}` is required"))),
    }
}

/// An optional non-negative integer argument (a JSON number or a numeric string).
pub(crate) fn opt_usize(args: &serde_json::Value, key: &str) -> Result<Option<usize>, ToolError> {
    match args.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(n)) => n
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .map(Some)
            .ok_or_else(|| ToolError::Args(format!("`{key}` must be a non-negative integer"))),
        Some(serde_json::Value::String(s)) => s
            .trim()
            .parse::<usize>()
            .map(Some)
            .map_err(|_| ToolError::Args(format!("`{key}` must be a non-negative integer"))),
        Some(_) => Err(ToolError::Args(format!(
            "`{key}` must be a non-negative integer"
        ))),
    }
}
