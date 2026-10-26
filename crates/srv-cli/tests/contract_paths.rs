//! pr-ready section 4: every interface path srv-cli consumes (I-1, I-2, I-4, I-13 in
//! agent-memory/interfaces.yaml) and every public entry point main.rs and the tests use
//! must exist at exactly that path; a wrong path fails compilation here.

#![allow(unused_imports)]

use harness_core::class::Class;
use harness_core::mcp::{
    dispatch, serve, serve_io, Classified, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput,
    ToolRegistry, ToolSpec,
};
use harness_core::memory::MemoryStore;
use harness_core::sched::notify_changed;
use harness_core::vault::{scrub, vault_literal, vault_refs, Vault};

use srv_cli::{cli, cloudflare, dns, flags, gh, text, tools, tools_with, Config};

#[test]
fn server_exposes_its_tools_with_read_floors() {
    let all = tools_with(Config::default());
    let names: Vec<&str> = all.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["gh", "dns", "cloudflare", "flags", "cli"]);
    // Declared class is the floor; writes are raised per call by classify (D-540, D-541).
    assert!(all.iter().all(|t| t.class == Class::Read));
    assert!(all.iter().all(|t| t.args["type"] == "object"));
    // Each module's own list, and the env-configured list main.rs serves.
    assert_eq!(gh::tools().len(), 1);
    assert_eq!(dns::tools().len(), 1);
    assert_eq!(cloudflare::tools().len(), 1);
    assert_eq!(flags::tools().len(), 1);
    assert_eq!(cli::tools().len(), 1);
    assert_eq!(tools().len(), 5);
}

#[test]
fn config_defaults_are_applied() {
    let c = Config::default();
    assert_eq!(c.gh, vec!["gh".to_string()]);
    assert_eq!(c.cloudflare_api, srv_cli::CLOUDFLARE_API);
    assert_eq!(c.cloudflare_auth, "Bearer @vault(cf_token)");
    assert_eq!(c.cloudflare_domain, "cloudflare.com");
    assert!(c.flags_file.ends_with("config/flags.toml"));
    assert!(c.cli_allow_file.ends_with("config/cli-allow.toml"));
    assert_eq!(c.proc_timeout.as_secs(), 120);
    assert_eq!(c.http_timeout.as_secs(), 30);
    assert_eq!(c.dns_timeout.as_secs(), 10);
    assert_eq!(text::MAX_TEXT_CHARS, 12_000);
}
