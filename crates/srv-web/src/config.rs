//! Server configuration: where the tool store lives, where SearXNG listens (D-31), the HTTP
//! limits of W-2, and a DNS override seam for tests.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Env var for the `tools/` root (default `tools`, relative to the server's working dir).
pub const TOOLS_ROOT_VAR: &str = "SB_TOOLS_ROOT";
/// Env var for the SearXNG base URL (default [`DEFAULT_SEARXNG_URL`]).
pub const SEARXNG_URL_VAR: &str = "SB_SEARXNG_URL";
/// D-31: the compose service `searxng` publishes here.
pub const DEFAULT_SEARXNG_URL: &str = "http://127.0.0.1:8888";
/// W-2: 15 s per request, body included.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
/// W-2: 5 MB body cap; the rest of the body is not read.
pub const DEFAULT_MAX_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Config {
    /// `tools/` root for site tools (W-7, docs/03 T-1).
    pub tools_root: PathBuf,
    /// SearXNG base URL, no trailing slash needed.
    pub searxng_url: String,
    /// Per-request timeout (connect to end of body).
    pub timeout: Duration,
    /// Body cap in bytes.
    pub max_bytes: usize,
    /// DNS overrides: `host` resolves to `addr`. A URL without an explicit port connects to
    /// the override's port. Tests use it to serve `http://docs.rs/...` from a local listener
    /// while classification still sees the real domain. Empty in production.
    pub resolve: Vec<(String, SocketAddr)>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            tools_root: PathBuf::from("tools"),
            searxng_url: DEFAULT_SEARXNG_URL.to_string(),
            timeout: DEFAULT_TIMEOUT,
            max_bytes: DEFAULT_MAX_BYTES,
            resolve: Vec::new(),
        }
    }
}

impl Config {
    /// Defaults overridden by `SB_TOOLS_ROOT` and `SB_SEARXNG_URL` when set and non-empty.
    pub fn from_env() -> Config {
        let mut c = Config::default();
        if let Some(root) = std::env::var_os(TOOLS_ROOT_VAR).filter(|v| !v.is_empty()) {
            c.tools_root = PathBuf::from(root);
        }
        if let Ok(url) = std::env::var(SEARXNG_URL_VAR) {
            if !url.trim().is_empty() {
                c.searxng_url = url.trim().to_string();
            }
        }
        c
    }

    /// Adds a DNS override (`host` -> `addr`).
    pub fn with_resolve(mut self, host: &str, addr: SocketAddr) -> Config {
        self.resolve.push((host.to_ascii_lowercase(), addr));
        self
    }
}
