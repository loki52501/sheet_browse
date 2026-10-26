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
//! The body is sniffed: HTML under any content type (or none) goes through the HTML walk,
//! never to the model as text (rule 5). Redirects are followed by [`http::get_following`],
//! which refuses a hop from a public URL into a private or loopback address.

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
            the last fetched page."
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
    pins: Pins<Url>,
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
    fn target(&self, args: &Value) -> Result<Url, ToolError> {
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
            return http::parse_url(href).map_err(|e| ToolError::Args(format!("url {raw}: {e}")));
        }
        http::parse_url(raw).map_err(|e| ToolError::Args(format!("url: {e}")))
    }

    async fn load(&self, ctx: &ToolCtx, url: &Url) -> Result<Loaded, ToolError> {
        let clients = self.web.clients()?;
        if let Some(why) = html::challenge_url(url) {
            return Ok(Loaded::Challenge(challenge_output(url, &why)));
        }
        let resp = http::get_following(&self.web.cfg, &clients.page, url, ACCEPT_PAGE)
            .await
            .map_err(|e| ToolError::Failed(format!("fetch: {e}")))?;
        let body = http::read_capped(resp, self.web.cfg.max_bytes)
            .await
            .map_err(|e| ToolError::Failed(format!("fetch: {e}")))?;
        let final_url = body.url.clone();
        let final_host = host_of(&final_url);
        let cf_challenge = cf_mitigated(&body);
        let mime = body.mime.clone();
        let doc = if is_html(&mime, &body.bytes) {
            parse_html(&body, "fetch").await?
        } else if cf_challenge {
            Doc::default()
        } else if mime == "text/plain" {
            html::plain_text(&http::decode(&body.bytes, body.charset.as_deref(), false))
        } else if mime.contains("json") {
            return Err(ToolError::Failed(format!(
                "fetch: {final_host} answered with JSON ({mime}); use api(url) for JSON"
            )));
        } else if mime.contains("xml") || mime.contains("rss") || mime.contains("calendar") {
            return Err(ToolError::Failed(format!(
                "fetch: {final_host} answered with {mime}; use feed(url) for feeds and calendars"
            )));
        } else {
            return Err(ToolError::Failed(format!(
                "fetch: {final_host} answered with {}, which fetch does not read (HTML and plain text only)",
                if mime.is_empty() { "no content type" } else { mime.as_str() }
            )));
        };
        let challenge = if cf_challenge {
            Some("Cloudflare challenge (cf-mitigated)".to_string())
        } else {
            doc.challenge.clone()
        };
        if let Some(why) = challenge {
            return Ok(Loaded::Challenge(challenge_output(&final_url, &why)));
        }
        let (folder, site_tools) = site::register(&self.web, ctx, &final_url).await;
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

/// `text/html`, XHTML, or a body that starts like HTML whatever its declared type (rule 5:
/// a server that labels HTML `text/plain`, or sends no type, still gets the HTML walk).
pub(crate) fn is_html(mime: &str, bytes: &[u8]) -> bool {
    mime == "text/html" || mime == "application/xhtml+xml" || http::looks_like_html(bytes)
}

/// The response carries `cf-mitigated: challenge` (Cloudflare's challenge marker).
pub(crate) fn cf_mitigated(body: &http::Body) -> bool {
    body.headers
        .get("cf-mitigated")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("challenge"))
}

/// The W-2 HTML walk of `body`, off the async threads.
pub(crate) async fn parse_html(body: &http::Body, tool: &str) -> Result<Doc, ToolError> {
    let text = http::decode(&body.bytes, body.charset.as_deref(), true);
    let base = body.url.clone();
    tokio::task::spawn_blocking(move || html::parse(&text, &base))
        .await
        .map_err(|_| ToolError::Failed(format!("{tool}: the HTML walk failed")))
}

/// D-5 for api and feed, with fetch's detector: the `cf-mitigated: challenge` header, a
/// challenge URL (after redirects), or an HTML body the walk finds a challenge in. The
/// output stops the call with class Challenge; `None` when the response is no challenge.
pub(crate) async fn challenge_stop(
    body: &http::Body,
    tool: &str,
) -> Result<Option<ToolOutput>, ToolError> {
    let why = if cf_mitigated(body) {
        Some("Cloudflare challenge (cf-mitigated)".to_string())
    } else if let Some(why) = html::challenge_url(&body.url) {
        Some(why)
    } else if is_html(&body.mime, &body.bytes) {
        parse_html(body, tool).await?.challenge
    } else {
        None
    };
    Ok(why.map(|w| challenge_output(&body.url, &w)))
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
        let url = self.target(args)?;
        if is_ref(args) {
            // The last page can change before call runs; call fetches exactly this URL.
            self.pins.put(ctx, args, url.clone());
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
        let url = if is_ref(&args) {
            self.pins.take(ctx, &args)?
        } else {
            self.target(&args)?
        };
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
            None => match self.load(ctx, &url).await? {
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
