//! W-2 HTML walk: fetched HTML -> `IndexRow`s (I-5, the same row shape the browser ships)
//! and ordered `TextBlock`s (I-6), so sb-sheet's one scorer and one reader work on fetched
//! pages exactly as on live ones (D-51).
//!
//! What the walk approximates without a layout engine:
//! * visibility: the `hidden` attribute, `aria-hidden="true"`, inline `display:none` or
//!   `visibility:hidden`, a closed `<dialog>`, the body of a closed `<details>`. Hidden
//!   controls keep a row with `visible: false`; hidden text is never emitted;
//! * landmarks: the nearest enclosing landmark, by role first, then by tag, with the same
//!   rules as `injected.js` (header/footer inside sectioning content and unnamed sections or
//!   forms are not landmarks);
//! * `nearby`: the last text seen before a field, when it is not the field's name.
//!
//! Safety (CLAUDE.md rules 2, 3, 5): a password or card field's value is only ever
//! `empty` or `filled`; nothing inside an `<iframe>` is read; every string that reaches a
//! row or a block goes through [`crate::escape`]; a challenge (captcha widget, challenge
//! frame, human-check prompt, Cloudflare interstitial) is reported in [`Doc::challenge`].

use crate::dom::{self, Budget};
use crate::escape;
use ego_tree::NodeRef;
use reqwest::Url;
use sb_sheet::{BlockKind, ElemRef, FieldValue, Flags, IndexRow, Span, State, TextBlock};
use scraper::{ElementRef, Html, Node, Selector};
use std::collections::HashMap;
use std::time::Instant;

/// `raw_main_html` cap (W-2 `mode="raw"`: 60 k chars).
pub const RAW_MAX_CHARS: usize = 60_000;
const NAME_MAX: usize = 200;
const NEARBY_MAX: usize = 100;
const VALUE_MAX: usize = 200;
const OPTIONS_MAX: usize = 25;
const OPTION_MAX: usize = 80;
/// A human-check prompt is short; a paragraph about captchas is not (as injected.js).
const CHALLENGE_TEXT_MAX: usize = 200;
/// An `onclick` container with more text than this is content, not a button.
const ONCLICK_TEXT_MAX: usize = 80;

/// Tags whose content is never page text. `xmp`, `plaintext`, `noembed` and `noframes` (like
/// `script`, `style` and `noscript`) hold raw text: the parser does not read markup inside
/// them, so their text can be a whole HTML document (`<plaintext>` swallows the rest of the
/// body). Rule 5: never emitted.
const SKIP_TAGS: &[&str] = &[
    "head",
    "script",
    "style",
    "noscript",
    "template",
    "svg",
    "math",
    "canvas",
    "object",
    "embed",
    "audio",
    "video",
    "source",
    "track",
    "meta",
    "link",
    "title",
    "xmp",
    "plaintext",
    "noembed",
    "noframes",
];

/// Tags that start a new text block (as injected.js `BLOCK_TAGS`, plus a few HTML5 ones).
const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "caption",
    "center",
    "dd",
    "details",
    "dialog",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "legend",
    "li",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "search",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "ul",
    "html",
];

/// Explicit roles that make an element a control (a row).
const CONTROL_ROLES: &[&str] = &[
    "button",
    "link",
    "checkbox",
    "radio",
    "switch",
    "textbox",
    "searchbox",
    "combobox",
    "listbox",
    "slider",
    "spinbutton",
    "tab",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "treeitem",
];

/// Roles that are fields (things a user types into or picks): they get `nearby`.
const FIELD_ROLES: &[&str] = &[
    "textbox",
    "searchbox",
    "combobox",
    "listbox",
    "checkbox",
    "radio",
    "switch",
    "slider",
    "spinbutton",
];

/// Tags that are never a challenge widget box (as injected.js `NOT_BOX_TAGS`).
const NOT_BOX_TAGS: &[&str] = &[
    "html", "body", "form", "main", "button", "input", "select", "textarea", "a", "label", "option",
];

/// Hosts that only serve challenges (injected.js `CHALLENGE_HOST_RE`).
const CHALLENGE_HOSTS: &[&str] = &[
    "challenges.cloudflare.com",
    "recaptcha.net",
    "hcaptcha.com",
    "arkoselabs.com",
    "funcaptcha.com",
    "geetest.com",
];

/// A fetched page as the sheet functions read it.
#[derive(Debug, Clone, Default)]
pub struct Doc {
    /// `<title>`, escaped, one line.
    pub title: String,
    pub index: Vec<IndexRow>,
    pub blocks: Vec<TextBlock>,
    /// Absolute http(s) target of each link row, by row id (`fetch("#12")`, D-502).
    pub hrefs: HashMap<u32, String>,
    /// Why this page is a challenge (D-5), if it is one.
    pub challenge: Option<String>,
    /// D-519: one line saying what of the page was not read (nested too deep, or markup
    /// that makes the parser repeat elements), shown under the skeleton.
    pub cut: Option<String>,
}

/// Walks `html` fetched from `url` (the final URL, used to resolve links).
pub fn parse(html: &str, url: &Url) -> Doc {
    parse_inner(html, url, None).unwrap_or_default()
}

/// [`parse`] that gives up at `deadline` (D-517, W-2): `None` when the walk did not finish
/// by then. The walk checks the clock as it goes, so a page that would take longer stops
/// its thread soon after the deadline instead of holding it.
pub(crate) fn parse_within(html: &str, url: &Url, deadline: Instant) -> Option<Doc> {
    parse_inner(html, url, Some(deadline))
}

fn parse_inner(html: &str, url: &Url, deadline: Option<Instant>) -> Option<Doc> {
    let budget = deadline.map_or_else(Budget::unlimited, Budget::until);
    // D-519: the parse stops past dom::NEST_MAX levels or on runaway element repetition,
    // and gives up at the deadline.
    let parsed = dom::document(html, &budget)?;
    let doc = parsed.html;
    let base = base_url(&doc, url);
    let mut w = Walker::new(&doc, base, &budget);
    w.walk(doc.tree.root(), &Cx::default());
    if w.out_of_time {
        return None;
    }
    w.flush();
    let title = page_title(&doc, &budget);
    if budget.spent() {
        return None;
    }
    let cut = match (parsed.cut, w.flattened) {
        (Some(c), _) => Some(c.note()),
        (None, true) => Some(format!(
            "page nests elements more than {WALK_DEPTH_MAX} levels deep; what is deeper is \
             shown as plain text without its links and fields"
        )),
        (None, false) => None,
    };
    let mut challenge = challenge_url(url).or(w.challenge);
    if challenge.is_none() {
        let t = title.trim().to_ascii_lowercase();
        if t.starts_with("just a moment") || t.starts_with("attention required! | cloudflare") {
            challenge = Some(format!("challenge interstitial \"{}\"", title.trim()));
        }
    }
    Some(Doc {
        title,
        index: w.rows,
        blocks: w.blocks,
        hrefs: w.hrefs,
        challenge,
        cut,
    })
}

/// A body not declared as HTML (`text/plain`, no content type, anything else fetch reads)
/// through the same walk as [`parse`] (rule 5, D-508: fail closed by construction). Nothing
/// decides whether the body "is" HTML: whatever markup it holds is parsed and only text
/// nodes come out, and text without markup comes out as itself. A `<p>` is put at every
/// blank line first, so plain text keeps its paragraphs (adding tags cannot let markup
/// through: the parser still reads all of it). Without a `<title>`, the first block names
/// the page.
///
/// D-512: before the walk, [`plain_source`] keeps angle-bracket text that is not markup
/// (`Vec<String>`, `#include <stdio.h>`, `<T>`): it comes out escaped as `&lt;...&gt;`
/// instead of being read as an unknown element and lost. Markup (an HTML element name, a
/// custom element, a tag with attributes, a comment, a doctype) is still parsed and dropped.
pub fn parse_text(text: &str, url: &Url) -> Doc {
    parse_text_inner(text, url, None).unwrap_or_default()
}

/// [`parse_text`] that gives up at `deadline`, like [`parse_within`].
pub(crate) fn parse_text_within(text: &str, url: &Url, deadline: Instant) -> Option<Doc> {
    parse_text_inner(text, url, Some(deadline))
}

fn parse_text_inner(text: &str, url: &Url, deadline: Option<Instant>) -> Option<Doc> {
    let text = plain_source(text);
    let mut marked = String::with_capacity(text.len() + 64);
    let mut blank = false;
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            blank = true;
            continue;
        }
        if blank && !marked.is_empty() {
            marked.push_str("\n<p>\n");
        } else if !marked.is_empty() {
            marked.push('\n');
        }
        blank = false;
        marked.push_str(line);
    }
    let mut doc = parse_inner(&marked, url, deadline)?;
    if doc.title.is_empty() {
        if let Some(b) = doc.blocks.first() {
            let first = b
                .spans
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            doc.title = escape::name(&first, TEXT_TITLE_MAX);
        }
    }
    Some(doc)
}

/// Title of a page without a `<title>`: the start of its first block.
const TEXT_TITLE_MAX: usize = 100;

/// Element names the HTML, SVG and MathML parsers know (lowercase). In a text/plain body a
/// tag with one of these names, or a custom element name (with a `-`), is markup.
#[rustfmt::skip]
const KNOWN_ELEMENTS: &[&str] = &[
    "a", "abbr", "acronym", "address", "applet", "area", "article", "aside", "audio", "b",
    "base", "basefont", "bdi", "bdo", "bgsound", "big", "blink", "blockquote", "body", "br",
    "button", "canvas", "caption", "center", "cite", "code", "col", "colgroup", "data",
    "datalist", "dd", "del", "details", "dfn", "dialog", "dir", "div", "dl", "dt", "em",
    "embed", "fieldset", "figcaption", "figure", "font", "footer", "form", "frame", "frameset",
    "h1", "h2", "h3", "h4", "h5", "h6", "head", "header", "hgroup", "hr", "html", "i", "iframe",
    "image", "img", "input", "ins", "isindex", "kbd", "keygen", "label", "legend", "li", "link",
    "listing", "main", "map", "mark", "marquee", "menu", "menuitem", "meta", "meter", "nav",
    "nobr", "noembed", "noframes", "noscript", "object", "ol", "optgroup", "option", "output",
    "p", "param", "picture", "plaintext", "portal", "pre", "progress", "q", "rb", "rp", "rt",
    "rtc", "ruby", "s", "samp", "script", "search", "section", "select", "selectedcontent",
    "slot", "small", "source", "spacer", "span", "strike", "strong", "style", "sub", "summary",
    "sup", "table", "tbody", "td", "template", "textarea", "tfoot", "th", "thead", "time",
    "title", "tr", "track", "tt", "u", "ul", "var", "video", "wbr", "xmp",
    // SVG
    "svg", "g", "path", "circle", "rect", "line", "polyline", "polygon", "ellipse", "text",
    "tspan", "textpath", "defs", "use", "symbol", "lineargradient", "radialgradient", "stop",
    "clippath", "mask", "pattern", "filter", "foreignobject", "desc", "marker", "animate",
    // MathML
    "math", "mi", "mo", "mn", "ms", "mtext", "mrow", "mfrac", "msup", "msub", "msubsup",
    "msqrt", "mroot", "mtable", "mtr", "mtd", "semantics", "annotation", "annotation-xml",
];

/// Is tag name `raw` (any case) in [`KNOWN_ELEMENTS`]? Lowercased on the stack and looked up
/// by binary search in a sorted copy, so the check per `<` stays cheap on a body of millions
/// of them. No known name is longer than [`KNOWN_NAME_MAX`] bytes.
fn known_element(raw: &[u8]) -> bool {
    static SORTED: std::sync::OnceLock<Vec<&'static [u8]>> = std::sync::OnceLock::new();
    if raw.len() > KNOWN_NAME_MAX {
        return false;
    }
    let mut buf = [0u8; KNOWN_NAME_MAX];
    let low = &mut buf[..raw.len()];
    low.copy_from_slice(raw);
    low.make_ascii_lowercase();
    let sorted = SORTED.get_or_init(|| {
        let mut v: Vec<&'static [u8]> = KNOWN_ELEMENTS.iter().map(|k| k.as_bytes()).collect();
        v.sort_unstable();
        v
    });
    sorted.binary_search(&&*low).is_ok()
}

/// The longest name in [`KNOWN_ELEMENTS`] (`selectedcontent`), checked by a test.
const KNOWN_NAME_MAX: usize = 15;

/// Longest `<...>` run [`plain_source`] keeps as text.
const PLAIN_BRACKET_MAX: usize = 200;

/// The marks [`plain_source`] writes for a `<` and `>` of text (shown as `&lt;`/`&gt;`).
const LT: &str = escape::LT_MARK_STR;
const GT: &str = escape::GT_MARK_STR;

/// D-512: a body walked as text (text/plain, or no HTML type) as HTML source in which
/// angle-bracket text that is not markup survives. Such a `<` becomes [`LT`], and the `>`
/// that closes it on the same line [`GT`] (two Unicode noncharacters, which the parser keeps
/// as text), and the shown text writes both as `&lt;`/`&gt;` ([`escape::angles`]), so the
/// walk shows `Vec&lt;String&gt;`: the text is kept, escaped, and no tag-shaped text from it
/// reaches the model (rule 5). D-517: marks instead of entities, because the parser takes
/// tens of microseconds per character reference in a debug build, and a body can hold
/// millions of brackets. Entities elsewhere decode as before (a body with no type may be
/// HTML); the walk's output then shows every `<` and `>`, decoded or literal, as
/// `&lt;`/`&gt;` (D-514). One pass, at most [`PLAIN_BRACKET_MAX`] bytes looked at past each
/// `<` (linear in the body). Markup is left for the parser to drop: a `<` before `!`, `?` or
/// a non-letter, a known element name ([`KNOWN_ELEMENTS`]), a custom element name (with a
/// `-`), and a tag with `=` or quotes (attributes). A comment is copied as written, so its
/// end is not touched.
pub(crate) fn plain_source(text: &str) -> String {
    // A byte loop: `<`, `>`, `-` and the other delimiters are ASCII, so every cut below is a
    // char boundary, and the loop costs a few steps per byte even in a debug build.
    let b = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len() + b.len() / 8);
    let mut opened = Vec::new();
    // `b[done..at]` is copied when the next `<` at `at` is reached.
    let mut done = 0;
    let mut at = 0;
    while at < b.len() {
        if b[at] != b'<' {
            at += 1;
            continue;
        }
        out.extend_from_slice(&b[done..at]);
        let rest = &text[at..];
        let next = if let Some(body) = rest.strip_prefix("<!--") {
            let end = body.find("-->").map_or(rest.len(), |e| 4 + e + 3);
            out.extend_from_slice(&rest.as_bytes()[..end]);
            at + end
        } else {
            match plain_bracket(rest, &mut opened) {
                Some(close) => {
                    // `rest[..close]` is `<...` without markup; `close` is its `>` (or the
                    // end of what is kept on this line when there is none).
                    out.extend_from_slice(LT.as_bytes());
                    out.extend_from_slice(&rest.as_bytes()[1..close]);
                    if rest.as_bytes().get(close) == Some(&b'>') {
                        out.extend_from_slice(GT.as_bytes());
                        at + close + 1
                    } else {
                        at + close
                    }
                }
                None => {
                    out.push(b'<');
                    at + 1
                }
            }
        };
        done = next;
        at = next;
    }
    out.extend_from_slice(&b[done..]);
    // Cut only at ASCII bytes of valid UTF-8, so this is valid UTF-8.
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// For `s` starting with `<`: `Some(end)` when it starts angle-bracket text that is not
/// markup, `end` being the byte offset of its closing `>` on the same line, or of the end of
/// the tag name when there is none; `None` when it is markup or not a tag at all. `opened`
/// holds the unknown names already opened as markup (a start tag with attributes): their end
/// tags are markup too.
fn plain_bracket(s: &str, opened: &mut Vec<String>) -> Option<usize> {
    // Bytes, no allocation on the common path: this runs once per `<` of a body that may
    // hold millions of them (W-2).
    let b = s.as_bytes();
    if b.first() != Some(&b'<') {
        return None;
    }
    let end_tag = b.get(1) == Some(&b'/');
    let name_from = usize::from(end_tag) + 1;
    if !b.get(name_from).is_some_and(u8::is_ascii_alphabetic) {
        return None;
    }
    // The tag name ends where the HTML tokenizer ends it: ASCII whitespace, `/` or `>`; and
    // at a `<`, which starts the next bracket here.
    let name_len = b[name_from..]
        .iter()
        .position(|c| c.is_ascii_whitespace() || matches!(c, b'/' | b'>' | b'<'))
        .unwrap_or(b.len() - name_from);
    let raw = &b[name_from..name_from + name_len];
    if raw.contains(&b'-') || known_element(raw) {
        return None;
    }
    let is_opened = |opened: &[String]| {
        opened
            .iter()
            .any(|o| o.as_bytes().eq_ignore_ascii_case(raw))
    };
    if end_tag && is_opened(opened) {
        return None;
    }
    // The closing `>`: the first `>`, `<` or line end within PLAIN_BRACKET_MAX bytes of the
    // `<` decides. The window is bounded, so the pass over a body stays linear whatever it
    // holds (review-T-12b Blocking: a line of `<x` repeated was quadratic). All four bytes
    // are ASCII, so a position found is a char boundary.
    let window = &b[1..b.len().min(PLAIN_BRACKET_MAX + 1)];
    let close = window
        .iter()
        .position(|c| matches!(c, b'>' | b'<' | b'\n' | b'\r'))
        .map(|c| c + 1)
        .filter(|&c| b[c] == b'>');
    let end = match close {
        Some(c) => c,
        // No `>` on the line: only the tag name is escaped; the parser would otherwise read
        // the rest of the line (and beyond) as attributes.
        None => name_from + name_len,
    };
    if b[1..end].iter().any(|c| matches!(c, b'=' | b'"' | b'\'')) {
        if !end_tag && opened.len() < 64 && !is_opened(opened) {
            // Every delimiter is ASCII, so the name is on char boundaries.
            opened.push(s[name_from..name_from + name_len].to_ascii_lowercase());
        }
        return None;
    }
    Some(end)
}

/// The text of an HTML fragment (a feed summary, an iCal field, a `<title>` or `<textarea>`
/// that holds markup), whitespace collapsed. Only text nodes outside [`SKIP_TAGS`],
/// `textarea` and `iframe` come out, so the result holds no markup (rule 5); block edges and
/// line breaks separate words.
///
/// D-519: within `budget`. The parse is bounded ([`dom::fragment`]: the text of a fragment
/// nested past [`dom::NEST_MAX`] levels ends there), and when the budget runs out the text
/// is empty and [`Budget::spent`] says so. The walk over the tree keeps its own stack, so
/// no depth can exhaust the thread's.
pub(crate) fn fragment_text_in(s: &str, budget: &Budget) -> String {
    if !s.contains('<') && !s.contains('&') {
        return s.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    let Some(frag) = dom::fragment(s, budget) else {
        return String::new();
    };
    let mut out = String::new();
    let mut stack = vec![frag.html.tree.root()];
    while let Some(node) = stack.pop() {
        match node.value() {
            Node::Text(t) => out.push_str(t),
            Node::Element(e) => {
                let name = e.name();
                // Raw text, field values and frame content are never text (rule 5).
                if SKIP_TAGS.contains(&name) || matches!(name, "textarea" | "iframe") {
                    continue;
                }
                // Block edges and line breaks separate words; inline tags do not.
                if name == "br" || BLOCK_TAGS.contains(&name) {
                    out.push(' ');
                }
                stack.extend(node.children().rev());
            }
            _ => stack.extend(node.children().rev()),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// [`fragment_text_in`] when `s` holds markup (raw-text and RCDATA content such as a
/// `<title>` or `<textarea>`), else `s` unchanged.
fn no_markup(s: &str, budget: &Budget) -> String {
    if crate::http::has_tag(s) {
        fragment_text_in(s, budget)
    } else {
        s.to_string()
    }
}

/// D-5: a URL that only serves challenges (a challenge host, reCAPTCHA's path on google.com,
/// Cloudflare's challenge platform path).
pub fn challenge_url(url: &Url) -> Option<String> {
    // A fully qualified `challenges.cloudflare.com.` is the same host.
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    let on = |d: &str| host == d || host.ends_with(&format!(".{d}"));
    if CHALLENGE_HOSTS.iter().any(|d| on(d)) {
        return Some(format!("challenge host {host}"));
    }
    let path = url.path().to_ascii_lowercase();
    if on("google.com") && path.starts_with("/recaptcha/") {
        return Some("reCAPTCHA page".into());
    }
    if path == "/cdn-cgi/challenge-platform" || path.starts_with("/cdn-cgi/challenge-platform/") {
        return Some("Cloudflare challenge platform".into());
    }
    None
}

/// The cleaned HTML of `<main>` (or `[role=main]`, or the block holding the most paragraph
/// text), scripts, styles, comments and most attributes removed, capped at
/// [`RAW_MAX_CHARS`]. Not a model-facing tool output (CLAUDE.md rule 5, B-500): it is kept
/// for in-process site-tool hosts that need structure.
pub fn raw_main_html(html: &str) -> String {
    // D-519: a bounded parse (no deadline: in-process hosts call this on a fetched page).
    let Some(doc) = dom::document(html, &Budget::unlimited()) else {
        return String::new();
    };
    let doc = doc.html;
    let pick = ["main", "[role=main]"]
        .iter()
        .filter_map(|s| Selector::parse(s).ok())
        .find_map(|sel| doc.select(&sel).next())
        .or_else(|| densest(&doc))
        .unwrap_or_else(|| doc.root_element());
    let mut out = String::new();
    serialize(*pick, &mut out);
    escape::cap_chars(&out, RAW_MAX_CHARS)
}

// ---------------------------------------------------------------------------------------
// The walk

#[derive(Clone)]
struct Cx {
    hidden: bool,
    landmark: Option<&'static str>,
    /// Inside article/aside/main/nav/section: header and footer are not landmarks there.
    sectioning: bool,
    form: Option<String>,
    kind: BlockKind,
    level: Option<u8>,
    list_depth: u8,
    pre: bool,
    table: Option<u32>,
    thead: bool,
    /// Inside a table cell: block tags add a space instead of starting a block.
    flatten: bool,
}

impl Default for Cx {
    fn default() -> Cx {
        Cx {
            hidden: false,
            landmark: None,
            sectioning: false,
            form: None,
            kind: BlockKind::Paragraph,
            level: None,
            list_depth: 0,
            pre: false,
            table: None,
            thead: false,
            flatten: false,
        }
    }
}

/// The block being filled.
#[derive(Default)]
struct Builder {
    spans: Vec<Span>,
    kind: Option<BlockKind>,
    level: Option<u8>,
    landmark: Option<&'static str>,
    table: Option<u32>,
    header: bool,
}

struct Walker<'a> {
    base: Url,
    by_id: HashMap<&'a str, ElementRef<'a>>,
    label_for: HashMap<&'a str, ElementRef<'a>>,
    rows: Vec<IndexRow>,
    hrefs: HashMap<u32, String>,
    blocks: Vec<TextBlock>,
    cur: Builder,
    heading: Option<String>,
    last_text: String,
    next_id: u32,
    forms: u32,
    tables: u32,
    order: u32,
    challenge: Option<String>,
    /// D-517: when the walk must stop ([`parse_within`]); `out_of_time` once it has.
    budget: &'a Budget,
    steps: u32,
    out_of_time: bool,
    /// D-519: how many walk calls are open (the walk's own recursion depth), and whether a
    /// subtree past [`WALK_DEPTH_MAX`] was read as plain text.
    depth: usize,
    flattened: bool,
}

/// D-519: the deepest the walk recurses. A subtree below this depth is read as plain text
/// ([`collect_text`], which keeps its own stack) instead of walked: its controls get no
/// rows. Each level costs two stack frames (the walk and the element), so 256 levels stay
/// well inside the 2 MiB stack of a test or tokio blocking thread even in a debug build
/// (`html::tests::the_walk_fits_a_small_stack` walks the cap on a 1 MiB thread).
pub(crate) const WALK_DEPTH_MAX: usize = 256;

/// The walk looks at the clock once per this many nodes.
const CLOCK_EVERY: u32 = 256;

impl<'a> Walker<'a> {
    fn new(doc: &'a Html, base: Url, budget: &'a Budget) -> Walker<'a> {
        let mut by_id = HashMap::new();
        let mut label_for = HashMap::new();
        for e in doc.root_element().descendent_elements() {
            if let Some(id) = e.value().id() {
                by_id.entry(id).or_insert(e);
            }
            if e.value().name() == "label" {
                if let Some(f) = e.attr("for") {
                    label_for.entry(f).or_insert(e);
                }
            }
        }
        Walker {
            base,
            by_id,
            label_for,
            rows: Vec::new(),
            hrefs: HashMap::new(),
            blocks: Vec::new(),
            cur: Builder::default(),
            heading: None,
            last_text: String::new(),
            next_id: 1,
            forms: 0,
            tables: 0,
            order: 0,
            challenge: None,
            budget,
            steps: 0,
            out_of_time: false,
            depth: 0,
            flattened: false,
        }
    }

    fn walk(&mut self, node: NodeRef<'a, Node>, cx: &Cx) {
        if self.out_of_time {
            return;
        }
        self.steps = self.steps.wrapping_add(1);
        if self.steps % CLOCK_EVERY == 1 && self.budget.spent() {
            self.out_of_time = true;
            return;
        }
        match node.value() {
            Node::Text(t) => {
                if !cx.hidden {
                    self.push_text(t, cx);
                }
            }
            Node::Element(_) => {
                if let Some(e) = ElementRef::wrap(node) {
                    if self.depth >= WALK_DEPTH_MAX {
                        self.flat(e, cx);
                    } else {
                        self.depth += 1;
                        self.element(e, cx);
                        self.depth -= 1;
                    }
                }
            }
            Node::Document | Node::Fragment => {
                for c in node.children() {
                    self.walk(c, cx);
                }
            }
            _ => {}
        }
    }

    fn element(&mut self, e: ElementRef<'a>, cx: &Cx) {
        let tag = e.value().name();
        if SKIP_TAGS.contains(&tag) {
            return;
        }
        if tag == "input" && attr_lower(e, "type").as_deref() == Some("hidden") {
            return;
        }
        let hidden = cx.hidden || is_hidden(e);
        let mut cx2 = cx.clone();
        cx2.hidden = hidden;
        let landmark = landmark_name(e, cx);
        if let Some(lm) = landmark {
            cx2.landmark = Some(lm);
        }
        if matches!(tag, "article" | "aside" | "main" | "nav" | "section") {
            cx2.sectioning = true;
        }
        if tag == "form" {
            self.forms += 1;
            cx2.form = Some(format!("f{}", self.forms));
        }
        if !hidden && self.challenge.is_none() && challenge_box(e) {
            self.challenge = Some(format!(
                "captcha widget ({})",
                escape::name(&box_label(e), 60)
            ));
        }
        if let Some(role) = control_role(e) {
            self.control(e, role, &cx2);
            return;
        }
        match tag {
            "br" => {
                self.push_raw(if cx.pre { "\n" } else { " " }, cx);
                return;
            }
            "tr" if !cx.flatten => {
                self.table_row(e, &cx2);
                return;
            }
            "table" => {
                cx2.table = Some(self.tables);
                self.tables += 1;
                cx2.thead = false;
            }
            "thead" => cx2.thead = true,
            "tbody" | "tfoot" => cx2.thead = false,
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                cx2.kind = BlockKind::Heading;
                cx2.level = tag[1..].parse().ok();
            }
            "li" | "dd" | "dt" => {
                cx2.kind = BlockKind::ListItem;
                cx2.level = Some(cx.list_depth.max(1));
            }
            "ul" | "ol" | "dl" | "menu" => cx2.list_depth = cx.list_depth.saturating_add(1),
            "pre" => {
                cx2.kind = BlockKind::Code;
                cx2.level = None;
                cx2.pre = true;
            }
            "blockquote" => {
                cx2.kind = BlockKind::Quote;
                cx2.level = None;
            }
            _ => {}
        }
        if role_of(e).as_deref() == Some("heading") {
            cx2.kind = BlockKind::Heading;
            cx2.level = e.attr("aria-level").and_then(|l| l.trim().parse().ok());
        }
        let block = BLOCK_TAGS.contains(&tag) || landmark.is_some();
        if block {
            self.boundary(cx);
        }
        let closed_details = tag == "details" && e.attr("open").is_none();
        for c in e.children() {
            if closed_details {
                let summary = ElementRef::wrap(c).is_some_and(|ce| ce.value().name() == "summary");
                if !summary {
                    let mut hid = cx2.clone();
                    hid.hidden = true;
                    self.walk(c, &hid);
                    continue;
                }
            }
            self.walk(c, &cx2);
        }
        if block {
            self.boundary(cx);
        }
    }

    /// A block edge: a new block, or a space inside a table cell.
    fn boundary(&mut self, cx: &Cx) {
        if cx.flatten {
            self.push_raw(" ", cx);
        } else {
            self.flush();
        }
    }

    fn push_text(&mut self, t: &str, cx: &Cx) {
        if !cx.pre && t.trim().is_empty() {
            if !self.cur.spans.is_empty() {
                self.push_raw(" ", cx);
            }
            return;
        }
        let collapsed = t.split_whitespace().collect::<Vec<_>>().join(" ");
        if !collapsed.is_empty() {
            let mut last = format!("{} {collapsed}", self.last_text);
            let n = last.chars().count();
            if n > NEARBY_MAX {
                last = last.chars().skip(n - NEARBY_MAX).collect();
            }
            self.last_text = last.trim().to_string();
        }
        self.push_raw(t, cx);
    }

    /// Appends text to the current block, starting a new block when the kind or landmark
    /// changes (inline content that crosses a block context).
    fn push_raw(&mut self, t: &str, cx: &Cx) {
        if t.is_empty() {
            return;
        }
        self.ensure_block(cx);
        match self.cur.spans.last_mut() {
            Some(s) if s.elem.is_none() => s.text.push_str(t),
            _ => self.cur.spans.push(Span {
                text: t.to_string(),
                elem: None,
            }),
        }
    }

    fn push_elem(&mut self, text: String, elem: ElemRef, cx: &Cx) {
        self.ensure_block(cx);
        self.cur.spans.push(Span {
            text,
            elem: Some(elem),
        });
    }

    fn ensure_block(&mut self, cx: &Cx) {
        let kind = if cx.flatten {
            BlockKind::TableRow
        } else {
            cx.kind
        };
        if !self.cur.spans.is_empty()
            && (self.cur.kind != Some(kind) || self.cur.landmark != cx.landmark)
        {
            self.flush();
        }
        if self.cur.spans.is_empty() {
            self.cur.kind = Some(kind);
            self.cur.level = cx.level;
            self.cur.landmark = cx.landmark;
            self.cur.table = cx.table;
        }
    }

    fn flush(&mut self) {
        let b = std::mem::take(&mut self.cur);
        if b.spans.is_empty()
            || b.spans
                .iter()
                .all(|s| s.elem.is_none() && s.text.trim().is_empty())
        {
            return;
        }
        let kind = b.kind.unwrap_or(BlockKind::Paragraph);
        let mut spans: Vec<Span> = b
            .spans
            .into_iter()
            .map(|s| Span {
                text: if kind == BlockKind::Code {
                    escape::code(&s.text)
                } else {
                    escape::text(&s.text)
                },
                elem: s.elem,
            })
            .collect();
        let plain = spans
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
        if self.challenge.is_none() && challenge_text(&plain) {
            self.challenge = Some(format!(
                "human-check prompt \"{}\"",
                escape::name(&plain, 80)
            ));
        }
        let level = match kind {
            BlockKind::Heading => Some(b.level.unwrap_or(2).clamp(1, 6)),
            BlockKind::ListItem => b.level,
            _ => None,
        };
        let pieces = if kind == BlockKind::TableRow {
            // A row stays one row: a cell is capped instead of split.
            for s in &mut spans {
                cap_span(s, BLOCK_MAX_CHARS);
            }
            vec![spans]
        } else {
            split_long(spans, BLOCK_MAX_CHARS)
        };
        for mut spans in pieces {
            // A prose block cannot open with `# ` or `- ` and pass for a heading or outline
            // line (each piece of a split block too).
            if !matches!(kind, BlockKind::Code | BlockKind::Heading) {
                if let Some(first) = spans.first_mut().filter(|s| s.elem.is_none()) {
                    first.text = escape::block_start(&first.text);
                }
            }
            self.blocks.push(TextBlock {
                order: self.order,
                frame: 0,
                kind,
                level,
                landmark: b.landmark.map(str::to_string),
                heading: self.heading.clone(),
                table: if kind == BlockKind::TableRow {
                    b.table
                } else {
                    None
                },
                header: b.header,
                spans,
            });
            self.order += 1;
        }
        if kind == BlockKind::Heading && !plain.is_empty() {
            self.heading = Some(escape::cap_chars(&plain, NAME_MAX));
        }
    }

    fn table_row(&mut self, tr: ElementRef<'a>, cx: &Cx) {
        self.flush();
        let mut cells: Vec<Span> = Vec::new();
        let mut all_th = true;
        for c in tr.children() {
            let Some(cell) = ElementRef::wrap(c) else {
                continue;
            };
            let tag = cell.value().name();
            if tag != "td" && tag != "th" {
                continue;
            }
            if cx.hidden || is_hidden(cell) {
                // Hidden cells still get rows for their controls, but no text.
                let mut hid = cx.clone();
                hid.hidden = true;
                hid.flatten = true;
                self.walk(c, &hid);
                continue;
            }
            all_th &= tag == "th";
            let saved = std::mem::take(&mut self.cur);
            let mut ccx = cx.clone();
            ccx.flatten = true;
            for ch in cell.children() {
                self.walk(ch, &ccx);
            }
            let got = std::mem::replace(&mut self.cur, saved);
            let elems: Vec<ElemRef> = got.spans.iter().filter_map(|s| s.elem).collect();
            let text = got
                .spans
                .iter()
                .map(|s| {
                    if s.elem.is_some() && s.text.trim().is_empty() {
                        s.elem
                            .and_then(|r| self.rows.iter().find(|row| row.elem() == r))
                            .map(|row| row.name.clone())
                            .unwrap_or_default()
                    } else {
                        s.text.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            cells.push(Span {
                text,
                elem: if elems.len() == 1 {
                    Some(elems[0])
                } else {
                    None
                },
            });
        }
        if cells.is_empty() || cells.iter().all(|c| c.text.is_empty() && c.elem.is_none()) {
            return;
        }
        self.cur = Builder {
            spans: cells,
            kind: Some(BlockKind::TableRow),
            level: None,
            landmark: cx.landmark,
            table: cx.table,
            header: all_th || cx.thead,
        };
        self.flush();
    }

    fn control(&mut self, e: ElementRef<'a>, role: String, cx: &Cx) {
        let tag = e.value().name();
        let elem = self.row(e, &role, cx);
        // Controls nested inside this one (rare: a button in a link) get rows too, so ids
        // stay complete; their text is part of this control's name already.
        let mut inner = cx.clone();
        inner.hidden = true;
        self.nested_controls(*e, &inner);
        if cx.hidden {
            return;
        }
        if tag == "iframe" || tag == "frame" {
            // I-11: an iframe is a placeholder block linking its row.
            self.boundary(cx);
            let mut ocx = cx.clone();
            ocx.kind = BlockKind::Other;
            self.push_elem(String::new(), elem, &ocx);
            self.boundary(cx);
            return;
        }
        let text = match tag {
            "input" | "select" | "textarea" => String::new(),
            _ => self.inner_text(e, false),
        };
        // A permalink anchor (`§`, `¶`, `#` linking a fragment of this page) keeps its row
        // but adds nothing to the text: it would sit in every heading and outline entry.
        let permalink = role == "link"
            && e.attr("href")
                .is_some_and(|h| h.trim_start().starts_with('#'))
            && !text.chars().any(char::is_alphanumeric);
        if permalink {
            return;
        }
        self.push_elem(escape::cap_chars(&text, NAME_MAX), elem, cx);
    }

    /// The index row of control `e` (its id, name, state and flags); `cx.hidden` makes it
    /// invisible. Adds nothing to the text and does not look at nested controls.
    fn row(&mut self, e: ElementRef<'a>, role: &str, cx: &Cx) -> ElemRef {
        let role = role.to_string();
        let tag = e.value().name();
        let id = self.next_id;
        self.next_id += 1;
        let elem = ElemRef::main(id);
        let name = self.accessible_name(e, &role);
        let input_type =
            (tag == "input").then(|| attr_lower(e, "type").unwrap_or_else(|| "text".to_string()));
        let autocomplete = attr_lower(e, "autocomplete").filter(|a| !a.is_empty());
        let mut flags = Flags::default();
        let mut state = State {
            visible: !cx.hidden,
            enabled: e.attr("disabled").is_none()
                && attr_lower(e, "aria-disabled").as_deref() != Some("true"),
            input_type: input_type.clone(),
            autocomplete: autocomplete.clone(),
            opens_new_tab: attr_lower(e, "target").as_deref() == Some("_blank"),
            ..State::default()
        };
        // Checkable controls.
        match (tag, input_type.as_deref()) {
            ("input", Some("checkbox" | "radio")) => {
                state.checked = Some(e.attr("checked").is_some())
            }
            _ if matches!(
                role.as_str(),
                "checkbox" | "radio" | "switch" | "menuitemcheckbox" | "menuitemradio"
            ) =>
            {
                state.checked = Some(attr_lower(e, "aria-checked").as_deref() == Some("true"));
            }
            _ => {}
        }
        // Values: secrets are only ever empty or filled (H-2, B-5).
        let secret = input_type.as_deref() == Some("password")
            || autocomplete
                .as_deref()
                .is_some_and(|a| a.contains("password") || a.contains("cc-"));
        match tag {
            "input"
                if !matches!(
                    input_type.as_deref(),
                    Some("checkbox" | "radio" | "submit" | "button" | "reset" | "image" | "file")
                ) =>
            {
                let filled = e.attr("value").is_some_and(|v| !v.is_empty());
                state.value = Some(if secret {
                    if filled {
                        FieldValue::Filled
                    } else {
                        FieldValue::Empty
                    }
                } else {
                    match e.attr("value").filter(|v| !v.trim().is_empty()) {
                        Some(v) => FieldValue::Text(escape::name(v, VALUE_MAX)),
                        None => FieldValue::Empty,
                    }
                });
            }
            "textarea" => {
                // RCDATA: markup inside a textarea is text to the parser, not to rule 5.
                let t = no_markup(&e.text().collect::<String>(), self.budget);
                state.value = Some(if t.trim().is_empty() {
                    FieldValue::Empty
                } else if secret {
                    FieldValue::Filled
                } else {
                    FieldValue::Text(escape::name(&t, VALUE_MAX))
                });
            }
            "select" => {
                let mut selected = None;
                for o in e
                    .descendent_elements()
                    .filter(|o| o.value().name() == "option")
                {
                    // collect_text skips script, style and template source (rule 5):
                    // o.text() would hand their markup to the model.
                    let label = o.attr("label").map(str::to_string).unwrap_or_else(|| {
                        let mut s = String::new();
                        collect_text(*o, false, &mut s, true);
                        s.split_whitespace().collect::<Vec<_>>().join(" ")
                    });
                    let label = escape::name(&label, OPTION_MAX);
                    if o.attr("selected").is_some() && selected.is_none() {
                        selected = Some(label.clone());
                    }
                    if state.options.len() < OPTIONS_MAX && !label.is_empty() {
                        state.options.push(label);
                    }
                }
                if selected.is_none() && e.attr("multiple").is_none() {
                    selected = state.options.first().cloned();
                }
                state.value = Some(match selected {
                    Some(s) if !s.is_empty() => FieldValue::Text(s),
                    _ => FieldValue::Empty,
                });
            }
            _ => {}
        }
        // Submit controls.
        if cx.form.is_some() {
            let btype = attr_lower(e, "type");
            let submit = match tag {
                "button" => btype.is_none() || btype.as_deref() == Some("submit"),
                "input" => matches!(input_type.as_deref(), Some("submit" | "image")),
                _ => false,
            };
            if submit {
                state.submitish = true;
                flags.submit = true;
            }
        }
        // Frames are never read inside; a challenge frame is a CHALLENGE row (D-5).
        if tag == "iframe" || tag == "frame" {
            flags.opaque = true;
            if challenge_frame(e) {
                flags.challenge = true;
                if !cx.hidden && self.challenge.is_none() {
                    self.challenge = Some(format!("captcha frame \"{name}\""));
                }
            }
        }
        let nearby = (FIELD_ROLES.contains(&role.as_str()) && !self.last_text.is_empty())
            .then(|| escape::name(&self.last_text, NEARBY_MAX))
            .filter(|n| !n.eq_ignore_ascii_case(&name) && !name.contains(n.as_str()));
        if let Some(href) = e.attr("href") {
            if let Some(abs) = self.absolute(href) {
                self.hrefs.insert(id, abs);
            }
        }
        self.rows.push(IndexRow {
            id,
            frame: 0,
            role: role.clone(),
            name: name.clone(),
            nearby,
            heading: self.heading.clone(),
            state,
            flags,
            form: cx.form.clone(),
            landmark: cx.landmark.map(str::to_string),
            child_frame: None,
            bbox: None,
        });
        elem
    }

    /// Rows for the controls under `node`, in document order, all with `cx` (hidden: their
    /// text is part of the outer control's name already). D-519: one loop with its own
    /// stack, also for controls inside controls, so no nesting costs thread stack.
    fn nested_controls(&mut self, node: NodeRef<'a, Node>, cx: &Cx) {
        let mut stack: Vec<NodeRef<'a, Node>> = node.children().rev().collect();
        while let Some(c) = stack.pop() {
            let Some(ce) = ElementRef::wrap(c) else {
                continue;
            };
            let tag = ce.value().name();
            if SKIP_TAGS.contains(&tag) || tag == "option" {
                continue;
            }
            if let Some(role) = control_role(ce) {
                self.row(ce, &role, cx);
            }
            stack.extend(c.children().rev());
        }
    }

    /// D-519: a subtree past [`WALK_DEPTH_MAX`] as plain text in the current block.
    fn flat(&mut self, e: ElementRef<'a>, cx: &Cx) {
        self.flattened = true;
        if cx.hidden {
            return;
        }
        let mut s = String::new();
        collect_text(*e, false, &mut s, false);
        self.push_text(&s, cx);
    }

    /// The B-5 name ladder, approximated: aria-labelledby, aria-label, label, content,
    /// placeholder, title.
    fn accessible_name(&self, e: ElementRef<'a>, role: &str) -> String {
        let tag = e.value().name();
        let raw = (|| {
            if let Some(ids) = e.attr("aria-labelledby") {
                let t: Vec<String> = ids
                    .split_whitespace()
                    .filter_map(|id| self.by_id.get(id))
                    .map(|r| self.inner_text(*r, false))
                    .filter(|t| !t.is_empty())
                    .collect();
                if !t.is_empty() {
                    return t.join(" ");
                }
            }
            if let Some(l) = e.attr("aria-label").filter(|l| !l.trim().is_empty()) {
                return l.to_string();
            }
            let field = matches!(tag, "input" | "select" | "textarea");
            if field {
                let itype = attr_lower(e, "type").unwrap_or_default();
                if matches!(itype.as_str(), "submit" | "button" | "reset") {
                    return e.attr("value").map(str::to_string).unwrap_or_else(|| {
                        if itype == "reset" { "Reset" } else { "Submit" }.to_string()
                    });
                }
                if itype == "image" {
                    if let Some(a) = e.attr("alt") {
                        return a.to_string();
                    }
                }
                if let Some(l) = e.value().id().and_then(|id| self.label_for.get(id)) {
                    let t = self.inner_text(*l, true);
                    if !t.is_empty() {
                        return t;
                    }
                }
                if let Some(l) = e
                    .ancestors()
                    .filter_map(ElementRef::wrap)
                    .find(|a| a.value().name() == "label")
                {
                    let t = self.inner_text(l, true);
                    if !t.is_empty() {
                        return t;
                    }
                }
                if let Some(p) = e.attr("placeholder").filter(|p| !p.trim().is_empty()) {
                    return p.to_string();
                }
            } else if tag == "iframe" || tag == "frame" {
                return e
                    .attr("title")
                    .or_else(|| e.attr("name"))
                    .unwrap_or("")
                    .to_string();
            } else {
                let t = self.inner_text(e, false);
                if !t.is_empty() {
                    return t;
                }
            }
            if let Some(t) = e.attr("title") {
                return t.to_string();
            }
            let _ = role;
            String::new()
        })();
        escape::name(&raw, NAME_MAX)
    }

    /// Visible text under `e` (image alt text included), collapsed. `skip_controls` leaves
    /// out form controls (a label's own input or select).
    fn inner_text(&self, e: ElementRef<'a>, skip_controls: bool) -> String {
        let mut out = String::new();
        collect_text(*e, skip_controls, &mut out, true);
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn absolute(&self, href: &str) -> Option<String> {
        let u = self.base.join(href.trim()).ok()?;
        matches!(u.scheme(), "http" | "https").then(|| u.to_string())
    }
}

/// D-517: the most characters one text block holds. A longer block (a text/plain body with
/// no blank line, a page that is one paragraph) is split into blocks of at most this many,
/// at a space where one is near: the reader re-reads a whole block for every chunk it plans,
/// so one block of megabytes costs time that grows with its square.
const BLOCK_MAX_CHARS: usize = 8_000;

/// How far back from a cut [`split_long`] looks for a space to cut at instead.
const SPLIT_LOOKBACK: usize = 200;

/// `spans` in groups of at most `max` characters each. A linked span is never split (one
/// longer than `max` is capped, [`cap_span`]); a plain span is cut at the last whitespace
/// within [`SPLIT_LOOKBACK`] characters of the limit, else at the limit, never inside an
/// `&lt;`/`&gt;` escape.
fn split_long(spans: Vec<Span>, max: usize) -> Vec<Vec<Span>> {
    let total: usize = spans.iter().map(|s| s.text.len()).sum();
    if total <= max {
        // Bytes bound chars from above: nothing to split.
        return vec![spans];
    }
    let mut out: Vec<Vec<Span>> = Vec::new();
    let mut cur: Vec<Span> = Vec::new();
    let mut used = 0usize;
    for mut s in spans {
        let len = s.text.chars().count();
        if used + len <= max {
            used += len;
            cur.push(s);
            continue;
        }
        if s.elem.is_some() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cap_span(&mut s, max);
            used = s.text.chars().count();
            cur.push(s);
            continue;
        }
        let mut rest = s.text.as_str();
        while !rest.is_empty() {
            if used >= max {
                out.push(std::mem::take(&mut cur));
                used = 0;
            }
            let at = cut_point(rest, max - used);
            let (piece, tail) = rest.split_at(at);
            used += piece.chars().count();
            cur.push(Span {
                text: piece.to_string(),
                elem: None,
            });
            rest = tail;
            if !rest.is_empty() {
                out.push(std::mem::take(&mut cur));
                used = 0;
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Byte offset at which to cut `s` so the head holds at most `room` characters (at least
/// one): after the last whitespace within [`SPLIT_LOOKBACK`] characters before the limit,
/// else at the limit moved back before an `&` that starts an escape it would cut.
fn cut_point(s: &str, room: usize) -> usize {
    let limit = match s.char_indices().nth(room.max(1)) {
        Some((at, _)) => at,
        None => return s.len(),
    };
    let head = &s[..limit];
    let floor = head
        .char_indices()
        .rev()
        .nth(SPLIT_LOOKBACK)
        .map_or(0, |(at, _)| at);
    if let Some(ws) = head[floor..].rfind(char::is_whitespace) {
        let at = floor + ws;
        if at > 0 {
            // Cut after the whitespace char.
            return at + head[at..].chars().next().map_or(1, char::len_utf8);
        }
    }
    // `&lt;` and `&gt;` are four bytes: an `&` in the last three bytes would be cut. `&` is
    // ASCII, so its position is a char boundary.
    let from = limit.saturating_sub(3);
    match head.as_bytes()[from..].iter().rposition(|&b| b == b'&') {
        Some(amp) if from + amp > 0 => from + amp,
        _ => limit,
    }
}

/// Caps a span's text at `max` characters, marking the cut with ` ...`.
fn cap_span(s: &mut Span, max: usize) {
    if s.text.len() > max && s.text.chars().count() > max {
        s.text = format!("{} ...", escape::cap_chars(&s.text, max.saturating_sub(4)));
    }
}

/// Visible text under `node` into `out` (image alt text included). `top`: `node` is the
/// element asked about, whose own hidden state the caller has decided. D-519: an explicit
/// stack, so a subtree of any depth costs no thread stack.
fn collect_text(node: NodeRef<'_, Node>, skip_controls: bool, out: &mut String, top: bool) {
    let mut stack = vec![(node, top)];
    while let Some((node, top)) = stack.pop() {
        let el = match node.value() {
            Node::Text(t) => {
                out.push(' ');
                out.push_str(t);
                continue;
            }
            Node::Element(el) => el,
            _ => continue,
        };
        let tag = el.name();
        // A frame's text is a separate document, never read (module doc, rule 5).
        if SKIP_TAGS.contains(&tag) || matches!(tag, "iframe" | "frame" | "frameset") {
            continue;
        }
        // A textarea adds nothing to a name: its text is the field's value (maybe a
        // secret, rule 5) and is reported once, as the value, through no_markup.
        if tag == "textarea" {
            continue;
        }
        if let Some(e) = ElementRef::wrap(node) {
            if !top && is_hidden(e) {
                continue;
            }
        }
        if skip_controls && matches!(tag, "input" | "select" | "textarea") {
            continue;
        }
        if tag == "img" {
            if let Some(a) = el.attr("alt") {
                out.push(' ');
                out.push_str(a);
            }
            continue;
        }
        stack.extend(node.children().rev().map(|c| (c, false)));
    }
}

// ---------------------------------------------------------------------------------------
// Element predicates

fn attr_lower(e: ElementRef<'_>, name: &str) -> Option<String> {
    e.attr(name).map(|v| v.trim().to_ascii_lowercase())
}

fn role_of(e: ElementRef<'_>) -> Option<String> {
    e.attr("role")
        .and_then(|r| r.split_whitespace().next())
        .map(str::to_ascii_lowercase)
}

fn is_hidden(e: ElementRef<'_>) -> bool {
    if e.attr("hidden").is_some() {
        return true;
    }
    if attr_lower(e, "aria-hidden").as_deref() == Some("true") {
        return true;
    }
    if e.value().name() == "dialog" && e.attr("open").is_none() {
        return true;
    }
    if let Some(style) = e.attr("style") {
        let s: String = style
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        if s.contains("display:none") || s.contains("visibility:hidden") {
            return true;
        }
    }
    false
}

fn has_acc_name(e: ElementRef<'_>) -> bool {
    ["aria-label", "aria-labelledby", "title"]
        .iter()
        .any(|a| e.attr(a).is_some_and(|v| !v.trim().is_empty()))
}

/// The landmark `e` opens, if any (injected.js `landmarkName`).
fn landmark_name(e: ElementRef<'_>, cx: &Cx) -> Option<&'static str> {
    if let Some(role) = role_of(e) {
        let lm = match role.as_str() {
            "main" => Some("main"),
            "navigation" => Some("nav"),
            "banner" => Some("header"),
            "contentinfo" => Some("footer"),
            "complementary" => Some("aside"),
            "form" | "search" => Some("form"),
            "region" => Some("region"),
            "dialog" | "alertdialog" => Some("dialog"),
            _ => None,
        };
        if lm.is_some() || (role != "none" && role != "presentation") {
            return lm;
        }
    }
    match e.value().name() {
        "main" => Some("main"),
        "nav" => Some("nav"),
        "aside" => Some("aside"),
        "dialog" => e.attr("open").map(|_| "dialog"),
        "header" if !cx.sectioning => Some("header"),
        "footer" if !cx.sectioning => Some("footer"),
        "section" if has_acc_name(e) => Some("region"),
        "form" if has_acc_name(e) => Some("form"),
        "search" => Some("form"),
        _ => None,
    }
}

/// The ARIA role of a control, or `None` when `e` is not one.
fn control_role(e: ElementRef<'_>) -> Option<String> {
    if let Some(r) = role_of(e) {
        if CONTROL_ROLES.contains(&r.as_str()) {
            return Some(r);
        }
    }
    let tag = e.value().name();
    let role = match tag {
        "a" | "area" if e.attr("href").is_some() => "link",
        "button" => "button",
        "input" => match attr_lower(e, "type").as_deref().unwrap_or("text") {
            "hidden" => return None,
            "button" | "submit" | "reset" | "image" | "file" | "color" => "button",
            "checkbox" => "checkbox",
            "radio" => "radio",
            "range" => "slider",
            "number" => "spinbutton",
            "search" => "searchbox",
            _ => "textbox",
        },
        "select" => {
            let size = e
                .attr("size")
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(0);
            if e.attr("multiple").is_some() || size > 1 {
                "listbox"
            } else {
                "combobox"
            }
        }
        "textarea" => "textbox",
        "iframe" | "frame" => "iframe",
        "summary" => "button",
        _ => {
            let editable = attr_lower(e, "contenteditable")
                .is_some_and(|v| v.is_empty() || v == "true" || v == "plaintext-only");
            if editable {
                "textbox"
            } else if e.attr("onclick").is_some() && onclick_button(e) {
                "button"
            } else {
                return None;
            }
        }
    };
    Some(role.to_string())
}

/// An `onclick` element is a button when it is short and holds no other control.
fn onclick_button(e: ElementRef<'_>) -> bool {
    let text: String = e.text().collect::<String>();
    if text.split_whitespace().map(str::len).sum::<usize>() > ONCLICK_TEXT_MAX {
        return false;
    }
    !e.descendent_elements()
        .skip(1)
        .any(|d| control_role(d).is_some())
}

/// A visible captcha widget container (injected.js `challengeBox`, D-405 (4), D-436).
fn challenge_box(e: ElementRef<'_>) -> bool {
    let tag = e.value().name();
    if NOT_BOX_TAGS.contains(&tag) || tag == "iframe" || tag == "frame" {
        return false;
    }
    let label = box_label(e).to_ascii_lowercase();
    let named = ["captcha", "turnstile", "geetest", "funcaptcha", "arkose"]
        .iter()
        .any(|w| label.contains(w))
        || e.attr("data-sitekey").is_some()
        || e.attr("data-hcaptcha-widget-id").is_some();
    named && !passive(e)
}

fn box_label(e: ElementRef<'_>) -> String {
    let classes = e.value().classes().collect::<Vec<_>>().join(" ");
    format!("{} {}", classes, e.value().id().unwrap_or(""))
        .trim()
        .to_string()
}

/// reCAPTCHA v3 / invisible widgets have nothing for a user to solve (D-405 (5), D-436).
fn passive(e: ElementRef<'_>) -> bool {
    let in_badge = std::iter::once(e)
        .chain(e.ancestors().filter_map(ElementRef::wrap))
        .any(|a| {
            a.value()
                .classes()
                .any(|c| c.eq_ignore_ascii_case("grecaptcha-badge"))
                || attr_lower(a, "data-size").as_deref() == Some("invisible")
        });
    in_badge
}

/// A challenge frame by its src, title, name or aria-label (injected.js
/// `challengeFrameEl`), unless it is a passive one.
fn challenge_frame(e: ElementRef<'_>) -> bool {
    let src = e.attr("src").unwrap_or("");
    if let Ok(u) = Url::parse(src) {
        let host = u.host_str().unwrap_or("").to_ascii_lowercase();
        let recaptcha = host == "recaptcha.net"
            || host.ends_with(".recaptcha.net")
            || ((host == "google.com" || host.ends_with(".google.com"))
                && u.path().starts_with("/recaptcha/"));
        if recaptcha && u.query().unwrap_or("").contains("size=invisible") {
            return false;
        }
        if challenge_url(&u).is_some() {
            return !passive(e);
        }
    }
    let label = [
        e.attr("src"),
        e.attr("title"),
        e.attr("name"),
        e.attr("aria-label"),
    ]
    .iter()
    .flatten()
    .copied()
    .collect::<Vec<_>>()
    .join(" ")
    .to_ascii_lowercase();
    let words = [
        "captcha",
        "turnstile",
        "cloudflare security challenge",
        "verify you are human",
        "human verification",
        "verification challenge",
        "arkose",
        "funcaptcha",
        "geetest",
    ];
    words.iter().any(|w| label.contains(w)) && !passive(e)
}

/// A short human-check prompt (injected.js `CHALLENGE_TEXT_RE`, the hold-to-verify rule).
fn challenge_text(plain: &str) -> bool {
    let n = plain.chars().count();
    if !(8..=CHALLENGE_TEXT_MAX).contains(&n) {
        return false;
    }
    let t = plain.to_lowercase().replace('\u{2019}', "'");
    let prompts = [
        "verify you are human",
        "verify that you are human",
        "verify you're human",
        "verify that you're human",
        "verify you are a human",
        "verify you are not a robot",
        "verify you are not a bot",
        "are you a robot",
        "i'm not a robot",
        "i am not a robot",
        "human verification",
        "checking your browser before accessing",
    ];
    if prompts.iter().any(|p| t.contains(p)) {
        return true;
    }
    let hold = t.contains("press and hold") || t.contains("press & hold");
    hold && t
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| matches!(w, "human" | "humans" | "robot" | "robots" | "bot" | "bots"))
}

// ---------------------------------------------------------------------------------------
// Page-level helpers

fn page_title(doc: &Html, budget: &Budget) -> String {
    let Ok(sel) = Selector::parse("title") else {
        return String::new();
    };
    doc.select(&sel)
        .next()
        // RCDATA: markup inside a title is text to the parser, not to rule 5.
        .map(|t| escape::name(&no_markup(&t.text().collect::<String>(), budget), NAME_MAX))
        .unwrap_or_default()
}

fn base_url(doc: &Html, url: &Url) -> Url {
    Selector::parse("base[href]")
        .ok()
        .and_then(|sel| doc.select(&sel).next())
        .and_then(|b| b.attr("href"))
        .and_then(|h| url.join(h).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .unwrap_or_else(|| url.clone())
}

/// The element whose direct `<p>` children hold the most text (readability's heuristic).
fn densest(doc: &Html) -> Option<ElementRef<'_>> {
    let mut best: Option<(usize, ElementRef<'_>)> = None;
    for e in doc.root_element().descendent_elements() {
        if !matches!(e.value().name(), "article" | "section" | "div" | "td") {
            continue;
        }
        let score: usize = e
            .child_elements()
            .filter(|c| c.value().name() == "p")
            .map(|p| p.text().map(str::len).sum::<usize>())
            .sum();
        if score > 0 && best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, e));
        }
    }
    best.map(|(_, e)| e)
}

const RAW_ATTRS: &[&str] = &[
    "href",
    "src",
    "alt",
    "title",
    "id",
    "name",
    "type",
    "role",
    "aria-label",
    "colspan",
    "rowspan",
    "for",
    "action",
    "method",
    "value",
    "placeholder",
];
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// D-519: an explicit stack of nodes to open and end tags to write, so a tree of any depth
/// costs no thread stack.
fn serialize(node: NodeRef<'_, Node>, out: &mut String) {
    enum Step<'a> {
        Open(NodeRef<'a, Node>),
        Close(&'a str),
    }
    let mut stack = vec![Step::Open(node)];
    while let Some(step) = stack.pop() {
        let node = match step {
            Step::Open(n) => n,
            Step::Close(tag) => {
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
                continue;
            }
        };
        let el = match node.value() {
            Node::Text(t) => {
                out.push_str(&esc_html(t));
                continue;
            }
            Node::Element(el) => el,
            _ => continue,
        };
        {
            let tag = el.name();
            if SKIP_TAGS.contains(&tag) || tag == "iframe" {
                continue;
            }
            if let Some(e) = ElementRef::wrap(node) {
                if is_hidden(e) {
                    continue;
                }
            }
            out.push('<');
            out.push_str(tag);
            for (k, v) in el.attrs() {
                if !RAW_ATTRS.contains(&k) {
                    continue;
                }
                // Never a field value for secrets (rule 2): drop every value attribute of
                // password-like inputs.
                if k == "value"
                    && (el
                        .attr("type")
                        .is_some_and(|t| t.eq_ignore_ascii_case("password"))
                        || el
                            .attr("autocomplete")
                            .is_some_and(|a| a.contains("password") || a.contains("cc-")))
                {
                    continue;
                }
                out.push(' ');
                out.push_str(k);
                out.push_str("=\"");
                out.push_str(&esc_html(v).replace('"', "&quot;"));
                out.push('"');
            }
            out.push('>');
            if VOID.contains(&tag) {
                continue;
            }
            stack.push(Step::Close(tag));
            stack.extend(node.children().rev().map(Step::Open));
        }
    }
}

fn esc_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review T-13b round 2: a trailing dot names the same host (D-5).
    #[test]
    fn a_challenge_host_with_a_trailing_dot_is_a_challenge() {
        for u in [
            "https://challenges.cloudflare.com./turnstile",
            "https://WWW.HCAPTCHA.COM./x",
            "https://www.google.com./recaptcha/api2/anchor",
        ] {
            let url = Url::parse(u).unwrap();
            assert!(challenge_url(&url).is_some(), "{u}");
        }
        let plain = Url::parse("https://example.com./x").unwrap();
        assert!(challenge_url(&plain).is_none());
    }

    fn page(html: &str) -> Doc {
        parse(html, &Url::parse("https://example.com/a/b").unwrap())
    }

    #[test]
    fn iframe_and_textarea_text_never_reach_names() {
        // review-T-12 final, Blocking: collect_text read raw iframe text into every name.
        let d = "<!DOCTYPE html><html><body><script>stealCookies()</script>";
        let src = format!(
            r#"<html><body>
            <a href="/x">Go<iframe>{d}</iframe></a>
            <button>Do<iframe>{d}</iframe></button>
            <label for="n">Name<iframe>{d}</iframe></label><input id="n">
            <span id="l">Mail<textarea>{d}</textarea></span><input aria-labelledby="l">
            <select><option>A<iframe>{d}</iframe></option></select>
            </body></html>"#
        );
        let doc = page(&src);
        let mut all = String::new();
        for r in &doc.index {
            all.push_str(&format!(
                "{} {:?} {:?}\n",
                r.name, r.state.options, r.state.value
            ));
        }
        for b in &doc.blocks {
            all.push_str(&format!("{:?}\n", b));
        }
        for bad in ["<", "DOCTYPE", "stealCookies"] {
            assert!(!all.contains(bad), "{bad} in {all}");
        }
        for name in ["Go", "Do", "Name"] {
            assert!(doc.index.iter().any(|r| r.name == name), "{name} in {all}");
        }
    }

    #[test]
    fn textarea_content_never_reaches_a_name() {
        // review-T-12 confirm, Blocking 1-2: a label's own textarea (secrets included) and
        // double-decoded entities reached names through collect_text.
        let doc = page(
            r#"<html><body>
            <label>Pass <textarea autocomplete="current-password">hunter2secret</textarea></label>
            <label for="p2">Pass2</label><textarea id="p2" autocomplete="current-password">hunter3secret</textarea>
            <label>Card <textarea autocomplete="cc-number">4111111111111111</textarea></label>
            <label>Comment <textarea>prefilled body text</textarea></label>
            <span id="l3">X<textarea>&amp;lt;script&amp;gt;stealCookies()&amp;lt;/script&amp;gt;</textarea></span>
            <input aria-labelledby="l3">
            <span id="l4">Secret<textarea autocomplete="new-password">hunter4secret</textarea></span>
            <input aria-labelledby="l4">
            </body></html>"#,
        );
        let mut all = String::new();
        for r in &doc.index {
            all.push_str(&format!("{} {:?}\n", r.name, r.state.value));
        }
        for b in &doc.blocks {
            all.push_str(&format!("{:?}\n", b));
        }
        for bad in [
            "hunter2secret",
            "hunter3secret",
            "hunter4secret",
            "4111111111111111",
            "<script",
        ] {
            assert!(!all.contains(bad), "{bad} in {all}");
        }
        // The l3 textarea's value is shown once, as a browser shows it (`&lt;script&gt;`,
        // decoded once); it never joins the input's name.
        let x = doc.index.iter().find(|r| r.name.starts_with('X')).unwrap();
        assert!(x.name == "X", "{all}");
        let names: Vec<&str> = doc.index.iter().map(|r| r.name.as_str()).collect();
        for want in ["Pass", "Pass2", "Card", "Comment", "X", "Secret"] {
            assert!(names.contains(&want), "{want} in {names:?}");
        }
    }

    #[test]
    fn option_labels_carry_no_script_source() {
        // review-T-12 closing round, Blocking 1: o.text() returned script/template source.
        let d = page(
            r#"<html><body><label for="s">Pick</label><select id="s">
            <option>A<script><!DOCTYPE html><html><body><iframe src=x></script></option>
            <option>B<style>p{}</style><template><head><meta></template></option>
            </select></body></html>"#,
        );
        let sel = d
            .index
            .iter()
            .find(|r| r.role == "combobox" || r.role == "listbox");
        let sel = sel.expect("select row");
        let all = format!("{:?} {:?}", sel.state.options, sel.state.value);
        for bad in ["<", "DOCTYPE", "iframe", "script", "meta", "p{}"] {
            assert!(!all.contains(bad), "{bad} in {all}");
        }
        assert!(sel.state.options.iter().any(|o| o == "A"), "{all}");
        assert!(sel.state.options.iter().any(|o| o == "B"), "{all}");
    }

    #[test]
    fn rows_and_blocks() {
        let d = page(
            r#"<html><head><title>T</title></head><body>
            <nav><a href="/home">Home</a></nav>
            <main><h1>Title <a href="x">link</a></h1>
            <p>Hello <b>world</b>.</p>
            <form aria-label="Sign up"><label for="e">Email</label><input id="e" type="email">
            <input type="password" value="hunter2-not-real"><button>Join</button></form>
            <ul><li>one</li><li>two</li></ul>
            <table><tr><th>a</th><th>b</th></tr><tr><td>1</td><td><a href="/r">r</a></td></tr></table>
            </main></body></html>"#,
        );
        assert_eq!(d.title, "T");
        let roles: Vec<&str> = d.index.iter().map(|r| r.role.as_str()).collect();
        assert_eq!(
            roles,
            ["link", "link", "textbox", "textbox", "button", "link"]
        );
        assert_eq!(d.index[0].landmark.as_deref(), Some("nav"));
        assert_eq!(d.index[2].name, "Email");
        assert_eq!(d.index[2].form.as_deref(), Some("f1"));
        assert_eq!(d.index[2].landmark.as_deref(), Some("form"));
        assert_eq!(d.index[3].state.value, Some(FieldValue::Filled));
        assert!(d.index[4].flags.submit);
        assert_eq!(
            d.hrefs.get(&1).map(String::as_str),
            Some("https://example.com/home")
        );
        assert_eq!(
            d.hrefs.get(&2).map(String::as_str),
            Some("https://example.com/a/x")
        );
        let h1 = d
            .blocks
            .iter()
            .find(|b| b.kind == BlockKind::Heading)
            .unwrap();
        assert_eq!(h1.level, Some(1));
        assert_eq!(h1.landmark.as_deref(), Some("main"));
        let rows: Vec<&TextBlock> = d
            .blocks
            .iter()
            .filter(|b| b.kind == BlockKind::TableRow)
            .collect();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].header && !rows[1].header);
        assert_eq!(rows[1].spans[1].elem, Some(ElemRef::main(6)));
        assert_eq!(rows[0].table, Some(0));
        let items = d
            .blocks
            .iter()
            .filter(|b| b.kind == BlockKind::ListItem)
            .count();
        assert_eq!(items, 2);
        assert!(d.challenge.is_none());
        let all = serde_json_free_dump(&d);
        assert!(!all.contains("hunter2"));
    }

    fn serde_json_free_dump(d: &Doc) -> String {
        format!("{:?} {:?}", d.index, d.blocks)
    }

    #[test]
    fn hidden_content_is_not_text_but_controls_keep_rows() {
        let d = page(
            r#"<body><p hidden>gone1</p><p style="display: none">gone2</p>
            <div aria-hidden="true">gone3 <button>B</button></div>
            <details><summary>More</summary><p>gone4</p></details><p>kept</p></body>"#,
        );
        let text = format!("{:?}", d.blocks);
        for g in ["gone1", "gone2", "gone3", "gone4"] {
            assert!(!text.contains(g), "{g}");
        }
        assert!(text.contains("kept"));
        let b = d.index.iter().find(|r| r.name == "B").unwrap();
        assert!(!b.state.visible);
    }

    #[test]
    fn challenges() {
        let turnstile =
            page(r#"<body><form><div class="cf-turnstile" data-sitekey="x"></div></form></body>"#);
        assert!(turnstile.challenge.is_some());
        let frame = page(
            r#"<body><iframe src="https://www.google.com/recaptcha/api2/anchor?k=1" title="reCAPTCHA"></iframe></body>"#,
        );
        assert!(frame.challenge.is_some());
        assert!(frame.index[0].flags.challenge);
        let invisible = page(
            r#"<body><div class="grecaptcha-badge"><iframe src="https://www.google.com/recaptcha/api2/anchor?size=invisible"></iframe></div><p>ok</p></body>"#,
        );
        assert!(invisible.challenge.is_none());
        let prompt = page("<body><p>Please verify you are human to continue.</p></body>");
        assert!(prompt.challenge.is_some());
        let article = page(&format!(
            "<body><p>{} verify you are human {}</p></body>",
            "long ".repeat(50),
            "text ".repeat(10)
        ));
        assert!(article.challenge.is_none());
        let cf = page("<html><head><title>Just a moment...</title></head><body></body></html>");
        assert!(cf.challenge.is_some());
    }

    #[test]
    fn raw_main_is_clean_and_capped() {
        let html = format!(
            "<body><nav>n</nav><main><script>x()</script><p onclick=\"y\">a &amp; b</p><input type=password value=\"pw-not-real\">{}</main></body>",
            "<p>z</p>".repeat(20_000)
        );
        let raw = raw_main_html(&html);
        assert!(raw.starts_with("<main><p>a &amp; b</p>"));
        assert!(
            !raw.contains("script") && !raw.contains("onclick") && !raw.contains("pw-not-real")
        );
        assert!(raw.chars().count() <= RAW_MAX_CHARS);
    }

    /// The text of every block of a text/plain walk, one line per block.
    fn plain_blocks(text: &str) -> String {
        let doc = parse_text(text, &Url::parse("https://example.com/f.txt").unwrap());
        doc.blocks
            .iter()
            .map(|b| b.spans.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// review-T-12 should-fix, D-512: angle-bracket text in a text/plain body is kept,
    /// escaped; markup in it is still dropped.
    #[test]
    fn plain_text_keeps_angle_bracket_text_escaped() {
        let t = plain_blocks(
            "fn f() -> Vec<String> { todo!() }\n\n#include <stdio.h>\n\nHashMap<K, V> and \
             </T> and x<y then z > 3\n\nmail <a.b@c.de> at <https://x.org/a>",
        );
        for want in [
            "Vec&lt;String&gt; {",
            "#include &lt;stdio.h&gt;",
            "HashMap&lt;K, V&gt; and &lt;/T&gt; and x&lt;y then z &gt; 3",
            "mail &lt;a.b@c.de&gt; at &lt;https://x.org/a&gt;",
        ] {
            assert!(t.contains(want), "missing {want:?} in {t:?}");
        }
        assert!(!crate::http::has_tag(&t), "{t}");
        // Markup is still markup: known names, custom elements, attributes, comments.
        let t = plain_blocks(
            "<p>Kept</p><script>stealCookies()</script><my-el>Mine</my-el>\
             <foo bar=\"1\">Attr</foo><!-- <T> hidden -->After <b>bold</b>",
        );
        assert!(
            t.contains("Kept") && t.contains("Mine") && t.contains("Attr"),
            "{t}"
        );
        assert!(t.contains("After bold"), "{t}");
        for bad in ["stealCookies", "<", "&lt;", "hidden"] {
            assert!(!t.contains(bad), "{bad} in {t:?}");
        }
        assert_eq!(
            plain_source("a < b, 3<4, <!x>, <?y>"),
            "a < b, 3<4, <!x>, <?y>"
        );
        assert_eq!(plain_source("<String>"), format!("{LT}String{GT}"));
        assert_eq!(plain_source("<P>x</P>"), "<P>x</P>");
        assert_eq!(plain_source("<é>"), "<é>");
        // The bracket window ends at PLAIN_BRACKET_MAX bytes: a longer run is not one bracket.
        let long = format!("<x {}>", "y".repeat(PLAIN_BRACKET_MAX));
        assert_eq!(
            plain_source(&long),
            format!("{LT}x {}>", "y".repeat(PLAIN_BRACKET_MAX))
        );
        let fits = format!("<x {}>", "y".repeat(PLAIN_BRACKET_MAX - 4));
        assert!(plain_source(&fits).ends_with(GT), "{fits}");
    }

    /// review-T-12b Blocking (W-2): hostile angle-bracket runs of several MB on one line
    /// are escaped in linear time (the window past each `<` is bounded), and the walk of
    /// such a body finishes well within W-2's budget even in a debug build.
    #[test]
    fn hostile_angle_runs_are_linear() {
        let mb = 1024 * 1024;
        for unit in ["<x", "<x ", "<ab/", "<x=", "</q", "<abc-"] {
            let body = unit.repeat(4 * mb / unit.len());
            let started = Instant::now();
            let out = plain_source(&body);
            let took = started.elapsed();
            assert!(out.len() >= body.len(), "{unit}");
            assert!(
                took < std::time::Duration::from_secs(3),
                "plain_source of 4 MB of {unit:?} took {took:?}"
            );
        }
        let body = "<x".repeat(2 * mb);
        let started = Instant::now();
        let doc = parse_text_within(
            &body,
            &Url::parse("https://example.com/f.txt").unwrap(),
            started + std::time::Duration::from_secs(60),
        )
        .expect("the walk finished");
        let took = started.elapsed();
        assert!(
            took < std::time::Duration::from_secs(12),
            "the walk of 4 MB of `<x` took {took:?}"
        );
        assert!(!doc.blocks.is_empty());
    }

    /// D-517: a long block is split into blocks of at most BLOCK_MAX_CHARS characters, at a
    /// space when one is near, never inside an escape; nothing is lost.
    #[test]
    fn long_blocks_are_split_without_losing_text() {
        let plain = |t: &str| Span {
            text: t.to_string(),
            elem: None,
        };
        let words = "word ".repeat(5_000);
        let groups = split_long(vec![plain(&words)], 1_000);
        assert!(groups.len() >= 25, "{}", groups.len());
        let mut joined = String::new();
        for g in &groups {
            let n: usize = g.iter().map(|s| s.text.chars().count()).sum();
            assert!(n <= 1_000, "{n}");
            joined.extend(g.iter().map(|s| s.text.as_str()));
        }
        assert_eq!(joined, words);
        assert!(groups[0][0].text.ends_with("word "), "cut at a space");
        // One long word of escapes and multi-byte chars: cut by characters, never in `&lt;`.
        let word = "&lt;éx".repeat(3_000);
        let groups = split_long(vec![plain(&word)], 997);
        let mut joined = String::new();
        for g in &groups {
            let t: String = g.iter().map(|s| s.text.as_str()).collect();
            assert!(t.chars().count() <= 997);
            assert!(
                !t.ends_with('&') && !t.ends_with("&l") && !t.ends_with("&lt"),
                "{t}"
            );
            joined.push_str(&t);
        }
        assert_eq!(joined, word);
        // A short block is one group, untouched.
        assert_eq!(split_long(vec![plain("a b")], 1_000).len(), 1);
        // The walk splits a long paragraph into several blocks.
        let doc = parse(
            &format!("<p>{}</p>", "word ".repeat(10_000)),
            &Url::parse("https://example.com/").unwrap(),
        );
        assert!(doc.blocks.len() >= 6, "{}", doc.blocks.len());
        assert!(doc.blocks.iter().all(|b| {
            b.spans
                .iter()
                .map(|s| s.text.chars().count())
                .sum::<usize>()
                <= BLOCK_MAX_CHARS
        }));
    }

    /// D-514 (rule 5): markup a text/html page spells with entities, or a literal `<` the
    /// parser keeps as text, reaches no block, code block, link name or title as a tag.
    #[test]
    fn entity_spelled_markup_in_html_is_shown_escaped() {
        let doc = page(
            "<title>T &lt;b&gt;x&lt;/b&gt;</title><p>a &lt;script&gt;steal()&lt;/script&gt; \
             and &#60;img src=x&#62; and a < b</p><pre>&lt;i&gt;code&lt;/i&gt;</pre>\
             <a href=\"/x\">&lt;svg onload=1&gt;</a>",
        );
        let text: String = doc
            .blocks
            .iter()
            .flat_map(|b| b.spans.iter().map(|s| s.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        let names: String = doc.index.iter().map(|r| r.name.clone()).collect();
        for t in [&text, &names, &doc.title] {
            assert!(!crate::http::has_tag(t), "{t}");
            assert!(!t.contains('<') && !t.contains('>'), "{t}");
        }
        assert!(
            text.contains("a &lt;script&gt;steal()&lt;/script&gt; and &lt;img src=x&gt;"),
            "{text}"
        );
        assert!(text.contains("a &lt; b"), "{text}");
        assert!(text.contains("&lt;i&gt;code&lt;/i&gt;"), "{text}");
        assert!(names.contains("&lt;svg onload=1&gt;"), "{names}");
        // A title that holds markup once decoded loses it (no_markup, as before).
        assert_eq!(doc.title, "T x");
    }

    #[test]
    fn known_names_are_found_in_any_case() {
        assert_eq!(
            KNOWN_ELEMENTS.iter().map(|k| k.len()).max(),
            Some(KNOWN_NAME_MAX)
        );
        for k in KNOWN_ELEMENTS {
            assert!(known_element(k.as_bytes()), "{k}");
            assert!(known_element(k.to_ascii_uppercase().as_bytes()), "{k}");
        }
        for no in ["string", "t", "stdio.h", "selectedcontentx", "x"] {
            assert!(!known_element(no.as_bytes()), "{no}");
        }
    }

    /// D-517: a walk whose deadline has passed stops and returns nothing; one with time
    /// finishes.
    #[test]
    fn a_walk_past_its_deadline_returns_nothing() {
        let url = Url::parse("https://example.com/p").unwrap();
        let html = "<div><p>words</p></div>".repeat(5_000);
        assert!(parse_within(&html, &url, Instant::now()).is_none());
        assert!(parse_text_within("plain words", &url, Instant::now()).is_none());
        let later = Instant::now() + std::time::Duration::from_secs(60);
        assert!(parse_within(&html, &url, later).is_some_and(|d| !d.blocks.is_empty()));
    }

    /// Runs `f` on a thread with a 1 MiB stack, half of what a test or tokio blocking thread
    /// gets. A stack overflow aborts the whole test binary, so passing proves the margin.
    fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn(f)
            .unwrap()
            .join()
            .unwrap()
    }

    fn shown(d: &Doc) -> String {
        let mut all = String::new();
        for b in &d.blocks {
            for s in &b.spans {
                all.push_str(&s.text);
                all.push(' ');
            }
        }
        for r in &d.index {
            all.push_str(&r.name);
            all.push(' ');
        }
        all
    }

    /// D-519, review-T-12b round 2 Blocking 1: a page nested deeper than the walk recurses
    /// (but not past the parse cap) is walked on half the usual stack, whatever the nesting
    /// is made of, and its deepest text is shown as plain text with a note.
    #[test]
    fn the_walk_fits_a_small_stack() {
        // (open, close, levels per repetition)
        let shapes = [
            ("<div>", "</div>", 1),
            ("<span>", "</span>", 1),
            ("<div role=button>", "</div>", 1),
            ("<a href=/x><div role=button>", "</div></a>", 2),
            ("<table><tr><td>", "</td></tr></table>", 4),
            ("<ul><li>", "</li></ul>", 2),
            ("<details open><summary>s</summary>", "</details>", 1),
            ("<label>L ", "</label>", 1),
            ("<form><fieldset>", "</fieldset></form>", 1),
        ];
        for (open, close, per) in shapes {
            let n = (dom::NEST_MAX - 40) / per;
            assert!(n * per > WALK_DEPTH_MAX + 100, "{open}");
            let page = format!(
                "<p>top words</p>{}deepest words <a href=/deep>deep link</a>{}<p>after words</p>",
                open.repeat(n),
                close.repeat(n)
            );
            let d = on_small_stack(move || page_at(&page));
            let all = shown(&d);
            for want in ["top words", "deepest words", "after words"] {
                assert!(all.contains(want), "{open}: {want} missing");
            }
            // A control takes its subtree as its name (nested controls get rows in one
            // loop); anything else is walked to the cap and read as plain text below it.
            let cut = d.cut.clone().unwrap_or_default();
            if open.contains("role=button") {
                assert_eq!(cut, "", "{open}");
                assert!(d.index.len() > n, "{open}: {} rows", d.index.len());
            } else {
                assert!(cut.contains("plain text"), "{open}: {cut:?}");
            }
        }
        // A page within the walk's depth has no note.
        let page = format!("{}x", "<div>".repeat(WALK_DEPTH_MAX - 10));
        assert_eq!(on_small_stack(move || page_at(&page)).cut, None);
    }

    fn page_at(html: &str) -> Doc {
        parse(html, &Url::parse("https://example.com/a/b").unwrap())
    }

    /// D-519, review-T-12b round 2 Blockings 1 and 2: 100 000 nested elements (as HTML and,
    /// with known names, as text/plain) parse only to the nesting cap, walk on half the
    /// usual stack, and say the rest is missing; the text before them is shown.
    #[test]
    fn a_hundred_thousand_nested_elements_return_quickly() {
        let url = Url::parse("https://example.com/p").unwrap();
        for open in ["<div>", "<span>", "<b>", "<div role=button>", "<td>"] {
            for plain in [false, true] {
                let page = format!("before words\n\n{}deep words", open.repeat(100_000));
                let u = url.clone();
                let t = Instant::now();
                let d = on_small_stack(move || {
                    if plain {
                        parse_text(&page, &u)
                    } else {
                        parse(&page, &u)
                    }
                });
                let took = t.elapsed();
                assert!(took.as_secs() < 10, "{open} plain={plain}: {took:?}");
                let all = shown(&d);
                assert!(all.contains("before words"), "{open} plain={plain}");
                if open != "<td>" {
                    // `<td>` outside a table is dropped by the parser: nothing nests.
                    let cut = d.cut.clone().unwrap_or_default();
                    assert!(
                        cut.contains("nested too deep"),
                        "{open} plain={plain}: {cut:?}"
                    );
                    assert!(!all.contains("deep words"), "{open} plain={plain}");
                }
            }
        }
        // The other walks over a parsed tree: fragment text and the raw main HTML.
        let deep = format!("<main>a {}b", "<div><i>".repeat(50_000));
        let (text, raw) = on_small_stack(move || {
            (
                fragment_text_in(&deep, &Budget::unlimited()),
                raw_main_html(&deep),
            )
        });
        assert!(text.starts_with("a"), "{text:.40}");
        assert!(raw.starts_with("<main>a <div><i>"), "{raw:.40}");
    }
}
