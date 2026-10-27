//! W-2 `fetch(url, mode="read")` and W-6 `find(q)` on a fetched page.
//!
//! A fetch without `cursor` or `region` always goes to the network and returns the page
//! skeleton (B-6.1, with the T-3 `tools for this site:` line), the first read chunk (W-5)
//! and a `cursor:` line when there is more. `cursor` and `region` re-read the cached page
//! (refetching only when it is not cached), so a continued read never repeats text and
//! costs no request. `mode="sheet"` returns numbered element lines (B-6.3).
//!
//! `mode="raw"` is refused (B-500): CLAUDE.md rule 5 says the model never receives raw
//! HTML. [`crate::html::raw_main_html`] stays in the crate for in-process hosts.
//!
//! `fetch(url="#12")` follows link `#12` of the last fetched page (D-502), resolved once in
//! classify and pinned for call ([`crate::pin`]), so the gated domain is the fetched one.
//!
//! Rule 5 fails closed by construction (D-508): nothing tries to recognise HTML. A JSON or
//! feed content type is refused with a pointer to `api` or `feed`, media and binary bodies
//! are refused, and every other body (HTML, text/plain, no content type, anything else) is
//! decoded and goes through the HTML walk, which keeps only text nodes. The model only sees
//! sb-sheet output built from the walk, never the body string.
//!
//! Requests go through [`http::get_page`]: one deadline for redirects and body, and the
//! private-address guard (a URL not written as a private address, or a `#id` link on a page
//! that was not, never reaches loopback, private or link-local addresses).

use crate::cache::Page;
use crate::html::Doc;
use crate::http::{self, host_of};
use crate::pin::Pins;
use crate::{escape, html, opt_str, opt_usize, req_str, site, Web};
use async_trait::async_trait;
use harness_core::{
    Class, Classified, StopReason, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec,
};
use reqwest::Url;
use sb_sheet::{Cursor, ElemRef, FindOpts, PageMeta, ReadOpts, SheetOpts, SkeletonOpts};
use serde_json::{json, Value};
use std::sync::Arc;

/// W-5 default chunk size, applied here so the documented default is visible in code.
pub const CHUNK_TOKENS: usize = 4000;
/// `mode="sheet"` output cap in characters (the sheet itself has no cap, B-6.3).
pub const SHEET_MAX_CHARS: usize = 60_000;
/// W-6 default hits per query part, and the clamp.
pub const FIND_K: usize = 3;
pub const FIND_K_MAX: usize = 10;

const ACCEPT_PAGE: &str = "text/html,application/xhtml+xml;q=0.9,text/plain;q=0.8,*/*;q=0.5";

/// The refusal text for `mode="raw"` (B-500).
pub const RAW_REFUSED: &str = "mode=raw is not available: the model never receives raw HTML \
     (CLAUDE.md rule 5, B-500). Use mode=read (optionally with region), mode=sheet, or find(q).";

pub(crate) fn spec(web: Arc<Web>) -> ToolSpec {
    ToolSpec {
        name: "fetch".into(),
        description: "HTTP GET a web page and read it without a browser. mode=read (default): \
            page skeleton, the first chunk as light markdown with [text](#id) links, and a \
            cursor; pass cursor (or region=\"## Heading\") with the same url to continue on the \
            cached page. mode=sheet: numbered element lines. url=\"#12\" follows link #12 of \
            the last fetched page. Fetching a page also announces that site's tools (a \
            `tools for this site:` line, e.g. a job board's list_jobs); prefer them over raw \
            api calls."
            .into(),
        class: Class::Read,
        args: json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "http(s) URL, or #id of a link on the last fetched page"},
                "mode": {"type": "string", "enum": ["read", "sheet"], "default": "read"},
                "cursor": {"type": "string", "description": "from a previous read of the same url"},
                "region": {"type": "string", "description": "landmark (main, nav, footer...) or heading (\"## FAQ\")"},
                "max_tokens": {"type": "integer", "minimum": 1, "description": "cap for the whole read; pass it again with each cursor"}
            },
            "required": ["url"]
        }),
        handler: Arc::new(Fetch {
            web,
            pins: Pins::default(),
        }),
    }
}

pub(crate) fn find_spec(web: Arc<Web>) -> ToolSpec {
    ToolSpec {
        name: "find".into(),
        description:
            "Find elements on a fetched page (the browser's scorer): q is comma-separated \
            parts, `name:role` forces a role. Uses the last fetched page unless url is given. \
            k hits per part (default 3, max 10)."
                .into(),
        class: Class::Read,
        args: json!({
            "type": "object",
            "properties": {
                "q": {"type": "string"},
                "url": {"type": "string", "description": "a page fetched earlier in this run"},
                "k": {"type": "integer", "minimum": 1, "maximum": FIND_K_MAX, "default": FIND_K}
            },
            "required": ["q"]
        }),
        handler: Arc::new(Find { web }),
    }
}

struct Fetch {
    web: Arc<Web>,
    /// `#id` targets resolved by classify, taken by call (rule 4, [`crate::pin`]).
    pins: Pins<Target>,
}

/// What to fetch, and whether it may reach a private address ([`http::get_page`]).
#[derive(Clone)]
struct Target {
    url: Url,
    allow_private: bool,
}

/// `url` is `#id` (a link on the last page), not a URL.
fn is_ref(args: &Value) -> bool {
    opt_str(args, "url")
        .ok()
        .flatten()
        .is_some_and(|u| u.trim().starts_with('#'))
}

enum Loaded {
    Page(Arc<Page>),
    Challenge(ToolOutput),
}

impl Fetch {
    /// The URL to fetch: the argument, or the href of link `#id` on the last page (D-502).
    /// It may reach a private address when the user wrote one: the argument itself, or for
    /// `#id` the URL of the page that holds the link.
    fn target(&self, args: &Value) -> Result<Target, ToolError> {
        let raw = req_str(args, "url")?.trim();
        if raw.starts_with('#') {
            let r: ElemRef = raw
                .parse()
                .map_err(|_| ToolError::Args("url: expected an http(s) URL or #id".into()))?;
            let page = self.web.cache.last().ok_or_else(|| {
                ToolError::Args(format!("url {raw}: no page fetched yet; fetch a URL first"))
            })?;
            let href = (r.frame == 0)
                .then(|| page.doc.hrefs.get(&r.id))
                .flatten()
                .ok_or_else(|| {
                    ToolError::Args(format!(
                        "url {raw}: that element on the last page has no link to follow"
                    ))
                })?;
            let url =
                http::parse_url(href).map_err(|e| ToolError::Args(format!("url {raw}: {e}")))?;
            let allow_private = Url::parse(&page.url)
                .ok()
                .is_some_and(|p| http::written_private(&p));
            return Ok(Target { url, allow_private });
        }
        let url = http::parse_url(raw).map_err(|e| ToolError::Args(format!("url: {e}")))?;
        let allow_private = http::written_private(&url);
        Ok(Target { url, allow_private })
    }

    async fn load(&self, ctx: &ToolCtx, target: &Target) -> Result<Loaded, ToolError> {
        let url = &target.url;
        let clients = self.web.clients()?;
        if let Some(why) = html::challenge_url(url) {
            self.web.challenged(url);
            return Ok(Loaded::Challenge(challenge_output(url, &why)));
        }
        let body = http::get_page(
            &self.web.cfg,
            clients,
            url,
            ACCEPT_PAGE,
            target.allow_private,
        )
        .await
        .map_err(|e| ToolError::Failed(format!("fetch: {e}")))?;
        let final_url = body.url.clone();
        let final_host = host_of(&final_url);
        // D-582: a challenge drops the cached pages of the site asked for and of the one
        // that answered.
        let challenged = || {
            self.web.challenged(url);
            self.web.challenged(&final_url);
        };
        if cf_mitigated(&body) {
            challenged();
            return Ok(Loaded::Challenge(challenge_output(
                &final_url,
                "Cloudflare challenge (cf-mitigated)",
            )));
        }
        let mime = body.mime.clone();
        // D-511: the header is server text; only a plain media type is shown as written.
        let shown_type = || http::shown_mime(&mime);
        if is_json_type(&mime) {
            return Err(ToolError::Failed(format!(
                "fetch: {final_host} answered with JSON ({}); use api(url) for JSON",
                shown_type()
            )));
        }
        if is_feed_type(&mime) {
            return Err(ToolError::Failed(format!(
                "fetch: {final_host} answered with {}; use feed(url) for feeds and calendars",
                shown_type()
            )));
        }
        if is_media_type(&mime) {
            return Err(ToolError::Failed(format!(
                "fetch: {final_host} answered with {}, which fetch does not read (pages and \
                 text only)",
                shown_type()
            )));
        }
        // D-508: every other body is decoded (BOM, charset) and walked; nothing is sniffed.
        let html_type = is_html_type(&mime);
        let text = body.text(mime != "text/plain");
        if http::looks_binary(&text) {
            return Err(ToolError::Failed(format!(
                "fetch: {final_host} answered with binary data ({}), which fetch does not read",
                shown_type()
            )));
        }
        let doc = walk(text, final_url.clone(), !html_type, "fetch").await?;
        if let Some(why) = doc.challenge.clone() {
            challenged();
            return Ok(Loaded::Challenge(challenge_output(&final_url, &why)));
        }
        let (folder, site_tools) = site::register(&self.web, ctx, &final_url, &doc.index).await;
        let page = Arc::new(Page {
            requested: url.to_string(),
            url: final_url.to_string(),
            doc,
            status: body.status,
            truncated: body.truncated,
            folder,
            site_tools,
        });
        self.web.cache.insert(page.clone());
        Ok(Loaded::Page(page))
    }
}

/// `text/html` or XHTML by declared type (walked as a document; anything else fetch reads is
/// walked as text with its paragraphs kept, [`html::parse_text`]).
pub(crate) fn is_html_type(mime: &str) -> bool {
    mime == "text/html" || mime == "application/xhtml+xml"
}

/// A JSON content type: `application/json`, `text/json` or any `+json` type.
pub(crate) fn is_json_type(mime: &str) -> bool {
    mime == "application/json" || mime == "text/json" || mime.ends_with("+json")
}

/// A feed or calendar content type (`feed` reads these), XHTML and SVG excluded.
pub(crate) fn is_feed_type(mime: &str) -> bool {
    !is_html_type(mime)
        && !is_media_type(mime)
        && (mime.contains("xml") || mime.contains("rss") || mime.contains("calendar"))
}

/// Images, audio, video, fonts and archives: never text.
fn is_media_type(mime: &str) -> bool {
    ["image/", "audio/", "video/", "font/"]
        .iter()
        .any(|p| mime.starts_with(p))
        || matches!(
            mime,
            "application/pdf"
                | "application/zip"
                | "application/gzip"
                | "application/x-gzip"
                | "application/x-tar"
                | "application/wasm"
        )
}

/// The response carries `cf-mitigated: challenge` (Cloudflare's challenge marker).
pub(crate) fn cf_mitigated(body: &http::Body) -> bool {
    body.headers
        .get("cf-mitigated")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("challenge"))
}

/// The W-2 walk of decoded `text`, off the async threads: as a document, or (`plain`) as
/// text whose blank lines separate paragraphs ([`html::parse_text`]).
pub(crate) async fn walk(
    text: String,
    base: Url,
    plain: bool,
    tool: &str,
) -> Result<Doc, ToolError> {
    tokio::task::spawn_blocking(move || {
        if plain {
            html::parse_text(&text, &base)
        } else {
            html::parse(&text, &base)
        }
    })
    .await
    .map_err(|_| ToolError::Failed(format!("{tool}: the HTML walk failed")))
}

/// D-5 by the response alone, for api and feed: the `cf-mitigated: challenge` header or a
/// challenge URL (after redirects). Costs no body work.
pub(crate) fn challenge_by_response(body: &http::Body) -> Option<ToolOutput> {
    let why = if cf_mitigated(body) {
        Some("Cloudflare challenge (cf-mitigated)".to_string())
    } else {
        html::challenge_url(&body.url)
    };
    why.map(|w| challenge_output(&body.url, &w))
}

/// D-5 by the body, for api and feed bodies that are not shown: decoded `text` with markup
/// in it goes through fetch's walk, and a challenge it finds stops the call. `None` when
/// the body holds no challenge.
pub(crate) async fn challenge_in_text(
    body: &http::Body,
    text: &str,
    tool: &str,
) -> Result<Option<ToolOutput>, ToolError> {
    if !http::has_tag(text) {
        return Ok(None);
    }
    let doc = walk(text.to_string(), body.url.clone(), false, tool).await?;
    Ok(doc.challenge.map(|w| challenge_output(&body.url, &w)))
}

/// D-5: a challenge stops the call; nothing of the page is read.
pub(crate) fn challenge_output(url: &Url, why: &str) -> ToolOutput {
    let host = host_of(url);
    let why = escape::name(why, 160);
    ToolOutput {
        text: format!(
            "{host} answered with a human check ({why}). I do not solve these (D-5): open \
             {} in your browser, complete it yourself, then tell me to continue.",
            escape::name(url.as_str(), 300)
        ),
        summary: Some(format!("challenge at {host}")),
        consequences: json!({ "url": url.as_str(), "challenge": true }),
        files: Vec::new(),
        stopped: Some(StopReason {
            class: Class::Challenge,
            why: format!("challenge page at {host}: {why}"),
            element: None,
        }),
    }
}

#[async_trait]
impl ToolHandler for Fetch {
    async fn classify(&self, ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let target = self.target(args)?;
        let url = target.url.clone();
        if is_ref(args) {
            // The last page can change before call runs; call fetches exactly this URL.
            self.pins.put(ctx, args, target);
        }
        let host = host_of(&url);
        Ok(Classified {
            class: Some(Class::Read),
            domain: Some(toolstore::registrable(&host)),
            what: Some(format!("fetch {}", escape::name(url.as_str(), 200))),
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let target = if is_ref(&args) {
            self.pins.take(ctx, &args)?
        } else {
            self.target(&args)?
        };
        let url = target.url.clone();
        let mode = opt_str(&args, "mode")?
            .map(|m| m.trim().to_ascii_lowercase())
            .unwrap_or_else(|| "read".into());
        match mode.as_str() {
            "read" | "sheet" | "" => {}
            "raw" => return Err(ToolError::Args(RAW_REFUSED.into())),
            other => {
                return Err(ToolError::Args(format!(
                    "mode must be read or sheet, not {:?}",
                    escape::name(other, 20)
                )))
            }
        }
        let cursor = opt_str(&args, "cursor")?
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string);
        let region = opt_str(&args, "region")?
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_string);
        let max_tokens = opt_usize(&args, "max_tokens")?;
        if max_tokens == Some(0) {
            return Err(ToolError::Args("max_tokens must be at least 1".into()));
        }
        let continued = cursor.is_some() || region.is_some();
        let cached = if continued {
            self.web.cache.get(url.as_str())
        } else {
            None
        };
        let page = match cached {
            Some(p) => p,
            None => match self.load(ctx, &target).await? {
                Loaded::Page(p) => p,
                Loaded::Challenge(out) => return Ok(out),
            },
        };
        let text = if mode == "sheet" {
            render_sheet(&page, region, !continued)
        } else {
            render_read(&page, cursor, region, max_tokens, !continued)
        };
        Ok(ToolOutput {
            text,
            summary: Some(format!(
                "fetched {} ({} elements)",
                escape::name(&page.url, 200),
                page.doc.index.len()
            )),
            consequences: json!({ "url": page.url, "status": page.status }),
            files: Vec::new(),
            stopped: None,
        })
    }
}

fn skeleton(page: &Page) -> String {
    let meta = PageMeta {
        url: page.url.clone(),
        title: page.doc.title.clone(),
        site_tools: page.site_tools.clone(),
        dialog: None,
    };
    let mut s = sb_sheet::skeleton(&page.doc.index, &page.doc.blocks, &SkeletonOpts { meta });
    if page.status >= 400 {
        s.push_str(&format!("\nHTTP {}", page.status));
    }
    if page.truncated {
        s.push_str("\npage cut at the 5 MB cap; the end of the page is missing");
    }
    s
}

fn render_read(
    page: &Page,
    cursor: Option<String>,
    region: Option<String>,
    max_tokens: Option<usize>,
    with_skeleton: bool,
) -> String {
    let out = sb_sheet::read(
        &page.doc.index,
        &page.doc.blocks,
        &ReadOpts {
            region,
            cursor: cursor.map(Cursor),
            max_tokens,
            chunk_tokens: CHUNK_TOKENS,
        },
    );
    let mut s = String::new();
    if with_skeleton {
        s.push_str(&skeleton(page));
        s.push_str("\n\n");
    }
    s.push_str(&out.markdown);
    if let Some(c) = out.cursor {
        s.push_str("\n\ncursor: ");
        s.push_str(&c.0);
    }
    s
}

fn render_sheet(page: &Page, region: Option<String>, with_skeleton: bool) -> String {
    let sheet = sb_sheet::sheet(&page.doc.index, &page.doc.blocks, &SheetOpts { region });
    let mut s = String::new();
    if with_skeleton {
        s.push_str(&skeleton(page));
        s.push_str("\n\n");
    }
    if sheet.chars().count() > SHEET_MAX_CHARS {
        let cut = escape::cap_chars(&sheet, SHEET_MAX_CHARS);
        let cut = cut.rsplit_once('\n').map(|(a, _)| a).unwrap_or(&cut);
        s.push_str(cut);
        s.push_str(&format!(
            "\n\ntruncated: sheet cut at {SHEET_MAX_CHARS} characters; pass region to see a part"
        ));
    } else {
        s.push_str(&sheet);
    }
    s
}

struct Find {
    web: Arc<Web>,
}

#[async_trait]
impl ToolHandler for Find {
    async fn call(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let q = req_str(&args, "q")?.trim().to_string();
        let k = match opt_usize(&args, "k")? {
            None => FIND_K,
            Some(0) => return Err(ToolError::Args("k must be at least 1".into())),
            Some(k) => k.min(FIND_K_MAX),
        };
        let page = match opt_str(&args, "url")?
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            Some(u) => {
                let key = http::parse_url(u)
                    .map(|p| p.to_string())
                    .unwrap_or_else(|_| u.to_string());
                self.web.cache.get(&key).ok_or_else(|| {
                    ToolError::Args("that url was not fetched in this run; fetch(url) first".into())
                })?
            }
            None => {
                self.web.cache.last().ok_or_else(|| {
                    ToolError::Args("no page fetched yet; fetch(url) first".into())
                })?
            }
        };
        let hits = sb_sheet::find(
            &page.doc.index,
            &page.doc.blocks,
            &FindOpts {
                query: q.clone(),
                k,
            },
        );
        let text = if hits.is_empty() {
            format!(
                "no match for {:?} on {}",
                escape::name(&q, 100),
                escape::name(&page.url, 200)
            )
        } else {
            sb_sheet::find_lines(&page.doc.index, &hits)
        };
        Ok(ToolOutput {
            text,
            summary: Some(format!("find: {} hits", hits.len())),
            consequences: Value::Null,
            files: Vec::new(),
            stopped: None,
        })
    }
}
