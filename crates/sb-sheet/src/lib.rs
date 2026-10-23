//! sb-sheet: pure (no I/O). Turns an index (I-5) and text blocks into what the model reads:
//! skeleton (B-6.1), find (B-6.2), sheet (B-6.3), read (B-6.4 / W-5). One code path for the
//! browser's live index and srv-web's fetched index.
//!
//! CONTRACT: `index` (I-5). DRAFT signatures (I-6), owned by area sheet: `text`, and the
//! four functions below with their option and output types.

pub mod index;
pub mod text;

pub use index::{BadElemRef, ElemRef, FieldValue, Flags, FrameId, IndexRow, Rect, State};
pub use text::{BlockKind, Span, TextBlock};

/// Page facts the index does not carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageMeta {
    pub url: String,
    pub title: String,
    /// The T-3 line, e.g. `tools for this site: luma.register(name,email)`; built by toolstore.
    pub site_tools: Option<String>,
    /// Open JS dialog text, if any.
    pub dialog: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkeletonOpts {
    pub meta: PageMeta,
}

/// B-6.1: ~80-150 tokens.
pub fn skeleton(index: &[IndexRow], text_blocks: &[TextBlock], opts: &SkeletonOpts) -> String {
    let _ = (index, text_blocks, opts);
    todo!("T-10: B-6.1 skeleton")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindOpts {
    /// Comma-separated parts; `name:role` forces a role (B-6.2).
    pub query: String,
    /// Hits per part (default 3).
    pub k: usize,
}

/// Why a row is in the find output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HitReason {
    /// Top-k for query part `part`.
    Match {
        part: usize,
    },
    /// Always appended (B-6.2).
    Challenge,
    Overlay,
    WinningSubmit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FindHit {
    pub elem: ElemRef,
    pub score: f32,
    /// Best score < 3: the line is marked `?`.
    pub weak: bool,
    pub reason: HitReason,
}

/// B-6.2: BM25 (k1 1.2, b 0.75) over name/nearby/heading + role prior + state + group bonus.
/// Output in document order. This is the only scorer, live and fetched (D-51): the browser
/// ships `__sb.index()` rows to Rust instead of scoring in the page. The `?` mark compares
/// the lexical part (3*name + 1.5*nearby + 0.5*heading) with 3, not the total (D-54).
/// Acceptance 6 runs with `k = 1`: the four top hits plus the always-appended rows (D-54).
pub fn find(index: &[IndexRow], text_blocks: &[TextBlock], opts: &FindOpts) -> Vec<FindHit> {
    let _ = (index, text_blocks, opts);
    todo!("T-10: B-6.2 find")
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SheetOpts {
    /// Landmark, form id (`f2`) or heading text; `None` = whole page.
    pub region: Option<String>,
}

/// B-6.3: every element in the region, numbered, document order, no cap.
pub fn sheet(index: &[IndexRow], text_blocks: &[TextBlock], opts: &SheetOpts) -> String {
    let _ = (index, text_blocks, opts);
    todo!("T-10: B-6.3 sheet")
}

/// Opaque continuation token for chunked reads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cursor(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOpts {
    /// Landmark or `## Heading` to jump to.
    pub region: Option<String>,
    pub cursor: Option<Cursor>,
    /// Cap for the whole read.
    pub max_tokens: Option<usize>,
    /// Per chunk; default 4 000 (W-5).
    pub chunk_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineEntry {
    pub heading: String,
    pub words: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOutput {
    /// Light markdown with `[text](#id)` links (W-5).
    pub markdown: String,
    /// 1-based.
    pub chunk: usize,
    pub chunks: usize,
    /// The `remaining:` outline.
    pub remaining: Vec<OutlineEntry>,
    pub cursor: Option<Cursor>,
}

/// B-6.4 / W-5 renderer. Never emits a password value or CHALLENGE-frame content.
pub fn read(index: &[IndexRow], text_blocks: &[TextBlock], opts: &ReadOpts) -> ReadOutput {
    let _ = (index, text_blocks, opts);
    todo!("T-10: W-5 read renderer")
}
