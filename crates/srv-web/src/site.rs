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
//! Registration honours tool health (D-506): `retired` tools are not registered. A tool a
//! fingerprint miss broke (row `needs_repair`, or disabled earlier in this process) is
//! registered only when its fingerprint matches or relocates on the fetched page (D-561).
//!
//! Running one (T-4): the page is the one classify resolved and gated ([`crate::pin`]).
//! The fingerprint is checked against it first. `match` runs, `partial` runs with a note
//! (D-505), `miss` refuses, records `FingerprintMiss` (status needs_repair) and disables the
//! tool (D-506). A broken tool's run is a repair (D-561): on `partial` it is relocated, and a
//! clean run records `Relocated` (`@fingerprint` patched, version + 1, status repaired) or,
//! on `match`, `Ok` (needs_repair -> repaired, version + 1, D-304), then the tool is enabled
//! again. srv-web never commits the patched file (D-560). The reader runs in
//! `spawn_blocking` over a `SheetPage`; `page.api(url)` is a GET limited to the page's
//! registrable domain (D-501) bridged with `Handle::block_on`, read up to the JSON cap
//! (`Config::json_max_bytes`, 32 MB, D-565) and bounded by the run's remaining time (D-64);
//! a body cut at the cap is reported as such (B-560). JSON strings in
//! the output cannot spell a `[text](#id)` link form.

use crate::cache::Page;
use crate::http::{self, host_of};
use crate::pin::Pins;
use crate::{escape, Web};
use async_trait::async_trait;
use harness_core::memory::{ts_now, ToolHealth, ToolStatus};
use harness_core::{Class, Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use reqwest::header::ACCEPT;
use reqwest::Url;
use sb_sheet::IndexRow;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;
use toolstore::{
    skeleton_line, ApiFetch, FingerprintResult, FpItem, RunError, RunOutcome, SheetPage, Tool,
    ToolKind, ToolRunResult, ToolStore,
};

/// Reader output shown to the model, in characters (the runner caps bytes at 64 KiB).
pub const SITE_OUTPUT_MAX_CHARS: usize = 20_000;

/// Registers the page's `@phase 1` readers. Returns the page's tool folder and the T-3
/// skeleton line.
///
/// Tool health is honoured (T-4, D-506): a tool whose `tools.health` row is `retired` is not
/// registered (and is dropped from the registry if an earlier fetch added it). A tool whose
/// row is `needs_repair`, or that a fingerprint miss disabled earlier in this process, is
/// registered only when its fingerprint is not a miss on `index`, the fetched page, so its
/// next run can repair it (D-561). When the health rows cannot be read no site tool is
/// registered, and the page's tools an earlier fetch registered are removed (fail closed).
pub(crate) async fn register(
    web: &Arc<Web>,
    ctx: &ToolCtx,
    url: &Url,
    index: &[IndexRow],
) -> (String, Option<String>) {
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
    let mut retired: Vec<(String, String)> = Vec::new();
    let mut broken: Vec<(String, String)> = Vec::new();
    for d in domains {
        match ctx.store().tool_health(d).await {
            Ok(rows) => {
                for h in rows {
                    match h.status {
                        ToolStatus::Retired => retired.push((h.domain, h.name)),
                        ToolStatus::NeedsRepair => broken.push((h.domain, h.name)),
                        _ => {}
                    }
                }
            }
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
    let listed = |list: &[(String, String)], t: &Tool| {
        list.iter()
            .any(|(d, n)| *d == t.domain && *n == t.header.name)
    };
    let (keep, drop): (Vec<Tool>, Vec<Tool>) = tools.into_iter().partition(|t| {
        if listed(&retired, t) {
            return false;
        }
        // D-561: a broken tool is offered again only where its fingerprint holds (match, or
        // partial and relocatable); its run then repairs it.
        if web.is_disabled(&t.namespaced) || listed(&broken, t) {
            return toolstore::fingerprint(t, index) != FingerprintResult::Miss;
        }
        true
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
        description: {
            let mut d = format!(
                "Site tool (reader) from tools/{}/{}.js; reads {site} and returns JSON.",
                tool.domain, tool.header.name
            );
            let about = about(&tool.source);
            if !about.is_empty() {
                d.push(' ');
                d.push_str(&about);
            }
            d
        },
        class: tool.header.gate.max(Class::Read),
        args: json!({ "type": "object", "properties": props, "required": required }),
        handler: Arc::new(SiteTool {
            web,
            tool,
            pins: Pins::default(),
        }),
    }
}

/// Characters of a tool file's header prose that go into its description (D-568).
const ABOUT_MAX_CHARS: usize = 400;

/// The first paragraph of plain comment lines in a tool file's header block (the prose
/// after the `// @key` lines, up to an empty `//` line), joined, escaped and capped
/// (D-568): what the arguments mean, so the model can use them in its first call.
fn about(source: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for line in source.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let Some(c) = t.strip_prefix("//") else {
            break;
        };
        let c = c.trim();
        if c.is_empty() && !parts.is_empty() {
            // An empty comment line ends the first paragraph; later notes are for people.
            break;
        }
        if c.is_empty() || c.starts_with('@') {
            continue;
        }
        parts.push(c);
    }
    escape::name(&parts.join(" "), ABOUT_MAX_CHARS)
}

struct SiteTool {
    web: Arc<Web>,
    tool: Tool,
    /// The page classify resolved (and gated the domain of), taken by call (rule 4).
    pins: Pins<Option<Arc<Page>>>,
}

impl SiteTool {
    /// `record` through toolstore; the error is a note for the model.
    async fn record_row(&self, ctx: &ToolCtx, outcome: RunOutcome) -> Result<ToolHealth, String> {
        let store = ToolStore::new(&self.web.cfg.tools_root);
        let result = ToolRunResult {
            domain: self.tool.domain.clone(),
            name: self.tool.header.name.clone(),
            outcome,
            at: ts_now(),
        };
        store.record(&**ctx.store(), result).await.map_err(|e| {
            format!(
                "note: tool health not recorded ({})",
                escape::name(&e.to_string(), 200)
            )
        })
    }

    async fn record(&self, ctx: &ToolCtx, outcome: RunOutcome) -> Option<String> {
        self.record_row(ctx, outcome).await.err()
    }

    /// The tool's row says `needs_repair` (a miss broke it). An unreadable row counts as not
    /// broken, so nothing is patched on a guess.
    async fn needs_repair(&self, ctx: &ToolCtx) -> bool {
        ToolStore::new(&self.web.cfg.tools_root)
            .health(&**ctx.store(), &self.tool.domain, &self.tool.header.name)
            .await
            .ok()
            .flatten()
            .is_some_and(|h| h.status == ToolStatus::NeedsRepair)
    }

    /// After a relocate patched the file: registers the patched tool in place of this one, so
    /// the next call checks the new `@fingerprint` (D-561). Left as is when the file cannot be
    /// read back as the same tool; the next fetch scans it again.
    async fn reregister(&self, ctx: &ToolCtx) {
        let old = self.tool.clone();
        let reread = tokio::task::spawn_blocking(move || {
            let source = std::fs::read_to_string(&old.path).ok()?;
            let header = toolstore::parse_header(&source).ok()?;
            (header.name == old.header.name && header.kind == ToolKind::Reader && header.phase == 1)
                .then_some(Tool {
                    header,
                    source,
                    ..old
                })
        })
        .await;
        if let Ok(Some(tool)) = reread {
            ctx.registry().add(spec(self.web.clone(), tool));
        }
    }
}

/// The `tools.health.status` spelling (`needs_repair`).
fn status_name(s: ToolStatus) -> &'static str {
    match s {
        ToolStatus::Ephemeral => "ephemeral",
        ToolStatus::Saved => "saved",
        ToolStatus::Repaired => "repaired",
        ToolStatus::NeedsRepair => "needs_repair",
        ToolStatus::Retired => "retired",
        ToolStatus::Ask => "ask",
    }
}

/// Fingerprint items for a note. A relocate copies page names into `@fingerprint`, so they
/// are escaped like any page text.
fn list(items: &[FpItem]) -> String {
    let joined = items
        .iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    escape::name(&joined, 300)
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
        let page = pinned.ok_or_else(|| {
            ToolError::Failed(format!(
                "{name}: no page from {} fetched in this run; fetch one first",
                self.tool.domain
            ))
        })?;
        // D-561: a disabled tool is only registered again on a page its fingerprint holds on,
        // and its run there is a repair; on a page it misses it is refused below as before.
        let disabled = self.web.is_disabled(&name);
        let mut notes: Vec<String> = Vec::new();
        // Set when this run repairs a broken tool by relocation: (missing, new fingerprint).
        let mut relocated: Option<(Vec<FpItem>, Vec<FpItem>)> = None;
        match toolstore::fingerprint(&self.tool, &page.doc.index) {
            FingerprintResult::Match => {}
            FingerprintResult::Partial { missing } => {
                // D-561: a broken tool (needs_repair, or disabled by a miss) is relocated, and
                // the patch is kept only if the run is clean. A healthy tool's partial match
                // runs with a note and is not patched (D-505).
                if disabled || self.needs_repair(ctx).await {
                    relocated = toolstore::relocate(&self.tool, &page.doc.index)
                        .map(|fp| (missing.clone(), fp));
                }
                if relocated.is_none() {
                    notes.push(format!(
                        "note: fingerprint partial, missing {}",
                        list(&missing)
                    ));
                }
            }
            FingerprintResult::Miss => {
                // D-506: a miss disables the tool until a fetched page it holds on (D-561).
                self.web.disable(&name);
                ctx.registry().remove(&name);
                let why = format!("fingerprint miss on {}", escape::name(&page.url, 200));
                let note = self.record(ctx, RunOutcome::FingerprintMiss { why }).await;
                return Err(ToolError::Failed(format!(
                    "{name} refused: this page does not match the tool's @fingerprint, so it \
                     was marked needs_repair and disabled for this run until a fetched page \
                     matches it; use find and fetch instead{}",
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
            // B-560, D-565: the JSON cap, not the page cap; the bytes never reach the model.
            cap: self.web.cfg.json_max_bytes,
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
                let outcome = match &relocated {
                    Some((_, fp)) => RunOutcome::Relocated {
                        fingerprint: fp.clone(),
                    },
                    None => RunOutcome::Ok,
                };
                match self.record_row(ctx, outcome).await {
                    Ok(row) => {
                        // D-561: a clean repair run (relocated, or a match recorded Ok)
                        // enables a tool a miss disabled.
                        self.web.enable(&name);
                        if let Some((missing, _)) = &relocated {
                            // D-560: srv-web never commits the patched file.
                            notes.push(format!(
                                "note: fingerprint relocated (was missing {}); \
                                 tools/{}/{}.js patched to version {}, status {}; not \
                                 committed (D-560)",
                                list(missing),
                                self.tool.domain,
                                self.tool.header.name,
                                row.version,
                                status_name(row.status)
                            ));
                            self.reregister(ctx).await;
                        }
                    }
                    Err(n) => notes.push(n),
                }
                escape::json_strings(&mut v);
                let mut body = serde_json::to_string_pretty(&v).unwrap_or_else(|_| "null".into());
                if body.chars().count() > SITE_OUTPUT_MAX_CHARS {
                    // D-569: compact JSON when only that fits (60 long job rows), so the
                    // paging fields after `jobs` are not cut off.
                    let compact = serde_json::to_string(&v).unwrap_or_default();
                    if compact.chars().count() <= SITE_OUTPUT_MAX_CHARS {
                        body = compact;
                    }
                }
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
            if body.truncated {
                // Cut JSON never parses; say why instead of "not JSON" (B-560).
                return Err(format!(
                    "page.api: {host} answered more than the {} cap",
                    http::cap_text(cap)
                ));
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
