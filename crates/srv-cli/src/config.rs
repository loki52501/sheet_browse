//! Server configuration. [`Config::from_env`] is what the binary uses; tests build a
//! [`Config`] directly and pass it to [`crate::tools_with`], so no test sets environment
//! variables (no races between tests).

use std::path::PathBuf;
use std::time::Duration;

/// Cloudflare API v4 base.
pub const CLOUDFLARE_API: &str = "https://api.cloudflare.com/client/v4";
/// The Authorization header template for Cloudflare; `@vault(cf_token)` is resolved only
/// inside the header write (H-2).
pub const CLOUDFLARE_AUTH: &str = "Bearer @vault(cf_token)";

#[derive(Debug, Clone)]
pub struct Config {
    /// The program (and any leading arguments) that runs `gh`. `SB_GH_BIN`, default `gh`.
    pub gh: Vec<String>,
    /// GitHub Enterprise hosts `gh api` may reach besides github.com (lowercase).
    /// `SB_GH_HOSTS`, comma-separated, default none.
    pub gh_hosts: Vec<String>,
    /// Cloudflare API base URL. `SB_CLOUDFLARE_API`, default [`CLOUDFLARE_API`]. Must be
    /// https (plain http only on loopback): the vault-resolved token goes in its header.
    pub cloudflare_api: String,
    /// Header value template for Cloudflare; must reference a vault entry.
    pub cloudflare_auth: String,
    /// Gate domain for Cloudflare calls (`gate.rules` key).
    pub cloudflare_domain: String,
    /// `config/flags.toml` (C-3). `SB_FLAGS_CONFIG`, else `<SB_CONFIG_DIR or config>/flags.toml`.
    pub flags_file: PathBuf,
    /// `config/cli-allow.toml` (C-4). `SB_CLI_ALLOW`, else `<SB_CONFIG_DIR or config>/cli-allow.toml`.
    pub cli_allow_file: PathBuf,
    /// Cap on one `gh` or `cli` program run.
    pub proc_timeout: Duration,
    /// Cap on one HTTP request (cloudflare, flags).
    pub http_timeout: Duration,
    /// Cap on one DNS lookup.
    pub dns_timeout: Duration,
}

impl Default for Config {
    /// The defaults with no environment applied.
    fn default() -> Config {
        let dir = PathBuf::from("config");
        Config {
            gh: vec!["gh".into()],
            gh_hosts: Vec::new(),
            cloudflare_api: CLOUDFLARE_API.into(),
            cloudflare_auth: CLOUDFLARE_AUTH.into(),
            cloudflare_domain: "cloudflare.com".into(),
            flags_file: dir.join("flags.toml"),
            cli_allow_file: dir.join("cli-allow.toml"),
            proc_timeout: Duration::from_secs(120),
            http_timeout: Duration::from_secs(30),
            dns_timeout: Duration::from_secs(10),
        }
    }
}

impl Config {
    /// [`Config::default`] with `SB_GH_BIN`, `SB_GH_HOSTS`, `SB_CLOUDFLARE_API`, `SB_CONFIG_DIR`,
    /// `SB_FLAGS_CONFIG` and `SB_CLI_ALLOW` applied (blank values are ignored).
    pub fn from_env() -> Config {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let mut c = Config::default();
        if let Some(gh) = env("SB_GH_BIN") {
            c.gh = vec![gh];
        }
        if let Some(hosts) = env("SB_GH_HOSTS") {
            c.gh_hosts = hosts
                .split(',')
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect();
        }
        if let Some(api) = env("SB_CLOUDFLARE_API") {
            c.cloudflare_api = api;
        }
        if let Some(dir) = env("SB_CONFIG_DIR") {
            let dir = PathBuf::from(dir);
            c.flags_file = dir.join("flags.toml");
            c.cli_allow_file = dir.join("cli-allow.toml");
        }
        if let Some(f) = env("SB_FLAGS_CONFIG") {
            c.flags_file = PathBuf::from(f);
        }
        if let Some(f) = env("SB_CLI_ALLOW") {
            c.cli_allow_file = PathBuf::from(f);
        }
        c
    }
}
