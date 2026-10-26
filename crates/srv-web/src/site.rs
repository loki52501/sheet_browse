//! W-7 site tools on fetched pages (docs/03 T-2..T-4, D-53).
//!
//! On every successful fetch the tool store is scanned for the page's host and path (T-3):
//! `@kind reader` tools with `@phase 1` whose domain and name pass
//! `ToolStore::try_tool_path` (never `tool_path`, D-72) are registered as `<site>.<name>`
//! (`_generic` tools as `generic.<name>`, at lower precedence: the loader drops a generic
//! tool a site tool shadows) at class `max(@gate, Read)`; the registry never lowers a class
//! (D-127). Planners and `@phase 2` tools need the browser and are not registered here.
//! The T-3 skeleton line goes into `PageMeta.site_tools`.
//!
//! Registration honours tool health (D-506): `retired` and `needs_repair` tools are not
//! registered.
//!
//! Running one (T-4): the page is the one classify resolved and gated ([`crate::pin`]).
//! The fingerprint is checked against it first. `match` runs, `partial` runs with a note
//! (D-505: relocation patches files and belongs to the browser path, which can commit),
//! `miss` refuses, records `FingerprintMiss` (status needs_repair) and disables the tool
//! for the rest of the process (D-506). The reader runs in `spawn_blocking` over a
//! `SheetPage`; `page.api(url)` is a GET limited to the page's registrable domain (D-501)
//! bridged with `Handle::block_on` and bounded by the run's remaining time (D-64). JSON
//! strings in the output cannot spell a `[text](#id)` link form.

use crate::cache::Page;
use crate::http::{self, host_of};
use crate::pin::Pins;
use crate::{escape, Web};
use async_trait::async_trait;
use harness_core::memory::{ts_now, ToolStatus};
use harness_core::{Class, Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use reqwest::header::ACCEPT;
use reqwest::Url;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;
use toolstore::{
    skeleton_line, ApiFetch, FingerprintResult, RunError, RunOutcome, SheetPage, Tool, ToolKind,
    ToolRunResult, ToolStore,
};

/// Reader output shown to the model, in characters (the runner caps bytes at 64 KiB).
pub const SITE_OUTPUT_MAX_CHARS: usize = 20_000;

/// Registers the page's `@phase 1` readers. Returns the page's tool folder and the T-3
/// skeleton line.
///
/// Tool health is honoured (T-4, D-506): a tool whose `tools.health` row is `retired` or
/// `needs_repair`, or that a fingerprint miss disabled earlier in this process, is not
/// registered (and is dropped from the registry if an earlier fetch added it). When the
/// health rows cannot be read no site tool is registered, and the page's tools an earlier
/// fetch registered are removed (fail closed).
pub(crate) async fn register(web: &Arc<Web>, ctx: &ToolCtx, url: &Url) -> (String, Option<String>) {
    let host = host_of(url);
    let root = web.cfg.tools_root.clone();
    let page_url = url.to_string();
    // The store scan reads files: off the async threads.
    let scanned = tokio::task::spawn_blocking(move || {
        let store = ToolStore::new(&root);
        let folder = store.folder_for(&host);
        if folder.is_empty() {
            return (folder, Vec::new());
        }
        let tools: Vec<Tool> = store
            .scan(&host, &page_url)
            .tools
            .into_iter()
            .filter(|t| t.header.kind == ToolKind::Reader && t.header.phase == 1)
            .filter(|t| store.try_tool_path(&t.domain, &t.header.name).is_ok())
            .collect();
        (folder, tools)
    })
    .await;
    let Ok((folder, tools)) = scanned else {
        return (String::new(), None);
    };
    if tools.is_empty() {
        return (folder, None);
    }
    let mut domains: Vec<&str> = tools.iter().map(|t| t.domain.as_str()).collect();
    domains.sort_unstable();
    domains.dedup();
    let registry = ctx.registry();
    let mut unhealthy: Vec<(String, String)> = Vec::new();
    for d in domains {
        match ctx.store().tool_health(d).await {
            Ok(rows) => unhealthy.extend(
                rows.into_iter()
                    .filter(|h| matches!(h.status, ToolStatus::Retired | ToolStatus::NeedsRepair))
                    .map(|h| (h.domain, h.name)),
            ),
            Err(_) => {
                // Fail closed (D-506): health unknown, so none of these tools may run, not
                // even ones an earlier fetch registered.
                for t in &tools {
                    registry.remove(&t.namespaced);
                }
                return (folder, None);
            }
        }
    }
    let (keep, drop): (Vec<Tool>, Vec<Tool>) = tools.into_iter().partition(|t| {
        !web.is_disabled(&t.namespaced)
            && !unhealthy
                .iter()
                .any(|(d, n)| *d == t.domain && *n == t.header.name)
    });
    for t in &drop {
        registry.remove(&t.namespaced);
    }
    for t in &keep {
        registry.add(spec(web.clone(), t.clone()));
    }
    (folder, skeleton_line(&keep))
}

fn spec(web: Arc<Web>, tool: Tool) -> ToolSpec {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for a in &tool.header.args {
        let ty = match a.ty.to_ascii_lowercase().as_str() {
            "number" | "float" => "number",
            "integer" | "int" => "integer",
            "boolean" | "bool" => "boolean",
            _ => "string",
        };
        props.insert(a.name.clone(), json!({ "type": ty }));
        if !a.optional {
            required.push(a.name.clone());
        }
    }
    let site = if tool.domain == toolstore::GENERIC {
        "the last fetched page".to_string()
    } else {
        format!("the last page fetched from {}", tool.domain)
    };
    ToolSpec {
        name: tool.namespaced.clone(),
        description: format!(
            "Site tool (reader) from tools/{}/{}.js; reads {site} and returns JSON.",
            tool.domain, tool.header.name
        ),
        class: tool.header.gate.max(Class::Read),
        args: json!({ "type": "object", "properties": props, "required": required }),
        handler: Arc::new(SiteTool {
            web,
            tool,
            pins: Pins::default(),
        }),
    }
}

struct SiteTool {
    web: Arc<Web>,
    tool: Tool,
    /// The page classify resolved (and gated the domain of), taken by call (rule 4).
    pins: Pins<Option<Arc<Page>>>,
}

impl SiteTool {
    async fn record(&self, ctx: &ToolCtx, outcome: RunOutcome) -> Option<String> {
        let store = ToolStore::new(&self.web.cfg.tools_root);
        let result = ToolRunResult {
            domain: self.tool.domain.clone(),
            name: self.tool.header.name.clone(),
            outcome,
            at: ts_now(),
        };
        match store.record(&**ctx.store(), result).await {
            Ok(_) => None,
            Err(e) => Some(format!(
                "note: tool health not recorded ({})",
                escape::name(&e.to_string(), 200)
            )),
        }
    }
}

#[async_trait]
impl ToolHandler for SiteTool {
    async fn classify(&self, ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let page = self.web.cache.for_folder(&self.tool.domain);
        // Call runs on exactly this page, whatever is fetched in between.
        self.pins.put(ctx, args, page.clone());
        let domain = match &page {
            Some(p) => Url::parse(&p.url)
                .ok()
                .map(|u| toolstore::registrable(&host_of(&u))),
            None if self.tool.domain != toolstore::GENERIC => Some(self.tool.domain.clone()),
            None => None,
        };
        Ok(Classified {
            class: Some(self.tool.header.gate.max(Class::Read)),
            domain,
            what: Some(format!(
                "run site tool {} on {}",
                self.tool.namespaced,
                page.as_ref()
                    .map(|p| escape::name(&p.url, 200))
                    .unwrap_or_else(|| "no page".into())
            )),
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let name = self.tool.namespaced.clone();
        let pinned = self.pins.take(ctx, &args)?;
        if self.web.is_disabled(&name) {
            return Err(ToolError::Failed(format!(
                "{name} is disabled for this run after a fingerprint miss (T-4); use find and \
                 fetch instead"
            )));
        }
        let page = pinned.ok_or_else(|| {
            ToolError::Failed(format!(
                "{name}: no page from {} fetched in this run; fetch one first",
                self.tool.domain
            ))
        })?;
        let mut notes: Vec<String> = Vec::new();
        match toolstore::fingerprint(&self.tool, &page.doc.index) {
            FingerprintResult::Match => {}
            // D-505: a partial match runs with a note. Relocation patches the tool file and
            // belongs to the browser path (T-4), which can verify the patched selectors.
            FingerprintResult::Partial { missing } => notes.push(format!(
                "note: fingerprint partial, missing {}",
                missing
                    .iter()
                    .map(|m| m.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            FingerprintResult::Miss => {
                // D-506: a miss disables the tool for the rest of this process.
                self.web.disable(&name);
                ctx.registry().remove(&name);
                let why = format!("fingerprint miss on {}", escape::name(&page.url, 200));
                let note = self.record(ctx, RunOutcome::FingerprintMiss { why }).await;
                return Err(ToolError::Failed(format!(
                    "{name} refused: this page does not match the tool's @fingerprint, so it \
                     was marked needs_repair and disabled for this run; use find and fetch \
                     instead{}",
                    note.map(|n| format!(" ({n})")).unwrap_or_default()
                )));
            }
        }
        let clients = self.web.clients()?;
        let page_url = Url::parse(&page.url)
            .map_err(|_| ToolError::Failed(format!("{name}: the cached page has no valid URL")))?;
        let api = PageApi {
            handle: Handle::current(),
            client: clients.api.clone(),
            domain: toolstore::registrable(&host_of(&page_url)),
            cap: self.web.cfg.max_bytes,
        };
        let tool = self.tool.clone();
        let run_page = page.clone();
        let run_args = args.clone();
        let res = tokio::task::spawn_blocking(move || {
            let host = SheetPage::new(
                &run_page.doc.index,
                &run_page.doc.blocks,
                &run_page.url,
                &run_page.doc.title,
            );
            toolstore::run_reader(&tool, &host, &run_args, Some(&api as &dyn ApiFetch))
        })
        .await
        .map_err(|_| ToolError::Failed(format!("{name}: the tool runner crashed")))?;
        match res {
            Ok(mut v) => {
                if let Some(n) = self.record(ctx, RunOutcome::Ok).await {
                    notes.push(n);
                }
                escape::json_strings(&mut v);
                let body = serde_json::to_string_pretty(&v).unwrap_or_else(|_| "null".into());
                let mut text = escape::cap_chars(&body, SITE_OUTPUT_MAX_CHARS);
                if body.chars().count() > SITE_OUTPUT_MAX_CHARS {
                    text.push_str(&format!(
                        "\ntruncated: output cut at {SITE_OUTPUT_MAX_CHARS} characters"
                    ));
                }
                for n in notes {
                    text.push('\n');
                    text.push_str(&n);
                }
                Ok(ToolOutput {
                    text,
                    summary: Some(format!("{name} ran on {}", escape::name(&page.url, 200))),
                    consequences: json!({ "url": page.url }),
                    files: Vec::new(),
                    stopped: None,
                })
            }
            Err(RunError::Args(msg)) => Err(ToolError::Args(escape::name(&msg, 300))),
            Err(e) => {
                let why = escape::name(&e.to_string(), 300);
                let note = self
                    .record(ctx, RunOutcome::Failed { why: why.clone() })
                    .await;
                Err(ToolError::Failed(format!(
                    "{name}: {why}{}",
                    note.map(|n| format!(" ({n})")).unwrap_or_default()
                )))
            }
        }
    }
}

/// `page.api(url)` for `@phase 1` readers (D-53, D-501): GET only, JSON only, hosts under
/// the page's registrable domain only, same-host redirects only, answered within the run's
/// remaining wall time (D-64). No caller headers, so no vault value can ride along.
struct PageApi {
    handle: Handle,
    client: reqwest::Client,
    domain: String,
    cap: usize,
}

impl ApiFetch for PageApi {
    fn api(&self, url: &str, remaining: Duration) -> Result<Value, String> {
        let u = http::parse_url(url).map_err(|e| format!("page.api: {e}"))?;
        let host = host_of(&u);
        if toolstore::registrable(&host) != self.domain {
            return Err(format!(
                "page.api: {host} is outside {}; a site tool may only call its own site (D-501)",
                self.domain
            ));
        }
        let client = self.client.clone();
        let cap = self.cap;
        let fut = async move {
            let resp = client
                .get(u)
                .header(ACCEPT, "application/json")
                .send()
                .await
                .map_err(|e| format!("page.api: {}", http::map_err(&e, &host)))?;
            let body = http::read_capped(resp, cap)
                .await
                .map_err(|e| format!("page.api: {e}"))?;
            if !(200..300).contains(&body.status) {
                return Err(format!("page.api: HTTP {} from {host}", body.status));
            }
            serde_json::from_slice::<Value>(&body.bytes)
                .map_err(|_| format!("page.api: {host} did not answer with JSON"))
        };
        self.handle.block_on(async move {
            tokio::time::timeout(remaining, fut)
                .await
                .map_err(|_| "page.api: no answer within the tool's time limit".to_string())?
        })
    }
}
