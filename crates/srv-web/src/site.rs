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
//! This amends T-4's "tool disabled for this run" (B-561, kept by D-581): a tool a miss
//! disabled stays off on every page it misses, but a later fetch in the same run of a page
//! it holds on offers it again, and a clean run there repairs it.
//!
//! Running one (T-4): the page is the one classify resolved and gated ([`crate::pin`]).
//! The fingerprint is checked against it first. `match` runs, `partial` runs with a note
//! (D-505), `miss` refuses, records `FingerprintMiss` (status needs_repair) and disables the
//! tool (D-506). A broken tool's run is a repair (D-561): on `partial` it is relocated, and a
//! clean run records `Relocated` (`@fingerprint` patched, version + 1, status repaired) or,
//! on `match`, `Ok` (needs_repair -> repaired, version + 1, D-304), then the tool is enabled
//! again. One call repairs a tool at a time (D-577): the repair holds a per-tool lock until
//! its outcome is recorded, and a call that waited sees the repaired tool; toolstore records
//! a relocate to the fingerprint the file already has as a clean run, which covers two
//! servers (runs) repairing at once. srv-web never commits the patched file (D-560). The
//! reader runs in
//! `spawn_blocking` over a `SheetPage`; `page.api(url)` is a GET limited to the page's
//! registrable domain (D-501) bridged with `Handle::block_on`, read up to the JSON cap
//! (`Config::json_max_bytes`, 32 MB, D-565) and bounded by the run's remaining time (D-64);
//! a body cut at the cap is reported as such (B-560). A challenge page.api meets (a
//! challenge answer, or a redirect to a challenge URL on any host, which is not followed)
//! stops the call like fetch does, whatever the reader does with the error; later page.api
//! calls in that call refuse without a request (D-5, D-580, D-582). A site tool never runs
//! on a challenge page, which is never cached, and a challenge on a site drops that site's
//! cached pages for the run (D-582), so it does not run on an older page of it either. JSON strings in the output cannot spell a `[text](#id)` link form. The
//! gate's `what` and the ledger summary name the call's arguments (D-579), so a list_jobs
//! call for another org names that board.

use crate::cache::Page;
use crate::fetch;
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
use std::sync::{Arc, Mutex};
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

/// Characters of the argument list in a site tool call's `what` and summary (D-579).
const ARGS_LABEL_MAX_CHARS: usize = 160;

impl SiteTool {
    /// `<site>.<name>` plus the call's arguments in `@args` order, `name=<compact JSON>`,
    /// escaped and capped (D-579): `ashbyhq.list_jobs(org="openai")`. No arguments: the name.
    fn label(&self, args: &Value) -> String {
        let parts: Vec<String> = self
            .tool
            .header
            .args
            .iter()
            .filter_map(|a| {
                let v = args.get(&a.name).filter(|v| !v.is_null())?;
                Some(format!("{}={}", a.name, serde_json::to_string(v).ok()?))
            })
            .collect();
        if parts.is_empty() {
            return self.tool.namespaced.clone();
        }
        format!(
            "{}({})",
            self.tool.namespaced,
            escape::name(&parts.join(", "), ARGS_LABEL_MAX_CHARS)
        )
    }

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
        self.status(ctx).await == Some(ToolStatus::NeedsRepair)
    }

    /// The tool's `tools.health` status; `None` when there is no row or it cannot be read.
    async fn status(&self, ctx: &ToolCtx) -> Option<ToolStatus> {
        ToolStore::new(&self.web.cfg.tools_root)
            .health(&**ctx.store(), &self.tool.domain, &self.tool.header.name)
            .await
            .ok()
            .flatten()
            .map(|h| h.status)
    }

    /// The tool file on disk already carries `fp` as its `@fingerprint`: another run
    /// relocated it first, and toolstore records this run's `Relocated` as a clean run
    /// without writing (D-577), so the note must not claim a patch (D-583). The same test
    /// toolstore makes; an unreadable file counts as not carrying it.
    async fn file_carries(&self, fp: &[FpItem]) -> bool {
        let path = self.tool.path.clone();
        let fp = fp.to_vec();
        tokio::task::spawn_blocking(move || {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|src| toolstore::parse_header(&src).ok())
                .is_some_and(|h| h.fingerprint == fp)
        })
        .await
        .unwrap_or(false)
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
                self.label(args),
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
        // D-577: held while this call repairs the tool, until its outcome is recorded.
        let mut _repairing: Option<tokio::sync::OwnedMutexGuard<()>> = None;
        match toolstore::fingerprint(&self.tool, &page.doc.index) {
            FingerprintResult::Match => {}
            FingerprintResult::Partial { missing } => {
                // D-561: a broken tool (needs_repair, or disabled by a miss) is relocated, and
                // the patch is kept only if the run is clean. A healthy tool's partial match
                // runs with a note and is not patched (D-505).
                if disabled || self.needs_repair(ctx).await {
                    let guard = self.web.repair_lock(&name).lock_owned().await;
                    // D-577: a call that waited for another call's repair finds the tool
                    // healthy and runs as one; only a still-broken tool is relocated.
                    if self.web.is_disabled(&name) || self.needs_repair(ctx).await {
                        relocated = toolstore::relocate(&self.tool, &page.doc.index)
                            .map(|fp| (missing.clone(), fp));
                    }
                    _repairing = Some(guard);
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
        let challenge: Arc<Mutex<Option<ToolOutput>>> = Arc::default();
        let api = PageApi {
            handle: Handle::current(),
            clients: clients.clone(),
            overrides: self.web.cfg.resolve.clone(),
            domain: toolstore::registrable(&host_of(&page_url)),
            // B-560, D-565: the JSON cap, not the page cap; the bytes never reach the model.
            cap: self.web.cfg.json_max_bytes,
            challenge: challenge.clone(),
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
        // D-5, D-580: a challenge page.api met stops the call, whatever the reader made of it;
        // it is not the tool's failure, so nothing is recorded.
        let stop = challenge.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(stop) = stop {
            // D-582: no site tool runs on an older cached page of a site that asked for a
            // human check.
            self.web.challenged(&page_url);
            return Ok(stop);
        }
        match res {
            Ok(mut v) => {
                // D-583: read before record, which may write the file itself.
                let already = match &relocated {
                    Some((_, fp)) => self.file_carries(fp).await,
                    None => false,
                };
                // D-518: when this read already saw the fingerprint in the file, the run is
                // recorded as the clean run toolstore would make of it (D-577), so the result
                // does not hang on toolstore reading the file a second time (a read that fails
                // there counts as "not carried" and patches again). D-527: not for a tool
                // retired meanwhile (its file can still be in place when the archive step
                // failed, D-316): it keeps Relocated, which toolstore refuses for a retired
                // tool (T-7), so the run neither counts as clean nor brings the tool back.
                let retired = already && self.status(ctx).await == Some(ToolStatus::Retired);
                let outcome = match &relocated {
                    Some((_, fp)) if !already || retired => RunOutcome::Relocated {
                        fingerprint: fp.clone(),
                    },
                    _ => RunOutcome::Ok,
                };
                match self.record_row(ctx, outcome).await {
                    Ok(row) if row.status == ToolStatus::Retired => {
                        // D-527: retired between the read and the record: never enabled or
                        // registered again (as register() drops a retired tool).
                        ctx.registry().remove(&name);
                        notes.push(format!("note: {name} is retired; it is not offered again"));
                    }
                    Ok(row) => {
                        // D-561: a clean repair run (relocated, or a match recorded Ok)
                        // enables a tool a miss disabled.
                        self.web.enable(&name);
                        if let Some((missing, _)) = &relocated {
                            // D-560: srv-web never commits the patched file.
                            notes.push(if already {
                                format!(
                                    "note: fingerprint already relocated by another run (was \
                                     missing {}); tools/{}/{}.js is already at version {}, \
                                     status {}; this call patched nothing",
                                    list(missing),
                                    self.tool.domain,
                                    self.tool.header.name,
                                    row.version,
                                    status_name(row.status)
                                )
                            } else {
                                format!(
                                    "note: fingerprint relocated (was missing {}); \
                                     tools/{}/{}.js patched to version {}, status {}; not \
                                     committed (D-560)",
                                    list(missing),
                                    self.tool.domain,
                                    self.tool.header.name,
                                    row.version,
                                    status_name(row.status)
                                )
                            });
                            self.reregister(ctx).await;
                        }
                    }
                    Err(n) => {
                        if retired {
                            // D-527: toolstore refused to revive it; neither does this server.
                            ctx.registry().remove(&name);
                        }
                        notes.push(n);
                    }
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
                    summary: Some(format!(
                        "{} ran on {}",
                        self.label(&args),
                        escape::name(&page.url, 200)
                    )),
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
/// remaining wall time (D-64). No caller headers, so no vault value can ride along. A
/// challenge answer or a redirect to a challenge URL (D-5) is kept in `challenge` for the
/// call to stop with (D-580), and the reader gets an error that names only the host; once
/// it is set, every later page.api call in the run refuses without a request (D-582).
struct PageApi {
    handle: Handle,
    /// The api client is picked per URL: never a proxy for a loopback or private target
    /// (B-590, D-509).
    clients: http::Clients,
    overrides: Vec<(String, std::net::SocketAddr)>,
    domain: String,
    cap: usize,
    challenge: Arc<Mutex<Option<ToolOutput>>>,
}

/// The stop for a challenge in a body page.api does not return (non-2xx, or not JSON):
/// fetch's walk over the decoded text (D-580). A walk that fails finds none; the call still
/// fails with its own error.
async fn challenge_in_body(body: &http::Body) -> Option<ToolOutput> {
    let text = body.text(true);
    fetch::challenge_in_text(body, &text, "page.api")
        .await
        .ok()
        .flatten()
}

impl ApiFetch for PageApi {
    fn api(&self, url: &str, remaining: Duration) -> Result<Value, String> {
        // D-582: after a human check nothing more goes to the network in this call, whatever
        // the reader does with the error.
        if self
            .challenge
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return Err(format!(
                "page.api: this call already met a human check on {}; no further requests \
                 (D-5)",
                self.domain
            ));
        }
        let u = http::parse_url(url).map_err(|e| format!("page.api: {e}"))?;
        let host = host_of(&u);
        if toolstore::registrable(&host) != self.domain {
            return Err(format!(
                "page.api: {host} is outside {}; a site tool may only call its own site (D-501)",
                self.domain
            ));
        }
        let clients = self.clients.clone();
        let overrides = self.overrides.clone();
        let cap = self.cap;
        let slot = self.challenge.clone();
        let fut = async move {
            let client = clients.api_for(&u, false, &overrides).await;
            // D-5, D-580: a human check stops the whole call; the reader only learns the host.
            let stopped = |out: ToolOutput| {
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(out);
                format!("page.api: {host} answered with a human check; the call stops (D-5)")
            };
            let resp = match client
                .get(u.clone())
                .header(ACCEPT, "application/json")
                .send()
                .await
            {
                Ok(r) => r,
                // A redirect to a challenge URL, on this host or another, is not followed.
                Err(e) => {
                    return Err(match http::challenge_redirect(&e) {
                        Some(why) => stopped(fetch::challenge_output(&u, &why)),
                        None => format!("page.api: {}", http::map_err(&e, &host)),
                    })
                }
            };
            let body = http::read_capped(resp, cap)
                .await
                .map_err(|e| format!("page.api: {e}"))?;
            if let Some(out) = fetch::challenge_by_response(&body) {
                return Err(stopped(out));
            }
            if !(200..300).contains(&body.status) {
                if let Some(out) = challenge_in_body(&body).await {
                    return Err(stopped(out));
                }
                return Err(format!("page.api: HTTP {} from {host}", body.status));
            }
            if body.truncated {
                // Cut JSON never parses; say why instead of "not JSON" (B-560).
                return Err(format!(
                    "page.api: {host} answered more than the {} cap",
                    http::cap_text(cap)
                ));
            }
            match serde_json::from_slice::<Value>(&body.bytes) {
                Ok(v) => Ok(v),
                Err(_) => match challenge_in_body(&body).await {
                    Some(out) => Err(stopped(out)),
                    None => Err(format!("page.api: {host} did not answer with JSON")),
                },
            }
        };
        self.handle.block_on(async move {
            tokio::time::timeout(remaining, fut)
                .await
                .map_err(|_| "page.api: no answer within the tool's time limit".to_string())?
        })
    }
}
