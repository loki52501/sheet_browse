//! Ordered text blocks, the second input of I-6 (`injected.js` `__sb.text(region)`, srv-web's
//! HTML walk). DRAFT owned by area sheet: propose changes through `interface_changes`, since
//! page (JS) and servers (srv-web) produce this shape.

use crate::index::{ElemRef, FrameId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Heading,
    Paragraph,
    ListItem,
    TableRow,
    Code,
    Quote,
    Other,
}

/// A run of text; `elem` set when the run is an actionable thing the reader links as `[text](#id)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elem: Option<ElemRef>,
}

/// One block of visible text in document order (W-5 input).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextBlock {
    /// Document order across the whole tab (frames interleaved where they sit).
    pub order: u32,
    pub frame: FrameId,
    pub kind: BlockKind,
    /// Heading level 1..=6, or list nesting depth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    /// Enclosing landmark: `main nav header footer aside form region dialog`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landmark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    /// For `TableRow`: which table (document-order id within the frame) the row belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<u32>,
    /// For `TableRow`: a header row (`th` cells), rendered before the `|---|` separator.
    #[serde(default)]
    pub header: bool,
    /// Text runs. For `TableRow` each span is one cell (a cell holding a link is one linked span).
    pub spans: Vec<Span>,
}
