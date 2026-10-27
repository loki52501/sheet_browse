//! T-28 (a), D-150: the `recipe` tool kind, `tools/<domain>/<name>.recipe.yaml` (the WAVE3
//! recipe format). A recipe is a list of BrowserOS calls the model executes in order. It
//! never holds a raw ref, a page or session id, a fixed sleep, a password or a vault value.
//!
//! Recipes are not a [`crate::ToolKind`] variant (D-90 freezes the exhaustive `ToolKind`
//! matches in srv-web and sb-mcp). They live beside the `.js` tools of a folder, share the
//! `tools.health` row keyed (domain, name) with them, and the `.js` loader ignores
//! `*.recipe.yaml` files without listing them as skipped.
//!
//! The API harness-forge builds on (D-151..D-153):
//!
//! - [`parse_recipe`]`(text, folder, stem)`: the one validator; every D-150 rule;
//! - [`render_recipe`]: YAML that `parse_recipe` reads back to an equal [`Recipe`];
//! - [`ToolStore::recipes`] (files only), [`ToolStore::recipes_live`] (also skips recipes
//!   whose `tools.health` row is retired, D-315), [`ToolStore::recipe`] (one by name);
//! - [`ToolStore::try_recipe_path`]: where a recipe of that name lives;
//! - [`placeholders`] and [`fill_placeholders`]: the `{arg}` syntax `tools.run` fills.
//!
//! Validation order matters for rule 2: every string of the file (keys included) is checked
//! for `***`, key shapes and vault values before anything else, so no later error message
//! can echo one of them.

use crate::domain::{check_tool_id, valid_domain};
use crate::header::{header_unsafe, is_tool_name, match_violation};
use crate::loader::retired_version;
use crate::{StoreError, ToolStore};
use harness_core::memory::MemoryStore;
use harness_core::vault::{scrub, vault_refs};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use serde_yaml_ng::{Mapping, Value as Yaml};
use std::fs;
use std::path::{Path, PathBuf};

/// File name suffix of a recipe: `tools/<domain>/<name>.recipe.yaml`.
pub const RECIPE_SUFFIX: &str = ".recipe.yaml";
/// The `kind:` every recipe carries.
pub const RECIPE_KIND: &str = "recipe";
/// The BrowserOS tools a step may call (D-150). Everything else (`evaluate`, `run`,
/// `upload`, `tabs`, `windows`, `save_skill`, `*human_help`, ...) is refused.
pub const RECIPE_TOOLS: [&str; 10] = [
    "navigate",
    "snapshot",
    "grep",
    "act",
    "wait",
    "read",
    "diff",
    "screenshot",
    "pdf",
    "download",
];
/// `act` kinds that act on one element and so need a `target` (D-150). `fill` with
/// `fields[]` needs `targets` instead, one per field.
pub const TARGET_ACT_KINDS: [&str; 8] = [
    "click", "fill", "check", "uncheck", "select", "hover", "focus", "drag",
];
/// A recipe has 1..=MAX_STEPS steps.
pub const MAX_STEPS: usize = 50;
/// `description` is one line of 1..=MAX_DESCRIPTION_CHARS chars.
pub const MAX_DESCRIPTION_CHARS: usize = 200;
/// A larger file is refused before it is parsed.
pub const MAX_RECIPE_BYTES: usize = 64 * 1024;

/// Keys no step may carry anywhere in its args (compared ASCII case-insensitively): refs and
/// page, session and agent ids are per session (WAVE3).
const FORBIDDEN_KEYS: [&str; 5] = ["ref", "targetref", "page", "session", "agentname"];
/// Longest arg name, `returns` name and locator role or name.
const MAX_IDENT: usize = 64;
const MAX_LOCATOR_CHARS: usize = 200;
const MAX_EXPECTS_CHARS: usize = 500;

/// The type of a recipe argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArgType {
    String,
    Int,
    Bool,
}

impl ArgType {
    pub fn as_str(self) -> &'static str {
        match self {
            ArgType::String => "string",
            ArgType::Int => "int",
            ArgType::Bool => "bool",
        }
    }

    /// `v` is a value of this type: a JSON string, an integer (no fraction), a bool.
    pub fn accepts(self, v: &Json) -> bool {
        match self {
            ArgType::String => v.is_string(),
            ArgType::Int => v.is_i64() || v.is_u64(),
            ArgType::Bool => v.is_boolean(),
        }
    }
}

/// One declared argument, in file order. `required` and `default` never both hold.
#[derive(Debug, Clone, PartialEq)]
pub struct RecipeArg {
    pub name: String,
    pub ty: ArgType,
    pub default: Option<Json>,
    pub required: bool,
}

/// A locator the model resolves to a ref from a fresh snapshot or `grep over=ax` at run
/// time: accessibility role and name, and which match (`nth`, 0-based) when several fit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub role: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nth: Option<u32>,
}

/// One BrowserOS call. `args` is always a JSON object (`{}` when the file has none).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeStep {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<Vec<Target>>,
    #[serde(default = "empty_object", skip_serializing_if = "is_empty_object")]
    pub args: Json,
    /// Name of the value this step yields (`[a-z_][a-z0-9_]*`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<String>,
}

/// Optional success check after the last step: `text` appears in what the run saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expects {
    pub text: String,
}

/// A parsed, validated recipe (D-150). Build one only through [`parse_recipe`]; a value
/// built by hand is checked when its [`render_recipe`] text is parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    pub name: String,
    /// The folder: a registrable host or `_generic`.
    pub domain: String,
    pub description: String,
    /// `match:` path globs; `["/*"]` when the file has none.
    pub matches: Vec<String>,
    pub args: Vec<RecipeArg>,
    pub steps: Vec<RecipeStep>,
    pub expects: Option<Expects>,
}

impl Recipe {
    /// The declared argument `name`.
    pub fn arg(&self, name: &str) -> Option<&RecipeArg> {
        self.args.iter().find(|a| a.name == name)
    }

    /// Step tool names in order (`navigate`, `act`, ...), as reflect's judge matches them.
    pub fn step_tools(&self) -> Vec<&str> {
        self.steps.iter().map(|s| s.tool.as_str()).collect()
    }
}

/// What [`ToolStore::recipes`] found in one folder: valid recipes in file-name order, and
/// recipe files it ignored with why (bad file, name collision, retired row).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecipeReport {
    pub recipes: Vec<Recipe>,
    pub skipped: Vec<(PathBuf, String)>,
}

fn empty_object() -> Json {
    Json::Object(Map::new())
}

fn is_empty_object(v: &Json) -> bool {
    v.as_object().is_some_and(Map::is_empty)
}

fn is_false(b: &bool) -> bool {
    !*b
}

// ---------------------------------------------------------------- placeholders

/// Byte ranges of the `{name}` placeholders in `s`, braces included, in order.
fn placeholder_spans(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            let mut j = i + 1;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            if j > i + 1 && j < b.len() && b[j] == b'}' {
                out.push((i, j + 1));
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The `{name}` placeholders in `s`, in order, duplicates included: `{` then one or more of
/// `[A-Za-z0-9_]` then `}`. Every one must be a declared arg (D-150).
pub fn placeholders(s: &str) -> Vec<&str> {
    placeholder_spans(s)
        .into_iter()
        .map(|(a, b)| &s[a + 1..b - 1])
        .collect()
}

/// `s` with every `{name}` that `values` holds replaced by its value (a string as is, a
/// number or bool as JSON text); other placeholders stay. One pass: a filled value that
/// itself contains `{x}` is not filled again. `tools.run` applies this to step args and
/// target names after checking the args (D-151).
pub fn fill_placeholders(s: &str, values: &Map<String, Json>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    for (start, end) in placeholder_spans(s) {
        if let Some(v) = values.get(&s[start + 1..end - 1]) {
            out.push_str(&s[at..start]);
            match v {
                Json::String(t) => out.push_str(t),
                other => out.push_str(&other.to_string()),
            }
            at = end;
        }
    }
    out.push_str(&s[at..]);
    out
}

/// `s` holds only placeholders, `@vault(k)` literals and whitespace: no literal text.
fn only_refs(s: &str) -> bool {
    let mut rest = s.to_string();
    for p in placeholders(s) {
        rest = rest.replacen(&format!("{{{p}}}"), "", 1);
    }
    if let Ok(names) = vault_refs(s) {
        for n in names {
            rest = rest.replacen(&format!("@vault({n})"), "", 1);
        }
    }
    rest.trim().is_empty()
}

// ---------------------------------------------------------------- secrets

/// A string shaped like an API key or private key (the patterns the repo's secret hook uses:
/// Anthropic, OpenAI, AWS, GitHub, Slack, Google keys, PEM private keys).
pub fn key_shaped(s: &str) -> bool {
    fn alnum(b: u8) -> bool {
        b.is_ascii_alphanumeric()
    }
    fn alnum_dash(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'-'
    }
    fn alnum_us(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }
    fn alnum_dash_us(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
    }
    fn upper_digit(b: u8) -> bool {
        b.is_ascii_uppercase() || b.is_ascii_digit()
    }
    type Class = fn(u8) -> bool;
    let pats: [(&str, Class, usize); 12] = [
        ("sk-ant-", alnum_dash_us, 10),
        ("sk-", alnum, 20),
        ("AKIA", upper_digit, 16),
        ("ghp_", alnum, 20),
        ("github_pat_", alnum_us, 20),
        ("xoxb-", alnum_dash, 10),
        ("xoxa-", alnum_dash, 10),
        ("xoxp-", alnum_dash, 10),
        ("xoxr-", alnum_dash, 10),
        ("xoxs-", alnum_dash, 10),
        ("AIza", alnum_dash_us, 30),
        ("PRIVATE KEY-----", |_| true, 0),
    ];
    let b = s.as_bytes();
    pats.iter().any(|(prefix, class, min)| {
        s.match_indices(prefix).any(|(i, _)| {
            let run = b[i + prefix.len()..]
                .iter()
                .take_while(|&&c| class(c))
                .count();
            run >= *min
        })
    })
}

/// A string that must never be in a recipe: a hook-scrubbed `***`, a key shape, or text the
/// process-wide vault scrub changes (a vault value in any spelling it knows).
fn bad_string(s: &str) -> Option<&'static str> {
    if s.contains("***") {
        Some("holds `***`, a value the BrowserOS hook scrubbed; write the step without it")
    } else if key_shaped(s) || scrub(s) != s {
        Some("holds a key-shaped string or a vault value (rule 2)")
    } else {
        None
    }
}

/// A key that names a secret (`password`, `otp`, `cardNumber`, `api_key`...). Split into
/// words on non-alphanumerics and camelCase, compared lower case, so `passengers` and
/// `shipping` are not secrets.
fn secret_key(key: &str) -> bool {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in key.chars() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && prev_lower && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    const WORDS: [&str; 20] = [
        "pass",
        "passwd",
        "password",
        "passcode",
        "passphrase",
        "passkey",
        "pwd",
        "secret",
        "secrets",
        "token",
        "tokens",
        "otp",
        "totp",
        "cvv",
        "cvv2",
        "cvc",
        "pin",
        "card",
        "cardnumber",
        "apikey",
    ];
    words.iter().any(|w| {
        WORDS.contains(&w.as_str())
            || w.ends_with("password")
            || w.ends_with("token")
            || w.ends_with("secret")
    }) || words.concat().contains("apikey")
}

// ---------------------------------------------------------------- pass 1: raw tree

/// A path segment safe to show in an error: a short identifier that is not itself bad.
fn seg_label(s: &str) -> String {
    let ident =
        !s.is_empty() && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if ident && bad_string(s).is_none() {
        s.to_string()
    } else {
        "?".to_string()
    }
}

fn join(path: &[String]) -> String {
    if path.is_empty() {
        "the file".to_string()
    } else {
        path.concat().trim_start_matches('.').to_string()
    }
}

/// Every string of the YAML tree, keys included, through [`bad_string`]; no tags, no
/// non-string keys. Runs before any typed parse, so no error from a later step can echo a
/// key shape or a vault value.
fn raw_pass(v: &Yaml, path: &mut Vec<String>) -> Result<(), String> {
    match v {
        Yaml::String(s) => match bad_string(s) {
            Some(why) => Err(format!("{}: {why}", join(path))),
            None => Ok(()),
        },
        Yaml::Sequence(items) => {
            for (i, item) in items.iter().enumerate() {
                path.push(format!("[{}]", i + 1));
                raw_pass(item, path)?;
                path.pop();
            }
            Ok(())
        }
        Yaml::Mapping(m) => {
            for (k, item) in m {
                let Yaml::String(key) = k else {
                    return Err(format!("{}: a mapping key is not a string", join(path)));
                };
                if let Some(why) = bad_string(key) {
                    return Err(format!("{}: a key {why}", join(path)));
                }
                path.push(format!(".{}", seg_label(key)));
                raw_pass(item, path)?;
                path.pop();
            }
            Ok(())
        }
        Yaml::Tagged(_) => Err(format!("{}: YAML tags are not allowed", join(path))),
        Yaml::Null | Yaml::Bool(_) | Yaml::Number(_) => Ok(()),
    }
}

/// A serde_yaml_ng error as safe text: scrubbed, and only its location when the message
/// itself still looks like a key.
fn yaml_error(e: &serde_yaml_ng::Error) -> String {
    let msg = scrub(&e.to_string());
    if key_shaped(&msg) || msg.contains("***") {
        match e.location() {
            Some(l) => format!("YAML error at line {} column {}", l.line(), l.column()),
            None => "YAML error".to_string(),
        }
    } else {
        format!("YAML: {msg}")
    }
}

// ---------------------------------------------------------------- pass 2: typed

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecipe {
    name: String,
    domain: String,
    kind: String,
    description: String,
    #[serde(rename = "match", default)]
    matches: Option<Vec<String>>,
    #[serde(default)]
    args: Option<Mapping>,
    steps: Vec<RecipeStep>,
    #[serde(default)]
    expects: Option<Expects>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawArg {
    #[serde(rename = "type")]
    ty: ArgType,
    #[serde(default)]
    default: Option<Json>,
    #[serde(default)]
    required: bool,
}

/// `[a-z_][a-z0-9_]*`, at most [`MAX_IDENT`] bytes: arg and `returns` names.
fn is_arg_name(s: &str) -> bool {
    let mut b = s.bytes();
    matches!(b.next(), Some(c) if c.is_ascii_lowercase() || c == b'_')
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        && s.len() <= MAX_IDENT
}

/// `[a-z0-9_]+` and a valid file name (D-312, D-310).
fn is_recipe_name(s: &str) -> bool {
    is_tool_name(s) && crate::domain::valid_name(s)
}

/// No control character or line separator (a description or locator is one line).
fn one_line(s: &str) -> bool {
    !s.chars().any(header_unsafe)
}

/// The `@vault(` opener in any ASCII case.
fn has_vault_opener(s: &str) -> bool {
    s.to_ascii_lowercase().contains("@vault(")
}

/// Rules every string outside step args obeys: declared placeholders, no vault literal, no
/// raw ref.
fn plain_string(at: &str, s: &str, declared: &[String]) -> Result<(), String> {
    if has_vault_opener(s) {
        return Err(format!(
            "{at}: @vault(k) is allowed only in act text/value and fields[].value (D-150)"
        ));
    }
    if s.contains("[ref=") {
        return Err(format!(
            "{at}: holds a raw ref; refs change between snapshots"
        ));
    }
    undeclared(at, s, declared)
}

fn undeclared(at: &str, s: &str, declared: &[String]) -> Result<(), String> {
    match placeholders(s)
        .into_iter()
        .find(|p| !declared.iter().any(|d| d == p))
    {
        Some(p) => Err(format!(
            "{at}: placeholder {{{p}}} is not a declared arg (declare it under args)"
        )),
        None => Ok(()),
    }
}

fn check_target(at: &str, t: &Target, declared: &[String]) -> Result<(), String> {
    if t.role.trim().is_empty() {
        return Err(format!("{at}: target role is empty"));
    }
    for (what, s) in [("role", &t.role), ("name", &t.name)] {
        if !one_line(s) || s.chars().count() > MAX_LOCATOR_CHARS {
            return Err(format!(
                "{at}: target {what} must be one line of at most {MAX_LOCATOR_CHARS} chars"
            ));
        }
        plain_string(&format!("{at}.target.{what}"), s, declared)?;
    }
    Ok(())
}

/// Path segment of a walk over step args.
#[derive(Clone, Copy)]
enum Seg<'a> {
    Key(&'a str),
    Idx(usize),
}

fn seg_text(path: &[Seg<'_>]) -> String {
    let mut out = String::new();
    for s in path {
        match s {
            Seg::Key(k) => {
                out.push('.');
                out.push_str(k);
            }
            Seg::Idx(i) => out.push_str(&format!("[{}]", i + 1)),
        }
    }
    out
}

/// `@vault(k)` may stand at `act` args `text`, `value` and `fields[].value` only (D-150).
fn vault_spot(is_act: bool, path: &[Seg<'_>]) -> bool {
    is_act
        && matches!(
            path,
            [Seg::Key("text")]
                | [Seg::Key("value")]
                | [Seg::Key("fields"), Seg::Idx(_), Seg::Key("value")]
        )
}

struct ArgWalk<'a> {
    at: &'a str,
    is_act: bool,
    declared: &'a [String],
}

impl ArgWalk<'_> {
    /// `key` is the nearest object key above `v` (array items inherit it).
    fn walk<'v>(&self, v: &'v Json, key: &'v str, path: &mut Vec<Seg<'v>>) -> Result<(), String> {
        let here = |p: &[Seg<'_>]| format!("{}.args{}", self.at, seg_text(p));
        match v {
            Json::Object(m) => {
                for (k, item) in m {
                    if FORBIDDEN_KEYS.contains(&k.to_ascii_lowercase().as_str()) {
                        return Err(format!(
                            "{}: key `{k}` is per session (refs, page, session and agent ids \
                             change); use a target locator or drop it (D-150)",
                            here(path)
                        ));
                    }
                    path.push(Seg::Key(k));
                    self.walk(item, k, path)?;
                    path.pop();
                }
                Ok(())
            }
            Json::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    path.push(Seg::Idx(i));
                    self.walk(item, key, path)?;
                    path.pop();
                }
                Ok(())
            }
            Json::String(s) => {
                if has_vault_opener(s) {
                    if !vault_spot(self.is_act, path) {
                        return Err(format!(
                            "{}: @vault(k) is allowed only in act text/value and \
                             fields[].value (D-150)",
                            here(path)
                        ));
                    }
                    if vault_refs(s).is_err() {
                        return Err(format!("{}: malformed @vault(...) literal", here(path)));
                    }
                }
                if s.contains("[ref=") {
                    return Err(format!(
                        "{}: holds a raw ref; refs change between snapshots",
                        here(path)
                    ));
                }
                if secret_key(key) && !only_refs(s) {
                    return Err(format!(
                        "{}: a literal value under a secret-looking key; use an {{arg}} or \
                         @vault(k) (D-150)",
                        here(path)
                    ));
                }
                undeclared(&here(path), s, self.declared)
            }
            Json::Number(_) if secret_key(key) => Err(format!(
                "{}: a literal value under a secret-looking key; use an {{arg}} or @vault(k) \
                 (D-150)",
                here(path)
            )),
            _ => Ok(()),
        }
    }
}

fn check_step(i: usize, step: &RecipeStep, declared: &[String]) -> Result<(), String> {
    let at = format!("steps[{}]", i + 1);
    if !RECIPE_TOOLS.contains(&step.tool.as_str()) {
        return Err(format!(
            "{at}: tool {:?} is not allowed in a recipe; allowed: {} (D-150)",
            step.tool,
            RECIPE_TOOLS.join(", ")
        ));
    }
    let Some(args) = step.args.as_object() else {
        return Err(format!("{at}: args must be a mapping"));
    };
    if let Some(t) = &step.target {
        check_target(&at, t, declared)?;
    }
    if let Some(ts) = &step.targets {
        if ts.is_empty() {
            return Err(format!("{at}: targets is empty"));
        }
        for t in ts {
            check_target(&at, t, declared)?;
        }
    }
    if let Some(r) = &step.returns {
        if !is_arg_name(r) {
            return Err(format!("{at}: returns must be [a-z_][a-z0-9_]*"));
        }
    }
    let is_act = step.tool == "act";
    ArgWalk {
        at: &at,
        is_act,
        declared,
    }
    .walk(&step.args, "", &mut Vec::new())?;
    match step.tool.as_str() {
        "act" => check_act(&at, step, args),
        "wait" => match args.get("for").and_then(Json::as_str) {
            None => Err(format!(
                "{at}: wait needs args.for (text, selector, ...); a fixed sleep is refused \
                 (rule 6)"
            )),
            Some(f) if f.trim().eq_ignore_ascii_case("time") => Err(format!(
                "{at}: wait for=time is a fixed sleep (rule 6); wait for a text or a selector"
            )),
            Some(_) => Ok(()),
        },
        _ => Ok(()),
    }
}

fn check_act(at: &str, step: &RecipeStep, args: &Map<String, Json>) -> Result<(), String> {
    let Some(kind) = args.get("kind").and_then(Json::as_str) else {
        return Err(format!(
            "{at}: act needs args.kind (click, type, fill, ...)"
        ));
    };
    if kind.ends_with("_at") {
        return Err(format!(
            "{at}: act kind {kind:?} acts on coordinates; a recipe acts on a target (D-150)"
        ));
    }
    match (kind, args.get("fields")) {
        ("fill", Some(fields)) => {
            let Some(fields) = fields.as_array().filter(|f| !f.is_empty()) else {
                return Err(format!("{at}: act fill fields must be a non-empty list"));
            };
            if fields.iter().any(|f| !f.is_object()) {
                return Err(format!("{at}: every act fill field must be a mapping"));
            }
            match &step.targets {
                Some(ts) if ts.len() == fields.len() => Ok(()),
                _ => Err(format!(
                    "{at}: act fill with fields[] needs targets, one per field ({} fields)",
                    fields.len()
                )),
            }
        }
        _ if TARGET_ACT_KINDS.contains(&kind) && step.target.is_none() => Err(format!(
            "{at}: act {kind} needs a target {{role, name}} (resolved to a ref at run time)"
        )),
        _ => Ok(()),
    }
}

fn build(raw: RawRecipe, folder: &str, stem: &str) -> Result<Recipe, String> {
    if raw.kind != RECIPE_KIND {
        return Err(format!("kind must be {RECIPE_KIND}, not {:?}", raw.kind));
    }
    if !is_recipe_name(&raw.name) {
        return Err(format!("name {:?} is not [a-z0-9_]+ (D-312)", raw.name));
    }
    if raw.name != stem {
        return Err(format!(
            "name {:?} does not equal the file stem {stem:?} ({stem}{RECIPE_SUFFIX})",
            raw.name
        ));
    }
    if raw.domain != folder {
        return Err(format!(
            "domain {:?} does not equal the folder {folder:?}",
            raw.domain
        ));
    }
    let d = &raw.description;
    let n = d.chars().count();
    if d.trim().is_empty() || n > MAX_DESCRIPTION_CHARS || !one_line(d) {
        return Err(format!(
            "description must be one line of 1..={MAX_DESCRIPTION_CHARS} chars"
        ));
    }
    let matches = raw.matches.unwrap_or_else(|| vec!["/*".to_string()]);
    if matches.is_empty() {
        return Err("match is empty (omit it for /*)".into());
    }
    for m in &matches {
        if !one_line(m) {
            return Err("match: a pattern holds a control character".into());
        }
        if let Some(why) = match_violation(m) {
            return Err(format!("match {m:?} {why}"));
        }
    }
    let mut args = Vec::new();
    for (k, v) in raw.args.unwrap_or_default() {
        let Yaml::String(name) = k else {
            return Err("args: a name is not a string".into());
        };
        if !is_arg_name(&name) {
            return Err(format!("args: name {name:?} is not [a-z_][a-z0-9_]*"));
        }
        let a: RawArg =
            serde_yaml_ng::from_value(v).map_err(|e| format!("args.{name}: {}", yaml_error(&e)))?;
        if let Some(def) = &a.default {
            if !a.ty.accepts(def) {
                return Err(format!(
                    "args.{name}: default is not a {} value",
                    a.ty.as_str()
                ));
            }
            if a.required {
                return Err(format!(
                    "args.{name}: required and default exclude each other (B-210)"
                ));
            }
        }
        args.push(RecipeArg {
            name,
            ty: a.ty,
            default: a.default,
            required: a.required,
        });
    }
    let declared: Vec<String> = args.iter().map(|a| a.name.clone()).collect();
    if raw.steps.is_empty() || raw.steps.len() > MAX_STEPS {
        return Err(format!(
            "a recipe has 1..={MAX_STEPS} steps, not {}",
            raw.steps.len()
        ));
    }
    plain_string("description", d, &declared)?;
    for m in &matches {
        plain_string("match", m, &declared)?;
    }
    for (i, s) in raw.steps.iter().enumerate() {
        check_step(i, s, &declared)?;
    }
    if let Some(e) = &raw.expects {
        let n = e.text.chars().count();
        if e.text.trim().is_empty() || n > MAX_EXPECTS_CHARS || !one_line(&e.text) {
            return Err(format!(
                "expects.text must be one line of 1..={MAX_EXPECTS_CHARS} chars"
            ));
        }
        plain_string("expects.text", &e.text, &declared)?;
    }
    Ok(Recipe {
        name: raw.name,
        domain: raw.domain,
        description: raw.description,
        matches,
        args,
        steps: raw.steps,
        expects: raw.expects,
    })
}

/// `<folder>/<stem>.recipe.yaml` for error messages, with `?` for a part that is not a safe
/// name (it may be model text).
fn display_path(folder: &str, stem: &str) -> PathBuf {
    let f = if valid_domain(folder) { folder } else { "?" };
    let s = if is_recipe_name(stem) { stem } else { "?" };
    PathBuf::from(f).join(format!("{s}{RECIPE_SUFFIX}"))
}

/// Parses and validates one recipe file (D-150). `folder` is the folder the file is in (or
/// will be written to), `stem` its file name without [`RECIPE_SUFFIX`]. The `.js` name
/// collision is checked by the [`ToolStore`] readers, which know the folder's files.
///
/// Errors are `StoreError::Recipe { path: <folder>/<stem>.recipe.yaml, msg }`; `msg` never
/// holds a vault value or a key-shaped string.
pub fn parse_recipe(text: &str, folder: &str, stem: &str) -> Result<Recipe, StoreError> {
    let path = display_path(folder, stem);
    let err = |msg: String| StoreError::Recipe {
        path: path.clone(),
        msg,
    };
    if !valid_domain(folder) {
        return Err(err(
            "folder is not `_generic` or a site folder [a-z0-9._-] (D-310)".into(),
        ));
    }
    if !is_recipe_name(stem) {
        return Err(err("file stem is not [a-z0-9_]+ (D-312)".into()));
    }
    if text.len() > MAX_RECIPE_BYTES {
        return Err(err(format!("file is larger than {MAX_RECIPE_BYTES} bytes")));
    }
    let tree: Yaml = serde_yaml_ng::from_str(text).map_err(|e| err(yaml_error(&e)))?;
    raw_pass(&tree, &mut Vec::new()).map_err(err)?;
    let raw: RawRecipe = serde_yaml_ng::from_value(tree).map_err(|e| err(yaml_error(&e)))?;
    build(raw, folder, stem).map_err(err)
}

#[derive(Serialize)]
struct RenderRecipe<'a> {
    name: &'a str,
    domain: &'a str,
    kind: &'static str,
    description: &'a str,
    #[serde(rename = "match")]
    matches: &'a [String],
    #[serde(skip_serializing_if = "Mapping::is_empty")]
    args: Mapping,
    steps: &'a [RecipeStep],
    #[serde(skip_serializing_if = "Option::is_none")]
    expects: Option<&'a Expects>,
}

#[derive(Serialize)]
struct RenderArg<'a> {
    #[serde(rename = "type")]
    ty: ArgType,
    #[serde(skip_serializing_if = "Option::is_none")]
    default: Option<&'a Json>,
    #[serde(skip_serializing_if = "is_false")]
    required: bool,
}

/// The YAML text of `r`, keys in the WAVE3 order (name, domain, kind, description, match,
/// args, steps, expects). For any `r` that [`parse_recipe`] returned,
/// `parse_recipe(&render_recipe(&r), &r.domain, &r.name) == Ok(r)`. A value that cannot be
/// serialized (not expected) renders as the empty string, which `parse_recipe` refuses, so a
/// writer fails closed.
pub fn render_recipe(r: &Recipe) -> String {
    let mut args = Mapping::new();
    for a in &r.args {
        let v = serde_yaml_ng::to_value(RenderArg {
            ty: a.ty,
            default: a.default.as_ref(),
            required: a.required,
        });
        match v {
            Ok(v) => {
                args.insert(Yaml::String(a.name.clone()), v);
            }
            Err(_) => return String::new(),
        }
    }
    let doc = RenderRecipe {
        name: &r.name,
        domain: &r.domain,
        kind: RECIPE_KIND,
        description: &r.description,
        matches: &r.matches,
        args,
        steps: &r.steps,
        expects: r.expects.as_ref(),
    };
    serde_yaml_ng::to_string(&doc).unwrap_or_default()
}

// ---------------------------------------------------------------- store readers

/// `(stem, path)` of every `*.recipe.yaml` directly in `dir`, sorted by stem. The suffix is
/// matched exactly (lower case), as the `.js` scan matches `js`.
fn recipe_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let stem = name.strip_suffix(RECIPE_SUFFIX)?.to_string();
            let path = e.path();
            path.is_file().then_some((stem, path))
        })
        .collect();
    files.sort();
    files
}

impl ToolStore {
    /// `tools/<domain>/<name>.recipe.yaml`, or `Err(BadPath)` when `domain` is not a folder
    /// name or `name` is not `[a-z0-9_]+` (D-310, D-312). Does not resolve hosts: pass a
    /// folder (`folder_for` first).
    pub fn try_recipe_path(&self, domain: &str, name: &str) -> Result<PathBuf, StoreError> {
        check_tool_id(domain, name)?;
        if !is_recipe_name(name) {
            return Err(StoreError::BadPath(format!(
                "recipe name {name:?} is not [a-z0-9_]+ (D-312)"
            )));
        }
        Ok(self
            .root
            .join(domain)
            .join(format!("{name}{RECIPE_SUFFIX}")))
    }

    /// Reads and validates one recipe file of `folder`: [`parse_recipe`], then the D-150
    /// rule that its name is not also a `.js` tool of the folder. Errors carry `path`.
    fn load_recipe_file(
        &self,
        folder: &str,
        stem: &str,
        path: &Path,
    ) -> Result<Recipe, StoreError> {
        let at = |msg: String| StoreError::Recipe {
            path: path.to_path_buf(),
            msg,
        };
        let text = fs::read_to_string(path).map_err(|e| at(format!("unreadable: {e}")))?;
        let r = parse_recipe(&text, folder, stem).map_err(|e| match e {
            StoreError::Recipe { msg, .. } => at(msg),
            other => other,
        })?;
        if self.root.join(folder).join(format!("{stem}.js")).exists() {
            return Err(at(format!(
                "name {stem} collides with the tool {stem}.js in the same folder (D-150)"
            )));
        }
        Ok(r)
    }

    /// The recipes of one folder, files only. `domain` is a host or a folder (it goes
    /// through `folder_for`); `_generic` recipes are not merged in (a folder lists its own).
    /// Invalid files and `.js` name collisions go to `skipped` with why.
    ///
    /// Does not consult `tools.health`: runtime callers use [`ToolStore::recipes_live`].
    pub fn recipes(&self, domain: &str) -> RecipeReport {
        self.read_recipes(&self.folder_for(domain))
    }

    fn read_recipes(&self, folder: &str) -> RecipeReport {
        let mut report = RecipeReport::default();
        // D-310: `folder_for` gives "" for anything that is not a folder.
        if !valid_domain(folder) {
            return report;
        }
        for (stem, path) in recipe_files(&self.root.join(folder)) {
            match self.load_recipe_file(folder, &stem, &path) {
                Ok(r) => report.recipes.push(r),
                Err(StoreError::Recipe { msg, .. }) => report.skipped.push((path, msg)),
                Err(e) => report.skipped.push((path, e.to_string())),
            }
        }
        report
    }

    /// [`ToolStore::recipes`] that also skips every recipe whose `tools.health` row in the
    /// folder is retired, even when its file is still in place; such a file goes to
    /// `skipped` as `retired (v<version>)` (D-315, as `scan_live`). Files are read before
    /// the rows; a memory error is `Err`, never an unfiltered list.
    pub async fn recipes_live(
        &self,
        store: &dyn MemoryStore,
        domain: &str,
    ) -> Result<RecipeReport, StoreError> {
        let folder = self.folder_for(domain);
        if !valid_domain(&folder) {
            return Ok(RecipeReport::default());
        }
        let found = self.read_recipes(&folder);
        let rows = store.tool_health(&folder).await?;
        let mut report = RecipeReport {
            recipes: Vec::new(),
            skipped: found.skipped,
        };
        for r in found.recipes {
            match retired_version(&rows, &r.name) {
                Some(v) => {
                    let path = self.try_recipe_path(&folder, &r.name)?;
                    report.skipped.push((path, format!("retired (v{v})")));
                }
                None => report.recipes.push(r),
            }
        }
        Ok(report)
    }

    /// One recipe by name, files only: `Ok(None)` when the folder has no
    /// `<name>.recipe.yaml` (the file name compared exactly, so on a case-insensitive volume
    /// `Inbox.recipe.yaml` is not `inbox`), `Err(Recipe)` when the file is invalid or
    /// collides with `<name>.js`, `Err(BadPath)` for a bad domain or name. `domain` is a
    /// host or a folder. A caller that runs it checks the `tools.health` row itself
    /// ([`ToolStore::health`]): a retired recipe is refused (D-151).
    pub fn recipe(&self, domain: &str, name: &str) -> Result<Option<Recipe>, StoreError> {
        let folder = self.folder_for(domain);
        let wanted = self.try_recipe_path(&folder, name)?;
        let Some(dir) = wanted.parent() else {
            return Ok(None);
        };
        match recipe_files(dir).into_iter().find(|(stem, _)| stem == name) {
            Some((stem, path)) => self.load_recipe_file(&folder, &stem, &path).map(Some),
            None => Ok(None),
        }
    }
}
