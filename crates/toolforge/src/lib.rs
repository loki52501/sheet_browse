//! toolforge (docs/06). SKELETON, Phase 2 (T-28). Learn as data, never as permission (G-4).

use toolstore::Tool;

/// G-1 rungs, cheapest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rung {
    /// 50 ms.
    Registry,
    /// 200 ms.
    Relocate,
    /// 300 ms.
    Compose,
    /// ≤ 10 s, max 2 per task.
    Forge,
    Fallback,
}

/// G-2 step 3: header, denylist, planner `?s`, `@gate` ≥ classified.
pub fn static_check(tool: &Tool) -> Result<(), String> {
    let _ = tool;
    todo!("T-28: G-2 static check")
}
