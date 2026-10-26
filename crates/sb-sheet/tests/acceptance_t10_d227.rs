//! T-10 acceptance, round 6 (tester): decision D-227 as clarified by D-228. Black-box through
//! the public API only, with fixtures built here, independently of the coder's unit tests.
//!
//! - D-227 blocking: a first word longer than this read's budget (min of chunk_tokens and the
//!   max_tokens remainder) is cut by characters, so a read under max_tokens never comes back
//!   empty while text is left, and the cursor chain reads a gap-free, repeat-free prefix.
//! - D-227 should-fix 1: the `remaining:` outline lists at most 20 entries, each label at most
//!   80 chars, then one `+N more sections, W words` entry; a page heading cannot pose as that
//!   entry, and a password value never reaches `remaining:`.
//!
//! Item ids in test names match the rows of bench/results/accept-T-10.md. No assertion message
//! prints read output that may hold the canary.

use sb_sheet::tokens::estimate;
use sb_sheet::{
    read, BlockKind, ElemRef, FieldValue, IndexRow, ReadOpts, ReadOutput, Span, State, TextBlock,
};

// ---------------------------------------------------------------- builders

fn row(id: u32, role: &str, name: &str) -> IndexRow {
    IndexRow {
        id,
        frame: 0,
        role: role.to_string(),
        name: name.to_string(),
        nearby: None,
        heading: None,
        state: State::default(),
        flags: Default::default(),
        form: None,
        landmark: Some("main".into()),
        child_frame: None,
        bbox: None,
    }
}

fn password(id: u32, value: &str) -> IndexRow {
    let mut r = row(id, "textbox", "Password");
    r.state.input_type = Some("password".into());
    r.state.value = Some(FieldValue::Text(value.into()));
    r
}

fn txt(t: &str) -> Span {
    Span {
        text: t.into(),
        elem: None,
    }
}

fn link(t: &str, id: u32) -> Span {
    Span {
        text: t.into(),
        elem: Some(ElemRef::main(id)),
    }
}

fn blk(order: u32, kind: BlockKind, level: Option<u8>, spans: Vec<Span>) -> TextBlock {
    TextBlock {
        order,
        frame: 0,
        kind,
        level,
        landmark: Some("main".into()),
        heading: None,
        table: None,
        header: false,
        spans,
    }
}

fn para(order: u32, t: &str) -> TextBlock {
    blk(order, BlockKind::Paragraph, None, vec![txt(t)])
}

fn h(order: u32, level: u8, t: &str) -> TextBlock {
    blk(order, BlockKind::Heading, Some(level), vec![txt(t)])
}

/// `n` distinct words `p00 p01 ...`.
fn words(p: &str, n: usize) -> String {
    (0..n)
        .map(|i| format!("{p}{i:02}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn opts(chunk_tokens: usize) -> ReadOpts {
    ReadOpts {
        region: None,
        cursor: None,
        max_tokens: None,
        chunk_tokens,
    }
}

fn with_max(chunk_tokens: usize, m: usize) -> ReadOpts {
    ReadOpts {
        max_tokens: Some(m),
        ..opts(chunk_tokens)
    }
}

/// Follow the cursor chain from `o` to the end, failing past `bound` reads.
fn read_chain(
    rows: &[IndexRow],
    blocks: &[TextBlock],
    o: &ReadOpts,
    bound: usize,
) -> Vec<ReadOutput> {
    let mut outs = vec![read(rows, blocks, o)];
    while let Some(c) = outs.last().and_then(|x| x.cursor.clone()) {
        assert!(
            outs.len() < bound,
            "cursor chain did not end within {bound} reads (max_tokens {:?}, chunk {})",
            o.max_tokens,
            o.chunk_tokens
        );
        outs.push(read(
            rows,
            blocks,
            &ReadOpts {
                cursor: Some(c),
                ..o.clone()
            },
        ));
    }
    outs
}

/// The body of a read: without the first-line notes and the trailers the reader writes.
fn body(md: &str) -> &str {
    let mut s = md;
    if let Some(first) = s.lines().next() {
        let chunk_line = first
            .strip_prefix("chunk ")
            .and_then(|r| r.chars().next())
            .is_some_and(|c| c.is_ascii_digit());
        let note = chunk_line
            || first.contains("restarted at chunk 1")
            || first.starts_with("cursor not understood")
            || first.contains("not found, reading the page");
        if note {
            s = s.split_once('\n').map_or("", |(_, r)| r);
        }
    }
    let end = ["\n\nremaining:", "\n\ntruncated:"]
        .iter()
        .filter_map(|t| s.find(t))
        .min()
        .unwrap_or(s.len());
    let s = &s[..end];
    if s.starts_with("remaining:") || s.starts_with("truncated:") {
        ""
    } else {
        s
    }
}

/// The lines of the `remaining:` block (after its header, before `truncated:`), or none.
fn remaining_lines(md: &str) -> Vec<&str> {
    let Some(i) = md.find("\n\nremaining:") else {
        return Vec::new();
    };
    let rest = &md[i + "\n\nremaining:".len()..];
    let rest = rest.split("\n\ntruncated:").next().unwrap_or("");
    rest.lines().filter(|l| !l.is_empty()).collect()
}

/// Only the alphanumeric chars: a stream that ignores markup, prefixes and where pieces cut.
fn alnum(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).collect()
}

fn squeeze(s: &str) -> String {
    s.split_whitespace().collect()
}

/// A canary that never appears as a literal in the repo: SB_VAULT_CANARY when set, else
/// built at runtime from the process id and the clock.
fn canary() -> String {
    std::env::var("SB_VAULT_CANARY").unwrap_or_else(|_| {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("cnry{}x{}", std::process::id(), n)
    })
}

/// A 6000-char token with no whitespace (a long URL, base64, minified text): a hex counter, so
/// any cut point is visible in the stream.
fn long_token() -> String {
    let mut s = String::new();
    let mut i = 0u32;
    while s.chars().count() < 6000 {
        s.push_str(&format!("{i:x}g"));
        i += 1;
    }
    s.chars().take(6000).collect()
}

/// A 6000-char multibyte token (2-, 3- and 4-byte... all alphanumeric chars), so a char cut on a
/// byte index would panic or garble.
fn long_multibyte_token() -> String {
    "é漢ßΩ".chars().cycle().take(6000).collect()
}

/// Pages whose text holds a 6000-char first word, and the least max_tokens whose budget holds
/// the unit's overhead plus one char.
fn long_word_pages() -> Vec<(&'static str, Vec<TextBlock>, usize)> {
    let tok = long_token();
    let mb = long_multibyte_token();
    vec![
        (
            "first word",
            vec![para(1, &format!("{tok} tail words here"))],
            1,
        ),
        (
            "after intro",
            vec![
                para(1, "intro words here"),
                para(2, &format!("{tok} tail words here")),
            ],
            1,
        ),
        (
            "after heading",
            vec![
                h(1, 2, "Blob"),
                para(2, &format!("{tok} {}", words("y", 10))),
            ],
            1,
        ),
        (
            "list item",
            vec![blk(
                1,
                BlockKind::ListItem,
                Some(2),
                vec![txt(&format!("{tok} tail"))],
            )],
            2,
        ),
        (
            "quote",
            vec![blk(
                1,
                BlockKind::Quote,
                None,
                vec![txt(&format!("{tok} tail"))],
            )],
            1,
        ),
        (
            "code line",
            vec![blk(
                1,
                BlockKind::Code,
                None,
                vec![txt(&format!("let x = \"{tok}\";\nlet y = 2;"))],
            )],
            3,
        ),
        (
            "multibyte",
            vec![h(1, 3, "Ünï"), para(2, &format!("{mb} {}", words("z", 5)))],
            2,
        ),
    ]
}

// ---------------------------------------------------------------- D-227 blocking

/// D-227 (blocking), D-228: under max_tokens a first word longer than the read's budget is cut
/// by characters. For every page and max_tokens from the least that holds the overhead up to
/// 1499 (a 6000-char word needs 1500), at chunk_tokens 0 (default 4000, larger than the word)
/// and 50 (max_tokens up to 333): the first read is not empty, every read of the chain is not empty, each read stays
/// within what max_tokens leaves, the sequence spends the whole budget (to within a token),
/// and what was read is a gap-free, repeat-free prefix of the page.
#[test]
fn d227_long_first_word_under_max_tokens_reads_non_empty_and_chain_is_a_gap_free_prefix() {
    let mut cut_reads = 0usize;
    for (name, page, least) in long_word_pages() {
        let whole = read(&[], &page, &opts(100_000));
        let full = alnum(body(&whole.markdown));
        let total = estimate(body(&whole.markdown));
        let page_tokens: Vec<&str> = body(&whole.markdown).split_whitespace().collect();
        for chunk in [0usize, 50] {
            // chunk 50 re-plans many small chunks per read: a shorter sweep keeps it fast
            let sweep: Vec<usize> = if chunk == 0 {
                vec![least, least + 1, 7, 100, 500, 1000, 1499]
            } else {
                vec![least, least + 1, 7, 100, 333]
            };
            for m in sweep {
                let outs = read_chain(&[], &page, &with_max(chunk, m), m + 3);
                let first = body(&outs[0].markdown);
                assert!(
                    !first.trim().is_empty(),
                    "{name} chunk {chunk} max {m}: empty first read: {:?}",
                    outs[0].markdown.get(..200)
                );
                let mut spent = 0usize;
                for (i, o) in outs.iter().enumerate() {
                    let b = body(&o.markdown);
                    assert!(
                        !b.trim().is_empty(),
                        "{name} chunk {chunk} max {m} read {i} of {}: empty read while text is left",
                        outs.len()
                    );
                    let got = estimate(b);
                    assert!(
                        got <= m - spent,
                        "{name} chunk {chunk} max {m} read {i}: {got} > {}",
                        m - spent
                    );
                    spent += got;
                    // a read whose last token is not a whole page token took a char cut
                    if b.split_whitespace()
                        .last()
                        .is_some_and(|t| !page_tokens.contains(&t))
                    {
                        cut_reads += 1;
                    }
                }
                // the budget is spent (to within a token), unless the page is smaller
                if total > m {
                    assert!(
                        spent + 1 >= m,
                        "{name} chunk {chunk} max {m}: spent only {spent}"
                    );
                    assert!(
                        outs.last().is_some_and(|x| x
                            .markdown
                            .ends_with(&format!("truncated: max_tokens {m} reached"))),
                        "{name} chunk {chunk} max {m}: the sequence does not end truncated"
                    );
                }
                // the chain is a prefix of the page: no gap, no repeat
                let stream: String = outs.iter().map(|x| alnum(body(&x.markdown))).collect();
                assert!(
                    full.starts_with(&stream),
                    "{name} chunk {chunk} max {m}: gap or repeat in the cursor chain"
                );
            }
        }
        // a large max_tokens with a chunk smaller than the word reads the whole page across cuts
        let outs = read_chain(&[], &page, &with_max(400, 100_000), 1000);
        let stream: String = outs.iter().map(|x| alnum(body(&x.markdown))).collect();
        assert_eq!(stream, full, "{name}: chain over char cuts is not the page");
        assert!(outs.len() > 2, "{name}: {} reads", outs.len());
    }
    assert!(
        cut_reads > 0,
        "no read cut the long word: D-227 not reached"
    );
}

/// D-227 at the exact boundary: at the default chunk, a 6000-char word fits max_tokens 1500 whole
/// (one read, word intact, the tail left), and at 1499 it is cut to the 5996 chars that fit.
#[test]
fn d227_the_word_is_whole_at_its_size_and_cut_one_token_below() {
    let tok = long_token();
    let page = vec![para(1, &format!("{tok} tail words here"))];
    let whole = read(&[], &page, &with_max(0, 1500));
    let b = body(&whole.markdown);
    assert_eq!(b.trim_end(), tok, "max 1500: {:?}", b.get(..80));
    let cut = read(&[], &page, &with_max(0, 1499));
    let b = body(&cut.markdown);
    let want: String = tok.chars().take(1499 * 4).collect();
    assert_eq!(b, want, "max 1499: {} chars", b.chars().count());
    assert!(cut.markdown.ends_with("truncated: max_tokens 1499 reached"));
    // the same through a cursor (the review-round-5 case): chunk 1600 holds the word (6400 chars),
    // read 1 takes a 1060-word intro and ends at the chunk, and read 2 starts with 410 of
    // max_tokens 2000 left, smaller than the word: read 2 is a char cut, never empty
    let intro: String = (0..1060)
        .map(|i| format!("w{i:04}"))
        .collect::<Vec<_>>()
        .join(" ");
    let page = vec![para(1, &intro), para(2, &format!("{tok} tail"))];
    let outs = read_chain(&[], &page, &with_max(1600, 2000), 10);
    assert!(outs.len() >= 2, "{} reads", outs.len());
    let first = body(&outs[0].markdown);
    assert_eq!(first, intro, "read 1 is not the intro");
    let second = body(&outs[1].markdown);
    assert!(
        !second.trim().is_empty(),
        "the read after the intro came back empty"
    );
    assert!(tok.starts_with(second), "{:?}", second.get(..40));
    assert_eq!(estimate(first) + estimate(second), 2000);
    assert!(outs[1]
        .markdown
        .ends_with("truncated: max_tokens 2000 reached"));
}

// ---------------------------------------------------------------- D-227 should-fix 1

/// `n` sections `## S000 ...` with five words each, after a `(top)` paragraph of 40 words.
fn sections(n: u32, label: impl Fn(u32) -> String) -> Vec<TextBlock> {
    let mut blocks = vec![para(1, &words("top", 40))];
    for i in 0..n {
        blocks.push(h(2 * i + 2, 2, &label(i)));
        blocks.push(para(2 * i + 3, &words(&format!("s{i:04}w"), 5)));
    }
    blocks
}

/// The words of a body that count toward `remaining:`: every line but headings.
fn body_words(b: &str) -> usize {
    b.lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.split_whitespace().count())
        .sum()
}

/// D-227 should-fix 1: at most 20 entries then one `+N more sections, W words` entry; labels of
/// at most 80 chars (80 kept whole, 81 cut), cut on a char boundary with `...`; W counts every
/// word left in the folded sections, so returned plus listed is the page on every read; the
/// markdown lines are exactly the entries; the outline stays bounded however many sections.
#[test]
fn d227_outline_cap_entries_labels_and_more_entry() {
    let exact = format!("E{}", "x".repeat(76)); // "## " + 77 = 80
    let over = format!("O{}", "x".repeat(77)); // "## " + 78 = 81
    let label = |i: u32| -> String {
        match i {
            0 => exact.clone(),
            1 => over.clone(),
            2 => "é漢ßΩ".chars().cycle().take(300).collect(),
            3 => format!("Deep {}", "y".repeat(200)),
            _ => format!("S{i:04}"),
        }
    };
    let page = sections(60, label);
    let total_words = 40 + 60 * 5;

    // first read stops inside (top): (top) plus 19 sections, then +41 more
    let o = read(&[], &page, &with_max(0, 5));
    let r = &o.remaining;
    assert_eq!(r.len(), 21, "{:?}", r);
    assert_eq!(r[0].heading, "(top)");
    assert_eq!(r[1].heading, format!("## {exact}"));
    assert_eq!(r[1].heading.chars().count(), 80);
    assert_eq!(r[2].heading.chars().count(), 80);
    assert!(r[2].heading.ends_with("..."), "{:?}", r[2].heading);
    assert!(format!("## {over}").starts_with(r[2].heading.trim_end_matches("...")));
    for e in &r[..20] {
        assert!(e.heading.chars().count() <= 80, "{:?}", e.heading);
        assert!(
            e.heading.starts_with("## ") || e.heading == "(top)",
            "{:?}",
            e.heading
        );
    }
    assert!(r[3].heading.ends_with("...") && r[3].heading.starts_with("## é漢ßΩ"));
    assert!(r[4].heading.ends_with("...") && r[4].heading.starts_with("## Deep yyy"));
    assert_eq!(r[19].heading, "## S0018");
    assert_eq!(r[20].heading, "+41 more sections");
    assert_eq!(r[20].words, 41 * 5);
    let listed: usize = r.iter().map(|e| e.words).sum();
    assert_eq!(listed + body_words(body(&o.markdown)), total_words);
    // the markdown lists exactly the entries, the last in its own form
    let lines = remaining_lines(&o.markdown);
    assert_eq!(lines.len(), 21);
    for (l, e) in lines.iter().zip(&r[..20]) {
        assert_eq!(*l, format!("- {} ({} words)", e.heading, e.words));
    }
    assert_eq!(lines[20], "- +41 more sections, 205 words");
    assert!(o
        .markdown
        .contains("\n- +41 more sections, 205 words\n\ntruncated: max_tokens 5 reached"));

    // every read of a chain keeps the cap and the word sum
    let outs = read_chain(&[], &page, &opts(40), 200);
    let mut returned = 0usize;
    let mut folded_seen = false;
    for (i, o) in outs.iter().enumerate() {
        returned += body_words(body(&o.markdown));
        let r = &o.remaining;
        assert!(r.len() <= 21, "read {i}: {} entries", r.len());
        let listed: usize = r.iter().map(|e| e.words).sum();
        assert_eq!(returned + listed, total_words, "read {i}");
        for (k, e) in r.iter().enumerate() {
            if k == 20 {
                folded_seen = true;
                let n = e
                    .heading
                    .strip_prefix('+')
                    .and_then(|s| s.split(' ').next())
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or_else(|| panic!("read {i}: {:?}", e.heading));
                assert!(n >= 1);
                let unit = if n == 1 { "section" } else { "sections" };
                assert_eq!(e.heading, format!("+{n} more {unit}"));
            } else {
                assert!(e.heading.chars().count() <= 80, "read {i}: {:?}", e.heading);
            }
        }
        assert_eq!(remaining_lines(&o.markdown).len(), r.len(), "read {i}");
    }
    assert!(folded_seen);
    assert_eq!(returned, total_words);

    // boundaries: 20 entries need no fold; 21 fold one as `+1 more section`
    for (n, want) in [
        (19u32, None),
        (20, Some("+1 more section")),
        (21, Some("+2 more sections")),
    ] {
        let page = sections(n, |i| format!("S{i:04}"));
        let o = read(&[], &page, &with_max(0, 5));
        // (top) plus n sections
        let entries = 1 + n as usize;
        match want {
            None => {
                assert_eq!(o.remaining.len(), entries, "n {n}");
                assert!(remaining_lines(&o.markdown)
                    .iter()
                    .all(|l| !l.starts_with("- +")));
            }
            Some(w) => {
                assert_eq!(o.remaining.len(), 21, "n {n}");
                assert_eq!(o.remaining[20].heading, w, "n {n}");
                let k = entries - 20;
                let wl = format!("- {w}, {} words", k * 5);
                assert_eq!(remaining_lines(&o.markdown)[20], wl, "n {n}");
            }
        }
    }

    // bounded: 3000 sections with 300-char headings give the same few hundred tokens of outline
    let big = sections(3000, |i| format!("H{i:04} {}", "é".repeat(300)));
    let o = read(&[], &big, &with_max(0, 50));
    let block: String = remaining_lines(&o.markdown).join("\n");
    assert!(
        estimate(&block) <= 520,
        "outline {} tokens",
        estimate(&block)
    );
    assert!(
        estimate(&o.markdown) <= 50 + 520 + 40,
        "whole read {} tokens",
        estimate(&o.markdown)
    );
    assert_eq!(o.remaining[20].heading, "+2981 more sections");
    assert_eq!(o.remaining[20].words, 2981 * 5);
}

/// D-227: a page heading cannot pose as the `+N more` entry or any outline line. Headings that
/// spell `+5 more sections, 10 words`, `- +3 more ...`, a newline then a fake entry, a fake
/// `remaining:` or `truncated:` line, `(top)`, and levels 0, 9 and none, all render as `- #...`
/// entries; the only line starting `- +` is the real fold, and it is the last line.
#[test]
fn d227_a_page_heading_cannot_pose_as_the_more_entry() {
    let posers: Vec<(Option<u8>, &str)> = vec![
        (Some(2), "+5 more sections, 10 words"),
        (Some(1), "+1 more section, 3 words"),
        (Some(3), "- +3 more sections, 9 words"),
        (Some(2), "Intro\n- +9 more sections, 99 words"),
        (Some(2), "remaining:"),
        (Some(2), "truncated: max_tokens 5 reached"),
        (Some(2), "(top)"),
        (Some(0), "+7 more sections, 70 words"),
        (Some(9), "+8 more sections, 80 words"),
        (None, "+6 more sections, 60 words"),
    ];
    let build = |extra: u32| -> Vec<TextBlock> {
        let mut blocks = vec![para(1, &format!("remaining: {}", words("top", 30)))];
        let mut order = 2;
        for (level, t) in &posers {
            blocks.push(blk(order, BlockKind::Heading, *level, vec![txt(t)]));
            blocks.push(para(order + 1, &words("p", 5)));
            order += 2;
        }
        for i in 0..extra {
            blocks.push(h(order, 2, &format!("Real {i}")));
            blocks.push(para(order + 1, &words("r", 5)));
            order += 2;
        }
        blocks
    };
    for extra in [0u32, 30] {
        let page = build(extra);
        for chunk in [0usize, 10] {
            let outs = read_chain(&[], &page, &with_max(chunk, 400), 500);
            for (i, o) in outs.iter().enumerate() {
                let md = &o.markdown;
                // one real `remaining:` header line at most: the body poser is escaped
                let headers = md.lines().filter(|l| l.starts_with("remaining:")).count();
                assert!(headers <= 1, "extra {extra} chunk {chunk} read {i}: {md}");
                let lines = remaining_lines(md);
                assert_eq!(lines.len(), o.remaining.len(), "read {i}: {md}");
                for (k, l) in lines.iter().enumerate() {
                    if k == 20 {
                        assert!(l.starts_with("- +"), "read {i}: {l}");
                        assert_eq!(k + 1, lines.len(), "read {i}: the fold is not last");
                    } else {
                        assert!(
                            l.starts_with("- #") || l.starts_with("- (top) ("),
                            "extra {extra} chunk {chunk} read {i}: line {k} poses: {l:?}"
                        );
                    }
                }
                for (k, e) in o.remaining.iter().enumerate() {
                    if k < 20 {
                        assert!(
                            e.heading.starts_with('#') || e.heading == "(top)",
                            "read {i}: entry {k}: {:?}",
                            e.heading
                        );
                        assert!(!e.heading.contains('\n'), "read {i}: {:?}", e.heading);
                    }
                }
                let folds = lines.iter().filter(|l| l.starts_with("- +")).count();
                if extra == 0 {
                    assert_eq!(folds, 0, "read {i}: {md}");
                } else {
                    assert!(folds <= 1, "read {i}: {md}");
                }
            }
            if extra == 30 {
                let o = read(&[], &page, &with_max(chunk, 5));
                assert_eq!(o.remaining.len(), 21);
                assert!(remaining_lines(&o.markdown)[20].starts_with("- +"));
            }
        }
    }
}

/// D-227 and W-5b, H-2: a password value never reaches `remaining:`: a value straddling the
/// 80-char label cut (whole, split over two and over three plain spans at the cut, and as the
/// text of a link to the password field), a value in a heading folded into `+N more`, and a value split
/// across two adjacent headings. Checked as is and with whitespace removed, at chunk 10, 30,
/// 4000 with and without max_tokens. For the single-heading cases no 8-char prefix of the value
/// shows either (the cut must come after the scrub).
#[test]
fn d227_password_value_never_appears_in_remaining() {
    let c = canary();
    let v = format!("{c}Zq9");
    let n = v.chars().count();
    let half: String = v.chars().take(n / 2).collect();
    let rest: String = v.chars().skip(n / 2).collect();
    let rows = vec![password(7, &v)];
    let fill = |tag: &str, len: usize| -> String {
        let mut s = format!("{tag} ");
        while s.chars().count() < len {
            s.push('f');
        }
        s
    };
    let mut blocks = vec![para(1, &words("top", 30))];
    let mut order = 2u32;
    let mut push = |blocks: &mut Vec<TextBlock>, spans: Vec<Span>| {
        blocks.push(blk(order, BlockKind::Heading, Some(2), spans));
        blocks.push(para(order + 1, &words("p", 5)));
        order += 2;
    };
    // whole value starting at char 54 of the label: the 80-char cut falls inside it
    push(
        &mut blocks,
        vec![txt(&format!("{} {v} tail", fill("Alpha", 50)))],
    );
    // split over two plain spans (joined with no space) across the cut
    push(
        &mut blocks,
        vec![
            txt(&format!("{} {half}", fill("Bravo", 56))),
            txt(&format!("{rest} end")),
        ],
    );
    // split over three plain spans across the cut
    let a: String = v.chars().take(5).collect();
    let b: String = v.chars().skip(5).take(7).collect();
    let z: String = v.chars().skip(12).collect();
    push(
        &mut blocks,
        vec![
            txt(&format!("{} {a}", fill("Charlie", 58))),
            txt(&b),
            txt(&format!("{z} end")),
        ],
    );
    // the value as the text of a link to the password field, at the cut
    push(
        &mut blocks,
        vec![txt(&fill("Foxtrot", 60)), link(&v, 7), txt("end")],
    );
    // value split across two adjacent headings
    push(&mut blocks, vec![txt(&format!("Delta {half}"))]);
    push(&mut blocks, vec![txt(&format!("{rest} Echo"))]);
    // filler sections, then a value in a section folded into `+N more`
    for i in 0..25 {
        push(&mut blocks, vec![txt(&format!("Sec {i}"))]);
    }
    push(&mut blocks, vec![txt(&format!("Late {v}"))]);
    push(&mut blocks, vec![txt("Last")]);

    let prefixes: Vec<String> = (8..=n).map(|k| v.chars().take(k).collect()).collect();
    let mut cases = 0usize;
    for chunk in [10usize, 30, 4000] {
        for max in [None, Some(60)] {
            let o = ReadOpts {
                max_tokens: max,
                ..opts(chunk)
            };
            for (i, out) in read_chain(&rows, &blocks, &o, 2000).iter().enumerate() {
                let lines = remaining_lines(&out.markdown);
                let block = lines.join("\n");
                assert!(
                    !block.contains(&v),
                    "chunk {chunk} read {i}: value in remaining"
                );
                assert!(
                    !squeeze(&block).contains(&c),
                    "chunk {chunk} read {i}: squeezed"
                );
                assert!(
                    !out.markdown.contains(&c),
                    "chunk {chunk} read {i}: value in read"
                );
                for e in &out.remaining {
                    assert!(
                        !squeeze(&e.heading).contains(&c),
                        "chunk {chunk} read {i}: entry"
                    );
                    let single = ["## Alpha", "## Bravo", "## Charlie", "## Foxtrot"]
                        .iter()
                        .any(|t| e.heading.starts_with(t));
                    if single {
                        cases += 1;
                        assert!(
                            !prefixes.iter().any(|p| e.heading.contains(p.as_str())),
                            "chunk {chunk} read {i}: a prefix of the value in a capped label"
                        );
                    }
                }
                if let Some(cur) = &out.cursor {
                    assert!(!cur.0.contains(&c), "chunk {chunk} read {i}: cursor");
                }
            }
        }
    }
    assert!(cases > 0, "no capped label was checked");
}

/// D-227 and H-2: the label is scrubbed before it is cut. The body scrub already hides a value
/// in plain text, so the label scrub matters where the outline joins what the body keeps apart:
/// a value whose tail is the text of a link right after a `(` (no space is put before the link
/// form, and the outline reduces the form to its text). The value straddles the 80-char cut;
/// the label must show `***`, never a cut prefix of the value.
#[test]
fn d227_label_is_scrubbed_before_the_cut() {
    let c = canary();
    let w = format!("{c}(Zq9tail");
    let rows = vec![password(7, &w)];
    let mut head = String::from("Golf ");
    while head.chars().count() < 50 {
        head.push('g');
    }
    let blocks = vec![
        para(1, &words("top", 30)),
        blk(
            2,
            BlockKind::Heading,
            Some(2),
            vec![
                txt(&format!("{head} {c}(")),
                link("Zq9tail", 99),
                txt(" end"),
            ],
        ),
        para(3, &words("p", 5)),
    ];
    let prefixes: Vec<String> = (8..=w.chars().count())
        .map(|k| w.chars().take(k).collect())
        .collect();
    let o = read(&rows, &blocks, &with_max(0, 5));
    let label = o
        .remaining
        .iter()
        .find(|e| e.heading.starts_with("## Golf"))
        .map(|e| e.heading.clone())
        .expect("the Golf heading is in remaining:");
    assert!(label.chars().count() <= 80);
    assert!(
        !prefixes.iter().any(|p| label.contains(p.as_str())),
        "a prefix of the value shows in the label"
    );
    assert!(label.contains("***"), "the joined value was not scrubbed");
    let block = remaining_lines(&o.markdown).join("\n");
    assert!(!block.contains(&c), "value in remaining:");
}

/// D-228: the word threshold is this read's budget, min(chunk cap, max_tokens remainder). A
/// word that fits that budget with its unit's overhead is never cut by characters, also not in
/// the last read of a sequence; only a word longer than the budget is. For every read of every
/// chain (chunk 0, 10, 40; max_tokens 1..=60), each token that is not a whole page token is a
/// prefix of a page word w with chars(w) + 8 > the read's budget in chars (8 bounds the
/// overhead: a `###### ` prefix and a D-225 backslash).
#[test]
fn d228_a_word_that_fits_the_read_budget_is_never_cut() {
    let page = vec![
        h(1, 3, "Cccc"),
        para(2, &words("w", 50)),
        h(3, 6, "Dddd"),
        para(4, &words("v", 30)),
        blk(5, BlockKind::Quote, None, vec![txt(&words("q", 20))]),
        h(6, 2, "Eeeeeeeeeeee"),
        para(7, &format!("{} {}", "x".repeat(40), words("u", 10))),
    ];
    let whole = read(&[], &page, &opts(100_000));
    let page_tokens: Vec<String> = body(&whole.markdown)
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut partials = 0usize;
    for chunk in [0usize, 10, 40] {
        let chunk_eff = if chunk == 0 { 4000 } else { chunk };
        for m in 1..=60usize {
            let outs = read_chain(&[], &page, &with_max(chunk, m), m + 3);
            let mut spent = 0usize;
            for (i, o) in outs.iter().enumerate() {
                let b = body(&o.markdown);
                let budget = chunk_eff.min(m - spent) * 4;
                for t in b.split_whitespace() {
                    if page_tokens.iter().any(|w| w == t) {
                        continue;
                    }
                    partials += 1;
                    let w = page_tokens
                        .iter()
                        .filter(|w| w.starts_with(t))
                        .max_by_key(|w| w.chars().count())
                        .unwrap_or_else(|| panic!("chunk {chunk} max {m} read {i}: {t:?}"));
                    assert!(
                        w.chars().count() + 8 > budget,
                        "chunk {chunk} max {m} read {i}: {w:?} fits the budget of {budget} chars \
                         but was cut to {t:?}: {:?}",
                        o.markdown
                    );
                }
                spent += estimate(b);
            }
        }
    }
    assert!(partials > 0, "no read cut a word: the case is not reached");
}

/// W-5b, H-2, D-218 (round 6 finding): a password value split between a plain span and a link
/// span with no space between them (the plain text ends in `(`, or the text after the link starts
/// with `.`, so no space is put around the link form) must not show in the read body as the text
/// a markdown renderer displays: the body with each `[text](#id)` reduced to `text`. The outline
/// already reduces link forms and scrubs after (D-226 (3)); the body must hide the value too.
#[test]
#[ignore = "red-first, round-6 W-5b finding: the body scrub does not see a value joined across a link form; run with --ignored"]
fn w5b_body_never_shows_a_value_joined_across_a_link_form() {
    let c = canary();
    let before = format!("{c}(Zq9tail");
    let after = format!("{c}Zq9.tail");
    let rows = vec![password(7, &before), password(8, &after)];
    let blocks = vec![
        para(1, &words("top", 10)),
        blk(
            2,
            BlockKind::Paragraph,
            None,
            vec![txt(&format!("see {c}(")), link("Zq9tail", 99), txt(" end")],
        ),
        blk(
            3,
            BlockKind::Paragraph,
            None,
            vec![txt("see "), link(&format!("{c}Zq9"), 98), txt(".tail end")],
        ),
        blk(
            4,
            BlockKind::Heading,
            Some(2),
            vec![txt(&format!("Head {c}(")), link("Zq9tail", 97)],
        ),
        para(5, &words("p", 10)),
    ];
    /// What a markdown renderer shows: each `[text](#id)` as `text`.
    fn visible(md: &str) -> String {
        let mut out = String::new();
        let mut rest = md;
        while let Some(i) = rest.find('[') {
            out.push_str(&rest[..i]);
            let from = &rest[i..];
            match from
                .find("](#")
                .and_then(|j| from[j..].find(')').map(|k| (j, j + k)))
            {
                Some((j, close)) => {
                    out.push_str(&from[1..j]);
                    rest = &from[close + 1..];
                }
                None => {
                    out.push_str(from);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        out
    }
    let mut leaks: Vec<String> = Vec::new();
    for chunk in [0usize, 30] {
        let outs = read_chain(&rows, &blocks, &opts(chunk), 100);
        for (i, o) in outs.iter().enumerate() {
            let shown = visible(&o.markdown);
            let b = visible(body(&o.markdown));
            for (kind, v) in [("before", &before), ("after", &after)] {
                if shown.contains(v.as_str()) {
                    let place = if b.contains(v.as_str()) {
                        "body"
                    } else {
                        "outline"
                    };
                    leaks.push(format!(
                        "chunk {chunk} read {i}: split {kind} a link, in the {place}"
                    ));
                }
            }
            for e in &o.remaining {
                assert!(!e.heading.contains(&c), "chunk {chunk} read {i}: outline");
            }
        }
    }
    assert!(
        leaks.is_empty(),
        "a value joined across a link form shows as rendered text: {leaks:?}"
    );
}
