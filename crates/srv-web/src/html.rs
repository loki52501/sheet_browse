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

use crate::escape;
use ego_tree::NodeRef;
use reqwest::Url;
use sb_sheet::{BlockKind, ElemRef, FieldValue, Flags, IndexRow, Span, State, TextBlock};
use scraper::{ElementRef, Html, Node, Selector};
use std::collections::HashMap;

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
}

/// Walks `html` fetched from `url` (the final URL, used to resolve links).
pub fn parse(html: &str, url: &Url) -> Doc {
    let doc = Html::parse_document(html);
    let base = base_url(&doc, url);
    let mut w = Walker::new(&doc, base);
    w.walk(doc.tree.root(), &Cx::default());
    w.flush();
    let title = page_title(&doc);
    let mut challenge = challenge_url(url).or(w.challenge);
    if challenge.is_none() {
        let t = title.trim().to_ascii_lowercase();
        if t.starts_with("just a moment") || t.starts_with("attention required! | cloudflare") {
            challenge = Some(format!("challenge interstitial \"{}\"", title.trim()));
        }
    }
    Doc {
        title,
        index: w.rows,
        blocks: w.blocks,
        hrefs: w.hrefs,
        challenge,
    }
}

/// A body not declared as HTML (`text/plain`, no content type, anything else fetch reads)
/// through the same walk as [`parse`] (rule 5, D-508: fail closed by construction). Nothing
/// decides whether the body "is" HTML: whatever markup it holds is parsed and only text
/// nodes come out, and text without markup comes out as itself. A `<p>` is put at every
/// blank line first, so plain text keeps its paragraphs (adding tags cannot let markup
/// through: the parser still reads all of it). Without a `<title>`, the first block names
/// the page.
pub fn parse_text(text: &str, url: &Url) -> Doc {
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
    let mut doc = parse(&marked, url);
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
    doc
}

/// Title of a page without a `<title>`: the start of its first block.
const TEXT_TITLE_MAX: usize = 100;

/// The text of an HTML fragment (a feed summary, an iCal field, a `<title>` or `<textarea>`
/// that holds markup), whitespace collapsed. Only text nodes outside [`SKIP_TAGS`],
/// `textarea` and `iframe` come out, so the result holds no markup (rule 5); block edges and
/// line breaks separate words.
pub(crate) fn fragment_text(s: &str) -> String {
    if !s.contains('<') && !s.contains('&') {
        return s.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    let frag = Html::parse_fragment(s);
    let mut out = String::new();
    for node in frag.tree.root().descendants() {
        match node.value() {
            Node::Text(t) => {
                let hidden = node.ancestors().any(|a| {
                    a.value().as_element().is_some_and(|e| {
                        SKIP_TAGS.contains(&e.name()) || matches!(e.name(), "textarea" | "iframe")
                    })
                });
                if !hidden {
                    out.push_str(t);
                }
            }
            // Block edges and line breaks separate words; inline tags do not.
            Node::Element(e) if e.name() == "br" || BLOCK_TAGS.contains(&e.name()) => {
                out.push(' ');
            }
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// [`fragment_text`] when `s` holds markup (raw-text and RCDATA content such as a `<title>`
/// or `<textarea>`), else `s` unchanged.
fn no_markup(s: &str) -> String {
    if crate::http::has_tag(s) {
        fragment_text(s)
    } else {
        s.to_string()
    }
}

/// D-5: a URL that only serves challenges (a challenge host, reCAPTCHA's path on google.com,
/// Cloudflare's challenge platform path).
pub fn challenge_url(url: &Url) -> Option<String> {
    let host = url.host_str()?.to_ascii_lowercase();
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
    let doc = Html::parse_document(html);
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
}

impl<'a> Walker<'a> {
    fn new(doc: &'a Html, base: Url) -> Walker<'a> {
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
        }
    }

    fn walk(&mut self, node: NodeRef<'a, Node>, cx: &Cx) {
        match node.value() {
            Node::Text(t) => {
                if !cx.hidden {
                    self.push_text(t, cx);
                }
            }
            Node::Element(_) => {
                if let Some(e) = ElementRef::wrap(node) {
                    self.element(e, cx);
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
        // A prose block cannot open with `# ` or `- ` and pass for a heading or outline line.
        if !matches!(kind, BlockKind::Code | BlockKind::Heading) {
            if let Some(first) = spans.first_mut().filter(|s| s.elem.is_none()) {
                first.text = escape::block_start(&first.text);
            }
        }
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
                let t = no_markup(&e.text().collect::<String>());
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

    /// Rows for the controls under `node` (outermost ones; each handles its own subtree).
    fn nested_controls(&mut self, node: NodeRef<'a, Node>, cx: &Cx) {
        for c in node.children() {
            let Some(ce) = ElementRef::wrap(c) else {
                continue;
            };
            let tag = ce.value().name();
            if SKIP_TAGS.contains(&tag) || tag == "option" {
                continue;
            }
            match control_role(ce) {
                Some(role) => self.control(ce, role, cx),
                None => self.nested_controls(c, cx),
            }
        }
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

fn collect_text(node: NodeRef<'_, Node>, skip_controls: bool, out: &mut String, top: bool) {
    match node.value() {
        Node::Text(t) => {
            out.push(' ');
            out.push_str(t);
        }
        Node::Element(el) => {
            let tag = el.name();
            // A frame's text is a separate document, never read (module doc, rule 5).
            if SKIP_TAGS.contains(&tag) || matches!(tag, "iframe" | "frame" | "frameset") {
                return;
            }
            // A textarea adds nothing to a name: its text is the field's value (maybe a
            // secret, rule 5) and is reported once, as the value, through no_markup.
            if tag == "textarea" {
                return;
            }
            if let Some(e) = ElementRef::wrap(node) {
                if !top && is_hidden(e) {
                    return;
                }
            }
            if skip_controls && matches!(tag, "input" | "select" | "textarea") {
                return;
            }
            if tag == "img" {
                if let Some(a) = el.attr("alt") {
                    out.push(' ');
                    out.push_str(a);
                }
                return;
            }
            for c in node.children() {
                collect_text(c, skip_controls, out, false);
            }
        }
        _ => {}
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

fn page_title(doc: &Html) -> String {
    let Ok(sel) = Selector::parse("title") else {
        return String::new();
    };
    doc.select(&sel)
        .next()
        // RCDATA: markup inside a title is text to the parser, not to rule 5.
        .map(|t| escape::name(&no_markup(&t.text().collect::<String>()), NAME_MAX))
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

fn serialize(node: NodeRef<'_, Node>, out: &mut String) {
    match node.value() {
        Node::Text(t) => out.push_str(&esc_html(t)),
        Node::Element(el) => {
            let tag = el.name();
            if SKIP_TAGS.contains(&tag) || tag == "iframe" {
                return;
            }
            if let Some(e) = ElementRef::wrap(node) {
                if is_hidden(e) {
                    return;
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
                return;
            }
            for c in node.children() {
                serialize(c, out);
            }
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
        _ => {}
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
}
