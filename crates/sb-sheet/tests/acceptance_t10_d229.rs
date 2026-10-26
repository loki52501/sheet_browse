//! T-10 acceptance, round 7 (tester): decisions D-229 and D-230 (W-5b, H-2). Black-box through
//! the public API only, with fixtures built here, independently of the coder's unit tests and of
//! round 6's `w5b_body_never_shows_a_value_joined_across_a_link_form`.
//!
//! D-229: a block's rendered text is also checked against every password value the way a
//! markdown renderer shows it (each `[text](#id)` as `text`); a contributing link span renders as
//! `***` with no link form, and the line is re-checked until no hit is left. D-230: a contributing
//! plain span hides only the characters it gives to the value, so its other text stays.
//!
//! Every read is checked in three spellings: the raw markdown, the renderer's reading (each link
//! form reduced to its bracket text) and the outline label's reading (`alias "label"` reduced to
//! the label), each as is, with whitespace removed and as alphanumerics only. No window of 8
//! chars of any value may show in any of them, in any output string (markdown, `remaining:`
//! entries, cursor), nor in the concatenation of a whole cursor chain (alphanumerics only, so
//! the line prefixes a continuation read repeats cannot hide a value cut into small pieces). No
//! assertion message prints read output that may hold the canary: messages name the case and the
//! reading only.

use std::collections::HashMap;

use sb_sheet::data::alias_for;
use sb_sheet::{
    read, skeleton, BlockKind, ElemRef, FieldValue, IndexRow, ReadOpts, ReadOutput, SkeletonOpts,
    Span, State, TextBlock,
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

/// A span linked to element `id`: a `[text](#id)` link form for an unknown id or a link row,
/// `[alias "text"](#id)` for another actionable row.
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

fn p(order: u32, spans: Vec<Span>) -> TextBlock {
    blk(order, BlockKind::Paragraph, None, spans)
}

fn para(order: u32, t: &str) -> TextBlock {
    p(order, vec![txt(t)])
}

fn hd(order: u32, level: u8, spans: Vec<Span>) -> TextBlock {
    blk(order, BlockKind::Heading, Some(level), spans)
}

fn trow(order: u32, header: bool, spans: Vec<Span>) -> TextBlock {
    TextBlock {
        table: Some(1),
        header,
        ..blk(order, BlockKind::TableRow, None, spans)
    }
}

/// `n` distinct words `p00 p01 ...`.
fn words(p: &str, n: usize) -> String {
    (0..n)
        .map(|i| format!("{p}{i:02}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn opts(chunk_tokens: usize, max_tokens: Option<usize>) -> ReadOpts {
    ReadOpts {
        region: None,
        cursor: None,
        max_tokens,
        chunk_tokens,
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

/// Each `[inner](#id)` link form replaced by `f(inner)`; everything else kept.
fn reduce_forms(md: &str, f: impl Fn(&str) -> &str) -> String {
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
                out.push_str(f(&from[1..j]));
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

/// What a markdown renderer shows: each `[text](#id)` as `text`.
fn visible(md: &str) -> String {
    reduce_forms(md, |inner| inner)
}

/// The outline label's reading: `alias "label"` as `label`, other link forms as their text.
fn label_view(md: &str) -> String {
    reduce_forms(md, |inner| match inner.find(" \"") {
        Some(q) if inner.ends_with('"') && inner.len() > q + 2 => &inner[q + 2..inner.len() - 1],
        _ => inner,
    })
}

/// Only the alphanumeric chars.
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

/// Every window of `w` chars of `v` (the whole of `v` when shorter).
fn windows(v: &str, w: usize) -> Vec<String> {
    let cs: Vec<char> = v.chars().collect();
    if cs.len() <= w {
        return vec![v.to_string()];
    }
    (0..=cs.len() - w)
        .map(|i| cs[i..i + w].iter().collect())
        .collect()
}

const WINDOW: usize = 8;

/// The name of the value owning the first 8-char window of `s` found in `wins` (every value
/// in these tests is longer than a window).
fn first_window_in(s: &str, wins: &HashMap<String, &'static str>) -> Option<&'static str> {
    let cs: Vec<char> = s.chars().collect();
    if cs.len() < WINDOW {
        return None;
    }
    (0..=cs.len() - WINDOW).find_map(|i| {
        let k: String = cs[i..i + WINDOW].iter().collect();
        wins.get(&k).copied()
    })
}

/// Named password values and the windows that must never show.
struct Secrets {
    /// Every 8-char window of every value, and of every value with whitespace removed.
    wins: HashMap<String, &'static str>,
    sq_wins: HashMap<String, &'static str>,
    /// The same over only the alphanumeric chars: a stream that ignores markup, line prefixes
    /// and where pieces were cut.
    an_wins: HashMap<String, &'static str>,
}

/// The name reported for a window that several values share (the canary part).
const SHARED: &str = "a part several values share (the canary)";

impl Secrets {
    fn new(values: Vec<(&'static str, String)>) -> Secrets {
        let mut wins = HashMap::new();
        let mut sq_wins = HashMap::new();
        let mut an_wins = HashMap::new();
        for (name, v) in &values {
            assert!(
                alnum(v).chars().count() > WINDOW,
                "{name}: value shorter than a window"
            );
            for (map, spelled) in [
                (&mut wins, v.clone()),
                (&mut sq_wins, squeeze(v)),
                (&mut an_wins, alnum(v)),
            ] {
                for w in windows(&spelled, WINDOW) {
                    let e = map.entry(w).or_insert(*name);
                    if *e != *name {
                        *e = SHARED;
                    }
                }
            }
        }
        Secrets {
            wins,
            sq_wins,
            an_wins,
        }
    }

    /// The first (case, reading) in which some window of a value shows in `s`, if any.
    fn leak_in(&self, s: &str) -> Option<String> {
        let readings = [
            ("raw", s.to_string()),
            ("rendered", visible(s)),
            ("label", label_view(s)),
        ];
        for (rname, r) in &readings {
            if let Some(name) = first_window_in(r, &self.wins) {
                return Some(format!("{name} in the {rname} reading"));
            }
            if let Some(name) = first_window_in(&squeeze(r), &self.sq_wins) {
                return Some(format!("{name} in the {rname} reading, whitespace removed"));
            }
            if let Some(name) = first_window_in(&alnum(r), &self.an_wins) {
                return Some(format!("{name} in the {rname} reading, alphanumerics only"));
            }
        }
        None
    }

    /// Leaks in every output string of a read: markdown, `remaining:` entries, cursor.
    fn leaks_in_read(&self, o: &ReadOutput, at: &str, out: &mut Vec<String>) {
        if let Some(l) = self.leak_in(&o.markdown) {
            out.push(format!("{at}: markdown: {l}"));
        }
        for (k, e) in o.remaining.iter().enumerate() {
            if let Some(l) = self.leak_in(&e.heading) {
                out.push(format!("{at}: remaining entry {k}: {l}"));
            }
        }
        if let Some(c) = &o.cursor {
            if let Some(l) = self.leak_in(&c.0) {
                out.push(format!("{at}: cursor: {l}"));
            }
        }
    }

    /// Leaks in every read of a chain and in the chain's bodies joined (a value cut in two
    /// across reads).
    fn leaks_in_chain(&self, outs: &[ReadOutput], at: &str, out: &mut Vec<String>) {
        let mut joined = String::new();
        for (i, o) in outs.iter().enumerate() {
            self.leaks_in_read(o, &format!("{at} read {i}"), out);
            joined.push_str(body(&o.markdown));
        }
        if let Some(l) = self.leak_in(&joined) {
            out.push(format!("{at}: chain bodies joined: {l}"));
        }
    }
}

/// A full read of the page (one chunk) and the chain at chunk 30, checked for leaks.
fn check_page(rows: &[IndexRow], blocks: &[TextBlock], s: &Secrets) -> Vec<String> {
    let mut leaks = Vec::new();
    for chunk in [0usize, 30] {
        let outs = read_chain(rows, blocks, &opts(chunk, None), 200);
        s.leaks_in_chain(&outs, &format!("chunk {chunk}"), &mut leaks);
    }
    leaks
}

fn full_body(rows: &[IndexRow], blocks: &[TextBlock]) -> String {
    let o = read(rows, blocks, &opts(100_000, None));
    assert!(
        o.cursor.is_none(),
        "a chunk of 100000 tokens holds the page"
    );
    body(&o.markdown).to_string()
}

fn button(id: u32, name: &str) -> IndexRow {
    row(id, "button", name)
}

// ---------------------------------------------------------------- pages

/// D-229 row 1: link + plain, the value starting in the link. Four placements: a link whose
/// text holds the value's head after other words (paragraph), a link that is exactly the head
/// with `!` after (list item), a button's quoted label as the head with `.` after (quote; the
/// value shows in the outline label's reading), and a value that only the renderer's reading
/// shows (plain head ending `(`, then the `alias "` of a button form).
fn link_then_plain(c: &str) -> (Vec<IndexRow>, Vec<TextBlock>, Secrets) {
    let alias = alias_for("button");
    let va = format!("{c}Za.tailA1234");
    let vb = format!("{c}Zb!tailB1234");
    let vc = format!("{c}Zc.tailC1234");
    let vd = format!("{c}({alias} \"Zd1234");
    let rows = vec![
        password(7, &va),
        password(8, &vb),
        password(9, &vc),
        password(10, &vd),
        button(5, "Send"),
        button(6, "Go"),
        row(50, "link", "Docs"),
    ];
    let blocks = vec![
        para(1, &words("top", 10)),
        p(
            2,
            vec![
                txt("intro "),
                link(&format!("go {c}Za"), 99),
                txt(".tailA1234 keep alpha see "),
                link("Docs", 50),
            ],
        ),
        blk(
            3,
            BlockKind::ListItem,
            Some(1),
            vec![link(&format!("{c}Zb"), 98), txt("!tailB1234 keep bravo")],
        ),
        blk(
            4,
            BlockKind::Quote,
            None,
            vec![link(&format!("{c}Zc"), 5), txt(".tailC1234 keep charlie")],
        ),
        p(
            5,
            vec![txt(&format!("keep delta {c}(")), link("Zd1234567", 6)],
        ),
        para(6, &words("end", 10)),
    ];
    let s = Secrets::new(vec![
        ("link-head paragraph", va),
        ("link-head list item", vb),
        ("button-label head (label reading)", vc),
        ("renderer-reading only", vd),
    ]);
    (rows, blocks, s)
}

/// D-229 row 2: a value across a table cell separator ` | `: plain then link cell, link then
/// plain cell, plain then plain cell (the D-218 literal scrub on the whole row), and over three
/// cells.
fn table_page(c: &str) -> (Vec<IndexRow>, Vec<TextBlock>, Secrets) {
    let ve = format!("{c} | Ze12345678");
    let vf = format!("{c}Zf | tailF1234");
    let vg = format!("{c} | Zg12345678");
    let vh = format!("{c} | Zh123 | tailH1234");
    let rows = vec![
        password(7, &ve),
        password(8, &vf),
        password(9, &vg),
        password(10, &vh),
    ];
    let blocks = vec![
        para(1, &words("top", 10)),
        trow(2, true, vec![txt("Name"), txt("Value"), txt("Note")]),
        trow(
            3,
            false,
            vec![
                txt(&format!("keep left1 {c}")),
                link("Ze12345678", 97),
                txt("keep note1"),
            ],
        ),
        trow(
            4,
            false,
            vec![
                link(&format!("{c}Zf"), 96),
                txt("tailF1234 keep right2"),
                txt("keep note2"),
            ],
        ),
        trow(
            5,
            false,
            vec![
                txt(&format!("keep l3 {c}")),
                txt("Zg12345678 keep r3"),
                txt("keep note3"),
            ],
        ),
        trow(
            6,
            false,
            vec![
                txt(&format!("keep a4 {c}")),
                link("Zh123", 95),
                txt("tailH1234 keep b4"),
            ],
        ),
        para(7, &words("end", 10)),
    ];
    let s = Secrets::new(vec![
        ("plain | link cell", ve),
        ("link | plain cell", vf),
        ("plain | plain cell", vg),
        ("three cells", vh),
    ]);
    (rows, blocks, s)
}

/// D-229 row 3: a value across three spans: plain `(` link `)` plain; link `.` link (a space
/// is put before the second link); plain, an empty link, plain (two spaces once the empty link
/// form is read as text, so only the whitespace-collapsed reading shows the value); and plain
/// `(` link `)` plain in a code block.
fn three_spans(c: &str) -> (Vec<IndexRow>, Vec<TextBlock>, Secrets) {
    let vi = format!("{c}(Zi12345).tailI1234");
    let vj = format!("{c}. Zj12345678");
    let vk = format!("{c} Zk12345678");
    let vl = format!("{c}(Zl12345678)");
    let rows = vec![
        password(7, &vi),
        password(8, &vj),
        password(9, &vk),
        password(10, &vl),
    ];
    let blocks = vec![
        para(1, &words("top", 10)),
        p(
            2,
            vec![
                txt(&format!("keep one {c}(")),
                link("Zi12345", 94),
                txt(").tailI1234 keep two"),
            ],
        ),
        p(
            3,
            vec![
                link(&format!("x {c}"), 93),
                txt("."),
                link("Zj12345678 y", 92),
            ],
        ),
        p(
            4,
            vec![
                txt(&format!("keep three {c} ")),
                link("", 91),
                txt(" Zk12345678 keep four"),
            ],
        ),
        blk(
            5,
            BlockKind::Code,
            None,
            vec![
                txt(&format!("let k = \"{c}(")),
                link("Zl12345678", 90),
                txt(")\";"),
            ],
        ),
        para(6, &words("end", 10)),
    ];
    let s = Secrets::new(vec![
        ("plain ( link ) plain", vi),
        ("link . link", vj),
        ("plain, empty link, plain", vk),
        ("code block", vl),
    ]);
    (rows, blocks, s)
}

/// D-229 row 4: the same joins in headings, read under a max_tokens small enough that they
/// feed `remaining:`; an h1 with a joined value feeds the skeleton.
fn heading_page(c: &str) -> (Vec<IndexRow>, Vec<TextBlock>, Secrets) {
    let vq = format!("{c}(Zq12345678");
    let vm = format!("{c}(Zm12345678");
    let vn = format!("{c}Zn.tailN1234");
    let vo = format!("{c}(Zo1234).tailO1234");
    let vp = format!("{c}(Zp12345678");
    let rows = vec![
        password(7, &vq),
        password(8, &vm),
        password(9, &vn),
        password(10, &vo),
        password(11, &vp),
        button(5, "Send"),
    ];
    let blocks = vec![
        hd(
            1,
            1,
            vec![txt(&format!("Title keep {c}(")), link("Zq12345678", 86)],
        ),
        para(2, &words("top", 30)),
        hd(
            3,
            2,
            vec![
                txt(&format!("Golf keep {c}(")),
                link("Zm12345678", 89),
                txt(" end golf"),
            ],
        ),
        para(4, &words("pa", 5)),
        hd(
            5,
            2,
            vec![link(&format!("{c}Zn"), 88), txt(".tailN1234 keep hotel")],
        ),
        para(6, &words("pb", 5)),
        hd(
            7,
            3,
            vec![
                txt(&format!("India keep {c}(")),
                link("Zo1234", 87),
                txt(").tailO1234 juliet"),
            ],
        ),
        para(8, &words("pc", 5)),
        hd(
            9,
            2,
            vec![txt(&format!("Kilo keep {c}(")), link("Zp12345678", 5)],
        ),
        para(10, &words("pd", 5)),
    ];
    let s = Secrets::new(vec![
        ("h1 plain ( link", vq),
        ("h2 plain ( link", vm),
        ("h2 link-head . plain", vn),
        ("h3 three spans", vo),
        ("h2 plain ( button label", vp),
    ]);
    (rows, blocks, s)
}

/// D-230 row: plain spans around a joined value keep their own text; an unrelated link in the
/// same block keeps its link form.
fn keep_page(c: &str) -> (Vec<IndexRow>, Vec<TextBlock>, Secrets) {
    let vs = format!("{c}(Zs12345678).tailS1234");
    let vt = format!("{c}(Zt12345678");
    let ve = format!("{c} | Ze12345678");
    let rows = vec![
        password(7, &vs),
        password(8, &vt),
        password(9, &ve),
        row(50, "link", "Docs"),
    ];
    let blocks = vec![
        para(1, &words("top", 30)),
        p(
            2,
            vec![
                txt(&format!("Alpha bravo charlie {c}(")),
                link("Zs12345678", 84),
                txt(").tailS1234 delta echo foxtrot"),
                txt(" golf "),
                link("Docs", 50),
            ],
        ),
        hd(
            3,
            2,
            vec![
                txt(&format!("Lima mike {c}(")),
                link("Zt12345678", 83),
                txt(" november oscar"),
            ],
        ),
        para(4, &words("pa", 5)),
        trow(5, true, vec![txt("Name"), txt("Value"), txt("Note")]),
        trow(
            6,
            false,
            vec![
                txt(&format!("keep left1 {c}")),
                link("Ze12345678", 97),
                txt("keep note1"),
            ],
        ),
        para(7, &words("end", 5)),
    ];
    let s = Secrets::new(vec![
        ("paragraph three spans", vs),
        ("heading", vt),
        ("table", ve),
    ]);
    (rows, blocks, s)
}

/// One placement of each kind above on one short page, for the cut sweep (a read renders the
/// whole page, so the page is kept small to keep the sweep fast in debug). The last paragraph
/// glues a word to the hidden text, so a char cut can land inside `glued***`.
fn sweep_page(c: &str) -> (Vec<IndexRow>, Vec<TextBlock>, Secrets) {
    let alias = alias_for("button");
    let vals: Vec<(&'static str, String)> = vec![
        ("heading plain ( link", format!("{c}(Zm12345678")),
        ("link-head paragraph", format!("{c}Za.tailA1234")),
        ("button-label head", format!("{c}Zc.tailC1234")),
        ("renderer-reading only", format!("{c}({alias} \"Zd1234")),
        ("plain | link cell", format!("{c} | Ze12345678")),
        ("plain ( link ) plain", format!("{c}(Zi12345).tailI1234")),
        ("plain, empty link, plain", format!("{c} Zk12345678")),
        ("code block", format!("{c}(Zl12345678)")),
        ("glued word", format!("{c}(Zr12345678")),
    ];
    let mut rows: Vec<IndexRow> = vals
        .iter()
        .enumerate()
        .map(|(i, (_, v))| password(200 + i as u32, v))
        .collect();
    rows.push(button(5, "Send"));
    rows.push(button(6, "Go"));
    let blocks = vec![
        para(1, "aa00 aa01 aa02"),
        hd(
            2,
            2,
            vec![
                txt(&format!("Golf {c}(")),
                link("Zm12345678", 89),
                txt(" end"),
            ],
        ),
        p(
            3,
            vec![
                txt("go "),
                link(&format!("x {c}Za"), 99),
                txt(".tailA1234 keep"),
            ],
        ),
        blk(
            4,
            BlockKind::Quote,
            None,
            vec![link(&format!("{c}Zc"), 5), txt(".tailC1234 keep")],
        ),
        p(5, vec![txt(&format!("keep {c}(")), link("Zd1234567", 6)]),
        trow(6, true, vec![txt("Name"), txt("Value")]),
        trow(
            7,
            false,
            vec![txt(&format!("left {c}")), link("Ze12345678", 97)],
        ),
        p(
            8,
            vec![
                txt(&format!("one {c}(")),
                link("Zi12345", 94),
                txt(").tailI1234 two"),
            ],
        ),
        p(
            9,
            vec![
                txt(&format!("three {c} ")),
                link("", 91),
                txt(" Zk12345678 four"),
            ],
        ),
        blk(
            10,
            BlockKind::Code,
            None,
            vec![
                txt(&format!("let k = \"{c}(")),
                link("Zl12345678", 90),
                txt(")\";"),
            ],
        ),
        p(11, vec![txt(&format!("glued{c}(")), link("Zr12345678", 85)]),
        para(12, "zz00 zz01"),
    ];
    (rows, blocks, Secrets::new(vals))
}

// ---------------------------------------------------------------- tests

/// D-229: link + plain with the value starting in the link (renderer and label readings).
#[test]
fn d229_value_starting_in_the_link_never_shows() {
    let c = canary();
    let (rows, blocks, s) = link_then_plain(&c);
    let leaks = check_page(&rows, &blocks, &s);
    assert!(leaks.is_empty(), "joined value shows: {leaks:?}");
    let b = full_body(&rows, &blocks);
    // D-229: a contributing link span keeps no link form; others keep theirs.
    for gone in ["](#99)", "](#98)", "](#5)", "](#6)"] {
        assert!(!b.contains(gone), "contributing link form {gone} kept");
    }
    assert!(b.contains("[Docs](#50)"), "an unrelated link lost its form");
    // D-230: the plain spans keep their other text.
    for keep in [
        "intro",
        "keep alpha see",
        "keep bravo",
        "keep charlie",
        "keep delta",
    ] {
        assert!(b.contains(keep), "plain text {keep:?} lost");
    }
}

/// D-229: a value across the ` | ` between table cells, with link or plain cells and over three
/// cells.
#[test]
fn d229_value_across_a_table_cell_separator_never_shows() {
    let c = canary();
    let (rows, blocks, s) = table_page(&c);
    let leaks = check_page(&rows, &blocks, &s);
    assert!(leaks.is_empty(), "joined value shows: {leaks:?}");
    let b = full_body(&rows, &blocks);
    for gone in ["](#97)", "](#96)", "](#95)"] {
        assert!(!b.contains(gone), "contributing link form {gone} kept");
    }
    for keep in [
        "keep left1",
        "keep note1",
        "keep right2",
        "keep note2",
        "keep a4",
        "keep b4",
    ] {
        assert!(b.contains(keep), "cell text {keep:?} lost");
    }
    // A row whose value runs through a link cell keeps its three cells.
    let r1 = b
        .lines()
        .find(|l| l.contains("keep left1"))
        .expect("row 1 is in the body");
    assert_eq!(
        r1.matches('|').count(),
        4,
        "row 1 lost a cell separator: {} cells",
        r1.matches('|').count().saturating_sub(1)
    );
}

/// D-229: a value across three spans (plain/link/plain, link/plain/link, plain/empty
/// link/plain), also in a code block.
#[test]
fn d229_value_across_three_spans_never_shows() {
    let c = canary();
    let (rows, blocks, s) = three_spans(&c);
    let leaks = check_page(&rows, &blocks, &s);
    assert!(leaks.is_empty(), "joined value shows: {leaks:?}");
    let b = full_body(&rows, &blocks);
    for gone in ["](#94)", "](#93)", "](#92)", "](#90)"] {
        assert!(!b.contains(gone), "contributing link form {gone} kept");
    }
    for keep in ["keep one", "keep two", "keep three", "keep four", "let k ="] {
        assert!(b.contains(keep), "plain text {keep:?} lost");
    }
}

/// D-229 + D-226 (3): the same joins in headings feed `remaining:` and the skeleton; the labels
/// keep the plain heading text (D-230).
#[test]
fn d229_joined_value_in_a_heading_never_reaches_remaining_or_skeleton() {
    let c = canary();
    let (rows, blocks, s) = heading_page(&c);
    let mut leaks = check_page(&rows, &blocks, &s);
    let mut entries_seen = 0usize;
    for chunk in [0usize, 10, 30] {
        for m in [3usize, 5, 8, 13, 21, 40] {
            let outs = read_chain(&rows, &blocks, &opts(chunk, Some(m)), m + 4);
            entries_seen += outs.iter().map(|o| o.remaining.len()).sum::<usize>();
            s.leaks_in_chain(&outs, &format!("chunk {chunk} max {m}"), &mut leaks);
        }
    }
    let sk = skeleton(&rows, &blocks, &SkeletonOpts::default());
    if let Some(l) = s.leak_in(&sk) {
        leaks.push(format!("skeleton: {l}"));
    }
    assert!(leaks.is_empty(), "joined value shows: {leaks:?}");
    assert!(entries_seen > 0, "no read had a remaining: outline");
    // The first read under max 5 has the headings in remaining:, labels keeping plain text.
    let o = read(&rows, &blocks, &opts(0, Some(5)));
    let labels: Vec<&str> = o.remaining.iter().map(|e| e.heading.as_str()).collect();
    for keep in ["Golf keep", "keep hotel", "India keep", "Kilo keep"] {
        assert!(
            labels.iter().any(|l| l.contains(keep)),
            "no remaining: label keeps {keep:?} ({} entries)",
            labels.len()
        );
    }
    assert!(
        labels.iter().any(|l| l.contains("***")),
        "no label shows the *** of a hidden value"
    );
    assert!(
        sk.contains("Title keep"),
        "the skeleton h1 lost its plain text"
    );
}

/// D-229 under small chunk_tokens and max_tokens: cuts land inside hidden regions; every cursor
/// chain is followed to its end; no read, no `remaining:` entry, no cursor and no chain shows
/// any 8-char window of a value (so no prefix of a value either).
#[test]
fn d229_cut_inside_a_hidden_region_never_shows_part_of_a_value() {
    let c = canary();
    let (rows, blocks, s) = sweep_page(&c);
    let mut leaks = Vec::new();
    let mut cuts_in_hidden = 0usize;
    let mut chains = 0usize;
    // chunk 1 with no max_tokens reads the whole page 4 chars at a time; max_tokens chains stop
    // at budgets spread over the page, so cuts land in every block.
    let mut sweep: Vec<(usize, Option<usize>)> = vec![(1, None)];
    for m in [3usize, 9, 17, 26, 38, 51, 67] {
        sweep.push((0, Some(m)));
    }
    for m in [5usize, 23, 47] {
        sweep.push((3, Some(m)));
    }
    for (chunk, m) in sweep {
        let bound = m.map_or(2000, |m| m + 4);
        let outs = read_chain(&rows, &blocks, &opts(chunk, m), bound);
        chains += 1;
        s.leaks_in_chain(&outs, &format!("chunk {chunk} max {m:?}"), &mut leaks);
        for w in outs.windows(2) {
            let a = body(&w[0].markdown).trim_end();
            let b = body(&w[1].markdown).trim_start();
            if a.ends_with('*') && b.starts_with('*') {
                cuts_in_hidden += 1;
            }
        }
    }
    assert!(leaks.is_empty(), "part of a value shows: {leaks:?}");
    assert_eq!(chains, 11, "sweep size");
    assert!(
        cuts_in_hidden > 0,
        "no cut landed inside a hidden region: the case is not reached"
    );
}

/// D-230: a plain span hides only the characters it gives to a joined value; its other text
/// stays, in a paragraph, a heading (body and `remaining:` label) and a table cell. A contributing
/// link renders as `***` with no form; an unrelated link keeps its form.
#[test]
fn d230_plain_span_keeps_its_non_value_text() {
    let c = canary();
    let (rows, blocks, s) = keep_page(&c);
    let leaks = check_page(&rows, &blocks, &s);
    assert!(leaks.is_empty(), "joined value shows: {leaks:?}");
    let b = full_body(&rows, &blocks);
    let para_line = b
        .lines()
        .find(|l| l.contains("Alpha"))
        .expect("the paragraph is in the body");
    assert!(
        para_line.starts_with("Alpha bravo charlie ***"),
        "the paragraph's head text was not kept before the hidden value"
    );
    assert!(
        para_line.contains("delta echo foxtrot golf [Docs](#50)"),
        "the paragraph's tail text or the unrelated link was not kept"
    );
    assert!(!para_line.contains("](#84)"), "contributing link form kept");
    assert!(
        para_line.matches("***").count() <= 3,
        "more than one *** per hidden run"
    );
    let head_line = b
        .lines()
        .find(|l| l.starts_with("## Lima"))
        .expect("the heading is in the body");
    assert!(
        head_line.starts_with("## Lima mike ***") && head_line.ends_with("november oscar"),
        "the heading's plain text was not kept"
    );
    let cell_line = b
        .lines()
        .find(|l| l.contains("keep left1"))
        .expect("the row is in the body");
    assert!(
        cell_line.contains("keep note1") && cell_line.contains("***"),
        "the row lost its plain text"
    );
    let o = read(&rows, &blocks, &opts(0, Some(5)));
    let label = o
        .remaining
        .iter()
        .find(|e| e.heading.contains("Lima"))
        .map(|e| e.heading.clone())
        .expect("the Lima heading is in remaining:");
    assert!(
        label.contains("Lima mike") && label.contains("november oscar"),
        "the remaining: label lost the heading's plain text"
    );
}
/// D-231 (review round 7 blocking, extends D-229): a value holding both the cell separator and
/// a `|` inside a cell crosses a table cell boundary. The row renders the in-cell pipe as `\|`,
/// which a markdown renderer shows as `|`, so the renderer's reading of the row holds the whole
/// value. Rows: plain | plain, link | plain, plain | link | plain, and a pipe in the first cell
/// with a link after. Red-first at c8bd2e4 (D-231 recorded, not yet implemented): committed
/// `#[ignore]`d so the suite stays green; run with `--ignored`.
#[test]
#[ignore = "red-first: D-231 not implemented at c8bd2e4"]
fn d231_value_with_an_in_cell_pipe_across_cells_never_shows() {
    let c = canary();
    let v1 = format!("{c} | Zu12|tail91");
    let v2 = format!("{c}Zv | Zv12|tail92");
    let v3 = format!("{c} | Zw|12 | tail93");
    let v4 = format!("{c}|Zx | Zx12345678");
    let rows = vec![
        password(7, &v1),
        password(8, &v2),
        password(9, &v3),
        password(10, &v4),
    ];
    let blocks = vec![
        para(1, &words("top", 10)),
        trow(2, true, vec![txt("Name"), txt("Value"), txt("Note")]),
        trow(
            3,
            false,
            vec![
                txt(&format!("keep x1 {c}")),
                txt("Zu12|tail91 keep y1"),
                txt("keep z1"),
            ],
        ),
        trow(
            4,
            false,
            vec![
                link(&format!("{c}Zv"), 81),
                txt("Zv12|tail92 keep y2"),
                txt("keep z2"),
            ],
        ),
        trow(
            5,
            false,
            vec![
                txt(&format!("keep x3 {c}")),
                link("Zw|12", 80),
                txt("tail93 keep z3"),
            ],
        ),
        trow(
            6,
            false,
            vec![
                txt(&format!("keep x4 {c}|Zx")),
                link("Zx12345678", 79),
                txt("keep z4"),
            ],
        ),
        para(7, &words("end", 10)),
    ];
    let named = vec![
        ("plain | plain with an in-cell pipe", v1),
        ("link | plain with an in-cell pipe", v2),
        ("plain | link | plain with an in-cell pipe", v3),
        ("pipe in cell 1, link after", v4),
    ];
    let s = Secrets::new(named.clone());
    let mut leaks = check_page(&rows, &blocks, &s);
    // The renderer's reading with `\|` read as `|`: name each value it shows whole.
    for chunk in [0usize, 30] {
        let outs = read_chain(&rows, &blocks, &opts(chunk, None), 200);
        for (i, o) in outs.iter().enumerate() {
            let shown = visible(&o.markdown).replace("\\|", "|");
            for (name, v) in &named {
                if shown.contains(v.as_str()) {
                    leaks.push(format!(
                        "chunk {chunk} read {i}: pipes unescaped: {name} whole"
                    ));
                }
            }
        }
    }
    assert!(leaks.is_empty(), "joined value shows: {leaks:?}");
}
