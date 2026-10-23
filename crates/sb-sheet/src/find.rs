//! B-6.2 find: BM25 + synonyms + role prior + state + group bonus (D-51, D-54, D-202).
//! Owned by coder A (T-10).

use crate::{FindHit, FindOpts, IndexRow, TextBlock};

pub(crate) fn find(index: &[IndexRow], text_blocks: &[TextBlock], opts: &FindOpts) -> Vec<FindHit> {
    let _ = (index, text_blocks, opts);
    todo!("T-10: B-6.2 find")
}

/// Renders find hits as sheet lines, weak hits prefixed with `?` (D-203).
pub fn find_lines(index: &[IndexRow], hits: &[FindHit]) -> String {
    let _ = (index, hits);
    todo!("T-10: find lines")
}
