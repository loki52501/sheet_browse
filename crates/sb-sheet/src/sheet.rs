//! B-6.3 sheet and the sheet line format (D-203). Owned by coder A (T-10).

use crate::{IndexRow, SheetOpts, TextBlock};

pub(crate) fn sheet(index: &[IndexRow], text_blocks: &[TextBlock], opts: &SheetOpts) -> String {
    let _ = (index, text_blocks, opts);
    todo!("T-10: B-6.3 sheet")
}

/// One numbered element line: `#7 input "Email" empty` (D-203).
pub fn sheet_line(row: &IndexRow) -> String {
    let _ = row;
    todo!("T-10: sheet line")
}
