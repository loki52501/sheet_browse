//! W-5 read renderer (`sb-sheet::reader`), B-6.4. Owned by coder B (T-10).

use crate::{IndexRow, ReadOpts, ReadOutput, TextBlock};

pub(crate) fn read(index: &[IndexRow], text_blocks: &[TextBlock], opts: &ReadOpts) -> ReadOutput {
    let _ = (index, text_blocks, opts);
    todo!("T-10: W-5 read renderer")
}
