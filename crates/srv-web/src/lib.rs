//! srv-web: Phase 1 web tool server: search, fetch, api, feed, read renderer (docs/04 W-1..W-7).
//! SKELETON owned by its area. Every tool registers through `harness_core::mcp::serve` (I-2) and nothing else.

use harness_core::ToolSpec;

/// W-1 search(q, k): SearXNG backend (D-31).
pub mod search {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-12")
    }
}

/// W-2 fetch(url, mode): HTML -> IndexRow via scraper, read/sheet/raw.
pub mod fetch {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-12")
    }
}

/// W-3 api(url, method, body, headers): @vault headers resolved at the sink.
pub mod api {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-12")
    }
}

/// W-4 feed(url): RSS, Atom, iCal, JSON Feed.
pub mod feed {
    use harness_core::ToolSpec;

    pub fn tools() -> Vec<ToolSpec> {
        todo!("T-12")
    }
}

/// Every tool this server exposes.
pub fn tools() -> Vec<ToolSpec> {
    let mut all = Vec::new();
    all.extend(search::tools());
    all.extend(fetch::tools());
    all.extend(api::tools());
    all.extend(feed::tools());
    all
}
