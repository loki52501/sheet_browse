//! `harness-forge`: the `forge` tool server over recipes (T-28 (b); D-151, D-152, D-156;
//! WAVE3 recipe format, D-150).
//!
//! Served by `harness_core::serve` with server name [`SERVER`] like every tool server (rule 4,
//! H-6): every call is classified, gated, scrubbed and in the ledger. The process is wired by
//! `core_pg::bootstrap` (`DATABASE_URL`, the vault installed as the process-wide scrubber,
//! `SB_RUNS_ROOT`, `SB_RUN_ID`); recipes live under `SB_TOOLS_ROOT` (default `./tools`).
//!
//! | tool         | class | does                                                            |
//! |--------------|-------|-----------------------------------------------------------------|
//! | `tools.list` | Read  | the live recipes of a host or folder: name, args, status, text  |
//! | `tools.run`  | Read  | checks args, returns the filled steps for the model to execute  |
//! | `tools.save` | Fill  | writes a recipe from this run's `browseros.jsonl` lines         |
//!
//! `tools.run` has no side effect: it returns text, and the model executes the steps through
//! BrowserOS, whose calls the PostToolUse hook records in `runs/<id>/browseros.jsonl` (D-98).
//! Its ledger event (`server = forge`, `tool = tools.run`, `consequences.tools` and
//! `consequences.expects`) is the run record reflect judges (D-153, [`crate::judge`]).
//!
//! Run-time rules (D-156, from B-596 and B-599): unknown args, a missing required arg and a
//! value of the wrong type are refused, defaults are applied; a caller's value holding
//! `@vault(` is refused (a recipe file may name a vault key, a caller may not), and so is an
//! em dash, en dash or ` -- ` in a string arg that fills an act `text`, `value` or
//! `fields[].value` (CLAUDE.md rule 7). A string that is exactly one placeholder takes the
//! arg's JSON type; a value filled into a navigate `url` is percent-encoded as a URL
//! component, into a grep `pattern` regex-escaped. Targets stay `{role, name, nth}` (name a
//! case-sensitive prefix, nth 1-based) for the model to resolve from a fresh snapshot.
//! `expects.text` comes back as a final `wait for=text` step.

use crate::forge_save;
use crate::CliError;
use async_trait::async_trait;
use harness_core::class::Class;
use harness_core::memory::ToolStatus;
use harness_core::vault::Vault;
use harness_core::{Classified, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use toolstore::{
    fill_placeholders, placeholders, registrable, Nth, Recipe, RecipeArg, StoreError, Target,
    ToolStore, GENERIC,
};

/// `ledger.events.server` of this server's tool events, and the MCP server name.
pub const SERVER: &str = "forge";
/// The tool names.
pub const LIST_TOOL: &str = "tools.list";
pub const RUN_TOOL: &str = "tools.run";
pub const SAVE_TOOL: &str = "tools.save";

/// One line of a list or error shows at most this many characters of a recipe's text (a
/// whole description: it may state when not to run the recipe).
const SHOW_MAX: usize = toolstore::recipe::MAX_DESCRIPTION_CHARS;

/// The instruction line `tools.run` puts before the steps (D-151).
pub const RUN_INSTRUCTION: &str = "Execute these steps in order through browseros on your \
     page (pass your page id). For each target, resolve {role, name (a case-sensitive \
     prefix of the accessible name), nth (1-based, default 1)} to a ref from a fresh \
     snapshot or grep over=ax right before that act. Stop at the first failed step and \
     report it.";

/// `SB_TOOLS_ROOT` (default `./tools`), made absolute so a later working-directory change
/// cannot move it.
pub fn tools_root_from_env() -> Result<PathBuf, CliError> {
    let root = std::env::var("SB_TOOLS_ROOT")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from("tools"), PathBuf::from);
    Ok(std::path::absolute(root)?)
}

/// Serves the `forge` tools over stdio until stdin closes (`harness-forge`).
pub async fn serve() -> Result<(), CliError> {
    let harness = core_pg::bootstrap().await?;
    let root = tools_root_from_env()?;
    let tools = tools(&harness, &root);
    harness_core::serve(SERVER, tools, harness).await?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    List,
    Run,
    Save,
}

struct Forge {
    op: Op,
    tools: ToolStore,
    /// Scrubs `browseros.jsonl` text before `tools.save` reads it (rule 2).
    vault: Arc<Vault>,
}

/// Every tool of the `forge` server, over the recipes under `tools_root`.
pub fn tools(harness: &Harness, tools_root: &Path) -> Vec<ToolSpec> {
    let spec = |op: Op, name: &str, class: Class, description: &str, args: Value| ToolSpec {
        name: name.into(),
        description: description.into(),
        class,
        args,
        handler: Arc::new(Forge {
            op,
            tools: ToolStore::new(tools_root),
            vault: harness.vault.clone(),
        }),
    };
    let target = json!({
        "type": "object",
        "properties": {
            "role": {"type": "string"},
            "name": {"type": "string"},
            "nth": {"type": "integer", "minimum": 1},
        },
        "required": ["role"],
        "additionalProperties": false,
    });
    vec![
        spec(
            Op::List,
            LIST_TOOL,
            Class::Read,
            "List the saved recipes of a site (host like mail.google.com, or a tool folder): \
             name, args, status, what it does. Run one with tools.run.",
            schema(json!({"domain": {"type": "string"}}), &["domain"]),
        ),
        spec(
            Op::Run,
            RUN_TOOL,
            Class::Read,
            "Get a recipe's steps with your args filled in, to execute in order through \
             browseros. Refuses a missing or retired recipe and bad args.",
            schema(
                json!({
                    "domain": {"type": "string"},
                    "name": {"type": "string"},
                    "args": {"type": "object"},
                }),
                &["domain", "name"],
            ),
        ),
        spec(
            Op::Save,
            SAVE_TOOL,
            Class::Fill,
            "Save the browseros calls of this run as a recipe: steps are 1-based line numbers \
             of the run's browseros.jsonl, each with the target {role, name, nth?} you saw for \
             a call that used a ref. args: {name: {type, default?, required?, value?}}; each \
             value seen in the calls becomes {name}.",
            schema(
                json!({
                    "domain": {"type": "string"},
                    "name": {"type": "string"},
                    "steps": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "line": {"type": "integer", "minimum": 1},
                                "target": target,
                                "targets": {"type": "array", "items": target},
                                "returns": {"type": "string"},
                            },
                            "required": ["line"],
                            "additionalProperties": false,
                        },
                    },
                    "description": {"type": "string"},
                    "args": {"type": "object"},
                    "expects": {"type": "string"},
                    "match": {"type": "array", "items": {"type": "string"}},
                }),
                &["domain", "name", "steps"],
            ),
        ),
    ]
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

#[async_trait]
impl ToolHandler for Forge {
    async fn classify(&self, _ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let folder = args
            .get("domain")
            .and_then(Value::as_str)
            .map(|d| self.tools.folder_for(d))
            .filter(|f| !f.is_empty() && f != GENERIC);
        let what = match self.op {
            Op::Save => {
                let name = args.get("name").and_then(Value::as_str).unwrap_or("?");
                let n = args
                    .get("steps")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                Some(self.vault.scrub(&format!(
                    "save the recipe {}/{} from {n} browseros calls of this run",
                    folder.as_deref().unwrap_or("?"),
                    clip(name, 64)
                )))
            }
            _ => None,
        };
        Ok(Classified {
            domain: folder.as_deref().map(registrable),
            what,
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        match self.op {
            Op::List => self.list(ctx, &args).await,
            Op::Run => self.run(ctx, &args).await,
            Op::Save => forge_save::save(&self.tools, &self.vault, ctx, &args).await,
        }
    }
}

/// The tool folder of `domain` (a host or a folder name), or `Args`.
pub(crate) fn folder_of(tools: &ToolStore, domain: &str) -> Result<String, ToolError> {
    let f = tools.folder_for(domain);
    if f.is_empty() {
        return Err(ToolError::Args(format!(
            "domain {:?} is not a host or a tool folder",
            clip(domain, 80)
        )));
    }
    Ok(f)
}

pub(crate) fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::Args(format!("{key} (string) is required")))
}

pub(crate) fn store_err(e: StoreError) -> ToolError {
    match e {
        StoreError::BadPath(m) => ToolError::Args(m),
        other => ToolError::Failed(other.to_string()),
    }
}

impl Forge {
    async fn list(&self, ctx: &ToolCtx, args: &Value) -> Result<ToolOutput, ToolError> {
        let folder = folder_of(&self.tools, req_str(args, "domain")?)?;
        let report = self
            .tools
            .recipes_live(&**ctx.store(), &folder)
            .await
            .map_err(store_err)?;
        let rows = ctx
            .store()
            .tool_health(&folder)
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        let n = report.recipes.len();
        let mut out = format!(
            "{folder}: {n} recipe{}{}",
            if n == 1 { "" } else { "s" },
            if n > 0 {
                " (run one with tools.run {domain, name, args})"
            } else {
                ""
            }
        );
        for r in &report.recipes {
            let status = rows
                .iter()
                .find(|h| h.name == r.name)
                .map_or("new", |h| status_str(h.status));
            out.push_str(&format!(
                "\n- {} [{status}]: {}",
                signature(r),
                clip(&r.description, SHOW_MAX)
            ));
        }
        for (path, why) in &report.skipped {
            let file = path.file_name().and_then(|f| f.to_str()).unwrap_or("?");
            out.push_str(&format!("\n- not usable: {file}: {}", clip(why, 160)));
        }
        Ok(ToolOutput {
            summary: Some(format!("{folder}: {n} recipes")),
            ..text(out)
        })
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> Result<ToolOutput, ToolError> {
        let folder = folder_of(&self.tools, req_str(args, "domain")?)?;
        let name = req_str(args, "name")?;
        let given = match args.get("args") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return Err(ToolError::Args("args must be an object".into())),
        };
        let recipe = match self.tools.recipe(&folder, name).map_err(store_err)? {
            Some(r) => r,
            None => {
                return Err(ToolError::Args(format!(
                    "no recipe {folder}/{}; tools.list shows what there is",
                    clip(name, 64)
                )))
            }
        };
        if let Some(h) = self
            .tools
            .health(&**ctx.store(), &folder, &recipe.name)
            .await
            .map_err(store_err)?
        {
            if h.status == ToolStatus::Retired {
                return Err(ToolError::Args(format!(
                    "recipe {folder}/{} is retired (v{}): {}",
                    recipe.name,
                    h.version,
                    clip(h.why.as_deref().unwrap_or("-"), SHOW_MAX)
                )));
            }
        }
        let p = prepare_run(&recipe, &given).map_err(ToolError::Args)?;
        let steps = Value::Array(p.steps.clone()).to_string();
        Ok(ToolOutput {
            summary: Some(format!(
                "recipe {folder}/{}: {} steps",
                recipe.name,
                p.steps.len()
            )),
            consequences: json!({
                "recipe": format!("{folder}/{}", recipe.name),
                "folder": folder,
                "name": recipe.name,
                "tools": p.tools,
                "expects": p.expects,
            }),
            ..text(format!("{RUN_INSTRUCTION}\n{steps}"))
        })
    }
}

fn text(s: String) -> ToolOutput {
    ToolOutput {
        text: s,
        ..ToolOutput::default()
    }
}

fn status_str(s: ToolStatus) -> &'static str {
    match s {
        ToolStatus::Ephemeral => "ephemeral",
        ToolStatus::Saved => "saved",
        ToolStatus::NeedsRepair => "needs_repair",
        ToolStatus::Repaired => "repaired",
        ToolStatus::Retired => "retired",
        ToolStatus::Ask => "ask",
    }
}

/// `name(to: string, count: int = 5, note?: string)`: required args bare, a default after
/// `=`, an optional arg with no default marked `?`.
pub fn signature(r: &Recipe) -> String {
    let args: Vec<String> = r.args.iter().map(arg_sig).collect();
    format!("{}({})", r.name, args.join(", "))
}

fn arg_sig(a: &RecipeArg) -> String {
    match &a.default {
        Some(d) => format!(
            "{}: {} = {}",
            a.name,
            a.ty.as_str(),
            clip(&d.to_string(), 40)
        ),
        None if a.required => format!("{}: {}", a.name, a.ty.as_str()),
        None => format!("{}?: {}", a.name, a.ty.as_str()),
    }
}

/// One line, at most `max` characters (`...` when cut).
pub(crate) fn clip(s: &str, max: usize) -> String {
    let one: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if one.chars().count() > max {
        let mut cut: String = one.chars().take(max.saturating_sub(3)).collect();
        cut.push_str("...");
        cut
    } else {
        one
    }
}

// ---------------------------------------------------------------- tools.run

/// A recipe with its args bound and filled: what `tools.run` returns.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    /// Compact step objects `{tool, target?, targets?, args?, returns?}`, plus a final
    /// `wait for=text` when the recipe has `expects`.
    pub steps: Vec<Value>,
    /// The recipe's own step tools in order (without the expects wait): what reflect looks
    /// for in the run's `browseros.jsonl` (D-153).
    pub tools: Vec<String>,
    /// `expects.text` with the args filled.
    pub expects: Option<String>,
}

/// Where a filled value lands, which decides how it is encoded (D-156).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Enc {
    Plain,
    /// navigate `args.url`: a URL component, percent-encoded.
    Url,
    /// grep `args.pattern`: regex-escaped.
    Regex,
}

/// The args that fill an act `text`, `value` or `fields[].value`: text the user sends as
/// themselves (rule 7).
fn act_text_args(r: &Recipe) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |v: Option<&Value>| {
        if let Some(s) = v.and_then(Value::as_str) {
            for p in placeholders(s) {
                if !out.iter().any(|o| o == p) {
                    out.push(p.to_owned());
                }
            }
        }
    };
    for s in r.steps.iter().filter(|s| s.tool == "act") {
        add(s.args.get("text"));
        add(s.args.get("value"));
        if let Some(fields) = s.args.get("fields").and_then(Value::as_array) {
            for f in fields {
                add(f.get("value"));
            }
        }
    }
    out
}

/// Checks `given` against the recipe's args and applies defaults (D-151, D-156). Messages
/// never repeat a value.
pub fn bind_args(r: &Recipe, given: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    for k in given.keys() {
        if r.arg(k).is_none() {
            return Err(format!(
                "unknown arg {:?}; {} takes {}",
                clip(k, 40),
                r.name,
                signature(r)
            ));
        }
    }
    let rule7 = act_text_args(r);
    let mut out = Map::new();
    for a in &r.args {
        let v = match given.get(&a.name) {
            None | Some(Value::Null) => match (&a.default, a.required) {
                (Some(d), _) => d.clone(),
                (None, true) => {
                    return Err(format!(
                        "missing required arg {}; {} takes {}",
                        a.name,
                        r.name,
                        signature(r)
                    ))
                }
                // B-215: an optional arg with no default has nothing to fill in.
                (None, false) => {
                    return Err(format!(
                        "arg {} has no value and the recipe gives no default; pass it",
                        a.name
                    ))
                }
            },
            Some(v) if a.ty.accepts(v) => v.clone(),
            Some(_) => return Err(format!("arg {} must be of type {}", a.name, a.ty.as_str())),
        };
        if let Value::String(s) = &v {
            if s.to_ascii_lowercase().contains("@vault(") {
                return Err(format!(
                    "arg {}: an arg value may not hold @vault(...); only a recipe file names \
                     a vault key (B-599 1)",
                    a.name
                ));
            }
            if rule7.contains(&a.name)
                && (s.contains('\u{2014}') || s.contains('\u{2013}') || s.contains(" -- "))
            {
                return Err(format!(
                    "arg {} is text sent as the user: no em dash, en dash or ` -- ` (rule 7); \
                     use a comma, colon or full stop",
                    a.name
                ));
            }
        }
        out.insert(a.name.clone(), v);
    }
    Ok(out)
}

/// `s` percent-encoded as a URL component: everything but `A-Za-z0-9-._~` (RFC 3986
/// unreserved) as `%XX` of its UTF-8 bytes.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `s` with every regex metacharacter escaped (the set `regex::escape` escapes).
pub fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn fill_str(s: &str, enc: Enc, vals: &Map<String, Value>) -> Value {
    let ph = placeholders(s);
    if enc == Enc::Plain && ph.len() == 1 && s.len() == ph[0].len() + 2 {
        if let Some(v) = vals.get(ph[0]) {
            return v.clone();
        }
    }
    let encoded: Map<String, Value> = vals
        .iter()
        .map(|(k, v)| {
            let v = match enc {
                Enc::Plain => v.clone(),
                Enc::Url => Value::String(percent_encode(&value_text(v))),
                Enc::Regex => Value::String(regex_escape(&value_text(v))),
            };
            (k.clone(), v)
        })
        .collect();
    Value::String(fill_placeholders(s, &encoded))
}

fn fill_json(v: &Value, enc: Enc, vals: &Map<String, Value>) -> Value {
    match v {
        Value::String(s) => fill_str(s, enc, vals),
        Value::Array(a) => Value::Array(a.iter().map(|x| fill_json(x, Enc::Plain, vals)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), fill_json(x, Enc::Plain, vals)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn fill_target(t: &Target, vals: &Map<String, Value>) -> Result<Value, String> {
    let mut o = Map::new();
    o.insert("role".into(), json!(fill_placeholders(&t.role, vals)));
    if !t.name.is_empty() {
        o.insert("name".into(), json!(fill_placeholders(&t.name, vals)));
    }
    match &t.nth {
        None => {}
        Some(Nth::Index(n)) => {
            o.insert("nth".into(), json!(n));
        }
        Some(Nth::Arg(s)) => {
            let arg = placeholders(s).first().copied().unwrap_or("");
            match vals.get(arg).and_then(Value::as_i64) {
                Some(n) if n >= 1 => {
                    o.insert("nth".into(), json!(n));
                }
                _ => {
                    return Err(format!(
                        "arg {arg} picks a 1-based match: it must be 1 or more"
                    ))
                }
            }
        }
    }
    Ok(Value::Object(o))
}

/// Binds `given` and fills every step (D-151, D-156). `Err` is a message for the model.
pub fn prepare_run(r: &Recipe, given: &Map<String, Value>) -> Result<Prepared, String> {
    let vals = bind_args(r, given)?;
    let mut steps = Vec::with_capacity(r.steps.len() + 1);
    for s in &r.steps {
        let mut o = Map::new();
        o.insert("tool".into(), json!(s.tool));
        if let Some(t) = &s.target {
            o.insert("target".into(), fill_target(t, &vals)?);
        }
        if let Some(ts) = &s.targets {
            let filled = ts
                .iter()
                .map(|t| fill_target(t, &vals))
                .collect::<Result<Vec<_>, _>>()?;
            o.insert("targets".into(), Value::Array(filled));
        }
        if let Some(args) = s.args.as_object().filter(|a| !a.is_empty()) {
            let mut filled = Map::new();
            for (k, v) in args {
                let enc = match (s.tool.as_str(), k.as_str()) {
                    ("navigate", "url") => Enc::Url,
                    ("grep", "pattern") => Enc::Regex,
                    _ => Enc::Plain,
                };
                filled.insert(k.clone(), fill_json(v, enc, &vals));
            }
            o.insert("args".into(), Value::Object(filled));
        }
        if let Some(ret) = &s.returns {
            o.insert("returns".into(), json!(ret));
        }
        steps.push(Value::Object(o));
    }
    let expects = r
        .expects
        .as_ref()
        .map(|e| fill_placeholders(&e.text, &vals));
    if let Some(t) = &expects {
        steps.push(json!({"tool": "wait", "args": {"for": "text", "value": t}}));
    }
    Ok(Prepared {
        steps,
        tools: r.steps.iter().map(|s| s.tool.clone()).collect(),
        expects,
    })
}

#[cfg(test)]
#[path = "forge_tests.rs"]
mod tests;
