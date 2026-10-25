//! W-5 / B-6.4 read renderer on a hand-built article page (T-10).
//!
//! Page shape: header nav (5 links), main with `# Tokio`, intro paragraphs, `##` sections
//! (one with a list, a table, code, a quote, element links), a `## FAQ`, a `## Changelog`,
//! an aside, a footer with links, a password field and a CHALLENGE frame.

use sb_sheet::{
    read, BlockKind, Cursor, ElemRef, FieldValue, Flags, IndexRow, ReadOpts, ReadOutput, Span,
    State, TextBlock,
};

const CANARY: &str = "hunter2-canary";

fn row(id: u32, role: &str, name: &str) -> IndexRow {
    IndexRow {
        id,
        frame: 0,
        role: role.to_string(),
        name: name.to_string(),
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

fn text(t: &str) -> Span {
    Span {
        text: t.to_string(),
        elem: None,
    }
}

fn link(t: &str, id: u32) -> Span {
    Span {
        text: t.to_string(),
        elem: Some(ElemRef::main(id)),
    }
}

struct Page {
    blocks: Vec<TextBlock>,
    next: u32,
}

impl Page {
    fn new() -> Page {
        Page {
            blocks: Vec::new(),
            next: 0,
        }
    }

    fn push(
        &mut self,
        landmark: Option<&str>,
        kind: BlockKind,
        level: Option<u8>,
        spans: Vec<Span>,
    ) -> &mut TextBlock {
        self.next += 1;
        self.blocks.push(TextBlock {
            order: self.next,
            frame: 0,
            kind,
            level,
            landmark: landmark.map(str::to_string),
            heading: None,
            table: None,
            header: false,
            spans,
        });
        self.blocks.last_mut().unwrap()
    }

    fn h(&mut self, lm: Option<&str>, level: u8, t: &str) {
        self.push(lm, BlockKind::Heading, Some(level), vec![text(t)]);
    }

    fn p(&mut self, lm: Option<&str>, spans: Vec<Span>) {
        self.push(lm, BlockKind::Paragraph, None, spans);
    }
}

/// A distinct ~60-word paragraph carrying a unique marker.
fn para(marker: &str) -> String {
    format!(
        "{marker} Tokio is an asynchronous runtime for the Rust programming language. \
         It provides the building blocks needed for writing network applications. \
         It gives the flexibility to target a wide range of systems, from large servers \
         with dozens of cores to small embedded devices. The scheduler is work stealing \
         and the reactor is backed by the operating system event queue. Timers, channels \
         and synchronization primitives are included as well."
    )
}

const MAIN_MARKERS: [&str; 14] = [
    "PARA01", "PARA02", "PARA03", "PARA04", "PARA05", "PARA06", "PARA07", "PARA08", "PARA09",
    "PARA10", "PARA11", "PARA12", "PARA13", "PARA14",
];

fn article() -> (Vec<IndexRow>, Vec<TextBlock>) {
    let mut rows = Vec::new();
    for (i, n) in ["Home", "Docs", "Blog", "Community", "Search"]
        .iter()
        .enumerate()
    {
        let mut r = row(i as u32 + 1, "link", n);
        r.landmark = Some("nav".into());
        rows.push(r);
    }
    rows.push(row(20, "link", "API docs"));
    rows.push(row(21, "button", "Add to cart"));
    let mut pw = row(22, "textbox", "Password");
    pw.state.input_type = Some("password".into());
    pw.state.value = Some(FieldValue::Text(CANARY.into()));
    rows.push(pw);
    let mut cap = row(30, "iframe", "reCAPTCHA");
    cap.flags.challenge = true;
    cap.flags.opaque = true;
    cap.child_frame = Some(4);
    rows.push(cap);
    rows.push(row(40, "link", "Privacy"));
    rows.push(row(41, "link", "Terms"));

    let m = Some("main");
    let mut pg = Page::new();
    // header nav: 5 links
    for (i, n) in ["NAVHome", "NAVDocs", "NAVBlog", "NAVCommunity", "NAVSearch"]
        .iter()
        .enumerate()
    {
        pg.push(
            Some("nav"),
            BlockKind::ListItem,
            Some(1),
            vec![link(n, i as u32 + 1)],
        );
    }
    pg.h(m, 1, "Tokio");
    pg.p(m, vec![text(&para("PARA01"))]);
    pg.p(m, vec![text(&para("PARA02"))]);
    pg.h(m, 2, "Getting started");
    pg.p(m, vec![text(&para("PARA03"))]);
    pg.push(
        m,
        BlockKind::ListItem,
        Some(1),
        vec![text("LISTA install Rust")],
    );
    pg.push(
        m,
        BlockKind::ListItem,
        Some(2),
        vec![text("LISTB nested step")],
    );
    pg.push(
        m,
        BlockKind::ListItem,
        Some(1),
        vec![text("LISTC add tokio")],
    );
    pg.p(
        m,
        vec![
            text(&para("PARA04")),
            text(" See the"),
            link("API docs", 20),
            text("or press"),
            link("Add to cart", 21),
            text("."),
        ],
    );
    pg.h(m, 2, "Runtime");
    pg.p(m, vec![text(&para("PARA05"))]);
    for (header, cells) in [
        (true, ["Name", "Value"]),
        (false, ["workers", "4"]),
        (false, ["blocking | max", "512"]),
    ] {
        let b = pg.push(
            m,
            BlockKind::TableRow,
            None,
            cells.iter().map(|c| text(c)).collect(),
        );
        b.table = Some(1);
        b.header = header;
    }
    pg.push(
        m,
        BlockKind::Code,
        None,
        vec![text(
            "#[tokio::main]\nasync fn main() {\n    run().await;\n}\n",
        )],
    );
    pg.push(
        m,
        BlockKind::Quote,
        None,
        vec![text("QUOTEX Fearless concurrency.")],
    );
    pg.p(m, vec![text(&para("PARA06"))]);
    pg.h(m, 2, "Security");
    pg.p(
        m,
        vec![
            text(&para("PARA07")),
            text(" Sign in with"),
            link(CANARY, 22),
        ],
    );
    // CHALLENGE: a block linking the challenge row, and blocks inside its frame.
    pg.p(
        m,
        vec![
            text("SECRETCHALLENGE I am not a robot"),
            link("checkbox", 30),
        ],
    );
    pg.push(
        m,
        BlockKind::Paragraph,
        None,
        vec![text("CHALLENGEFRAME select all buses")],
    )
    .frame = 4;
    pg.p(m, vec![text(&para("PARA08"))]);
    pg.h(m, 2, "FAQ");
    pg.h(m, 3, "Is Tokio fast?");
    pg.p(m, vec![text(&para("PARA09"))]);
    pg.h(m, 3, "Does it run on Windows?");
    pg.p(m, vec![text(&para("PARA10"))]);
    pg.p(m, vec![text(&para("PARA11"))]);
    pg.h(m, 2, "Changelog");
    pg.p(m, vec![text(&para("PARA12"))]);
    pg.p(m, vec![text(&para("PARA13"))]);
    pg.p(m, vec![text(&para("PARA14"))]);
    // aside and footer
    pg.p(
        Some("aside"),
        vec![text("ASIDETEXT Sponsored: buy more widgets today")],
    );
    pg.p(
        Some("footer"),
        vec![link("FOOTPrivacy", 40), link("FOOTTerms", 41)],
    );
    pg.p(
        Some("footer"),
        vec![text("FOOTCOPY Copyright 2026 Tokio contributors")],
    );
    (rows, pg.blocks)
}

fn opts(chunk: usize) -> ReadOpts {
    ReadOpts {
        region: None,
        cursor: None,
        max_tokens: None,
        chunk_tokens: chunk,
    }
}

/// Follow cursors until done; returns every output.
fn read_all(rows: &[IndexRow], blocks: &[TextBlock], first: ReadOpts) -> Vec<ReadOutput> {
    let mut outs = Vec::new();
    let mut o = first;
    for _ in 0..200 {
        let out = read(rows, blocks, &o);
        let cursor = out.cursor.clone();
        outs.push(out);
        match cursor {
            Some(c) => o.cursor = Some(c),
            None => return outs,
        }
    }
    panic!("read never finished");
}

fn first_heading(md: &str) -> Option<&str> {
    md.lines().find(|l| l.starts_with('#'))
}

#[test]
fn chunks_cover_main_once_without_boilerplate() {
    let (rows, blocks) = article();
    let outs = read_all(&rows, &blocks, opts(300));
    let n = outs[0].chunks;
    assert!(n >= 3, "expected >= 3 chunks, got {n}");
    assert_eq!(outs.len(), n, "one call per chunk");
    for (i, o) in outs.iter().enumerate() {
        assert_eq!(o.chunk, i + 1);
        assert_eq!(o.chunks, n);
        let line = format!("chunk {} of {n}", i + 1);
        assert_eq!(o.markdown.lines().next(), Some(line.as_str()));
        assert!(
            sb_sheet::tokens::estimate(&o.markdown) < 300 + 120,
            "{}",
            o.markdown
        );
        // A heading is never left dangling at the end of a chunk.
        let body = o.markdown.split("\n\nremaining:").next().unwrap();
        let last = body.lines().rev().find(|l| !l.is_empty()).unwrap();
        assert!(!last.starts_with('#'), "chunk {} ends on {last:?}", i + 1);
    }
    assert!(outs.last().unwrap().cursor.is_none());
    assert!(outs.last().unwrap().remaining.is_empty());

    let all: String = outs
        .iter()
        .map(|o| o.markdown.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for m in MAIN_MARKERS {
        assert_eq!(all.matches(m).count(), 1, "{m} must appear exactly once");
    }
    for m in ["LISTA", "LISTB", "LISTC", "QUOTEX"] {
        assert_eq!(all.matches(m).count(), 1, "{m}");
    }
    for bad in ["NAV", "FOOT", "ASIDETEXT", "Sponsored", "Copyright"] {
        assert!(!all.contains(bad), "boilerplate {bad} leaked:\n{all}");
    }
    assert_eq!(first_heading(&outs[0].markdown), Some("# Tokio"));
    assert!(!all.contains('\u{2014}'), "no em dashes");

    // Rendering: list nesting, table, code, quote, element links.
    assert!(all.contains("- LISTA install Rust\n  - LISTB nested step\n- LISTC add tokio"));
    assert!(
        all.contains("| Name | Value |\n|---|---|\n| workers | 4 |\n| blocking \\| max | 512 |")
    );
    assert!(all.contains("```\n#[tokio::main]\nasync fn main() {\n    run().await;\n}\n```"));
    assert!(all.contains("> QUOTEX Fearless concurrency."));
    assert!(all.contains("See the [API docs](#20) or press [button \"Add to cart\"](#21)."));
    assert!(all.contains("## FAQ") && all.contains("### Is Tokio fast?"));
}

#[test]
fn remaining_outline_lists_sections_with_words() {
    let (rows, blocks) = article();
    let out = read(&rows, &blocks, &opts(300));
    assert!(out.cursor.is_some());
    let faq = out
        .remaining
        .iter()
        .find(|e| e.heading == "## FAQ")
        .expect("FAQ in remaining");
    assert_eq!(faq.words, 0, "FAQ's words live under its ### questions");
    let q = out
        .remaining
        .iter()
        .find(|e| e.heading == "### Is Tokio fast?")
        .unwrap();
    assert!(q.words >= 50, "{q:?}");
    assert!(out
        .remaining
        .iter()
        .any(|e| e.heading == "## Changelog" && e.words >= 150));
    assert!(out.markdown.contains("\n\nremaining:\n- "));
    assert!(out
        .markdown
        .contains(&format!("- ### Is Tokio fast? ({} words)", q.words)));
    // Nothing already returned is listed.
    assert!(!out.remaining.iter().any(|e| e.heading == "# Tokio"));
}

#[test]
fn region_jumps_to_faq_section() {
    let (rows, blocks) = article();
    let mut o = opts(4000);
    o.region = Some("## FAQ".into());
    let out = read(&rows, &blocks, &o);
    assert_eq!(first_heading(&out.markdown), Some("## FAQ"));
    for m in ["PARA09", "PARA10", "PARA11"] {
        assert!(out.markdown.contains(m), "{m}");
    }
    for m in ["PARA08", "PARA12", "PARA01", "Changelog"] {
        assert!(!out.markdown.contains(m), "{m} is outside the FAQ section");
    }
    assert_eq!(out.chunks, 1);
    assert!(out.cursor.is_none());

    // Plain heading text works too, and a landmark region reads that landmark.
    o.region = Some("faq".into());
    assert_eq!(
        first_heading(&read(&rows, &blocks, &o).markdown),
        Some("## FAQ")
    );
    o.region = Some("nav".into());
    let nav = read(&rows, &blocks, &o).markdown;
    assert!(nav.contains("[NAVHome](#1)") && !nav.contains("PARA01"));
    o.region = Some("## Nope".into());
    let miss = read(&rows, &blocks, &o).markdown;
    assert!(miss.lines().next().unwrap().contains("not found"));
    assert!(miss.contains("PARA01"));
}

#[test]
fn region_cursor_stays_in_region() {
    let (rows, blocks) = article();
    let mut o = opts(120);
    o.region = Some("## FAQ".into());
    let outs = read_all(&rows, &blocks, o);
    assert!(outs.len() >= 2);
    let all: String = outs.iter().map(|o| o.markdown.as_str()).collect();
    for m in ["PARA09", "PARA10", "PARA11"] {
        assert_eq!(all.matches(m).count(), 1, "{m}");
    }
    assert!(!all.contains("PARA12"));
}

#[test]
fn max_tokens_caps_the_whole_read() {
    let (rows, blocks) = article();
    let mut o = opts(300);
    o.max_tokens = Some(500);
    let outs = read_all(&rows, &blocks, o);
    let last = outs.last().unwrap();
    assert!(last.cursor.is_none());
    assert!(
        last.markdown.contains("truncated: max_tokens 500 reached"),
        "{}",
        last.markdown
    );
    assert!(
        !last.remaining.is_empty(),
        "what was not read is still outlined"
    );
    let all: String = outs.iter().map(|o| o.markdown.as_str()).collect();
    assert!(!all.contains("PARA14"));
    // Body tokens across calls stay within the cap (outline and header lines aside).
    let body: usize = outs
        .iter()
        .map(|o| {
            let md = o.markdown.split("\n\nremaining:").next().unwrap();
            let md = md.split("\n\ntruncated:").next().unwrap();
            let md = md.split_once('\n').map(|x| x.1).unwrap_or(md);
            sb_sheet::tokens::estimate(md)
        })
        .sum();
    assert!(body <= 500, "body tokens {body}");
}

#[test]
fn password_value_never_emitted() {
    let (rows, blocks) = article();
    let mut runs = vec![
        read_all(&rows, &blocks, opts(300)),
        read_all(&rows, &blocks, opts(40)),
    ];
    let mut o = opts(4000);
    o.region = Some("## Security".into());
    runs.push(read_all(&rows, &blocks, o));
    let mut saw_form = false;
    for outs in runs {
        for out in outs {
            assert!(!out.markdown.contains(CANARY), "{}", out.markdown);
            assert!(!out.remaining.iter().any(|e| e.heading.contains(CANARY)));
            assert!(!out.cursor.is_some_and(|c| c.0.contains(CANARY)));
            saw_form |= out.markdown.contains("[input \"Password\"](#22)");
        }
    }
    assert!(saw_form, "the password field is still linkable");

    // Even a stray plain span carrying the value is scrubbed.
    let mut blocks2 = blocks.clone();
    blocks2[10]
        .spans
        .push(text(&format!(" leaked {CANARY} here")));
    let out = read(&rows, &blocks2, &opts(4000));
    assert!(!out.markdown.contains(CANARY));
}

#[test]
fn challenge_frame_content_never_emitted() {
    let (rows, blocks) = article();
    for chunk in [4000, 300, 50] {
        for out in read_all(&rows, &blocks, opts(chunk)) {
            assert!(!out.markdown.contains("CHALLENGEFRAME"));
            assert!(!out.markdown.contains("SECRETCHALLENGE"));
            assert!(!out.markdown.contains("buses"));
        }
    }
    let out = read(&rows, &blocks, &opts(4000));
    assert!(out.markdown.contains("[iframe \"reCAPTCHA\"](#30)"));
    // Asking for the frame's landmark does not help either.
    let mut o = opts(4000);
    o.region = Some("main".into());
    assert!(!read(&rows, &blocks, &o).markdown.contains("CHALLENGEFRAME"));
}

#[test]
fn mainless_page_drops_link_dense_blocks() {
    let rows: Vec<IndexRow> = (1..=6).map(|i| row(i, "link", "x")).collect();
    let mut pg = Page::new();
    pg.p(
        None,
        vec![
            link("Home", 1),
            link("Products", 2),
            link("About us", 3),
            link("Contact", 4),
        ],
    );
    pg.p(Some("nav"), vec![text("NAVTEXT Skip to content")]);
    pg.h(None, 1, "Release notes");
    pg.p(None, vec![text(&para("BODY1"))]);
    pg.p(
        None,
        vec![
            text(&para("BODY2")),
            text(" Read the"),
            link("full guide", 5),
            text("."),
        ],
    );
    pg.p(
        None,
        vec![text("Shared footer line about cookies and privacy")],
    );
    pg.p(
        Some("footer"),
        vec![text("Shared footer line about cookies and privacy")],
    );
    pg.p(None, vec![link("Twitter", 6)]);
    let out = read(&rows, &pg.blocks, &opts(4000));
    let md = &out.markdown;
    assert!(md.contains("# Release notes"));
    assert!(md.contains("BODY1") && md.contains("BODY2"));
    assert!(md.contains("[full guide](#5)"));
    for bad in ["Products", "About us", "NAVTEXT", "cookies", "Twitter"] {
        assert!(!md.contains(bad), "{bad} should be boilerplate:\n{md}");
    }
}

#[test]
fn garbage_and_stale_cursors_restart() {
    let (rows, blocks) = article();
    for junk in [
        "",
        "zzz",
        "v1",
        "v1::",
        "v1:x:notanumber:abc",
        "v1::99999:0000000000000000",
        "v1::3.999999:ffffffffffffffff",
        "v9:main:1:00",
        "v1::\u{e9}\u{e9}:\u{e9}",
    ] {
        let mut o = opts(300);
        o.cursor = Some(Cursor(junk.into()));
        let out = read(&rows, &blocks, &o);
        assert_eq!(out.chunk, 1, "{junk:?}");
        let first = out.markdown.lines().next().unwrap();
        assert!(
            first.contains("cursor") || first.contains("restarted"),
            "{junk:?}: {first}"
        );
        assert!(out.markdown.contains("PARA01"));
    }

    // A cursor from a page that changed since: restart at chunk 1 and say so.
    let first = read(&rows, &blocks, &opts(300));
    let mut changed = blocks.clone();
    let i = changed
        .iter()
        .position(|b| b.spans.iter().any(|s| s.text.contains("PARA05")))
        .unwrap();
    changed[i].spans = vec![text(&para("PARA05 edited"))];
    let mut o = opts(300);
    o.cursor = first.cursor.clone();
    let out = read(&rows, &changed, &o);
    assert_eq!(out.chunk, 1);
    assert!(out
        .markdown
        .lines()
        .next()
        .unwrap()
        .contains("restarted at chunk 1"));
    assert!(out.markdown.contains("PARA01"));

    // The same cursor on the unchanged page continues at chunk 2.
    let out = read(&rows, &blocks, &o);
    assert_eq!(out.chunk, 2);
    assert!(!out.markdown.contains("PARA01"));
}

#[test]
fn oversized_block_splits_without_loss() {
    let rows = Vec::new();
    let words: Vec<String> = (0..400).map(|i| format!("w{i:03}")).collect();
    let mut long = String::new();
    for (i, w) in words.iter().enumerate() {
        long.push_str(w);
        long.push_str(if i % 17 == 16 { ". " } else { " " });
    }
    let mut pg = Page::new();
    pg.h(Some("main"), 1, "Big");
    pg.p(Some("main"), vec![text(&long)]);
    pg.p(Some("main"), vec![text("TAIL end")]);
    let outs = read_all(&rows, &pg.blocks, opts(100));
    assert!(outs.len() >= 4);
    let mut seen: Vec<String> = Vec::new();
    for o in &outs {
        let body = o.markdown.split("\n\nremaining:").next().unwrap();
        let body = body.split_once('\n').map(|x| x.1).unwrap_or(body);
        assert!(sb_sheet::tokens::estimate(body) <= 100, "{body}");
        seen.extend(
            body.split_whitespace()
                .map(|w| w.trim_end_matches('.').to_string())
                .filter(|w| w.starts_with('w') && w.len() == 4),
        );
    }
    let want: Vec<String> = words.clone();
    assert_eq!(seen, want, "every word once, in order");
    assert!(outs.last().unwrap().markdown.contains("TAIL end"));
}

#[test]
fn single_chunk_has_no_header_and_no_cursor() {
    let (rows, blocks) = article();
    let out = read(&rows, &blocks, &opts(0));
    assert_eq!((out.chunk, out.chunks), (1, 1));
    assert!(out.cursor.is_none() && out.remaining.is_empty());
    assert!(out.markdown.starts_with("# Tokio"));
    let empty = read(&[], &[], &opts(0));
    assert_eq!(empty.markdown, "(no readable text)");
}
