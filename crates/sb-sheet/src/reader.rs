//! W-5 read renderer (`sb-sheet::reader`), B-6.4. Owned by coder B (T-10).
//!
//! Pipeline: drop blocks of sealed frames (hosted by CHALLENGE or OPAQUE rows), pick the
//! content (the top frame's `main` landmark, else boilerplate scoring), apply `region`, render
//! each block to light markdown (one [`Unit`] per block, password values already scrubbed),
//! then cut chunks between units (D-204). A unit bigger than a chunk splits at sentence, then
//! word boundaries; the cursor then carries a byte offset into the unit body (D-210).
//!
//! Safety rules (H-2, D-5, D-55, D-212): a span linking a password field (D-206: input_type
//! or autocomplete) renders as the link form only, never its text; any password value the
//! index carries is scrubbed from every unit before chunking, so a split cannot cut it in two;
//! blocks inside a CHALLENGE or OPAQUE frame are never read, and a block linking a CHALLENGE
//! row is reduced to the `[iframe "reCAPTCHA"](#id)` link form.

use crate::index::{ElemRef, FieldValue, FrameId, IndexRow};
use crate::sheet::{is_password, sealed_frames};
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
    let asked: Option<String> = opts
        .region
        .as_ref()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty());
    // An explicit region that differs from the cursor's wins: the read restarts there at
    // chunk 1, keeping only the cursor's max_tokens budget (D-220). The same region (or none)
    // continues the cursor's read.
    let switched = match (&parsed, &asked) {
        (Some(p), Some(a)) => norm(a) != norm(&p.region),
        _ => false,
    };
    if switched {
        if let Some(a) = &asked {
            notes.push(format!(
                "region {a:?} is not the cursor's region, restarted at chunk 1"
            ));
        }
    }
    let region: Option<String> = match &parsed {
        Some(_) if switched => asked,
        Some(p) if p.region.is_empty() => None,
        Some(p) => Some(p.region.clone()),
        None => asked,
    };

    // Document order; drop everything inside a CHALLENGE or OPAQUE frame (D-5, D-55, I-5).
    let mut blocks: Vec<&TextBlock> = text_blocks
        .iter()
        .filter(|b| !ctx.sealed.contains(&b.frame))
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
        // The budget survives a page change or a region switch; only the position resets
        // (D-220: max_tokens caps the whole read sequence, fails closed).
        used = p.used;
        if !switched {
            if p.hash != hash {
                notes.push("page changed since the cursor, restarted at chunk 1".to_string());
            } else {
                start = resolve_pos(&units, p.order, p.off);
            }
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
        let (pieces, next) = next_chunk(&units, start, budget, left);
        (join_pieces(&units, &pieces), next)
    };
    // Units are scrubbed already; this pass is defence in depth.
    let body = ctx.scrub(&body);
    // `used` comes from a caller-supplied cursor: never overflow on a forged value.
    let used_after = used.saturating_add(estimate(&body));
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
// Context: row lookup, sealed frames, password values to scrub.

pub(crate) struct Ctx<'a> {
    rows: HashMap<ElemRef, &'a IndexRow>,
    /// The row hosting each child frame.
    hosts: HashMap<FrameId, &'a IndexRow>,
    /// Frames hosted by CHALLENGE or OPAQUE rows and frames nested in them (shared with
    /// sheet, find and skeleton).
    sealed: HashSet<FrameId>,
    secrets: Vec<String>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(index: &'a [IndexRow]) -> Ctx<'a> {
        let rows: HashMap<ElemRef, &IndexRow> = index.iter().map(|r| (r.elem(), r)).collect();

        let sealed = sealed_frames(index);
        let mut hosts: HashMap<FrameId, &IndexRow> = HashMap::new();
        for r in index {
            if let Some(c) = r.child_frame.filter(|c| *c != r.frame) {
                hosts.entry(c).or_insert(r);
            }
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
            // Rendered text is whitespace-collapsed and escaped: scrub those spellings too.
            .flat_map(|t| {
                let c = collapse(&t);
                [t, c]
            })
            .flat_map(|t| escaped_spellings(&t))
            .filter(|t| !t.is_empty())
            .collect();
        secrets.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        secrets.dedup();

        Ctx {
            rows,
            hosts,
            sealed,
            secrets,
        }
    }

    fn row(&self, e: &ElemRef) -> Option<&'a IndexRow> {
        self.rows.get(e).copied()
    }

    /// What a span linking `e` may show. A row inside a sealed frame (hosted by a CHALLENGE
    /// or OPAQUE row) is that frame's content, so it stands in for the outermost host row,
    /// or for nothing when the host is not in the index (D-55, D-212).
    fn linked(&self, e: &ElemRef) -> Linked<'a> {
        let Some(mut r) = self.row(e) else {
            return Linked::Unknown;
        };
        if !self.sealed.contains(&r.frame) {
            return Linked::Row(r);
        }
        // Walk up to the host that sits outside every sealed frame; bounded against cycles.
        for _ in 0..=self.hosts.len() {
            match self.hosts.get(&r.frame) {
                Some(h) if self.sealed.contains(&h.frame) => r = h,
                Some(h) => return Linked::Sealed(Some(h)),
                None => break,
            }
        }
        Linked::Sealed(None)
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

/// The row a span links, as the reader may show it.
enum Linked<'a> {
    /// Not in the index.
    Unknown,
    /// A row outside every sealed frame.
    Row(&'a IndexRow),
    /// A row inside a sealed frame: its outermost host row, if known.
    Sealed(Option<&'a IndexRow>),
}

impl<'a> Linked<'a> {
    /// The row whose CHALLENGE flag decides the block (the host for a sealed row).
    fn challenge(&self) -> Option<&'a IndexRow> {
        match self {
            Linked::Row(r) | Linked::Sealed(Some(r)) if r.flags.challenge => Some(r),
            _ => None,
        }
    }
}

/// `t` as the renderers may escape it: table cells turn `|` into `\|`, link labels and plain
/// spans turn `[` `]` into `(` `)`, quoted labels turn `"` into `'`, and inline text cuts runs
/// of three or more backticks to two (D-219). The escapes touch different characters, so
/// every combination is listed and a scrub catches the value however it was rendered (D-212,
/// D-218).
fn escaped_spellings(t: &str) -> Vec<String> {
    let mut out = vec![t.to_string()];
    for mask in 1u8..16 {
        let mut v = t.to_string();
        if mask & 1 != 0 {
            v = link_text(&v);
        }
        if mask & 2 != 0 {
            v = v.replace('"', "'");
        }
        if mask & 4 != 0 {
            v = v.replace('|', "\\|");
        }
        if mask & 8 != 0 {
            v = cut_backticks(&v);
        }
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
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
/// boilerplate score. The landmark belongs to its document, so only the top frame's `main`
/// decides (D-216): a `<main>` inside an iframe does not replace a main-less top page.
fn default_content<'b>(blocks: &[&'b TextBlock], ctx: &Ctx<'_>) -> Vec<&'b TextBlock> {
    let in_main = |b: &&TextBlock| b.frame == 0 && b.landmark.as_deref() == Some("main");
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

/// A block that links a CHALLENGE row, or a row inside a challenge's frame, shows only the
/// challenge's link forms (D-55, D-212). Empty when the block links no challenge.
fn challenge_forms(b: &TextBlock, ctx: &Ctx<'_>) -> Vec<String> {
    let mut challenge: Vec<String> = Vec::new();
    for e in b.spans.iter().filter_map(|s| s.elem.as_ref()) {
        if let Some(r) = ctx.linked(e).challenge() {
            let form = elem_form(r, "");
            if !challenge.contains(&form) {
                challenge.push(form);
            }
        }
    }
    challenge
}

impl Ctx<'_> {
    /// One block's spans on one line under the reader's span rules: the challenge link forms
    /// for a block linking a CHALLENGE row, else [`inline`] (password spans as link forms,
    /// plain text escaped, D-219), scrubbed. Shared with the skeleton's h1/h2 line.
    pub(crate) fn block_line(&self, b: &TextBlock) -> String {
        let challenge = challenge_forms(b, self);
        if challenge.is_empty() {
            self.scrub(&inline(&b.spans, self))
        } else {
            self.scrub(&challenge.join(" "))
        }
    }
}

fn render_block(b: &TextBlock, ctx: &Ctx<'_>) -> Option<Unit> {
    let challenge = challenge_forms(b, ctx);
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
        unit.body = ctx.scrub(&challenge.join(" "));
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
            // The label is printed in the `remaining:` outline: escape it like plain spans.
            let label = cut_backticks(&link_text(plain_text(b, ctx).trim()));
            unit.label = Some(format!("{hashes} {label}"));
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
            // One backtick longer than any run in the body, so the body cannot close it
            // (D-219). The scrub below only replaces text with `***`, never adds backticks.
            let fence = "`".repeat(longest_backtick_run(&body).saturating_add(1).max(3));
            unit.prefix = format!("{fence}\n");
            unit.cont_prefix = unit.prefix.clone();
            unit.close = format!("\n{fence}");
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
    // Scrub before chunking and hashing, so a split can never cut a password value in two
    // and the hash and outline never see one (D-212).
    unit.body = ctx.scrub(&unit.body);
    unit.label = unit.label.map(|l| ctx.scrub(&l));
    if unit.body.is_empty() {
        return None;
    }
    Some(unit)
}

fn heading_level(b: &TextBlock) -> u8 {
    b.level.unwrap_or(2).clamp(1, 6)
}

/// A span as it renders: plain text, `[text](#id)` for links, `[alias "name"](#id)` for
/// other actionable things. Password fields never show span text. Plain text goes through
/// [`link_text`], so page text cannot forge a link form (D-219); it is scrubbed first.
fn span_form(s: &Span, ctx: &Ctx<'_>) -> (String, bool) {
    let Some(e) = &s.elem else {
        return (link_text(&ctx.scrub(&s.text)), false);
    };
    let text = collapse(&ctx.scrub(&s.text));
    let form = match ctx.linked(e) {
        Linked::Unknown => format!("[{}]({e})", link_text(&text)),
        // Nothing from inside a sealed frame: the host's link form, or nothing at all.
        Linked::Sealed(Some(h)) => elem_form(h, ""),
        Linked::Sealed(None) => return (String::new(), false),
        Linked::Row(r) if r.flags.challenge || is_password(r) => elem_form(r, ""),
        Linked::Row(r) if r.role == "link" => {
            let t = if text.is_empty() {
                collapse(&r.name)
            } else {
                text
            };
            format!("[{}]({e})", link_text(&t))
        }
        Linked::Row(r) => elem_form(r, &text),
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

/// Runs of three or more backticks cut to two, so text outside a code block cannot open or
/// close a fence (D-219).
fn cut_backticks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = 0usize;
    for c in s.chars() {
        if c == '`' {
            run += 1;
            if run > 2 {
                continue;
            }
        } else {
            run = 0;
        }
        out.push(c);
    }
    out
}

/// The longest run of consecutive backticks in `s`.
fn longest_backtick_run(s: &str) -> usize {
    let mut best = 0usize;
    let mut run = 0usize;
    for c in s.chars() {
        if c == '`' {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

/// Inline text: spans concatenated (a space inserted around link forms when the source has
/// none), whitespace collapsed, backtick runs cut after joining (spans may meet in a run).
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
    cut_backticks(&collapse(&raw))
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
        let t = match s.elem.as_ref().map(|e| ctx.linked(e)) {
            Some(Linked::Sealed(Some(h))) => collapse(&h.name),
            Some(Linked::Sealed(None)) => String::new(),
            Some(Linked::Row(r)) if r.flags.challenge || is_password(r) => collapse(&r.name),
            _ => ctx.scrub(&s.text),
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
/// A chunk that holds only headings never ends there: the next unit is cut into the room
/// left, at least a quarter of the chunk, but never past `hard` tokens (what max_tokens
/// leaves, D-222). Always makes progress.
fn next_chunk(units: &[Unit], pos: Pos, budget: usize, hard: Option<usize>) -> (Vec<Piece>, Pos) {
    // `budget` is a caller option: a huge value saturates instead of overflowing.
    let cap = budget.max(1).saturating_mul(CHARS_PER_TOKEN);
    let hard_cap = hard.map_or(usize::MAX, |h| h.saturating_mul(CHARS_PER_TOKEN));
    let mut used = 0usize;
    let mut pieces: Vec<Piece> = Vec::new();
    let mut cur = pos;
    while cur.unit < units.len() {
        let u = &units[cur.unit];
        let start = skip_lead(u, cur.off);
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
        let headings_only = !pieces.is_empty()
            && u.kind != BlockKind::Heading
            && pieces
                .iter()
                .all(|p| units[p.unit].kind == BlockKind::Heading);
        if !pieces.is_empty() && !headings_only {
            break;
        }
        // Too big for an empty (or headings-only) chunk: split this unit's body.
        let prefix = if start == 0 {
            &u.prefix
        } else {
            &u.cont_prefix
        };
        let overhead = chars(prefix) + chars(&u.close);
        let avail = if headings_only {
            // The quarter-chunk floor only where max_tokens leaves room for it (D-222).
            let taken = used + sep + overhead;
            let room = cap.saturating_sub(taken).max(cap / 4);
            let room = room.min(hard_cap.saturating_sub(taken));
            let suffix_only =
                used + sep + whole_chars > hard_cap && chars(rest_of(u, start)) <= room;
            if room == 0 || suffix_only {
                // No room under max_tokens for a piece of this unit (or only for the body
                // without its table suffix): end the chunk on the headings.
                break;
            }
            room
        } else {
            cap.saturating_sub(overhead)
        }
        .max(1);
        let rest = rest_of(u, start);
        if chars(rest) <= avail {
            // Only the suffix overflowed; take the whole unit.
            pieces.push(whole);
            cur = Pos {
                unit: cur.unit + 1,
                off: 0,
            };
            break;
        }
        let cut = start
            + if u.kind == BlockKind::Code {
                code_cut_point(rest, avail)
            } else {
                cut_point(rest, avail)
            };
        pieces.push(Piece {
            unit: cur.unit,
            start,
            end: cut,
        });
        let next_off = skip_lead(u, cut);
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

/// The body of `u` from byte `start` (a char boundary from [`skip_lead`]); empty if not.
fn rest_of(u: &Unit, start: usize) -> &str {
    u.body.get(start..).unwrap_or("")
}

fn skip_ws(s: &str, off: usize) -> usize {
    match s.get(off..) {
        Some(rest) => off + (rest.len() - rest.trim_start().len()),
        None => s.len(),
    }
}

/// Where a piece of `u` starting at `off` really starts. Prose drops the whitespace at a cut.
/// Code keeps its indentation: the first line is taken as is, a cut at a line break resumes at
/// the next line with its leading spaces, and only a cut inside an over-long line drops the
/// spaces at the cut (W-5: a cursor continues with no gap).
fn skip_lead(u: &Unit, off: usize) -> usize {
    if u.kind != BlockKind::Code {
        return skip_ws(&u.body, off);
    }
    let Some(rest) = u.body.get(off..) else {
        return u.body.len();
    };
    if rest.trim().is_empty() {
        return u.body.len();
    }
    if off == 0 || u.body[..off].ends_with('\n') {
        return off;
    }
    if rest.starts_with('\n') {
        return off + 1;
    }
    let t = rest.trim_start_matches([' ', '\t']);
    let t = t.strip_prefix('\n').unwrap_or(t);
    off + (rest.len() - t.len())
}

/// Like [`cut_point`], but code is cut at the last line break that fits, so every line keeps
/// its indentation; a line longer than the room falls back to a word cut.
fn code_cut_point(s: &str, max_chars: usize) -> usize {
    let limit = s
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    if s[limit..].starts_with('\n') && limit > 0 {
        return limit;
    }
    match s[..limit].rfind('\n') {
        Some(i) if i > 0 => i,
        _ => cut_point(s, max_chars),
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
        let (pieces, next) = next_chunk(units, pos, chunk_tokens, None);
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
// Cursor (D-204, D-210): `v1:<region>:<order>[.<offset>][+<used>]:<hash hex>`.

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

    use crate::index::{Flags, State};

    fn row(id: u32, role: &str, name: &str) -> IndexRow {
        IndexRow {
            id,
            frame: 0,
            role: role.into(),
            name: name.into(),
            nearby: None,
            heading: None,
            state: State::default(),
            flags: Flags::default(),
            form: None,
            landmark: None,
            child_frame: None,
            bbox: None,
        }
    }

    fn block(order: u32, frame: FrameId, kind: BlockKind, spans: Vec<Span>) -> TextBlock {
        TextBlock {
            order,
            frame,
            kind,
            level: (kind == BlockKind::Heading).then_some(2),
            landmark: None,
            heading: None,
            table: None,
            header: false,
            spans,
        }
    }

    fn text(t: &str) -> Span {
        Span {
            text: t.into(),
            elem: None,
        }
    }

    fn para(order: u32, t: &str) -> TextBlock {
        block(order, 0, BlockKind::Paragraph, vec![text(t)])
    }

    fn opts(chunk_tokens: usize) -> ReadOpts {
        ReadOpts {
            region: None,
            cursor: None,
            max_tokens: None,
            chunk_tokens,
        }
    }

    /// Every chunk of a read, following cursors.
    fn read_all(rows: &[IndexRow], blocks: &[TextBlock], o: ReadOpts) -> Vec<ReadOutput> {
        let mut out = vec![read(rows, blocks, &o)];
        while let Some(c) = out.last().and_then(|r| r.cursor.clone()) {
            assert!(out.len() < 500, "runaway read");
            let next = ReadOpts {
                cursor: Some(c),
                ..o.clone()
            };
            out.push(read(rows, blocks, &next));
        }
        out
    }

    #[test]
    fn autocomplete_password_is_a_password_field() {
        let mut pw = row(5, "textbox", "Password");
        pw.state.input_type = Some("text".into());
        pw.state.autocomplete = Some("current-password".into());
        pw.state.value = Some(FieldValue::Text("Sekr3tPass".into()));
        let blocks = vec![
            block(
                1,
                0,
                BlockKind::Paragraph,
                vec![
                    text("Password "),
                    Span {
                        text: "Sekr3tPass".into(),
                        elem: Some(pw.elem()),
                    },
                ],
            ),
            para(2, "you typed Sekr3tPass"),
        ];
        let out = read(&[pw], &blocks, &opts(0));
        assert!(!out.markdown.contains("Sekr3tPass"), "{}", out.markdown);
        assert!(
            out.markdown.contains("[input \"Password\"](#5)"),
            "{}",
            out.markdown
        );
        assert!(out.markdown.contains("you typed ***"), "{}", out.markdown);
    }

    #[test]
    fn split_never_cuts_a_password_in_two() {
        let mut pw = row(5, "textbox", "Password");
        pw.state.input_type = Some("password".into());
        pw.state.value = Some(FieldValue::Text("correct horse battery staple".into()));
        let body = format!(
            "{} correct horse battery staple {}",
            "lorem ipsum dolor sit amet ".repeat(3),
            "and more words ".repeat(10)
        );
        let blocks = vec![para(1, &body)];
        let all: String = read_all(&[pw], &blocks, opts(25))
            .iter()
            .map(|o| o.markdown.clone())
            .collect();
        for w in ["correct horse", "horse battery", "battery staple"] {
            assert!(!all.contains(w), "{w} leaked: {all}");
        }
        assert!(all.contains("***"));
    }

    #[test]
    fn opaque_frames_are_never_read() {
        let mut host = row(3, "iframe", "Payments widget");
        host.flags.opaque = true;
        host.child_frame = Some(4);
        let blocks = vec![
            para(1, "Checkout page text here for you"),
            block(
                2,
                4,
                BlockKind::Paragraph,
                vec![text("SECRET cross origin")],
            ),
        ];
        let out = read(&[host], &blocks, &opts(0));
        assert!(out.markdown.contains("Checkout page"), "{}", out.markdown);
        assert!(!out.markdown.contains("SECRET"), "{}", out.markdown);
    }

    #[test]
    fn iframe_main_does_not_replace_a_mainless_top_page() {
        let mut host = row(9, "iframe", "Demo");
        host.child_frame = Some(2);
        let long = "flour water salt yeast and time make a good loaf of bread at home with care and patience every day";
        let mut demo = block(
            4,
            2,
            BlockKind::Paragraph,
            vec![text("Demo widget loaded.")],
        );
        demo.landmark = Some("main".into());
        let blocks = vec![
            block(1, 0, BlockKind::Heading, vec![text("How to bake bread")]),
            para(2, long),
            para(3, long),
            demo,
        ];
        let out = read(&[host], &blocks, &opts(0));
        assert!(
            out.markdown.contains("How to bake bread"),
            "{}",
            out.markdown
        );
        assert!(out.markdown.contains("flour water"), "{}", out.markdown);
        // a top-frame main still wins
        let mut b = blocks.clone();
        b[2].landmark = Some("main".into());
        let out = read(&[], &b, &opts(0));
        assert_eq!(out.markdown, long);
    }

    #[test]
    fn heading_before_a_big_block_carries_content() {
        let big = "word ".repeat(200);
        let blocks = vec![
            block(1, 0, BlockKind::Heading, vec![text("Top")]),
            para(2, &big),
            para(3, "short tail"),
        ];
        for chunk_tokens in [10usize, 12, 60] {
            for o in read_all(&[], &blocks, opts(chunk_tokens)) {
                let body: Vec<&str> = o
                    .markdown
                    .lines()
                    .take_while(|l| !l.starts_with("remaining:"))
                    .filter(|l| !l.trim().is_empty() && !l.starts_with("chunk "))
                    .collect();
                assert!(
                    body.iter().any(|l| !l.starts_with('#')),
                    "{chunk_tokens}: {body:?}"
                );
            }
        }
        // default chunk size, heading before a block larger than a chunk
        let listing = "let x = 1;
"
        .repeat(2000);
        let blocks = vec![
            block(1, 0, BlockKind::Heading, vec![text("Listing")]),
            block(2, 0, BlockKind::Code, vec![text(&listing)]),
        ];
        let first = read(&[], &blocks, &opts(0));
        assert!(
            first.markdown.contains(
                "## Listing

```
let x"
            ),
            "{}",
            &first.markdown[..80]
        );
    }

    #[test]
    fn forged_cursor_used_does_not_overflow() {
        let blocks: Vec<TextBlock> = (0..5).map(|i| para(i, &"word ".repeat(50))).collect();
        let first = read(
            &[],
            &blocks,
            &ReadOpts {
                max_tokens: Some(100_000),
                ..opts(60)
            },
        );
        let c = first.cursor.expect("more to read").0;
        let hash = c.rsplit(':').next().unwrap_or_default().to_string();
        let forged = format!("v1::1+{}:{hash}", usize::MAX);
        let out = read(
            &[],
            &blocks,
            &ReadOpts {
                cursor: Some(Cursor(forged)),
                ..opts(60)
            },
        );
        assert!(out.markdown.contains("word"));
    }

    fn link(t: &str, e: ElemRef) -> Span {
        Span {
            text: t.into(),
            elem: Some(e),
        }
    }

    #[test]
    fn span_into_a_challenge_frame_shows_only_the_host() {
        // audit round 2 item 5: a span linking a row inside the captcha frame
        let mut host = row(3, "iframe", "reCAPTCHA");
        host.flags.challenge = true;
        host.child_frame = Some(5);
        let mut inner = row(1, "checkbox", "I'm not a robot, select all traffic lights");
        inner.frame = 5;
        let inner_ref = inner.elem();
        let rows = vec![host, inner];
        let blocks = vec![
            block(
                1,
                0,
                BlockKind::Paragraph,
                vec![
                    text("Please confirm: "),
                    link("", inner_ref),
                    text(" before you continue with the signup form below."),
                ],
            ),
            block(
                2,
                0,
                BlockKind::Heading,
                vec![link("Select all traffic lights", inner_ref)],
            ),
            para(
                3,
                "Welcome to the signup page, fill in the form below to continue.",
            ),
        ];
        for o in [opts(0), opts(8)] {
            for out in read_all(&rows, &blocks, o) {
                let md = &out.markdown;
                assert!(!md.contains("robot"), "{md}");
                assert!(!md.to_lowercase().contains("traffic"), "{md}");
                assert!(!md.contains("#f5.1"), "{md}");
                for e in &out.remaining {
                    assert!(!e.heading.to_lowercase().contains("traffic"), "{e:?}");
                }
            }
        }
        let md = read(&rows, &blocks, &opts(0)).markdown;
        assert!(md.contains("[iframe \"reCAPTCHA\"](#3)"), "{md}");

        // an OPAQUE (non-challenge) host stands in for the rows of its frames, nested ones
        // included
        let mut video = row(4, "iframe", "Video player");
        video.flags.opaque = true;
        video.child_frame = Some(7);
        let mut play = row(1, "button", "Play secret preview");
        play.frame = 7;
        let mut nested = row(9, "iframe", "nested");
        nested.frame = 7;
        nested.child_frame = Some(8);
        let mut deep = row(2, "button", "Deep inner");
        deep.frame = 8;
        let rows = vec![video, play.clone(), nested, deep.clone()];
        let blocks = vec![block(
            1,
            0,
            BlockKind::Paragraph,
            vec![
                text("Watch "),
                link("Play secret preview", play.elem()),
                text(" or "),
                link("Deep inner", deep.elem()),
                text(" now."),
            ],
        )];
        let md = read(&rows, &blocks, &opts(0)).markdown;
        assert!(!md.contains("secret") && !md.contains("Deep"), "{md}");
        assert_eq!(
            md,
            "Watch [iframe \"Video player\"](#4) or [iframe \"Video player\"](#4) now."
        );
    }

    #[test]
    fn escaped_password_values_are_scrubbed() {
        // audit round 2 item 6: values holding | [ ] " survive table, link and label escapes
        let mut rows = Vec::new();
        for (id, v) in [(1, "ab|cd"), (2, "q[w]e"), (3, "ab\"cd"), (4, "x[y|z\"w]")] {
            let mut pw = row(id, "textbox", "Password");
            pw.state.input_type = Some("password".into());
            pw.state.value = Some(FieldValue::Text(v.into()));
            rows.push(pw);
        }
        let l = row(9, "link", "Docs");
        let b = row(8, "button", "Go");
        let mut cell = block(1, 0, BlockKind::TableRow, vec![text("ab|cd"), text("x")]);
        cell.table = Some(0);
        let mut cell2 = block(
            2,
            0,
            BlockKind::TableRow,
            vec![link("x[y|z\"w]", b.elem()), text("y")],
        );
        cell2.table = Some(0);
        let blocks = vec![
            cell,
            cell2,
            block(
                3,
                0,
                BlockKind::Paragraph,
                vec![
                    text("see "),
                    link("q[w]e", l.elem()),
                    text(" and "),
                    link("ab\"cd", b.elem()),
                    text(" plus some more words here to keep it"),
                ],
            ),
            block(4, 0, BlockKind::Heading, vec![link("q[w]e", l.elem())]),
            para(
                5,
                "a closing paragraph with enough words to be read as content",
            ),
        ];
        rows.push(l);
        rows.push(b);
        let leaks = [
            "ab|cd", "ab\\|cd", "q[w]e", "q(w)e", "ab\"cd", "ab'cd", "y\\|z", "y|z",
        ];
        for o in [opts(0), opts(6)] {
            for out in read_all(&rows, &blocks, o) {
                let md = &out.markdown;
                for leak in leaks {
                    assert!(!md.contains(leak), "{leak} in {md}");
                }
                for e in &out.remaining {
                    assert!(!e.heading.contains("q(w)e") && !e.heading.contains("q[w]e"));
                }
            }
        }
        let md = read(&rows, &blocks, &opts(0)).markdown;
        assert!(md.contains("| *** | x |"), "{md}");
        assert!(
            md.contains("[***](#9)") && md.contains("[button \"***\"](#8)"),
            "{md}"
        );
    }

    #[test]
    fn split_code_keeps_indentation() {
        // audit round 2 item 7
        let src = "def f():\n    if x:\n        return 1\n    return 2\n".repeat(4);
        let blocks = vec![block(1, 0, BlockKind::Code, vec![text(&src)])];
        let whole = read(&[], &blocks, &opts(0)).markdown;
        assert_eq!(whole, format!("```\n{}\n```", src.trim_end()));
        let chunks = read_all(&[], &blocks, opts(12));
        assert!(chunks.len() > 2, "{}", chunks.len());
        let mut pieces: Vec<String> = Vec::new();
        for c in &chunks {
            let body = c.markdown.split("\n\nremaining:").next().unwrap_or("");
            let body = match body.strip_prefix("chunk ") {
                Some(_) => body.split_once('\n').map(|x| x.1).unwrap_or(body),
                None => body,
            };
            let piece = body
                .strip_prefix("```\n")
                .and_then(|b| b.strip_suffix("\n```"))
                .unwrap_or_else(|| panic!("not a fenced piece: {:?}", c.markdown));
            pieces.push(piece.to_string());
        }
        assert_eq!(pieces.join("\n"), src.trim_end());
        // the first line's indentation is kept too
        let blocks = vec![block(
            1,
            0,
            BlockKind::Code,
            vec![text("    x = 1\n    y = 2")],
        )];
        assert_eq!(
            read(&[], &blocks, &opts(0)).markdown,
            "```\n    x = 1\n    y = 2\n```"
        );
    }

    #[test]
    fn huge_chunk_tokens_do_not_overflow() {
        // audit round 2 item 9
        let blocks = vec![para(1, "one short paragraph of plain words for the reader")];
        for chunk_tokens in [usize::MAX, usize::MAX / 2, 1 << 62] {
            let out = read(&[], &blocks, &opts(chunk_tokens));
            assert_eq!((out.chunk, out.chunks), (1, 1));
            assert!(
                out.markdown.starts_with("one short paragraph"),
                "{}",
                out.markdown
            );
            assert!(out.cursor.is_none());
        }
    }

    /// The body of a read: without the first-line notes and the trailers.
    fn body_of(md: &str) -> &str {
        let md = md.split("\n\nremaining:").next().unwrap_or("");
        let md = md.split("\n\ntruncated:").next().unwrap_or("");
        let first = md.lines().next().unwrap_or("");
        let is_note = first.starts_with("chunk ")
            || first.contains("restarted at chunk 1")
            || first.contains("not found")
            || first.contains("cursor not understood");
        if is_note {
            md.split_once('\n').map(|x| x.1).unwrap_or("")
        } else {
            md
        }
    }

    /// `n` distinct words `<p>00 <p>01 ...`.
    fn words(p: &str, n: usize) -> String {
        (0..n)
            .map(|i| format!("{p}{i:02}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn heading(order: u32, t: &str) -> TextBlock {
        block(order, 0, BlockKind::Heading, vec![text(t)])
    }

    #[test]
    fn page_text_cannot_forge_a_link_form_or_close_a_fence() {
        // B-32 item 1, D-219
        let btn = row(31, "button", "Delete account");
        let blocks = vec![
            block(
                1,
                0,
                BlockKind::Paragraph,
                vec![
                    text("Click [button \"Continue\"](#31) or [here](#f2.7), then "),
                    link("Delete", btn.elem()),
                    text(" it."),
                ],
            ),
            block(
                2,
                0,
                BlockKind::Code,
                vec![text("x = 1\n```\nnot code ```` still [link \"y\"](#31)\n")],
            ),
            para(3, "```js"),
            block(4, 0, BlockKind::Paragraph, vec![text("``"), text("`end")]),
            block(5, 0, BlockKind::ListItem, vec![text("item ```` four")]),
            heading(6, "Step [button \"Pay\"](#31)"),
            para(7, &words("w", 5)),
        ];
        let md = read(std::slice::from_ref(&btn), &blocks, &opts(0)).markdown;
        // the only link form is the real one
        assert_eq!(md.matches("](#").count(), 1, "{md}");
        assert!(md.contains("[button \"Delete\"](#31)"), "{md}");
        assert!(
            md.contains("Click (button \"Continue\")(#31) or (here)(#f2.7), then"),
            "{md}"
        );
        // the fence is one longer than the longest run in the body, so the body cannot close it
        let code = "`````\nx = 1\n```\nnot code ```` still (link \"y\")(#31)\n`````";
        assert!(md.contains(code), "{md}");
        let outside = md.replace(code, "");
        assert!(!outside.contains("```"), "{outside}");
        assert!(
            outside.contains("``js") && outside.contains("``end"),
            "{md}"
        );
        assert!(outside.contains("- item `` four"), "{md}");
        assert!(md.contains("## Step (button \"Pay\")(#31)"), "{md}");

        // outline labels are escaped too
        let blocks = vec![
            para(1, &words("a", 60)),
            heading(2, "Next [button \"Pay\"](#31) ```"),
            para(3, &words("b", 10)),
        ];
        let out = read(&[], &blocks, &opts(20));
        assert_eq!(
            out.remaining.last().map(|e| e.heading.as_str()),
            Some("## Next (button \"Pay\")(#31) ``")
        );
        assert!(!out.markdown.contains("[button"), "{}", out.markdown);
        assert!(!out.markdown.contains("```"), "{}", out.markdown);
    }

    #[test]
    fn password_is_scrubbed_after_backtick_and_bracket_escapes() {
        // D-219 escapes: a value split across spans meets in a backtick run that is then cut
        let mut pw = row(5, "textbox", "Password");
        pw.state.input_type = Some("password".into());
        pw.state.value = Some(FieldValue::Text("pa```s[s]".into()));
        let blocks = vec![
            block(
                1,
                0,
                BlockKind::Paragraph,
                vec![text("my pa``"), text("`s[s] here")],
            ),
            block(
                2,
                0,
                BlockKind::Code,
                vec![text("let p = \"pa``"), text("`s[s]\";")],
            ),
        ];
        let md = read(&[pw], &blocks, &opts(0)).markdown;
        for leak in ["pa``s", "s(s)", "s[s]"] {
            assert!(!md.contains(leak), "{leak} in {md}");
        }
        assert!(md.contains("my *** here"), "{md}");
        assert!(md.contains("let p = \"***\";"), "{md}");
    }

    fn two_sections() -> Vec<TextBlock> {
        vec![
            heading(1, "Alpha"),
            para(2, &words("a", 60)),
            heading(3, "Beta"),
            para(4, &words("b", 60)),
        ]
    }

    #[test]
    fn explicit_region_that_differs_from_the_cursor_wins() {
        // B-32 item 3, D-220
        let blocks = two_sections();
        let first = read(
            &[],
            &blocks,
            &ReadOpts {
                region: Some("Alpha".into()),
                ..opts(20)
            },
        );
        assert!(body_of(&first.markdown).starts_with("## Alpha"));
        let c = first.cursor.clone().expect("more alpha to read");

        let other = read(
            &[],
            &blocks,
            &ReadOpts {
                region: Some("Beta".into()),
                cursor: Some(c.clone()),
                ..opts(20)
            },
        );
        assert_eq!(other.chunk, 1, "{}", other.markdown);
        assert!(
            other
                .markdown
                .contains("region \"Beta\" is not the cursor's region, restarted at chunk 1"),
            "{}",
            other.markdown
        );
        assert!(
            body_of(&other.markdown).starts_with("## Beta\n\nb00"),
            "{}",
            other.markdown
        );
        assert!(!other.markdown.contains("a5"), "{}", other.markdown);
        // its cursor continues Beta
        let next = read(
            &[],
            &blocks,
            &ReadOpts {
                cursor: other.cursor.clone(),
                ..opts(20)
            },
        );
        assert!(!body_of(&next.markdown).contains('a'), "{}", next.markdown);

        // the same region (any case or spacing) or none continues the cursor's read
        for region in [Some("Alpha"), Some(" alpha "), None] {
            let cont = read(
                &[],
                &blocks,
                &ReadOpts {
                    region: region.map(str::to_string),
                    cursor: Some(c.clone()),
                    ..opts(20)
                },
            );
            assert_eq!(cont.chunk, 2, "{region:?}: {}", cont.markdown);
            assert!(!cont.markdown.contains("restarted"), "{}", cont.markdown);
            let body = body_of(&cont.markdown);
            assert!(body.starts_with('a') && !body.contains("a00"), "{body}");
        }
    }

    fn body_words(md: &str) -> usize {
        body_of(md)
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(word_count)
            .sum()
    }

    #[test]
    fn remaining_outline_counts_only_words_not_yet_returned() {
        // B-32 item 4, D-211
        let blocks = vec![
            para(1, &words("t", 7)),
            heading(2, "One"),
            para(3, &words("o", 50)),
            para(4, &words("p", 30)),
            heading(5, "Two"),
            para(6, &words("w", 12)),
        ];
        let total = 7 + 50 + 30 + 12;
        let outs = read_all(&[], &blocks, opts(25));
        assert!(outs.len() > 3, "{}", outs.len());
        // chunk 1 holds (top) only: the heading travels with its section
        assert_eq!(body_of(&outs[0].markdown), words("t", 7));
        let entries = |o: &ReadOutput| -> Vec<(String, usize)> {
            o.remaining
                .iter()
                .map(|e| (e.heading.clone(), e.words))
                .collect()
        };
        assert_eq!(
            entries(&outs[0]),
            [("## One".to_string(), 80), ("## Two".to_string(), 12)]
        );
        assert!(
            outs[0]
                .markdown
                .contains("remaining:\n- ## One (80 words)\n- ## Two (12 words)"),
            "{}",
            outs[0].markdown
        );
        // chunk 2 splits the 50-word paragraph: One counts only the words not returned
        let got = body_words(&outs[1].markdown);
        assert!(got > 0 && got < 50, "{}", outs[1].markdown);
        assert_eq!(
            entries(&outs[1]),
            [("## One".to_string(), 80 - got), ("## Two".to_string(), 12)]
        );
        // every chunk: returned so far + remaining = all words
        let mut returned = 0;
        for o in &outs {
            returned += body_words(&o.markdown);
            let left: usize = o.remaining.iter().map(|e| e.words).sum();
            assert_eq!(returned + left, total, "{}", o.markdown);
        }
        assert!(outs.last().is_some_and(|o| o.remaining.is_empty()));
        // (top) is listed while text before the first heading is unread
        let out = read(&[], &blocks[..3], &opts(5));
        assert_eq!(
            out.remaining.first().map(|e| e.heading.as_str()),
            Some("(top)")
        );
    }

    #[test]
    fn headings_only_fill_stays_within_max_tokens() {
        // B-32 item 6, D-222: four headings use 34 chars; 10 tokens leave 4 chars, less than
        // the quarter-chunk floor (10 chars)
        let blocks = vec![
            heading(1, "Aaaa"),
            heading(2, "Bbbb"),
            heading(3, "Cccc"),
            heading(4, "Dddd"),
            para(5, &words("w", 100)),
        ];
        let out = read(
            &[],
            &blocks,
            &ReadOpts {
                max_tokens: Some(10),
                ..opts(0)
            },
        );
        let body = body_of(&out.markdown);
        assert!(estimate(body) <= 10, "{} tokens: {body:?}", estimate(body));
        assert!(body.contains("w00"), "{body:?}");
        assert!(out.cursor.is_none());
        assert!(
            out.markdown.ends_with("truncated: max_tokens 10 reached"),
            "{}",
            out.markdown
        );
        // without max_tokens the floor still applies (a chunk is never headings only)
        let out = read(&[], &blocks, &opts(10));
        assert!(body_of(&out.markdown).contains("w00"), "{}", out.markdown);
    }

    #[test]
    fn page_change_keeps_the_max_tokens_budget() {
        // B-32 item 7, D-220
        let page =
            |p: &str| -> Vec<TextBlock> { (0..5u32).map(|i| para(i, &words(p, 50))).collect() };
        let o = |cursor: Option<Cursor>| ReadOpts {
            max_tokens: Some(60),
            cursor,
            ..opts(90)
        };
        let first = read(&[], &page("a"), &o(None));
        let used = estimate(body_of(&first.markdown));
        assert!(used > 40 && used < 60, "{used}");
        let c = first.cursor.clone().expect("more to read");

        let out = read(&[], &page("b"), &o(Some(c)));
        assert!(
            out.markdown.contains("page changed since the cursor"),
            "{}",
            out.markdown
        );
        let body = body_of(&out.markdown);
        assert!(body.starts_with("b00"), "{body:?}");
        assert!(
            estimate(body) <= 60 - used,
            "{} > {}: {body:?}",
            estimate(body),
            60 - used
        );
        assert!(out.cursor.is_none());
        assert!(
            out.markdown.ends_with("truncated: max_tokens 60 reached"),
            "{}",
            out.markdown
        );
    }
}
