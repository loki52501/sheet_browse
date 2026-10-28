//! `tools.save` of the `forge` server (T-28 (b), D-152): a recipe from this run's BrowserOS
//! calls, read back from `runs/<SB_RUN_ID>/browseros.jsonl`.
//!
//! * `steps` is a list of `{line, target?, targets?, returns?}`; `line` is a 1-based line of
//!   the file (the number report.md's `Browser:` section shows, B-211). Each line must exist,
//!   be `ok: true` and call a D-150 recipe tool.
//! * The text of every line is scrubbed with the harness vault first (rule 2): a vault value
//!   becomes `@vault(name)`, which `parse_recipe` allows only at act `text`, `value` and
//!   `fields[].value`.
//! * Saved args drop `page`, `session`, `agentName`, `ref`, `targetRef`, `diff` and
//!   `fields[].ref`. A call that carried a ref needs a target: the model's `target` /
//!   `targets` (it saw the snapshot), else one read from an earlier line's result summary
//!   that shows `<role> "<name>" [ref=<same ref>]`; else the save is refused naming the line.
//! * `args` is `{name: {type, default?, required?, value?}}`: every occurrence of a declared
//!   string or int `value` of 2 or more characters in the saved strings (step args, target
//!   names, `expects`) becomes `{name}`, longest value first, in one pass; a navigate url
//!   also matches the value percent-encoded and a grep pattern the value regex-escaped (the
//!   forms `tools.run` fills back in, D-156). A JSON number equal to an int value becomes the
//!   whole placeholder `"{name}"`.
//! * `match` defaults to `[<first navigate URL path>*]` (the path up to the first character
//!   outside `A-Za-z0-9/_.~-`, so up to the first placeholder) else `["/*"]`; `description`
//!   to the folder plus the step tools.
//! * The rendered YAML must pass `parse_recipe`. An existing `<name>.recipe.yaml` or
//!   `<name>.js`, or a live `tools.health` row of that name, is refused (no overwrite in
//!   T-28). The file is written atomically without replacing anything, registered as an
//!   ephemeral (forged) tool, and committed with `ToolStore::git_commit`, message
//!   `tools: save <domain>/<name> recipe (T-28; run <id>)`.

use crate::forge::{clip, folder_of, percent_encode, regex_escape, store_err};
use harness_core::ledger::BROWSEROS_FILE;
use harness_core::memory::ToolStatus;
use harness_core::vault::Vault;
use harness_core::{ToolCtx, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use toolstore::{
    parse_recipe, render_recipe, ArgType, Expects, Recipe, RecipeArg, RecipeStep, Target,
    ToolStore, RECIPE_SUFFIX, RECIPE_TOOLS,
};

/// Keys dropped from every saved call's args (D-152): per-session ids and the act diff.
pub const DROPPED_KEYS: [&str; 6] = ["page", "session", "agentName", "ref", "targetRef", "diff"];

/// A declared `value` shorter than this is never substituted (it would hit unrelated text).
pub const MIN_VALUE_CHARS: usize = 2;

/// One `steps[]` item of a `tools.save` call.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveStep {
    pub line: usize,
    #[serde(default)]
    pub target: Option<Target>,
    #[serde(default)]
    pub targets: Option<Vec<Target>>,
    #[serde(default)]
    pub returns: Option<String>,
}

/// One `args.<name>` of a `tools.save` call.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveArg {
    #[serde(rename = "type")]
    pub ty: ArgType,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub required: Option<bool>,
    /// The value this run used; its occurrences become `{name}`. Never saved.
    #[serde(default)]
    pub value: Option<Value>,
}

/// The arguments of a `tools.save` call.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveRequest {
    pub domain: String,
    pub name: String,
    pub steps: Vec<SaveStep>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub args: Option<Map<String, Value>>,
    /// The success text (a string, or `{text}`).
    #[serde(default)]
    pub expects: Option<Value>,
    #[serde(default, rename = "match")]
    pub matches: Option<Vec<String>>,
}

/// One `browseros.jsonl` line as the hook writes it (scrubbed again here).
#[derive(Debug, Clone)]
struct Line {
    tool: String,
    ok: bool,
    args: Map<String, Value>,
    summary: String,
}

/// `v` with every string (object keys included) passed through `scrub`.
fn scrub_json(v: &Value, scrub: &dyn Fn(&str) -> String) -> Value {
    match v {
        Value::String(s) => Value::String(scrub(s)),
        Value::Array(a) => Value::Array(a.iter().map(|x| scrub_json(x, scrub)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (scrub(k), scrub_json(x, scrub)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Line `n` (1-based, physical line of the file), parsed and scrubbed.
fn line_at(lines: &[&str], n: usize, scrub: &dyn Fn(&str) -> String) -> Result<Line, String> {
    let Some(raw) = n.checked_sub(1).and_then(|i| lines.get(i)) else {
        return Err(format!(
            "line {n}: browseros.jsonl has {} lines",
            lines.len()
        ));
    };
    if raw.trim().is_empty() {
        return Err(format!("line {n} is blank"));
    }
    let v: Value = serde_json::from_str(raw)
        .map_err(|_| format!("line {n} is not a browseros.jsonl record"))?;
    let v = scrub_json(&v, scrub);
    let (Some(tool), Some(ok)) = (
        v.get("tool").and_then(Value::as_str),
        v.get("ok").and_then(Value::as_bool),
    ) else {
        return Err(format!("line {n} is not a browseros.jsonl record"));
    };
    Ok(Line {
        tool: tool.to_owned(),
        ok,
        args: v
            .get("args")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        summary: v
            .get("result_summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    })
}

/// The `<role> "<name>"` right before `[ref=<r>]` in a snapshot-like summary.
pub fn target_in_summary(summary: &str, r: &str) -> Option<Target> {
    let needle = format!("[ref={r}]");
    for (at, _) in summary.match_indices(&needle) {
        let before = summary[..at].trim_end();
        let Some(body) = before.strip_suffix('"') else {
            continue;
        };
        let Some(open) = body.rfind('"') else {
            continue;
        };
        let name = &body[open + 1..];
        let head = body[..open].trim_end();
        let role: String = head
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if role.is_empty() || name.is_empty() {
            continue;
        }
        return Some(Target {
            role,
            name: name.to_owned(),
            nth: None,
        });
    }
    None
}

/// The target of ref `r` from the summaries of lines before `n`, latest first.
fn target_before(
    lines: &[&str],
    n: usize,
    r: &str,
    scrub: &dyn Fn(&str) -> String,
) -> Option<Target> {
    (1..n)
        .rev()
        .filter_map(|i| line_at(lines, i, scrub).ok())
        .find_map(|l| target_in_summary(&l.summary, r))
}

/// One declared value to turn back into `{name}`.
#[derive(Debug, Clone)]
struct Sub {
    name: String,
    text: String,
    int: Option<i64>,
}

/// Replaces every occurrence of a sub's text with `{name}`, longest first, in one pass.
fn subst_str(s: &str, subs: &[Sub]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if let Some(sub) = subs.iter().find(|x| s[i..].starts_with(&x.text)) {
            out.push('{');
            out.push_str(&sub.name);
            out.push('}');
            i += sub.text.len();
            continue;
        }
        let Some(c) = s[i..].chars().next() else {
            break;
        };
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn subst_json(v: &Value, subs: &[Sub]) -> Value {
    match v {
        Value::String(s) => Value::String(subst_str(s, subs)),
        Value::Number(n) => match subs.iter().find(|x| x.int.is_some() && x.int == n.as_i64()) {
            Some(sub) => Value::String(format!("{{{}}}", sub.name)),
            None => v.clone(),
        },
        Value::Array(a) => Value::Array(a.iter().map(|x| subst_json(x, subs)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), subst_json(x, subs)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// `subs` plus each text in the form `enc` gives it, longest first.
fn with_form(subs: &[Sub], enc: fn(&str) -> String) -> Vec<Sub> {
    let mut out: Vec<Sub> = subs.to_vec();
    for s in subs {
        let t = enc(&s.text);
        if t != s.text {
            out.push(Sub {
                name: s.name.clone(),
                text: t,
                int: None,
            });
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.text.len()));
    out
}

/// `[<path>*]` from a navigate URL: the path up to the first character outside
/// `A-Za-z0-9/_.~-`; `/*` when that is not a plain path.
pub fn default_match(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = match rest.find(['/', '?', '#']) {
        Some(i) if rest[i..].starts_with('/') => &rest[i..],
        _ => "/",
    };
    let prefix: String = path
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || "/_.~-".contains(*c))
        .collect();
    if !prefix.starts_with('/') || prefix.contains("//") || prefix.split('/').any(|s| s == "..") {
        return "/*".to_owned();
    }
    format!("{prefix}*")
}

fn tool_err(n: usize, msg: &str) -> String {
    format!("line {n}: {msg}")
}

/// The ref a call carried, if any (a string `ref`).
fn take_ref(args: &mut Map<String, Value>) -> Option<String> {
    let r = args.get("ref").and_then(Value::as_str).map(str::to_owned);
    for k in DROPPED_KEYS {
        args.remove(k);
    }
    r
}

/// Builds the recipe `tools.save` would write (D-152), without touching disk. `jsonl` is the
/// run's `browseros.jsonl`; `scrub` the vault scrub. `Err` is a message for the model.
pub fn build_recipe(
    folder: &str,
    req: &SaveRequest,
    jsonl: &str,
    scrub: &dyn Fn(&str) -> String,
) -> Result<Recipe, String> {
    if req.steps.is_empty() {
        return Err("steps is empty".into());
    }
    let lines: Vec<&str> = jsonl.lines().collect();

    // Declared args and their substitutions.
    let mut args = Vec::new();
    let mut subs = Vec::new();
    for (name, spec) in req.args.clone().unwrap_or_default() {
        let a: SaveArg =
            serde_json::from_value(spec).map_err(|e| format!("args.{}: {e}", clip(&name, 40)))?;
        let required = a.required.unwrap_or(false);
        if let Some(d) = &a.default {
            if d.as_str()
                .is_some_and(|s| s.to_ascii_lowercase().contains("@vault("))
            {
                return Err(format!(
                    "args.{}: a default may not hold @vault(...) (B-599 1)",
                    clip(&name, 40)
                ));
            }
        }
        if let Some(v) = &a.value {
            if !a.ty.accepts(v) {
                return Err(format!(
                    "args.{}: value is not a {}",
                    clip(&name, 40),
                    a.ty.as_str()
                ));
            }
            let (text, int) = match (a.ty, v) {
                (ArgType::String, Value::String(s)) => (scrub(s), None),
                (ArgType::Int, v) => (v.to_string(), v.as_i64()),
                _ => (String::new(), None),
            };
            if text.chars().count() >= MIN_VALUE_CHARS {
                subs.push(Sub {
                    name: name.clone(),
                    text,
                    int,
                });
            }
        }
        args.push(RecipeArg {
            name,
            ty: a.ty,
            default: a.default,
            required,
        });
    }
    subs.sort_by_key(|s| std::cmp::Reverse(s.text.len()));
    let url_subs = with_form(&subs, percent_encode);
    let regex_subs = with_form(&subs, regex_escape);
    let fill = |t: &Target| Target {
        role: t.role.clone(),
        name: subst_str(&scrub(&t.name), &subs),
        nth: t.nth.clone(),
    };

    let mut steps = Vec::with_capacity(req.steps.len());
    let mut first_url: Option<String> = None;
    for s in &req.steps {
        let n = s.line;
        let line = line_at(&lines, n, scrub)?;
        if !line.ok {
            return Err(tool_err(
                n,
                "the call failed (ok: false); save only calls that worked",
            ));
        }
        if !RECIPE_TOOLS.contains(&line.tool.as_str()) {
            return Err(tool_err(
                n,
                &format!(
                    "tool {:?} cannot be a recipe step; allowed: {} (D-150)",
                    clip(&line.tool, 40),
                    RECIPE_TOOLS.join(", ")
                ),
            ));
        }
        let mut a = line.args.clone();
        let r = take_ref(&mut a);
        let mut field_refs: Vec<Option<String>> = Vec::new();
        if let Some(Value::Array(fields)) = a.get_mut("fields") {
            for f in fields.iter_mut() {
                field_refs.push(
                    f.as_object_mut()
                        .and_then(|o| o.remove("ref"))
                        .and_then(|v| v.as_str().map(str::to_owned)),
                );
            }
        }
        let target = match (&s.target, &r) {
            (Some(t), _) => Some(fill(t)),
            (None, Some(r)) => match target_before(&lines, n, r, scrub) {
                Some(t) => Some(fill(&t)),
                None => {
                    return Err(tool_err(
                        n,
                        &format!(
                            "the call acted on ref {} and no earlier result summary names it; \
                             pass target {{role, name}} for line {n} (the element you saw in \
                             the snapshot)",
                            clip(r, 20)
                        ),
                    ))
                }
            },
            (None, None) => None,
        };
        let targets = match &s.targets {
            Some(ts) => Some(ts.iter().map(fill).collect::<Vec<_>>()),
            None if field_refs.iter().any(Option::is_some) => {
                let mut ts = Vec::new();
                for (i, fr) in field_refs.iter().enumerate() {
                    let t = fr
                        .as_deref()
                        .and_then(|r| target_before(&lines, n, r, scrub));
                    match t {
                        Some(t) => ts.push(fill(&t)),
                        None => {
                            return Err(tool_err(
                                n,
                                &format!(
                                    "fields[{}] has no target an earlier result summary \
                                     names; pass targets (one {{role, name}} per field) for \
                                     line {n}",
                                    i + 1
                                ),
                            ))
                        }
                    }
                }
                Some(ts)
            }
            None => None,
        };
        let mut filled = Map::new();
        for (k, v) in &a {
            let v = match (line.tool.as_str(), k.as_str()) {
                ("navigate", "url") => subst_json(v, &url_subs),
                ("grep", "pattern") => subst_json(v, &regex_subs),
                _ => subst_json(v, &subs),
            };
            filled.insert(k.clone(), v);
        }
        if line.tool == "navigate" && first_url.is_none() {
            first_url = filled.get("url").and_then(Value::as_str).map(str::to_owned);
        }
        steps.push(RecipeStep {
            tool: line.tool.clone(),
            target,
            targets,
            args: Value::Object(filled),
            returns: s.returns.clone(),
        });
    }

    let expects = match &req.expects {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) => Some(t.clone()),
        Some(Value::Object(o)) => match o.get("text").and_then(Value::as_str) {
            Some(t) if o.len() == 1 => Some(t.to_owned()),
            _ => return Err("expects must be a string or {text}".into()),
        },
        Some(_) => return Err("expects must be a string or {text}".into()),
    }
    .map(|t| Expects {
        text: subst_str(&scrub(&t), &subs),
    });
    let matches = match &req.matches {
        Some(m) => m.iter().map(|p| scrub(p)).collect(),
        None => vec![first_url
            .as_deref()
            .map_or_else(|| "/*".to_owned(), default_match)],
    };
    let description = match &req.description {
        Some(d) => scrub(d),
        None => {
            let tools: Vec<&str> = steps.iter().map(|s| s.tool.as_str()).collect();
            clip(
                &format!("{folder}: {}", tools.join(", ")),
                toolstore::recipe::MAX_DESCRIPTION_CHARS,
            )
        }
    };
    Ok(Recipe {
        name: req.name.clone(),
        domain: folder.to_owned(),
        description,
        matches,
        args,
        steps,
        expects,
    })
}

/// Writes `text` to `path` through a temp file, never replacing an existing file: the temp
/// file is hard-linked to `path` (fails if it exists), else renamed after a check.
fn write_new(path: &Path, text: &str) -> Result<(), ToolError> {
    let dir = path
        .parent()
        .ok_or_else(|| ToolError::Failed("recipe path has no folder".into()))?;
    std::fs::create_dir_all(dir).map_err(|e| ToolError::Failed(format!("creating folder: {e}")))?;
    let file = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("recipe");
    let tmp = dir.join(format!(".{file}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| ToolError::Failed(format!("writing {file}: {e}")))?;
    let exists = || {
        ToolError::Args(format!(
            "{file} exists; tools.save never overwrites a recipe"
        ))
    };
    let res = match std::fs::hard_link(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(exists()),
        Err(_) if path.exists() => Err(exists()),
        Err(_) => {
            return std::fs::rename(&tmp, path)
                .map_err(|e| ToolError::Failed(format!("writing {file}: {e}")))
        }
    };
    let _ = std::fs::remove_file(&tmp);
    res
}

/// `tools.save` (D-152).
pub(crate) async fn save(
    tools: &ToolStore,
    vault: &Vault,
    ctx: &ToolCtx,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let req: SaveRequest = serde_json::from_value(args.clone())
        .map_err(|e| ToolError::Args(format!("tools.save args: {e}")))?;
    let folder = folder_of(tools, &req.domain)?;
    let path = tools
        .try_recipe_path(&folder, &req.name)
        .map_err(store_err)?;
    let file = format!("{}{RECIPE_SUFFIX}", req.name);
    if path.exists() {
        return Err(ToolError::Args(format!(
            "tools/{folder}/{file} exists; tools.save does not overwrite a recipe, pick another \
             name"
        )));
    }
    if tools
        .try_tool_path(&folder, &req.name)
        .map_err(store_err)?
        .exists()
    {
        return Err(ToolError::Args(format!(
            "tools/{folder}/{}.js exists; a recipe may not share a tool's name",
            req.name
        )));
    }
    let store = &**ctx.store();
    if let Some(h) = tools
        .health(store, &folder, &req.name)
        .await
        .map_err(store_err)?
    {
        if h.status != ToolStatus::Retired {
            return Err(ToolError::Args(format!(
                "a tool {folder}/{} is registered in tools.health (v{}); pick another name",
                req.name, h.version
            )));
        }
    }
    let jsonl_path = ctx.run_dir.join(BROWSEROS_FILE);
    let jsonl = match std::fs::read_to_string(&jsonl_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ToolError::Args(format!(
                "run {} has no {BROWSEROS_FILE}: no BrowserOS call of this run was recorded",
                ctx.run_id.0
            )))
        }
        Err(e) => return Err(ToolError::Failed(format!("reading {BROWSEROS_FILE}: {e}"))),
    };
    let scrub = |s: &str| vault.scrub(s);
    let recipe = build_recipe(&folder, &req, &jsonl, &scrub).map_err(ToolError::Args)?;
    let text = format!(
        "# Saved by tools.save from run {} (T-28, D-152).\n{}",
        ctx.run_id.0,
        render_recipe(&recipe)
    );
    // D-152: the file must load as written.
    let parsed = parse_recipe(&text, &folder, &recipe.name).map_err(|e| match e {
        toolstore::StoreError::Recipe { msg, .. } => {
            ToolError::Args(format!("the recipe would not load: {msg}"))
        }
        other => store_err(other),
    })?;
    write_new(&path, &text)?;
    let row = match tools.register_forged(store, &folder, &recipe.name).await {
        Ok(h) => h,
        Err(e) => {
            // The file is not registered: take it back so file and row never disagree.
            let _ = std::fs::remove_file(&path);
            return Err(store_err(e));
        }
    };
    let rel = PathBuf::from(&folder).join(&file);
    let msg = format!(
        "tools: save {folder}/{} recipe (T-28; run {})",
        recipe.name, ctx.run_id.0
    );
    let (commit, commit_note) = match tools.git_commit(std::slice::from_ref(&rel), &msg) {
        Ok(c) => (Some(c), String::new()),
        Err(e) => (
            None,
            format!(
                "; not committed ({}), commit it by hand",
                clip(&e.to_string(), 160)
            ),
        ),
    };
    let shown = format!("tools/{folder}/{file}");
    Ok(ToolOutput {
        summary: Some(format!("saved {shown} ({} steps)", parsed.steps.len())),
        consequences: json!({
            "written": shown,
            "version": row.version,
            "status": "ephemeral",
            "commit": commit,
        }),
        text: format!(
            "saved {shown}: {} steps, args ({}), ephemeral v{}{}{}; run it with tools.run \
             {{domain: {folder:?}, name: {:?}}}",
            parsed.steps.len(),
            parsed
                .args
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            row.version,
            commit
                .as_deref()
                .map(|c| format!(", commit {}", &c[..c.len().min(12)]))
                .unwrap_or_default(),
            commit_note,
            parsed.name
        ),
        ..ToolOutput::default()
    })
}
