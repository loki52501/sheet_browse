//! T-10 acceptance (tester, rounds 1 and 2): docs/05 B-6 (acceptance 6, pure half of 7, B-6.1, B-6.3)
//! and docs/04 W-5 (pure half). Fixtures here are built by hand from bench/pages/register.html
//! and an invented article page, independently of the coders' tests and of
//! examples/register.index.json.
//!
//! Item ids in test names match bench/results/accept-T-10.md.

use sb_sheet::tokens::estimate;
use sb_sheet::{
    find, find_lines, read, sheet, sheet_line, skeleton, BlockKind, Cursor, FieldValue, FindOpts,
    HitReason, IndexRow, PageMeta, ReadOpts, ReadOutput, SheetOpts, SkeletonOpts, Span, State,
    TextBlock,
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
        landmark: None,
        child_frame: None,
        bbox: None,
    }
}

trait RowExt {
    fn lm(self, l: &str) -> Self;
    fn hd(self, h: &str) -> Self;
    fn fm(self, f: &str) -> Self;
    fn near(self, n: &str) -> Self;
    fn hidden(self) -> Self;
    fn input(self, t: &str) -> Self;
    fn auto(self, a: &str) -> Self;
    fn val(self, v: FieldValue) -> Self;
    fn submit(self) -> Self;
    fn frame(self, f: u32) -> Self;
}

impl RowExt for IndexRow {
    fn lm(mut self, l: &str) -> Self {
        self.landmark = Some(l.into());
        self
    }
    fn hd(mut self, h: &str) -> Self {
        self.heading = Some(h.into());
        self
    }
    fn fm(mut self, f: &str) -> Self {
        self.form = Some(f.into());
        self
    }
    fn near(mut self, n: &str) -> Self {
        self.nearby = Some(n.into());
        self
    }
    fn hidden(mut self) -> Self {
        self.state.visible = false;
        self
    }
    fn input(mut self, t: &str) -> Self {
        self.state.input_type = Some(t.into());
        self
    }
    fn auto(mut self, a: &str) -> Self {
        self.state.autocomplete = Some(a.into());
        self
    }
    fn val(mut self, v: FieldValue) -> Self {
        self.state.value = Some(v);
        self
    }
    fn submit(mut self) -> Self {
        self.flags.submit = true;
        self.state.submitish = true;
        self
    }
    fn frame(mut self, f: u32) -> Self {
        self.frame = f;
        self
    }
}

fn span(t: &str) -> Span {
    Span {
        text: t.into(),
        elem: None,
    }
}

fn link(t: &str, id: u32) -> Span {
    Span {
        text: t.into(),
        elem: Some(sb_sheet::ElemRef::main(id)),
    }
}

fn blk(
    order: u32,
    kind: BlockKind,
    level: Option<u8>,
    landmark: Option<&str>,
    heading: Option<&str>,
    spans: Vec<Span>,
) -> TextBlock {
    TextBlock {
        order,
        frame: 0,
        kind,
        level,
        landmark: landmark.map(Into::into),
        heading: heading.map(Into::into),
        table: None,
        header: false,
        spans,
    }
}

/// Ids at the start of each `#id ...` line (optionally after a `?` weak mark), in output order.
fn line_ids(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim_start().trim_start_matches('?').trim_start();
            l.starts_with('#')
                .then(|| l.split_whitespace().next().unwrap_or("").to_string())
        })
        .collect()
}

fn ids(v: &[u32]) -> Vec<String> {
    v.iter().map(|i| format!("#{i}")).collect()
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

// ------------------------------------------------- register.html, walked by hand

/// bench/pages/register.html in walk order. The promo subtree is display:none (visible false);
/// .g-recaptcha + its iframe are ONE row flagged CHALLENGE + OPAQUE (D-55); the cookie banner
/// container is the OVERLAY row.
fn register_rows() -> Vec<IndexRow> {
    vec![
        row(1, "link", "Home").lm("nav"),
        row(2, "link", "Pricing").lm("nav"),
        row(3, "link", "Sign in").lm("nav"),
        // .promo, display:none
        row(4, "textbox", "Email for the offer")
            .lm("main")
            .hd("Limited offer")
            .near("Register now and get 50% off your first year.")
            .input("email")
            .val(FieldValue::Empty)
            .hidden(),
        row(5, "button", "Register for the offer")
            .lm("main")
            .hd("Limited offer")
            .hidden(),
        // form#signup
        row(6, "textbox", "Email")
            .lm("main")
            .hd("Create your account")
            .fm("f1")
            .input("email")
            .auto("email")
            .val(FieldValue::Empty),
        row(7, "textbox", "Password")
            .lm("main")
            .hd("Create your account")
            .fm("f1")
            .input("password")
            .auto("new-password")
            .val(FieldValue::Empty),
        row(8, "textbox", "Full name")
            .lm("main")
            .hd("Create your account")
            .fm("f1")
            .input("text")
            .val(FieldValue::Empty),
        {
            let mut r = row(9, "combobox", "Plan")
                .lm("main")
                .hd("Create your account")
                .fm("f1")
                .val(FieldValue::Text("Basic".into()));
            r.state.options = vec!["Basic".into(), "Plus".into(), "Pro".into()];
            r
        },
        {
            let mut r = row(10, "checkbox", "I agree to the Terms of Service")
                .lm("main")
                .hd("Create your account")
                .fm("f1")
                .input("checkbox");
            r.state.checked = Some(false);
            r
        },
        {
            let mut r = row(11, "iframe", "reCAPTCHA")
                .lm("main")
                .hd("Create your account")
                .fm("f1");
            r.flags.challenge = true;
            r.flags.opaque = true;
            r.child_frame = Some(1);
            r
        },
        row(12, "button", "Register")
            .lm("main")
            .hd("Create your account")
            .fm("f1")
            .submit(),
        // footer, form#newsletter
        row(13, "textbox", "Email")
            .lm("footer")
            .hd("Stay in the loop")
            .fm("f2")
            .input("email")
            .val(FieldValue::Empty),
        row(14, "button", "Subscribe")
            .lm("footer")
            .hd("Stay in the loop")
            .fm("f2")
            .submit(),
        row(15, "link", "Privacy")
            .lm("footer")
            .hd("Stay in the loop"),
        row(16, "link", "Terms").lm("footer").hd("Stay in the loop"),
        // #cookie-banner, position:fixed
        {
            let mut r = row(17, "region", "Cookie consent").lm("region");
            r.flags.overlay = true;
            r
        },
        row(18, "button", "Accept all")
            .lm("region")
            .near("We use cookies to run this site."),
        row(19, "button", "Reject")
            .lm("region")
            .near("We use cookies to run this site."),
    ]
}

fn register_blocks() -> Vec<TextBlock> {
    use BlockKind::*;
    vec![
        blk(
            0,
            Paragraph,
            None,
            Some("nav"),
            None,
            vec![link("Home", 1), link("Pricing", 2), link("Sign in", 3)],
        ),
        blk(
            1,
            Heading,
            Some(1),
            Some("main"),
            None,
            vec![span("Create your account")],
        ),
        blk(
            2,
            Paragraph,
            None,
            Some("main"),
            Some("Create your account"),
            vec![span("Start your free trial. No card required.")],
        ),
        blk(
            3,
            Paragraph,
            None,
            Some("main"),
            Some("Create your account"),
            vec![span("Email")],
        ),
        blk(
            4,
            Paragraph,
            None,
            Some("main"),
            Some("Create your account"),
            vec![span("Plan")],
        ),
        blk(
            5,
            Heading,
            Some(2),
            Some("footer"),
            None,
            vec![span("Stay in the loop")],
        ),
        blk(
            6,
            Paragraph,
            None,
            Some("footer"),
            Some("Stay in the loop"),
            vec![span("Email")],
        ),
        blk(
            7,
            Paragraph,
            None,
            Some("footer"),
            Some("Stay in the loop"),
            vec![link("Privacy", 15), span(" · "), link("Terms", 16)],
        ),
        blk(
            8,
            Paragraph,
            None,
            Some("region"),
            None,
            vec![span("We use cookies to run this site.")],
        ),
    ]
}

fn register_meta(site_tools: Option<&str>) -> SkeletonOpts {
    SkeletonOpts {
        meta: PageMeta {
            url: "http://localhost:8137/bench/pages/register.html".into(),
            title: "Create your account · Fixture Co".into(),
            site_tools: site_tools.map(Into::into),
            dialog: None,
        },
    }
}

fn fopts(q: &str, k: usize) -> FindOpts {
    FindOpts { query: q.into(), k }
}

// ---------------------------------------------------------------- A6

#[test]
fn a6_find_register_four_plus_challenge_and_overlay_in_document_order() {
    let rows = register_rows();
    for blocks in [Vec::new(), register_blocks()] {
        let hits = find(
            &rows,
            &blocks,
            &fopts("email, password, agree, register", 1),
        );
        let got: Vec<String> = hits.iter().map(|h| h.elem.to_string()).collect();
        assert_eq!(
            got,
            ids(&[6, 7, 10, 11, 12, 17]),
            "find hits (document order) with {} text blocks",
            blocks.len()
        );
        let by_id = |id: u32| hits.iter().find(|h| h.elem.id == id).unwrap();
        for (id, part) in [(6, 0), (7, 1), (10, 2), (12, 3)] {
            let h = by_id(id);
            assert_eq!(h.reason, HitReason::Match { part }, "reason of #{id}");
            assert!(!h.weak, "#{id} should not be weak");
        }
        assert_eq!(by_id(11).reason, HitReason::Challenge);
        assert_eq!(by_id(17).reason, HitReason::Overlay);

        // rendered lines carry the same ids in the same order, no `?` on the four
        let lines = find_lines(&rows, &hits);
        assert_eq!(line_ids(&lines), ids(&[6, 7, 10, 11, 12, 17]), "{lines}");
        assert!(
            !lines.lines().any(|l| l.trim_start().starts_with('?')),
            "{lines}"
        );
        assert!(lines.contains("CHALLENGE") && lines.contains("OVERLAY"));
    }
}

#[test]
fn a6_never_hidden_promo_and_newsletter_ranks_below_signup() {
    let rows = register_rows();
    let hits = find(&rows, &[], &fopts("email, password, agree, register", 3));
    let got: Vec<u32> = hits.iter().map(|h| h.elem.id).collect();
    assert!(
        !got.contains(&4) && !got.contains(&5),
        "hidden promo surfaced: {got:?}"
    );
    // document order holds for k = 3 too
    let mut sorted = got.clone();
    sorted.sort();
    assert_eq!(got, sorted);
    // with k = 3 the newsletter Email may appear but scores below the signup Email
    if let Some(nl) = hits.iter().find(|h| h.elem.id == 13) {
        let su = hits.iter().find(|h| h.elem.id == 6).unwrap();
        assert!(
            su.score > nl.score,
            "signup {} vs newsletter {}",
            su.score,
            nl.score
        );
    }
}

#[test]
fn a6_document_order_follows_index_order_not_id() {
    // Ids are stable and never reused (D-55): an element inserted later gets a fresh, higher
    // id but sits earlier in the document. Output order is the index (walk) order.
    let mut rows = register_rows();
    let pos = rows.iter().position(|r| r.id == 7).unwrap();
    rows[pos].id = 99; // the password field was re-rendered and got a new id
    let hits = find(&rows, &[], &fopts("email, password, agree, register", 1));
    let got: Vec<String> = hits.iter().map(|h| h.elem.to_string()).collect();
    assert_eq!(got, ids(&[6, 99, 10, 11, 12, 17]));
}

// ---------------------------------------------------------------- B61

#[test]
fn b61_skeleton_register_budget_and_content() {
    let rows = register_rows();
    let blocks = register_blocks();
    let tools = "tools for this site: fixture.register(email,password)";
    for (site_tools, label) in [(None, "no site tools"), (Some(tools), "with site tools")] {
        let opts = register_meta(site_tools);
        let s = skeleton(&rows, &blocks, &opts);
        let est = estimate(&s);
        let own = s.chars().count().div_ceil(4); // D-200, computed independently
        assert_eq!(est, own, "tokens::estimate disagrees with ceil(chars/4)");
        assert!((80..=150).contains(&own), "{label}: {own} tokens\n{s}");

        assert!(s.contains("Create your account · Fixture Co"), "title\n{s}");
        assert!(s.contains(&opts.meta.url), "url\n{s}");
        let f1 = s
            .lines()
            .find(|l| l.contains("f1"))
            .unwrap_or_else(|| panic!("no f1 line\n{s}"));
        assert!(
            f1.contains("5 fields") && f1.contains("Register"),
            "f1: {f1}"
        );
        let f2 = s
            .lines()
            .find(|l| l.contains("f2"))
            .unwrap_or_else(|| panic!("no f2 line\n{s}"));
        assert!(
            f2.contains("1 field") && f2.contains("Subscribe"),
            "f2: {f2}"
        );
        let ch = s.lines().find(|l| l.contains("CHALLENGE")).unwrap_or("");
        assert!(ch.contains("#11"), "challenge warning\n{s}");
        let ov = s.lines().find(|l| l.contains("OVERLAY")).unwrap_or("");
        assert!(ov.contains("#17"), "overlay warning\n{s}");
        match site_tools {
            Some(t) => assert!(s.lines().any(|l| l == t), "site_tools line verbatim\n{s}"),
            None => assert!(!s.contains("tools for this site"), "{s}"),
        }
        // hidden promo never counted or named
        assert!(
            !s.contains("Limited offer") && !s.contains("for the offer"),
            "{s}"
        );
        // no em dashes in crate output (task rule; CLAUDE.md 7)
        assert!(!s.contains('\u{2014}'), "{s}");
    }
}

// ---------------------------------------------------------------- B63

fn sheet_of(rows: &[IndexRow], region: Option<&str>) -> String {
    sheet(
        rows,
        &[],
        &SheetOpts {
            region: region.map(Into::into),
        },
    )
}

#[test]
fn b63_sheet_region_landmark_form_heading() {
    let rows = register_rows();
    // landmark
    assert_eq!(line_ids(&sheet_of(&rows, Some("nav"))), ids(&[1, 2, 3]));
    assert_eq!(
        line_ids(&sheet_of(&rows, Some("footer"))),
        ids(&[13, 14, 15, 16])
    );
    assert_eq!(
        line_ids(&sheet_of(&rows, Some("region"))),
        ids(&[17, 18, 19])
    );
    // form id
    let f1 = sheet_of(&rows, Some("f1"));
    assert_eq!(line_ids(&f1), ids(&[6, 7, 8, 9, 10, 11, 12]), "{f1}");
    assert_eq!(line_ids(&sheet_of(&rows, Some("f2"))), ids(&[13, 14]));
    // heading (exact text, and `## Heading` form)
    assert_eq!(
        line_ids(&sheet_of(&rows, Some("Stay in the loop"))),
        ids(&[13, 14, 15, 16])
    );
    assert_eq!(
        line_ids(&sheet_of(&rows, Some("## Create your account"))),
        ids(&[6, 7, 8, 9, 10, 11, 12])
    );
    // hidden rows are listed by sheet, marked hidden (D-202)
    let promo = sheet_of(&rows, Some("Limited offer"));
    assert_eq!(line_ids(&promo), ids(&[4, 5]), "{promo}");
    assert!(promo.lines().all(|l| l.contains("hidden")), "{promo}");
    // whole page: every row once, in order
    let all = sheet_of(&rows, None);
    assert_eq!(line_ids(&all), ids(&(1..=19).collect::<Vec<_>>()), "{all}");
    // the lines are the shared renderer's lines (D-203)
    for r in &rows {
        assert!(
            all.lines().any(|l| l.trim() == sheet_line(r).trim()),
            "#{}",
            r.id
        );
    }
    assert!(!all.contains('\u{2014}'));
}

#[test]
fn b63_sheet_no_cap_and_document_order() {
    // 600 links in main, ids deliberately not monotonic with document order
    let mut rows = vec![row(1, "link", "Skip to content").lm("nav")];
    for i in 0..600u32 {
        let id = if i % 2 == 0 { 10_000 - i } else { 20 + i };
        rows.push(row(id, "link", &format!("Result {i}")).lm("main"));
    }
    rows.push(row(5, "link", "Imprint").lm("footer"));
    let want: Vec<String> = rows[1..601].iter().map(|r| format!("#{}", r.id)).collect();
    let s = sheet_of(&rows, Some("main"));
    assert_eq!(
        line_ids(&s),
        want,
        "sheet(main) must list all 600 in index order"
    );
    let all = sheet_of(&rows, None);
    assert_eq!(line_ids(&all).len(), 602);
}

// ---------------------------------------------------------------- article page (A7, W5)

/// An invented article page: nav + header + main (h1, 4 h2 sections, list, table, a block much
/// larger than the chunk budget) + aside + footer. Every main text unit carries a unique marker.
fn article() -> (Vec<IndexRow>, Vec<TextBlock>) {
    use BlockKind::*;
    let rows = vec![
        row(1, "link", "Navhome").lm("nav"),
        row(2, "link", "Navblog").lm("nav"),
        row(3, "link", "Navpricing").lm("nav"),
        row(4, "link", "Navlogin").lm("nav"),
        row(5, "link", "tokio spawn")
            .lm("main")
            .hd("Getting Started"),
        {
            let mut r = row(6, "button", "Copy snippet")
                .lm("main")
                .hd("Configuration");
            r.state.submitish = false;
            r
        },
        row(7, "link", "Footimprint").lm("footer"),
        row(8, "link", "Asiderelated").lm("aside"),
    ];
    let mut b = Vec::new();
    let mut o = 0u32;
    let mut push = |b: &mut Vec<TextBlock>, mut t: TextBlock| {
        t.order = o;
        o += 1;
        b.push(t);
    };
    push(
        &mut b,
        blk(
            0,
            Paragraph,
            None,
            Some("nav"),
            None,
            vec![
                link("Navhome", 1),
                link("Navblog", 2),
                link("Navpricing", 3),
                link("Navlogin", 4),
            ],
        ),
    );
    push(
        &mut b,
        blk(
            0,
            Paragraph,
            None,
            Some("header"),
            None,
            vec![span("Headerbanner site tagline for everyone")],
        ),
    );
    push(
        &mut b,
        blk(
            0,
            Heading,
            Some(1),
            Some("main"),
            None,
            vec![span("Tokio Runtime Guide")],
        ),
    );
    let para = |m: &str, h: &str| {
        blk(
            0,
            Paragraph,
            None,
            Some("main"),
            Some(h),
            vec![span(&format!(
                "{m} explains how the runtime schedules tasks across worker threads and why \
                 blocking calls should move to a dedicated pool so other tasks keep running."
            ))],
        )
    };
    push(&mut b, para("Pmark01", "Tokio Runtime Guide"));
    push(
        &mut b,
        blk(
            0,
            Heading,
            Some(2),
            Some("main"),
            Some("Tokio Runtime Guide"),
            vec![span("Getting Started")],
        ),
    );
    push(
        &mut b,
        blk(
            0,
            Paragraph,
            None,
            Some("main"),
            Some("Getting Started"),
            vec![
                span("Pmark02 start with "),
                link("tokio spawn", 5),
                span(" to run a future in the background and await its handle later on."),
            ],
        ),
    );
    for m in ["Lmark01", "Lmark02", "Lmark03"] {
        let mut li = blk(
            0,
            ListItem,
            Some(1),
            Some("main"),
            Some("Getting Started"),
            vec![span(&format!("{m} enable the full feature set in Cargo"))],
        );
        li.level = Some(1);
        push(&mut b, li);
    }
    push(
        &mut b,
        blk(
            0,
            Heading,
            Some(2),
            Some("main"),
            Some("Tokio Runtime Guide"),
            vec![span("Configuration")],
        ),
    );
    push(&mut b, para("Pmark03", "Configuration"));
    for (i, cells) in [
        ["Tcolkey", "Tcolvalue"],
        ["Tmarkworkers", "Tmarkeight"],
        ["Tmarkstack", "Tmarktwomb"],
    ]
    .iter()
    .enumerate()
    {
        let mut tr = blk(
            0,
            TableRow,
            None,
            Some("main"),
            Some("Configuration"),
            cells.iter().map(|c| span(c)).collect(),
        );
        tr.table = Some(0);
        tr.header = i == 0;
        push(&mut b, tr);
    }
    push(
        &mut b,
        blk(
            0,
            Paragraph,
            None,
            Some("main"),
            Some("Configuration"),
            vec![
                span("Pmark04 copy the snippet with "),
                link("Copy snippet", 6),
            ],
        ),
    );
    push(
        &mut b,
        blk(
            0,
            Heading,
            Some(2),
            Some("main"),
            Some("Tokio Runtime Guide"),
            vec![span("Big Section")],
        ),
    );
    let words: Vec<String> = (1..=400).map(|i| format!("wd{i:03}q")).collect();
    // sentences of 10 words so the splitter has boundaries
    let big: String = words
        .chunks(10)
        .map(|c| format!("{}.", c.join(" ")))
        .collect::<Vec<_>>()
        .join(" ");
    push(
        &mut b,
        blk(
            0,
            Paragraph,
            None,
            Some("main"),
            Some("Big Section"),
            vec![span(&big)],
        ),
    );
    push(
        &mut b,
        blk(
            0,
            Heading,
            Some(2),
            Some("main"),
            Some("Tokio Runtime Guide"),
            vec![span("FAQ")],
        ),
    );
    push(&mut b, para("Pmark05", "FAQ"));
    push(&mut b, para("Pmark06", "FAQ"));
    push(
        &mut b,
        blk(
            0,
            Paragraph,
            None,
            Some("aside"),
            None,
            vec![link("Asiderelated", 8)],
        ),
    );
    push(
        &mut b,
        blk(
            0,
            Paragraph,
            None,
            Some("footer"),
            None,
            vec![span("Footcopyright 2026 "), link("Footimprint", 7)],
        ),
    );
    (rows, b)
}

const MAIN_MARKERS: [&str; 15] = [
    "Pmark01",
    "Pmark02",
    "Pmark03",
    "Pmark04",
    "Pmark05",
    "Pmark06",
    "Lmark01",
    "Lmark02",
    "Lmark03",
    "Tmarkworkers",
    "Tmarkeight",
    "Tmarkstack",
    "Tmarktwomb",
    "Tcolkey",
    "Tcolvalue",
];
const CHROME_MARKERS: [&str; 8] = [
    "Navhome",
    "Navblog",
    "Navpricing",
    "Navlogin",
    "Headerbanner",
    "Footcopyright",
    "Footimprint",
    "Asiderelated",
];

fn ropts(chunk_tokens: usize, cursor: Option<Cursor>) -> ReadOpts {
    ReadOpts {
        region: None,
        cursor,
        max_tokens: None,
        chunk_tokens,
    }
}

/// Reads from the start following cursors to the end; panics on a loop.
fn read_all(rows: &[IndexRow], blocks: &[TextBlock], chunk_tokens: usize) -> Vec<ReadOutput> {
    let mut out = vec![read(rows, blocks, &ropts(chunk_tokens, None))];
    while let Some(c) = out.last().unwrap().cursor.clone() {
        assert!(out.len() < 200, "cursor never ends");
        out.push(read(rows, blocks, &ropts(chunk_tokens, Some(c))));
    }
    out
}

#[test]
fn a7_article_markdown_without_nav() {
    let (rows, blocks) = article();
    let r = read(&rows, &blocks, &ropts(4000, None));
    assert_eq!((r.chunk, r.chunks), (1, 1), "fits in one 4000-token chunk");
    assert!(r.cursor.is_none());
    for m in MAIN_MARKERS {
        assert_eq!(r.markdown.matches(m).count(), 1, "{m}\n{}", r.markdown);
    }
    for m in CHROME_MARKERS {
        assert!(!r.markdown.contains(m), "chrome {m} leaked\n{}", r.markdown);
    }
    for id in [1, 2, 3, 4, 7, 8] {
        assert!(!r.markdown.contains(&format!("(#{id})")), "link #{id}");
    }
    // actionable things stay actionable (W-5)
    assert!(r.markdown.contains("(#5)") && r.markdown.contains("(#6)"));
    assert!(r.markdown.contains("## Getting Started"));
    assert!(r.markdown.lines().any(|l| l.starts_with("- ")), "list");
    assert!(r.markdown.lines().any(|l| l.starts_with('|')), "table");
    assert!(!r.markdown.contains('\u{2014}'));
}

#[test]
fn a7_chunk_cursors_no_repeats_no_gaps() {
    let (rows, blocks) = article();
    for chunk_tokens in [60usize, 120, 333] {
        let out = read_all(&rows, &blocks, chunk_tokens);
        let n = out[0].chunks;
        assert!(
            n > 1,
            "chunk_tokens {chunk_tokens}: expected several chunks"
        );
        for (i, o) in out.iter().enumerate() {
            assert_eq!(o.chunk, i + 1, "chunk numbering");
            assert_eq!(o.chunks, n, "chunk count stable");
            assert!(
                o.markdown.contains(&format!("chunk {} of {}", i + 1, n)),
                "chunk header\n{}",
                o.markdown
            );
        }
        assert_eq!(out.len(), n, "cursor chain length equals chunks");
        let all: String = out
            .iter()
            .map(|o| o.markdown.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for m in MAIN_MARKERS {
            assert_eq!(all.matches(m).count(), 1, "{chunk_tokens}: {m} count");
        }
        for h in [
            "# Tokio Runtime Guide",
            "## Getting Started",
            "## Configuration",
            "## Big Section",
            "## FAQ",
        ] {
            let c = all.lines().filter(|l| l.trim() == h).count();
            assert_eq!(c, 1, "{chunk_tokens}: heading {h} printed {c} times");
        }
        // the oversized block: every word once, in order
        let mut last = 0usize;
        for i in 1..=400 {
            let w = format!("wd{i:03}q");
            assert_eq!(all.matches(&w).count(), 1, "{chunk_tokens}: {w}");
            let p = all.find(&w).unwrap();
            assert!(p >= last, "{chunk_tokens}: {w} out of order");
            last = p;
        }
        for m in CHROME_MARKERS {
            assert!(!all.contains(m), "{chunk_tokens}: chrome {m}");
        }
        // each chunk respects its budget, give or take the header and one heading line
        for o in &out {
            assert!(
                estimate(&o.markdown) <= chunk_tokens + 40,
                "{chunk_tokens}: chunk {} is {} tokens",
                o.chunk,
                estimate(&o.markdown)
            );
        }
    }
}

#[test]
fn w5a_first_heading_title_remaining_lists_sections_cursor_continues() {
    let (rows, blocks) = article();
    let first = read(&rows, &blocks, &ropts(120, None));
    let h = first
        .markdown
        .lines()
        .find(|l| l.starts_with('#'))
        .unwrap_or("");
    assert_eq!(
        h, "# Tokio Runtime Guide",
        "first heading\n{}",
        first.markdown
    );
    for m in CHROME_MARKERS {
        assert!(!first.markdown.contains(m), "{m}");
    }
    // remaining: lists the sections not yet read, with word counts
    let names: Vec<&str> = first.remaining.iter().map(|e| e.heading.as_str()).collect();
    for s in ["Configuration", "Big Section", "FAQ"] {
        let e = first
            .remaining
            .iter()
            .find(|e| e.heading.contains(s))
            .unwrap_or_else(|| panic!("remaining lacks {s}: {names:?}"));
        assert!(e.words > 0, "{s} has 0 words");
    }
    let big = first
        .remaining
        .iter()
        .find(|e| e.heading.contains("Big Section"))
        .unwrap();
    assert!(big.words >= 400, "Big Section words {}", big.words);
    assert!(
        first.markdown.contains("remaining:"),
        "remaining: outline in markdown\n{}",
        first.markdown
    );
    // cursor continues without repeating text: no line of chunk 1's body reappears
    let second = read(&rows, &blocks, &ropts(120, first.cursor.clone()));
    assert_eq!(second.chunk, 2);
    let body = |md: &str| -> Vec<String> {
        md.lines()
            .map(str::trim)
            .filter(|l| {
                !l.is_empty()
                    && !l.starts_with("chunk ")
                    && !l.starts_with("remaining:")
                    && !l.starts_with("- ") // outline/list lines checked via markers
                    && !l.starts_with("cursor")
            })
            .map(String::from)
            .collect()
    };
    let b1 = body(&first.markdown);
    for l in body(&second.markdown) {
        assert!(!b1.contains(&l), "repeated line: {l}");
    }
    // last chunk: no cursor, nothing remaining
    let out = read_all(&rows, &blocks, 120);
    let last = out.last().unwrap();
    assert!(last.cursor.is_none());
    assert!(
        last.remaining.iter().all(|e| e.words == 0),
        "remaining at end: {:?}",
        last.remaining
    );
}

// ---------------------------------------------------------------- W5-b password

fn password_page(c: &str) -> (Vec<IndexRow>, Vec<TextBlock>) {
    use BlockKind::*;
    let mut rows = register_rows();
    // the producer put a real value on the password rows (must never happen; defence in depth)
    for r in rows.iter_mut().filter(|r| r.id == 7) {
        r.state.value = Some(FieldValue::Text(c.to_string()));
    }
    // a password row identified only by autocomplete
    rows.insert(
        8,
        row(30, "textbox", "Confirm password")
            .lm("main")
            .hd("Create your account")
            .fm("f1")
            .auto("current-password")
            .val(FieldValue::Text(c.to_string())),
    );
    let mut blocks = register_blocks();
    blocks.insert(
        5,
        blk(
            5,
            Paragraph,
            None,
            Some("main"),
            Some("Create your account"),
            vec![
                span("Password "),
                Span {
                    text: c.to_string(),
                    elem: Some(sb_sheet::ElemRef::main(7)),
                },
            ],
        ),
    );
    for (i, b) in blocks.iter_mut().enumerate() {
        b.order = i as u32;
    }
    (rows, blocks)
}

#[test]
fn w5b_password_value_never_emitted() {
    let c = canary();
    let (rows, blocks) = password_page(&c);
    let mut outputs: Vec<(String, String)> = Vec::new();
    for ct in [4000usize, 30] {
        for region in [None, Some("main"), Some("Create your account")] {
            let mut cur = None;
            for _ in 0..100 {
                let r = read(
                    &rows,
                    &blocks,
                    &ReadOpts {
                        region: region.map(Into::into),
                        cursor: cur.clone(),
                        max_tokens: None,
                        chunk_tokens: ct,
                    },
                );
                outputs.push((format!("read {region:?} {ct} md"), r.markdown.clone()));
                outputs.push((
                    format!("read {region:?} {ct} outline"),
                    format!("{:?}", r.remaining),
                ));
                if let Some(Cursor(s)) = &r.cursor {
                    outputs.push((format!("read {region:?} {ct} cursor"), s.clone()));
                }
                cur = r.cursor;
                if cur.is_none() {
                    break;
                }
            }
        }
    }
    for region in [None, Some("f1"), Some("main"), Some("Create your account")] {
        outputs.push((format!("sheet {region:?}"), sheet_of(&rows, region)));
    }
    for r in &rows {
        outputs.push((format!("sheet_line #{}", r.id), sheet_line(r)));
    }
    for q in ["password", "confirm password", "password:input", &c] {
        for k in [1, 3, 50] {
            let hits = find(&rows, &blocks, &fopts(q, k));
            outputs.push(("find_lines".into(), find_lines(&rows, &hits)));
        }
    }
    outputs.push((
        "skeleton".into(),
        skeleton(&rows, &blocks, &register_meta(None)),
    ));
    let mut leaks = Vec::new();
    for (what, text) in &outputs {
        if text.contains(c.as_str()) {
            leaks.push(what.clone()); // never print the text itself: it holds the canary
        }
    }
    assert!(leaks.is_empty(), "password canary emitted by: {leaks:?}");
    // the password rows still say whether they are filled
    let l7 = sheet_line(rows.iter().find(|r| r.id == 7).unwrap());
    assert!(
        l7.contains("filled") || l7.contains("empty"),
        "#7 line lacks state"
    );
}

// ---------------------------------------------------------------- W5-c challenge content

fn challenge_page() -> (Vec<IndexRow>, Vec<TextBlock>) {
    use BlockKind::*;
    let mut rows = register_rows();
    // a producer that (wrongly) walked inside the challenge frame, and a frame nested in it
    let pos = rows.iter().position(|r| r.id == 11).unwrap() + 1;
    let mut nested = row(2, "iframe", "Chalnestframe").frame(1).lm("main");
    nested.child_frame = Some(2);
    rows.splice(
        pos..pos,
        [
            row(1, "checkbox", "Chalrobot I am not a robot")
                .frame(1)
                .lm("main"),
            nested,
            row(1, "button", "Chalverify").frame(2).lm("main"),
        ],
    );
    let mut blocks = register_blocks();
    let fb = |frame: u32, text: &str| {
        let mut b = blk(
            0,
            Paragraph,
            None,
            Some("main"),
            Some("Create your account"),
            vec![span(text)],
        );
        b.frame = frame;
        b
    };
    let b1 = fb(1, "Chalframetext select all squares with traffic lights");
    let b2 = fb(2, "Chalnestedtext audio challenge instructions");
    blocks.splice(4..4, [b1, b2]);
    for (i, b) in blocks.iter_mut().enumerate() {
        b.order = i as u32;
    }
    (rows, blocks)
}

const CHAL_MARKERS: [&str; 5] = [
    "Chalframetext",
    "Chalnestedtext",
    "Chalrobot",
    "Chalverify",
    "Chalnestframe",
];

#[test]
fn w5c_challenge_frame_content_never_emitted_by_read() {
    let (rows, blocks) = challenge_page();
    for region in [
        None,
        Some("main"),
        Some("Create your account"),
        Some("## Create your account"),
    ] {
        for ct in [4000usize, 25] {
            let mut cur = None;
            for _ in 0..100 {
                let r = read(
                    &rows,
                    &blocks,
                    &ReadOpts {
                        region: region.map(Into::into),
                        cursor: cur.clone(),
                        max_tokens: None,
                        chunk_tokens: ct,
                    },
                );
                let all = format!("{}\n{:?}", r.markdown, r.remaining);
                for m in CHAL_MARKERS {
                    assert!(!all.contains(m), "read {region:?}/{ct}: {m}\n{all}");
                }
                cur = r.cursor;
                if cur.is_none() {
                    break;
                }
            }
        }
    }
    // the main-frame text around it is still read
    let r = read(&rows, &blocks, &ropts(4000, None));
    assert!(
        r.markdown.contains("Start your free trial"),
        "{}",
        r.markdown
    );
}

#[test]
fn w5c_extra_challenge_frame_content_not_in_sheet_find_skeleton() {
    let (rows, blocks) = challenge_page();
    let mut outs = vec![
        sheet_of(&rows, None),
        sheet_of(&rows, Some("main")),
        sheet_of(&rows, Some("f1")),
        skeleton(&rows, &blocks, &register_meta(None)),
    ];
    for q in ["robot", "verify", "traffic lights", "not a robot, verify"] {
        let hits = find(&rows, &blocks, &fopts(q, 5));
        assert!(
            hits.iter().all(|h| h.elem.frame == 0),
            "{q}: frame hit {hits:?}"
        );
        outs.push(find_lines(&rows, &hits));
    }
    for o in outs {
        for m in CHAL_MARKERS {
            assert!(!o.contains(m), "{m}\n{o}");
        }
    }
}

/// D-211: "A heading never ends a chunk when content follows." A chunk that is only a heading
/// costs the model a turn for nothing (CLAUDE.md priorities: efficiency first).
#[test]
fn a7_no_chunk_ends_on_a_heading_when_content_follows() {
    let (rows, blocks) = article();
    for chunk_tokens in [60usize, 120, 333] {
        let out = read_all(&rows, &blocks, chunk_tokens);
        let n = out.len();
        for o in out.iter().take(n.saturating_sub(1)) {
            let body: Vec<&str> = o
                .markdown
                .lines()
                .map(str::trim)
                .take_while(|l| !l.starts_with("remaining:"))
                .filter(|l| !l.is_empty() && !l.starts_with("chunk "))
                .collect();
            assert!(
                body.iter().any(|l| !l.starts_with('#')),
                "chunk_tokens {chunk_tokens}: chunk {} of {} has no content, only {:?}",
                o.chunk,
                o.chunks,
                body
            );
            let last = body.last().copied().unwrap_or("");
            assert!(
                !last.starts_with('#'),
                "chunk_tokens {chunk_tokens}: chunk {} of {} ends on heading {last:?}",
                o.chunk,
                o.chunks
            );
        }
    }
}

// ---------------------------------------------------------------- round 2 (after review fixes)

/// Every read output (markdown, outline, cursor) for one region and chunk size, following
/// cursors to the end.
fn read_texts(
    rows: &[IndexRow],
    blocks: &[TextBlock],
    region: Option<&str>,
    ct: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = None;
    for _ in 0..300 {
        let r = read(
            rows,
            blocks,
            &ReadOpts {
                region: region.map(Into::into),
                cursor: cur.clone(),
                max_tokens: None,
                chunk_tokens: ct,
            },
        );
        out.push(r.markdown.clone());
        out.push(format!("{:?}", r.remaining));
        if let Some(Cursor(s)) = &r.cursor {
            out.push(s.clone());
        }
        cur = r.cursor;
        if cur.is_none() {
            return out;
        }
    }
    panic!("read cursor never ends for {region:?}/{ct}");
}

/// W5-b round 2: a span linked to a password row known only by autocomplete (D-206/D-216)
/// renders as the field, never its text; a password value that sits in plain text of a long
/// paragraph is scrubbed before chunking, so no chunk split can leak it across two reads; the
/// whitespace-collapsed spelling of a value is scrubbed too.
#[test]
fn r2_w5b_autocomplete_password_span_and_scrub_before_chunking() {
    use BlockKind::*;
    let c = canary();
    let mut rows = register_rows();
    rows.insert(
        7,
        row(31, "textbox", "Current password")
            .lm("main")
            .hd("Create your account")
            .fm("f1")
            .auto("current-password")
            .val(FieldValue::Text(format!("{c}A"))),
    );
    for r in rows.iter_mut().filter(|r| r.id == 7) {
        r.state.value = Some(FieldValue::Text(format!("{c}B  {c}C")));
    }
    let mut blocks = register_blocks();
    let filler: String = (0..120).map(|i| format!("word{i} ")).collect();
    blocks.insert(
        3,
        blk(
            3,
            Paragraph,
            None,
            Some("main"),
            Some("Create your account"),
            vec![
                span("Current password "),
                Span {
                    text: format!("{c}Z"),
                    elem: Some(sb_sheet::ElemRef::main(31)),
                },
            ],
        ),
    );
    blocks.insert(
        4,
        blk(
            4,
            Paragraph,
            None,
            Some("main"),
            Some("Create your account"),
            vec![span(&format!(
                "{filler}pasted {c}A here. {filler}and {c}B {c}C there. {filler}"
            ))],
        ),
    );
    for (i, b) in blocks.iter_mut().enumerate() {
        b.order = i as u32;
    }
    let mut leaks = Vec::new();
    for ct in [4000usize, 40, 25] {
        for region in [None, Some("main"), Some("Create your account")] {
            let texts = read_texts(&rows, &blocks, region, ct);
            // joined with nothing between chunks: a value cut over a chunk boundary would show
            let joined: String = texts.concat();
            if texts.iter().any(|t| t.contains(c.as_str())) || joined.contains(c.as_str()) {
                leaks.push(format!("read {region:?}/{ct}"));
            }
        }
    }
    let full = read(&rows, &blocks, &ropts(4000, None)).markdown;
    assert!(
        full.contains("(#31)"),
        "the autocomplete password field is still linked as a field"
    );
    // not vacuous: the paragraph holding the values is read, with the values scrubbed
    assert!(
        full.contains("pasted") && full.contains("there."),
        "paragraph with password values was not read"
    );
    for region in [None, Some("f1"), Some("main")] {
        if sheet_of(&rows, region).contains(c.as_str()) {
            leaks.push(format!("sheet {region:?}"));
        }
    }
    for q in ["current password", "password", "pasted"] {
        let hits = find(&rows, &blocks, &fopts(q, 50));
        if find_lines(&rows, &hits).contains(c.as_str()) {
            leaks.push(format!("find_lines {q}"));
        }
    }
    if skeleton(&rows, &blocks, &register_meta(None)).contains(c.as_str()) {
        leaks.push("skeleton".into());
    }
    assert!(leaks.is_empty(), "password canary emitted by: {leaks:?}");
}

/// W5-c round 2: content of a frame hosted by an OPAQUE (non-challenge) row is never read
/// (B-5 "OPAQUE ... never read inside", D-216), nor listed by sheet or found by find.
#[test]
fn r2_w5c_opaque_frame_content_never_emitted() {
    use BlockKind::*;
    let mut rows = register_rows();
    let pos = rows.iter().position(|r| r.id == 12).unwrap();
    let mut host = row(40, "iframe", "Payment widget")
        .lm("main")
        .hd("Create your account")
        .fm("f1");
    host.flags.opaque = true;
    host.child_frame = Some(3);
    rows.splice(
        pos..pos,
        [
            host,
            row(1, "textbox", "Opqcardnumber").frame(3).lm("main"),
            row(2, "button", "Opqpaybutton").frame(3).lm("main"),
        ],
    );
    let mut blocks = register_blocks();
    let mut b = blk(
        0,
        Paragraph,
        None,
        Some("main"),
        Some("Create your account"),
        vec![span("Opqframetext enter your card details below")],
    );
    b.frame = 3;
    blocks.insert(4, b);
    for (i, b) in blocks.iter_mut().enumerate() {
        b.order = i as u32;
    }
    let markers = ["Opqframetext", "Opqcardnumber", "Opqpaybutton"];
    let mut outs = Vec::new();
    for region in [None, Some("main"), Some("Create your account")] {
        for ct in [4000usize, 25] {
            outs.extend(read_texts(&rows, &blocks, region, ct));
        }
    }
    for region in [None, Some("main"), Some("f1")] {
        outs.push(sheet_of(&rows, region));
    }
    for q in ["card details", "pay", "card number"] {
        let hits = find(&rows, &blocks, &fopts(q, 5));
        assert!(
            hits.iter().all(|h| h.elem.frame == 0),
            "{q}: hit inside the opaque frame {hits:?}"
        );
        outs.push(find_lines(&rows, &hits));
    }
    outs.push(skeleton(&rows, &blocks, &register_meta(None)));
    for o in &outs {
        for m in markers {
            assert!(!o.contains(m), "{m}\n{o}");
        }
    }
    // the main-frame text is still read
    let r = read(&rows, &blocks, &ropts(4000, None));
    assert!(
        r.markdown.contains("Start your free trial"),
        "{}",
        r.markdown
    );
}

/// CLAUDE.md rule 3 / D-5 / D-215: a CHALLENGE row whose producer marked it not visible is
/// still appended by find and still warned about by the skeleton.
#[test]
fn r2_hidden_challenge_still_reported_by_find_and_skeleton() {
    let mut rows = register_rows();
    for r in rows.iter_mut().filter(|r| r.id == 11) {
        r.state.visible = false;
    }
    let hits = find(&rows, &[], &fopts("email, password, agree, register", 1));
    let got: Vec<String> = hits.iter().map(|h| h.elem.to_string()).collect();
    assert_eq!(got, ids(&[6, 7, 10, 11, 12, 17]));
    assert_eq!(
        hits.iter().find(|h| h.elem.id == 11).unwrap().reason,
        HitReason::Challenge
    );
    let lines = find_lines(&rows, &hits);
    assert!(
        lines
            .lines()
            .any(|l| l.contains("#11") && l.contains("CHALLENGE")),
        "{lines}"
    );
    // an unrelated query still carries the challenge
    let hits = find(&rows, &[], &fopts("pricing", 1));
    assert!(hits.iter().any(|h| h.elem.id == 11), "{hits:?}");
    let s = skeleton(&rows, &register_blocks(), &register_meta(None));
    assert!(
        s.lines()
            .any(|l| l.contains("CHALLENGE") && l.contains("#11")),
        "{s}"
    );
}
