//! sb-mcp: Browser MCP server: open find sheet read act tabs screenshot video.* eval cdp network (docs/05 B-1, B-10).
//! SKELETON owned by its area. Every tool registers through `harness_core::mcp::serve` (I-2) and nothing else.

use harness_core::ToolSpec;

/// B-1 browser tools registered through harness_core::mcp::serve.
pub mod tools {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-23")
    }
}

/// B-10 eval, cdp, network: classified, through the gate.
pub mod hatches {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-25")
    }
}

/// Every tool this server exposes.
pub fn tools() -> Vec<ToolSpec> {
    let mut all = Vec::new();
    all.extend(tools::tools());
    all.extend(hatches::tools());
    all
}
