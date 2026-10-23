//! srv-files: Phase 1 files tool server: fs, shell, docs, csv.clean, diff, history.search (docs/04 F-1..F-6).
//! SKELETON owned by its area. Every tool registers through `harness_core::mcp::serve` (I-2) and nothing else.

use harness_core::ToolSpec;

/// F-1 fs: read write glob move mkdir; delete outside runs/ is Delete.
pub mod fs {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-14")
    }
}

/// F-2 shell(cmd, cwd, timeout): class from the command.
pub mod shell {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-14")
    }
}

/// F-3 pdf/xlsx/docx/pptx/md wrappers over the existing skill scripts.
pub mod docs {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-14")
    }
}

/// F-4 csv.clean(path, rules).
pub mod csv {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-14")
    }
}

/// F-5 diff(a, b, mode).
pub mod diff {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-14")
    }
}

/// F-6 history.search(q, since) over a copied Chrome History.
pub mod history {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-14")
    }
}

/// Every tool this server exposes.
pub fn tools() -> Vec<ToolSpec> {
    let mut all = Vec::new();
    all.extend(fs::tools());
    all.extend(shell::tools());
    all.extend(docs::tools());
    all.extend(csv::tools());
    all.extend(diff::tools());
    all.extend(history::tools());
    all
}
