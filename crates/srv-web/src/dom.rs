//! W-2, D-519: every HTML parse in srv-web goes through [`document`] or [`fragment`], so no
//! page can hold a thread for minutes or take the process down.
//!
//! html5ever's tree builder checks element scope by walking its stack of open elements, so
//! one start tag costs time in proportion to how deep the page is nested: a page that only
//! opens `<div>` costs time growing with the square of its length (80 000 levels, 400 KB:
//! 31 s in a release build), and one call cannot be interrupted. Its misnesting repair can
//! also rebuild hundreds of formatting elements for every few bytes of input (one `<p>x`
//! after `<b class=1>`...`<b class=500>`), so a small body can build a tree of millions of
//! nodes.
//!
//! So the source is fed to the parser [`FEED_BYTES`] at a time, and between two pieces:
//! * the deadline is checked ([`Budget`]): a parse past it gives up (`None`);
//! * the tree's depth at the newest node (the depth of the parser's open elements) is
//!   checked: past [`NEST_MAX`] levels the parse stops there ([`Cut::Deep`]);
//! * the number of nodes built is checked against the bytes fed: more than
//!   [`NODES_PER_BYTE`] per byte (no page without that repair gets near it) stops the parse
//!   there ([`Cut::Repeats`]).
//!
//! What was parsed before the stop is kept, and the caller says the rest is missing. One
//! piece costs at most `FEED_BYTES` tags at a depth of at most `NEST_MAX + FEED_BYTES / 3`,
//! so the time between two checks stays in milliseconds.

use html5ever::driver::{self, ParseOpts, Parser};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::{local_name, ns, QualName};
use scraper::{Html, HtmlTreeSink};
use std::cell::Cell;
use std::time::Instant;

/// The deepest nesting a parse goes on past (D-519): Chromium's HTML parser stops nesting
/// at the same depth (`kMaximumHTMLParserDOMTreeDepth`), so no page a browser shows as
/// intended is deeper. 512 nested `<div>` parse in about 10 ms in a debug build.
pub(crate) const NEST_MAX: usize = 512;

/// Bytes fed to the parser between two checks.
const FEED_BYTES: usize = 1024;

/// Most nodes a parse may build per byte fed (plus [`NODES_SLACK`]): a tag takes at least
/// three bytes and adjacent text is one node, so only the parser's misnesting repair gets
/// past this.
const NODES_PER_BYTE: usize = 2;
const NODES_SLACK: usize = 1024;

/// Why a parse stopped before the end of its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cut {
    /// Nested deeper than [`NEST_MAX`] levels.
    Deep,
    /// The markup made the parser build far more elements than the source holds.
    Repeats,
}

impl Cut {
    /// One line for the model: what happened and that the rest is missing.
    pub(crate) fn note(self) -> String {
        match self {
            Cut::Deep => format!(
                "page nested too deep to read past {NEST_MAX} levels; the rest of the page \
                 after that point is missing"
            ),
            Cut::Repeats => "page markup makes the parser repeat elements far beyond its size; \
                 the rest of the page after that point is missing"
                .to_string(),
        }
    }
}

/// A parsed tree and, when the parse stopped early, why.
pub(crate) struct Parsed {
    pub html: Html,
    pub cut: Option<Cut>,
}

/// When a parse or walk must stop (D-517). Once spent it stays spent, so a caller can ask
/// afterwards whether any part ran out of time.
#[derive(Debug, Default)]
pub(crate) struct Budget {
    deadline: Option<Instant>,
    out: Cell<bool>,
}

impl Budget {
    /// No deadline (unit tests and callers that have none).
    pub(crate) fn unlimited() -> Budget {
        Budget::default()
    }

    pub(crate) fn until(deadline: Instant) -> Budget {
        Budget {
            deadline: Some(deadline),
            out: Cell::new(false),
        }
    }

    /// Has the deadline passed? Looks at the clock.
    pub(crate) fn spent(&self) -> bool {
        if !self.out.get() && self.deadline.is_some_and(|d| Instant::now() >= d) {
            self.out.set(true);
        }
        self.out.get()
    }
}

/// `src` as a whole document; `None` when `budget` ran out first.
pub(crate) fn document(src: &str, budget: &Budget) -> Option<Parsed> {
    let parser = driver::parse_document(
        HtmlTreeSink::new(Html::new_document()),
        ParseOpts::default(),
    );
    feed(parser, src, budget)
}

/// `src` as the content of a `<body>` (scraper's `Html::parse_fragment`); `None` when
/// `budget` ran out first.
pub(crate) fn fragment(src: &str, budget: &Budget) -> Option<Parsed> {
    let parser = driver::parse_fragment(
        HtmlTreeSink::new(Html::new_fragment()),
        ParseOpts::default(),
        QualName::new(None, ns!(html), local_name!("body")),
        Vec::new(),
        false,
    );
    feed(parser, src, budget)
}

fn feed(mut parser: Parser<HtmlTreeSink>, src: &str, budget: &Budget) -> Option<Parsed> {
    let mut at = 0;
    let mut cut = None;
    while at < src.len() {
        if budget.spent() {
            return None;
        }
        let mut end = (at + FEED_BYTES).min(src.len());
        while !src.is_char_boundary(end) {
            end += 1;
        }
        parser.process(StrTendril::from_slice(&src[at..end]));
        at = end;
        if at < src.len() {
            cut = check(&parser, at);
            if cut.is_some() {
                break;
            }
        }
    }
    // The end of input closes what is open: one pass over the open elements.
    let html = parser.finish();
    Some(Parsed { html, cut })
}

/// After `fed` bytes: should the parse stop, and why.
fn check(parser: &Parser<HtmlTreeSink>, fed: usize) -> Option<Cut> {
    let html = parser.tokenizer.sink.sink.0.borrow();
    let nodes = html.tree.values().len();
    if nodes > fed.saturating_mul(NODES_PER_BYTE) + NODES_SLACK {
        return Some(Cut::Repeats);
    }
    // The newest node hangs off the parser's current node, so its depth is the depth of the
    // stack of open elements, which is what each further tag costs.
    let depth = html
        .tree
        .nodes()
        .next_back()
        .map_or(0, |n| n.ancestors().count());
    (depth > NEST_MAX).then_some(Cut::Deep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn depth_of(html: &Html) -> usize {
        html.tree
            .nodes()
            .map(|n| n.ancestors().count())
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn ordinary_pages_parse_whole() {
        let page = format!(
            "<html><head><title>T</title></head><body>{}</body></html>",
            "<div><p>Hello <b>there</b> <a href=/x>link</a></p><table><tr><td>a<td>b</table></div>"
                .repeat(2_000)
        );
        let p = document(&page, &Budget::unlimited()).unwrap();
        assert_eq!(p.cut, None);
        let text: String = p.html.root_element().text().collect();
        assert_eq!(text.matches("link").count(), 2_000);
        // A fragment too, split across many pieces, multibyte text included.
        let frag = "<p>caf\u{e9} \u{1f600}</p>".repeat(3_000);
        let p = fragment(&frag, &Budget::unlimited()).unwrap();
        assert_eq!(p.cut, None);
        let text: String = p.html.root_element().text().collect();
        assert_eq!(text.matches('\u{1f600}').count(), 3_000);
    }

    /// Review-T-12b round 2 Blocking 2: deep nesting stops the parse soon after NEST_MAX
    /// levels instead of costing time with the square of the depth.
    #[test]
    fn deep_nesting_stops_the_parse_quickly() {
        for open in ["<div>", "<b>", "<span>", "<div/>", "<i class=x>"] {
            let page = format!("<p>before</p>{}deep text", open.repeat(100_000));
            let t = Instant::now();
            let p = document(&page, &Budget::unlimited()).unwrap();
            let took = t.elapsed();
            assert_eq!(p.cut, Some(Cut::Deep), "{open}");
            let d = depth_of(&p.html);
            assert!(
                d > NEST_MAX && d < NEST_MAX + FEED_BYTES,
                "{open}: depth {d}"
            );
            let text: String = p.html.root_element().text().collect();
            assert!(text.contains("before"), "{open}");
            assert!(!text.contains("deep text"), "{open}");
            assert!(took < Duration::from_secs(5), "{open}: {took:?}");
            let p = fragment(&page, &Budget::unlimited()).unwrap();
            assert_eq!(p.cut, Some(Cut::Deep), "{open} as a fragment");
        }
        // NEST_MAX - 10 levels is read whole, well under a second even in a debug build.
        let page = format!("{}end", "<div>".repeat(NEST_MAX - 10));
        let t = Instant::now();
        let p = document(&page, &Budget::unlimited()).unwrap();
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        assert_eq!(p.cut, None);
        let text: String = p.html.root_element().text().collect();
        assert!(text.contains("end"));
    }

    /// The parser's misnesting repair rebuilds every open formatting element for each
    /// paragraph: the node count stops it.
    #[test]
    fn repeated_reconstruction_stops_the_parse() {
        let opened: String = (0..400).map(|i| format!("<b class=c{i}>")).collect();
        let page = format!("<p>{opened}</p>{}", "<p>x</p>".repeat(200_000));
        let t = Instant::now();
        let p = document(&page, &Budget::unlimited()).unwrap();
        assert_eq!(p.cut, Some(Cut::Repeats));
        assert!(p.html.tree.values().len() < 4 * page.len());
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    }

    #[test]
    fn a_spent_budget_gives_up() {
        let page = "<p>x</p>".repeat(10_000);
        let b = Budget::until(Instant::now());
        assert!(document(&page, &b).is_none());
        assert!(b.spent());
        assert!(fragment(&page, &b).is_none());
        let b = Budget::until(Instant::now() + Duration::from_secs(60));
        assert!(document(&page, &b).is_some());
        assert!(!b.spent());
    }
}
