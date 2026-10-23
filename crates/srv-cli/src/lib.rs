//! srv-cli: Phase 1 CLI tool server: gh, dns, flags, allowlisted cli (docs/04 C-1..C-4).
//! SKELETON owned by its area. Every tool registers through `harness_core::mcp::serve` (I-2) and nothing else.

use harness_core::ToolSpec;

/// C-1 gh(args): read by default; writes are Account.
pub mod gh {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-15")
    }
}

/// C-2 dns(name, type).
pub mod dns {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-15")
    }
}

/// C-3 flags(provider, env).
pub mod flags {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-15")
    }
}

/// C-4 cli(cmd) behind config/cli-allow.toml.
pub mod cli {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-15")
    }
}

/// Every tool this server exposes.
pub fn tools() -> Vec<ToolSpec> {
    let mut all = Vec::new();
    all.extend(gh::tools());
    all.extend(dns::tools());
    all.extend(flags::tools());
    all.extend(cli::tools());
    all
}
