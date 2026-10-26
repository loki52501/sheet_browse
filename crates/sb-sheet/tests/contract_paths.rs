//! Contract paths (pr-ready step 4, D-74): every item agent-memory/interfaces.yaml lists for
//! sb-sheet under I-5 and I-6, at exactly that path, so a moved or renamed item fails to
//! compile here. `*Opts` in I-6 means SkeletonOpts, FindOpts, SheetOpts and ReadOpts.

#![allow(unused_imports)]

// I-5.
use sb_sheet::index::{ElemRef, FieldValue, Flags, FrameId, IndexRow, Rect, State};
// I-6, root.
use sb_sheet::{
    find, find_lines, read, sheet, sheet_line, skeleton, BlockKind, Cursor, FindHit, FindOpts,
    HitReason, PageMeta, ReadOpts, ReadOutput, SheetOpts, SkeletonOpts, Span, TextBlock,
};
// I-6, modules.
use sb_sheet::data::{alias_for, aria_roles, role_prior, synonyms};
use sb_sheet::tokens::estimate;

/// The I-6 signatures, not just the names: a changed argument or return type fails here.
#[test]
fn i6_signatures_hold() {
    let _: fn(&[IndexRow], &[TextBlock], &SkeletonOpts) -> String = skeleton;
    let _: fn(&[IndexRow], &[TextBlock], &FindOpts) -> Vec<FindHit> = find;
    let _: fn(&[IndexRow], &[TextBlock], &SheetOpts) -> String = sheet;
    let _: fn(&[IndexRow], &[TextBlock], &ReadOpts) -> ReadOutput = read;
    let _: fn(&IndexRow) -> String = sheet_line;
    let _: fn(&[IndexRow], &[FindHit]) -> String = find_lines;
    let _: fn(&str) -> &'static [String] = aria_roles;
    let _: fn(&str) -> &str = alias_for;
    let _: fn(&str) -> &'static [String] = role_prior;
    let _: fn(&str) -> &'static [String] = synonyms;
    let _: fn(&str) -> usize = estimate;
}
