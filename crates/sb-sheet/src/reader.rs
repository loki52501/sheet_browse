//! W-5 read renderer (`sb-sheet::reader`), B-6.4. Owned by coder B (T-10).
//!
//! Pipeline: drop CHALLENGE-frame blocks, pick the content (`main` landmark, else boilerplate
//! scoring), apply `region`, render each block to light markdown (one [`Unit`] per block),
//! then cut chunks between units (D-204). A unit bigger than a chunk splits at sentence, then
//! word boundaries; the cursor then carries a byte offset into the unit body (D-205).
//!
//! Safety rules (H-2, D-5, D-55): a span linking a password field renders as the link form
//! only, never its text; any password value the index carries is scrubbed from the output;
//! blocks inside a CHALLENGE frame are never read, and a block linking a CHALLENGE row is
//! reduced to the `[iframe "reCAPTCHA"](#id)` link form.

use crate::index::{ElemRef, FieldValue, FrameId, IndexRow};
use crate::text::{BlockKind, Span, TextBlock};
use crate::{Cursor, OutlineEntry, ReadOpts, ReadOutput};
use std::collections::{HashMap, HashSet};

/// W-5 default chunk size.
const DEFAULT_CHUNK_TOKENS: usize = 4000;
/// Boilerplate score at or above which a block is kept on a `main`-less page.
const KEEP_THRESHOLD: f32 = 0.5;
/// Landmark vocabulary shared with `TextBlock.landmark` and `IndexRow.landmark`.
const LANDMARKS: [&str; 8] = [
    "main", "nav", "header", "footer", "aside", "form", "region", "dialog",
];
const CURSOR_VERSION: &str = "v1";
/// Characters per token (D-200); used to turn a token budget into a character budget.
const CHARS_PER_TOKEN: usize = 4;

pub(crate) fn read(index: &[IndexRow], text_blocks: &[TextBlock], opts: &ReadOpts) -> ReadOutput {
    let ctx = Ctx::new(index);
    let mut notes: Vec<String> = Vec::new();

    // Cursor first: it owns the region of a continued read.
    let parsed = match &opts.cursor {
        None => None,
        Some(c) => match parse_cursor(&c.0) {
            Some(p) => Some(p),
            None => {
                notes.push("cursor not understood, reading from the start".to_string());
                None
            }
        },
    };
    let region: Option<String> = match &parsed {
        Some(p) if p.region.is_empty() => None,
        Some(p) => Some(p.region.clone()),
        None => opts
            .region
            .as_ref()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty()),
    };

    // Document order; drop everything inside a CHALLENGE frame (D-5, D-55).
    let mut blocks: Vec<&TextBlock> = text_blocks
        .iter()
        .filter(|b| !ctx.challenge_frames.contains(&b.frame))
        .collect();
    blocks.sort_by_key(|b| b.order);

    let (selected, region_note) = select(&blocks, &ctx, region.as_deref());
    if let Some(n) = region_note {
        notes.push(n);
    }
    let units: Vec<Unit> = selected
        .iter()
        .filter_map(|b| render_block(b, &ctx))
        .collect();
    let hash = content_hash(&units);

    // Resolve the start position.
    let mut start = Pos { unit: 0, off: 0 };
    let mut used = 0usize;
    if let Some(p) = &parsed {
        if p.hash != hash {
            notes.push("page changed since the cursor, restarted at chunk 1".to_string());
        } else {
            used = p.used;
            start = resolve_pos(&units, p.order, p.off);
        }
    }

    let chunk_tokens = if opts.chunk_tokens == 0 {
        DEFAULT_CHUNK_TOKENS
    } else {
        opts.chunk_tokens
    };

    // Chunk numbering: chunks of the canonical plan that start before `start`, plus the
    // chunks needed from `start` to the end.
    let before = plan_starts(&units, Pos { unit: 0, off: 0 }, chunk_tokens)
        .into_iter()
        .filter(|s| *s < start)
        .count();
    let after = plan_starts(&units, start, chunk_tokens).len();
    let chunks = (before + after).max(1);
    let chunk = (before + 1).min(chunks);

    // This call's budget: one chunk, capped by what max_tokens leaves.
    let left = opts.max_tokens.map(|m| m.saturating_sub(used));
    let budget = match left {
        Some(l) => chunk_tokens.min(l),
        None => chunk_tokens,
    };

    let (body, next) = if start.unit >= units.len() || budget == 0 {
        (String::new(), start)
    } else {
        let (pieces, next) = next_chunk(&units, start, budget);
        (join_pieces(&units, &pieces), next)
    };
    let body = ctx.scrub(&body);
    let used_after = used + estimate(&body);
    let more = next.unit < units.len();
    let truncated = more && opts.max_tokens.is_some_and(|m| used_after >= m);

    let remaining: Vec<OutlineEntry> = if more {
        outline_from(&units, next)
            .into_iter()
            .map(|e| OutlineEntry {
                heading: ctx.scrub(&e.heading),
                words: e.words,
            })
            .collect()
    } else {
        Vec::new()
    };
    let cursor = if more && !truncated {
        Some(Cursor(encode_cursor(
            region.as_deref().unwrap_or(""),
            &units,
            next,
            opts.max_tokens.map(|_| used_after),
            hash,
        )))
    } else {
        None
    };

    let mut first: Vec<String> = Vec::new();
    if chunks > 1 {
        first.push(format!("chunk {chunk} of {chunks}"));
    }
    first.extend(notes);

    let mut md = String::new();
    if !first.is_empty() {
        md.push_str(&first.join(" · "));
        md.push('\n');
    }
    if body.is_empty() {
        if units.is_empty() {
            md.push_str("(no readable text)");
        } else if start.unit >= units.len() {
            md.push_str("(nothing left to read)");
        }
    } else {
        md.push_str(&body);
    }
    if !remaining.is_empty() {
        md.push_str("\n\nremaining:");
        for e in &remaining {
            md.push_str(&format!("\n- {} ({} words)", e.heading, e.words));
        }
    }
    if truncated {
        if let Some(m) = opts.max_tokens {
            md.push_str(&format!("\n\ntruncated: max_tokens {m} reached"));
        }
    }

    ReadOutput {
        markdown: ctx.scrub(md.trim_start_matches('\n')),
        chunk,
        chunks,
        remaining,
        cursor,
    }
}

// ---------------------------------------------------------------------------------------------
// Context: row lookup, CHALLENGE frames, password values to scrub.

struct Ctx<'a> {
    rows: HashMap<ElemRef, &'a IndexRow>,
    challenge_frames: HashSet<FrameId>,
    secrets: Vec<String>,
}

impl<'a> Ctx<'a> {
    fn new(index: &'a [IndexRow]) -> Ctx<'a> {
        let rows: HashMap<ElemRef, &IndexRow> = index.iter().map(|r| (r.elem(), r)).collect();

        // Frames hosted by CHALLENGE rows, plus any frame nested inside one of them.
        let mut challenge_frames: HashSet<FrameId> = index
            .iter()
            .filter(|r| r.flags.challenge)
            .filter_map(|r| r.child_frame)
            .collect();
        loop {
            let nested: Vec<FrameId> = index
                .iter()
                .filter(|r| challenge_frames.contains(&r.frame))
                .filter_map(|r| r.child_frame)
                .filter(|f| !challenge_frames.contains(f))
                .collect();
            if nested.is_empty() {
                break;
            }
            challenge_frames.extend(nested);
        }

        // A password value should never reach the index (B-5), but if one does, it is
        // scrubbed from everything the reader returns.
        let mut secrets: Vec<String> = index
            .iter()
            .filter(|r| is_password(r))
            .filter_map(|r| match &r.state.value {
                Some(FieldValue::Text(t)) if !t.trim().is_empty() => Some(t.clone()),
                _ => None,
            })
            .collect();
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();

        Ctx {
            rows,
            challenge_frames,
            secrets,
        }
    }

    fn row(&self, e: &ElemRef) -> Option<&'a IndexRow> {
        self.rows.get(e).copied()
    }

    fn scrub(&self, s: &str) -> String {
        let mut out = s.to_string();
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), "***");
            }
        }
        out
    }
}

fn is_password(r: &IndexRow) -> bool {
    r.state
        .input_type
        .as_deref()
        .is_some_and(|t| t.eq_ignore_ascii_case("password"))
}

// ---------------------------------------------------------------------------------------------
// Selection: main landmark, boilerplate scoring, region.

fn select<'b>(
    blocks: &[&'b TextBlock],
    ctx: &Ctx<'_>,
    region: Option<&str>,
) -> (Vec<&'b TextBlock>, Option<String>) {
    let content = default_content(blocks, ctx);
    let Some(region) = region else {
        return (content, None);
    };

    let lower = region.to_ascii_lowercase();
    if !region.starts_with('#') && LANDMARKS.contains(&lower.as_str()) {
        let picked: Vec<&TextBlock> = blocks
            .iter()
            .copied()
            .filter(|b| b.landmark.as_deref().map(str::to_ascii_lowercase) == Some(lower.clone()))
            .collect();
        if picked.is_empty() {
            return (
                content,
                Some(format!("region {region:?} not found, reading the page")),
            );
        }
        return (picked, None);
    }

    let hashes = region.chars().take_while(|c| *c == '#').count();
    let want_level = (hashes > 0).then(|| hashes.min(6) as u8);
    let want_text = norm(region.trim_start_matches('#'));
    if want_text.is_empty() {
        return (
            content,
            Some(format!("region {region:?} not found, reading the page")),
        );
    }
    for list in [&content, &blocks.to_vec()] {
        if let Some(section) = heading_section(list, ctx, &want_text, want_level) {
            return (section, None);
        }
    }
    (
        content,
        Some(format!("region {region:?} not found, reading the page")),
    )
}

/// The section under a heading: from the heading to the next heading of the same or a
/// higher level. Exact level match first, then any level; exact text, then prefix.
fn heading_section<'b>(
    list: &[&'b TextBlock],
    ctx: &Ctx<'_>,
    want: &str,
    level: Option<u8>,
) -> Option<Vec<&'b TextBlock>> {
    let headings: Vec<(usize, u8, String)> = list
        .iter()
        .enumerate()
        .filter(|(_, b)| b.kind == BlockKind::Heading)
        .map(|(i, b)| (i, heading_level(b), norm(&plain_text(b, ctx))))
        .collect();
    let tries: [(bool, bool); 4] = [(true, true), (false, true), (true, false), (false, false)];
    let found = tries.iter().find_map(|(by_level, exact)| {
        if *by_level && level.is_none() {
            return None;
        }
        headings.iter().find(|(_, l, t)| {
            let level_ok = !*by_level || Some(*l) == level;
            let text_ok = if *exact {
                t == want
            } else {
                t.starts_with(want)
            };
            level_ok && text_ok
        })
    })?;
    let (start, lvl, _) = found;
    let end = list
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, b)| b.kind == BlockKind::Heading && heading_level(b) <= *lvl)
        .map(|(i, _)| i)
        .unwrap_or(list.len());
    Some(list[*start..end].to_vec())
}

/// Pages with a `main` landmark use it directly; others keep blocks that pass the
/// boilerplate score.
fn default_content<'b>(blocks: &[&'b TextBlock], ctx: &Ctx<'_>) -> Vec<&'b TextBlock> {
    let in_main = |b: &&TextBlock| b.landmark.as_deref() == Some("main");
    if blocks.iter().any(in_main) {
        return blocks.iter().copied().filter(in_main).collect();
    }
    let keep = boilerplate_keep(blocks, ctx);
    blocks
        .iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(b, _)| *b)
        .collect()
}

/// W-5 boilerplate score per block: landmark role, link density, text density and text
/// repeated across the page (same text in header and footer).
fn boilerplate_keep(blocks: &[&TextBlock], ctx: &Ctx<'_>) -> Vec<bool> {
    let texts: Vec<String> = blocks.iter().map(|b| norm(&plain_text(b, ctx))).collect();
    let mut seen: HashMap<&str, (usize, HashSet<Option<&str>>)> = HashMap::new();
    for (t, b) in texts.iter().zip(blocks) {
        if t.is_empty() {
            continue;
        }
        let e = seen.entry(t.as_str()).or_default();
        e.0 += 1;
        e.1.insert(b.landmark.as_deref());
    }

    blocks
        .iter()
        .zip(&texts)
        .map(|(b, t)| {
            if t.is_empty() {
                return false;
            }
            let (total, links) = link_chars(b, ctx);
            let density = if total == 0 {
                0.0
            } else {
                links as f32 / total as f32
            };
            let words = t.split_whitespace().count();
            let mut score = 1.0f32;
            score += match b.landmark.as_deref() {
                Some("nav" | "footer" | "aside") => -1.5,
                Some("header") => -0.75,
                Some("main" | "region") => 0.5,
                _ => 0.0,
            };
            if b.kind != BlockKind::Heading {
                score -= 2.0 * density;
                if words >= 20 {
                    score += 0.5;
                } else if words < 4 {
                    score -= 0.5;
                }
            }
            if let Some((count, landmarks)) = seen.get(t.as_str()) {
                if (*count >= 2 && landmarks.len() >= 2) || *count >= 3 {
                    score -= 1.0;
                }
            }
            score >= KEEP_THRESHOLD
        })
        .collect()
}

/// (all non-space chars, chars inside spans that link somewhere).
fn link_chars(b: &TextBlock, ctx: &Ctx<'_>) -> (usize, usize) {
    let mut total = 0;
    let mut links = 0;
    for s in &b.spans {
        let n = s.text.chars().filter(|c| !c.is_whitespace()).count();
        total += n;
        if let Some(e) = &s.elem {
            // An unresolved elem is still something clickable: count it as a link.
            let is_link = ctx.row(e).is_none_or(|r| r.role == "link");
            if is_link {
                links += n;
            }
        }
    }
    (total, links)
}

// ---------------------------------------------------------------------------------------------
// Rendering: one block -> one unit of light markdown.

struct Unit {
    order: u32,
    kind: BlockKind,
    /// Frame and table id for `TableRow`.
    table: Option<(FrameId, u32)>,
    /// Heading level for headings.
    level: u8,
    /// Before the first piece.
    prefix: String,
    /// Before a continuation piece of a split body.
    cont_prefix: String,
    body: String,
    /// After every piece (closing code fence).
    close: String,
    /// After the last piece only (table header separator).
    suffix: String,
    /// Plain-text label for outlines (`## FAQ`), headings only.
    label: Option<String>,
}

fn render_block(b: &TextBlock, ctx: &Ctx<'_>) -> Option<Unit> {
    // A block that links a CHALLENGE row shows only the link form (D-55).
    let challenge: Vec<String> = b
        .spans
        .iter()
        .filter_map(|s| s.elem.as_ref())
        .filter_map(|e| ctx.row(e))
        .filter(|r| r.flags.challenge)
        .map(|r| elem_form(r, ""))
        .collect();
    let mut unit = Unit {
        order: b.order,
        kind: b.kind,
        table: None,
        level: 0,
        prefix: String::new(),
        cont_prefix: String::new(),
        body: String::new(),
        close: String::new(),
        suffix: String::new(),
        label: None,
    };
    if !challenge.is_empty() {
        unit.kind = BlockKind::Paragraph;
        unit.body = challenge.join(" ");
        return Some(unit);
    }

    match b.kind {
        BlockKind::Heading => {
            let level = heading_level(b);
            let body = inline(&b.spans, ctx);
            if body.is_empty() {
                return None;
            }
            let hashes = "#".repeat(level as usize);
            unit.level = level;
            unit.prefix = format!("{hashes} ");
            unit.cont_prefix = unit.prefix.clone();
            unit.label = Some(format!("{hashes} {}", plain_text(b, ctx).trim()));
            unit.body = body;
        }
        BlockKind::ListItem => {
            let depth = b.level.unwrap_or(1).clamp(1, 8) as usize;
            let indent = "  ".repeat(depth - 1);
            unit.prefix = format!("{indent}- ");
            unit.cont_prefix = format!("{indent}  ");
            unit.body = inline(&b.spans, ctx);
        }
        BlockKind::TableRow => {
            let cells: Vec<String> = b
                .spans
                .iter()
                .map(|s| inline(std::slice::from_ref(s), ctx).replace('|', "\\|"))
                .collect();
            if cells.iter().all(String::is_empty) {
                return None;
            }
            unit.table = Some((b.frame, b.table.unwrap_or(u32::MAX)));
            unit.body = format!("| {} |", cells.join(" | "));
            if b.header {
                unit.suffix = format!("\n|{}", "---|".repeat(cells.len()));
            }
        }
        BlockKind::Code => {
            let body = code_text(&b.spans, ctx);
            if body.trim().is_empty() {
                return None;
            }
            unit.prefix = "```\n".to_string();
            unit.cont_prefix = unit.prefix.clone();
            unit.close = "\n```".to_string();
            unit.body = body;
        }
        BlockKind::Quote => {
            unit.prefix = "> ".to_string();
            unit.cont_prefix = unit.prefix.clone();
            unit.body = inline(&b.spans, ctx);
        }
        BlockKind::Paragraph | BlockKind::Other => {
            unit.body = inline(&b.spans, ctx);
        }
    }
    if unit.body.is_empty() {
        return None;
    }
    Some(unit)
}

fn heading_level(b: &TextBlock) -> u8 {
    b.level.unwrap_or(2).clamp(1, 6)
}

/// A span as it renders: plain text, `[text](#id)` for links, `[alias "name"](#id)` for
/// other actionable things. Password fields never show span text.
fn span_form(s: &Span, ctx: &Ctx<'_>) -> (String, bool) {
    let Some(e) = &s.elem else {
        return (s.text.clone(), false);
    };
    let text = collapse(&s.text);
    let form = match ctx.row(e) {
        None => format!("[{}]({e})", link_text(&text)),
        Some(r) if r.flags.challenge || is_password(r) => elem_form(r, ""),
        Some(r) if r.role == "link" => {
            let t = if text.is_empty() {
                collapse(&r.name)
            } else {
                text
            };
            format!("[{}]({e})", link_text(&t))
        }
        Some(r) => elem_form(r, &text),
    };
    (form, true)
}

/// `[alias "label"](#id)`; the label is the given text, else the row's name.
fn elem_form(r: &IndexRow, text: &str) -> String {
    let label = if text.is_empty() {
        collapse(&r.name)
    } else {
        text.to_string()
    };
    let label = link_text(&label).replace('"', "'");
    // Short alias from data/roles.txt (D-49), the same one sheet lines print.
    let alias = crate::data::alias_for(&r.role);
    if label.is_empty() {
        format!("[{alias}]({})", r.elem())
    } else {
        format!("[{alias} \"{label}\"]({})", r.elem())
    }
}

fn link_text(s: &str) -> String {
    s.replace('[', "(").replace(']', ")")
}

/// Inline text: spans concatenated (a space inserted around link forms when the source has
/// none), whitespace collapsed.
fn inline(spans: &[Span], ctx: &Ctx<'_>) -> String {
    let mut raw = String::new();
    let mut prev_link = false;
    for s in spans {
        let (form, is_link) = span_form(s, ctx);
        if form.is_empty() {
            continue;
        }
        if (is_link || prev_link) && needs_space(&raw, &form) {
            raw.push(' ');
        }
        raw.push_str(&form);
        prev_link = is_link;
    }
    collapse(&raw)
}

fn needs_space(before: &str, next: &str) -> bool {
    let Some(a) = before.chars().last() else {
        return false;
    };
    let Some(b) = next.chars().next() else {
        return false;
    };
    !a.is_whitespace()
        && !b.is_whitespace()
        && !matches!(a, '(' | '"' | '\'')
        && !".,;:!?)'\"".contains(b)
}

/// Code keeps its line breaks; trailing blank lines go.
fn code_text(spans: &[Span], ctx: &Ctx<'_>) -> String {
    let mut out = String::new();
    for s in spans {
        out.push_str(&span_form(s, ctx).0);
    }
    out.trim_end().trim_start_matches('\n').to_string()
}

/// The block's text as words (links as their visible text), for scoring and outlines.
fn plain_text(b: &TextBlock, ctx: &Ctx<'_>) -> String {
    let mut out = String::new();
    for s in &b.spans {
        let t = match s.elem.as_ref().and_then(|e| ctx.row(e)) {
            Some(r) if r.flags.challenge || is_password(r) => collapse(&r.name),
            _ => s.text.clone(),
        };
        if !out.is_empty() && !out.ends_with(char::is_whitespace) {
            out.push(' ');
        }
        out.push_str(&t);
    }
    collapse(&out)
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn norm(s: &str) -> String {
    collapse(s).to_lowercase()
}

// ---------------------------------------------------------------------------------------------
// Chunking.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Pos {
    unit: usize,
    /// Byte offset into the unit body (0 = whole unit).
    off: usize,
}

#[derive(Debug, Clone, Copy)]
struct Piece {
    unit: usize,
    start: usize,
    end: usize,
}

fn estimate(s: &str) -> usize {
    crate::tokens::estimate(s)
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

fn separator(a: &Unit, b: &Unit) -> &'static str {
    let same_table =
        a.kind == BlockKind::TableRow && b.kind == BlockKind::TableRow && a.table == b.table;
    let both_list = a.kind == BlockKind::ListItem && b.kind == BlockKind::ListItem;
    if same_table || both_list {
        "\n"
    } else {
        "\n\n"
    }
}

fn piece_text(u: &Unit, p: &Piece) -> String {
    let prefix = if p.start == 0 {
        &u.prefix
    } else {
        &u.cont_prefix
    };
    let mut s = String::with_capacity(prefix.len() + (p.end - p.start) + 8);
    s.push_str(prefix);
    s.push_str(u.body[p.start..p.end].trim_end());
    s.push_str(&u.close);
    if p.end >= u.body.len() {
        s.push_str(&u.suffix);
    }
    s
}

fn join_pieces(units: &[Unit], pieces: &[Piece]) -> String {
    let mut out = String::new();
    let mut prev: Option<&Unit> = None;
    for p in pieces {
        let u = &units[p.unit];
        if let Some(a) = prev {
            out.push_str(separator(a, u));
        }
        out.push_str(&piece_text(u, p));
        prev = Some(u);
    }
    out
}

/// Fill one chunk from `pos` within `budget` tokens. Breaks between units; a unit that does
/// not fit an empty chunk is cut at a sentence, else a word, else a char boundary (D-204).
/// Always makes progress.
fn next_chunk(units: &[Unit], pos: Pos, budget: usize) -> (Vec<Piece>, Pos) {
    let cap = budget.max(1) * CHARS_PER_TOKEN;
    let mut used = 0usize;
    let mut pieces: Vec<Piece> = Vec::new();
    let mut cur = pos;
    while cur.unit < units.len() {
        let u = &units[cur.unit];
        let start = skip_ws(&u.body, cur.off);
        if start >= u.body.len() {
            cur = Pos {
                unit: cur.unit + 1,
                off: 0,
            };
            continue;
        }
        let sep = match pieces.last() {
            Some(last) => chars(separator(&units[last.unit], u)),
            None => 0,
        };
        let whole = Piece {
            unit: cur.unit,
            start,
            end: u.body.len(),
        };
        let whole_chars = chars(&piece_text(u, &whole));
        if used + sep + whole_chars <= cap {
            used += sep + whole_chars;
            pieces.push(whole);
            cur = Pos {
                unit: cur.unit + 1,
                off: 0,
            };
            continue;
        }
        if !pieces.is_empty() {
            break;
        }
        // Too big for an empty chunk: split this unit's body.
        let prefix = if start == 0 {
            &u.prefix
        } else {
            &u.cont_prefix
        };
        let overhead = chars(prefix) + chars(&u.close);
        let avail = cap.saturating_sub(overhead).max(1);
        let rest = &u.body[start..];
        if chars(rest) <= avail {
            // Only the suffix overflowed; take the whole unit.
            pieces.push(whole);
            cur = Pos {
                unit: cur.unit + 1,
                off: 0,
            };
            break;
        }
        let cut = start + cut_point(rest, avail);
        pieces.push(Piece {
            unit: cur.unit,
            start,
            end: cut,
        });
        let next_off = skip_ws(&u.body, cut);
        cur = if next_off >= u.body.len() {
            Pos {
                unit: cur.unit + 1,
                off: 0,
            }
        } else {
            Pos {
                unit: cur.unit,
                off: next_off,
            }
        };
        break;
    }
    // A heading never ends a chunk when content follows: it travels with its section.
    while pieces.len() > 1 && cur.unit < units.len() {
        let Some(last) = pieces.last() else { break };
        let u = &units[last.unit];
        if u.kind != BlockKind::Heading || last.start != 0 || last.end < u.body.len() {
            break;
        }
        cur = Pos {
            unit: last.unit,
            off: 0,
        };
        pieces.pop();
    }
    (pieces, cur)
}

fn skip_ws(s: &str, off: usize) -> usize {
    match s.get(off..) {
        Some(rest) => off + (rest.len() - rest.trim_start().len()),
        None => s.len(),
    }
}

/// Byte length of the prefix of `s` to take, at most `max_chars` chars: the last sentence
/// end if it keeps at least half the room, else the last word break, else a char boundary.
fn cut_point(s: &str, max_chars: usize) -> usize {
    let limit = s
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let head = &s[..limit];
    let half = limit / 2;
    let mut sentence = None;
    let mut word = None;
    let mut prev: Option<char> = None;
    for (i, c) in head.char_indices() {
        if c.is_whitespace() {
            if matches!(prev, Some('.' | '!' | '?')) {
                sentence = Some(i);
            }
            word = Some(i);
        }
        prev = Some(c);
    }
    // The char right after `head` may be the whitespace that ends the last sentence/word.
    if s[limit..].starts_with(char::is_whitespace) {
        if matches!(prev, Some('.' | '!' | '?')) {
            sentence = Some(limit);
        }
        word = Some(limit);
    }
    match (sentence, word) {
        (Some(i), _) if i >= half && i > 0 => i,
        (_, Some(i)) if i > 0 => i,
        _ => {
            if limit > 0 {
                limit
            } else {
                s.chars().next().map(char::len_utf8).unwrap_or(s.len())
            }
        }
    }
}

/// Start positions of every chunk from `from` to the end.
fn plan_starts(units: &[Unit], from: Pos, chunk_tokens: usize) -> Vec<Pos> {
    let mut starts = Vec::new();
    let mut pos = from;
    while pos.unit < units.len() {
        let (pieces, next) = next_chunk(units, pos, chunk_tokens);
        if pieces.is_empty() && next == pos {
            break;
        }
        if !pieces.is_empty() {
            starts.push(pos);
        }
        pos = next;
    }
    starts
}

// ---------------------------------------------------------------------------------------------
// Outline of what is left.

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Sections (heading + words) with any content at or after `from`. Text before the first
/// heading is reported as `(top)`.
fn outline_from(units: &[Unit], from: Pos) -> Vec<OutlineEntry> {
    let mut out: Vec<OutlineEntry> = Vec::new();
    let mut current: Option<OutlineEntry> = None;
    let mut current_live = false;
    for (i, u) in units.iter().enumerate() {
        if let Some(label) = &u.label {
            if let Some(e) = current.take() {
                if current_live {
                    out.push(e);
                }
            }
            current = Some(OutlineEntry {
                heading: label.clone(),
                words: 0,
            });
            current_live = i >= from.unit;
            continue;
        }
        if i < from.unit {
            continue;
        }
        let body = if i == from.unit {
            u.body.get(from.off..).unwrap_or("")
        } else {
            u.body.as_str()
        };
        let words = word_count(body);
        if words == 0 {
            continue;
        }
        let e = current.get_or_insert_with(|| OutlineEntry {
            heading: "(top)".to_string(),
            words: 0,
        });
        e.words += words;
        current_live = true;
    }
    if let Some(e) = current {
        if current_live {
            out.push(e);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Cursor (D-204, D-205): `v1:<region>:<order>[.<offset>][+<used>]:<hash hex>`.

struct ParsedCursor {
    region: String,
    order: u32,
    off: usize,
    used: usize,
    hash: u64,
}

fn encode_cursor(region: &str, units: &[Unit], pos: Pos, used: Option<usize>, hash: u64) -> String {
    let order = units.get(pos.unit).map(|u| u.order).unwrap_or(u32::MAX);
    let mut p = order.to_string();
    if pos.off > 0 {
        p.push_str(&format!(".{}", pos.off));
    }
    if let Some(n) = used {
        p.push_str(&format!("+{n}"));
    }
    format!("{CURSOR_VERSION}:{region}:{p}:{hash:016x}")
}

fn parse_cursor(s: &str) -> Option<ParsedCursor> {
    let rest = s.trim().strip_prefix(CURSOR_VERSION)?.strip_prefix(':')?;
    let mut it = rest.rsplitn(3, ':');
    let hash = u64::from_str_radix(it.next()?, 16).ok()?;
    let pos = it.next()?;
    let region = it.next()?.to_string();
    let (pos, used) = match pos.split_once('+') {
        Some((p, u)) => (p, u.parse().ok()?),
        None => (pos, 0),
    };
    let (order, off) = match pos.split_once('.') {
        Some((o, f)) => (o.parse().ok()?, f.parse().ok()?),
        None => (pos.parse().ok()?, 0),
    };
    Some(ParsedCursor {
        region,
        order,
        off,
        used,
        hash,
    })
}

/// The unit holding block `order` (the first after it if absent); an offset that is not a
/// char boundary inside the body restarts that unit.
fn resolve_pos(units: &[Unit], order: u32, off: usize) -> Pos {
    let unit = units
        .iter()
        .position(|u| u.order >= order)
        .unwrap_or(units.len());
    let off = match units.get(unit) {
        Some(u)
            if units[unit].order == order && off < u.body.len() && u.body.is_char_boundary(off) =>
        {
            off
        }
        _ => 0,
    };
    Pos { unit, off }
}

/// FNV-1a 64 over the rendered units: stable across processes and releases.
fn content_hash(units: &[Unit]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for u in units {
        eat(&u.order.to_le_bytes());
        for part in [&u.prefix, &u.body, &u.close, &u.suffix] {
            eat(part.as_bytes());
            eat(&[0xff]);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips_with_colons_in_region() {
        let c = "v1:a:b:12.40+300:00000000000000ff";
        let p = parse_cursor(c).unwrap();
        assert_eq!(p.region, "a:b");
        assert_eq!((p.order, p.off, p.used, p.hash), (12, 40, 300, 255));
        for bad in [
            "",
            "v1",
            "v2:x:1:ff",
            "v1:x:y:ff",
            "v1:x:1:zz",
            "v1:1:ff",
            "v1:x:1.q:ff",
        ] {
            assert!(parse_cursor(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn cut_prefers_sentences_then_words() {
        let s = "One two three. Four five six seven eight";
        let i = cut_point(s, 24);
        assert_eq!(&s[..i], "One two three.");
        let s = "alpha beta gamma delta epsilon";
        let i = cut_point(s, 12);
        assert_eq!(&s[..i], "alpha beta");
        let s = "abcdefghijklmnop";
        assert_eq!(cut_point(s, 5), 5);
        assert_eq!(cut_point("é", 0), 2);
    }
}
