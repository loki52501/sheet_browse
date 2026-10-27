//! C-1 `gh(args)`: the GitHub CLI, run as an argument vector (no shell).
//!
//! Classification (D-540, fails closed): reads on an allowlist are Read (`run list`,
//! `run view [--log]`, `pr view|list|diff|checks|status`, `issue view|list|status`,
//! `repo view`, `workflow list|view`, `search ...`, `status`, and `api` with GET or HEAD);
//! `--web`/`-w` on a read opens a browser and is Navigate. `api` with `-X`/`--method` POST,
//! PUT, PATCH or any other method is Account, DELETE is Delete, and any `-f`, `-F`,
//! `--field`, `--raw-field` or `--input` makes it Account even with `-X GET`. Clustered
//! short flags are read letter by letter (`-iXPOST`, `-iftitle=x`, `-cw`); an unknown `api`
//! flag is Account. Any `<group> delete` is Delete. `gh auth` and `gh config` are refused
//! outright: `gh auth token` and `gh config get oauth_token` print a credential the vault
//! does not know, so the scrubber cannot catch it (D-540, D-542). An `api` endpoint that is
//! a full URL (or starts with `//`) and `--hostname` must name a GitHub API host
//! (`https://api.github.com`, or a configured GHE host), which becomes the gate domain;
//! anything else is refused. Everything else (`pr merge`, `release ...`, `workflow run`,
//! extensions, unknown commands) is Account.
//!
//! The subcommand is the first word after the group and `-R/--repo VALUE` (`gh run -R o/r
//! delete 5` is Delete); after any other flag the class is the highest any later word
//! gives, and at least Account (D-592). On a read-allowlist command, a short-flag cluster
//! is read with that command's own flags, so a value is not read as `-w` (`-Rw/x`,
//! `run list -w ci.yml`), and an `R` inside a cluster (`-cRHOST/o/r`) is a repo whose host
//! is checked.
//!
//! A flag before the group (D-594): gh's parser skips it to find the command and hands it
//! to that command, so `gh --repo=o/r run delete 5` runs `run delete`. The server moves
//! `--name=value` and `-R`, `--repo` or `--hostname` with its value behind the group, the
//! way gh reads them, and classifies and runs that ([`group_first`]); any other flag before
//! the group is refused (a lone `--help` or `--version` is kept).
//!
//! Aliases (D-591, [`alias`]): a call that is not on the read allowlist or `api` may name a
//! user alias, so the server reads `gh alias list` (refusing the call when it cannot) and
//! classifies and runs the expansion instead of the alias. `alias set` is classified at the
//! class of the command its expansion runs (at least Account) and refused when that command
//! would be refused or is a shell alias; `alias import` is refused (D-597).
//! `call` plans again with a fresh list and runs only when that plan (the words, the host,
//! the class) equals the one classify made for it (D-595).
//!
//! `run view <id> --log` (or `--log-failed`) saves the whole log, scrubbed, to
//! `runs/<run>/files/gh-run-<id>.log` and returns only the first failing step.

pub mod alias;
pub mod log;

use crate::{argv, proc, text, Config};
pub use alias::Aliases;
use async_trait::async_trait;
use harness_core::class::Class;
use harness_core::mcp::{Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Stdout kept for a run log.
const LOG_MAX_BYTES: usize = 32 * 1024 * 1024;
/// Stdout kept for any other gh call.
const OUT_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Stderr shown on failure, in chars.
const ERR_CHARS: usize = 2_000;
/// Stdout kept for `gh alias list`.
const ALIAS_MAX_BYTES: usize = 1024 * 1024;
/// Cap on one `gh alias list` (it reads a local file).
const ALIAS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// Aliases may expand to aliases; deeper than this is refused.
const MAX_ALIAS_DEPTH: usize = 4;

/// No prompts, no pager, no colour, no update check: gh runs without a terminal.
const GH_ENV: [(&str, &str); 5] = [
    ("GH_PROMPT_DISABLED", "1"),
    ("GH_PAGER", "cat"),
    ("NO_COLOR", "1"),
    ("CLICOLOR", "0"),
    ("GH_NO_UPDATE_NOTIFIER", "1"),
];

/// `gh` with [`Config::from_env`].
pub fn tools() -> Vec<ToolSpec> {
    tools_with(&Arc::new(Config::from_env()))
}

pub fn tools_with(cfg: &Arc<Config>) -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "gh".into(),
        description: "gh(args): GitHub CLI without a shell, e.g. \"run list\", \"run view 123 \
                      --log\" (returns the first failing step; full log saved under files/), \
                      \"pr view 5\", \"api repos/o/r\". Reads run at once; writes (api -X POST, \
                      pr merge, release) ask first."
            .into(),
        class: Class::Read,
        args: json!({
            "type": "object",
            "properties": {
                "args": {
                    "description": "gh arguments: one string (quoted like a shell, no pipes or redirects) or an array of strings",
                    "oneOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                }
            },
            "required": ["args"]
        }),
        handler: Arc::new(Gh {
            cfg: cfg.clone(),
            planned: Mutex::new(VecDeque::new()),
        }),
    }]
}

struct Gh {
    cfg: Arc<Config>,
    /// What each classify planned, until its call (D-595).
    planned: Mutex<VecDeque<Planned>>,
}

/// Classify's plan for one call, kept for the call that follows it (D-595).
struct Planned {
    /// The address of the call's [`ToolCtx`]. Core's `dispatch` hands one context, a local
    /// of its pinned future, to classify and then to call, so the address names the call;
    /// if it ever did not, call finds no plan and refuses (fails closed). B-591 proposes a
    /// token in the interface instead.
    ctx: usize,
    run: String,
    named: Vec<String>,
    plan: GhPlan,
}

/// Plans kept at once; the oldest goes first (one whose call never came: the gate refused).
const MAX_PLANNED: usize = 64;

fn ctx_key(ctx: &ToolCtx) -> usize {
    ctx as *const ToolCtx as usize
}

/// The gh arguments of a call, without a leading `gh` word, the group first
/// ([`group_first`]).
fn gh_argv(args: &Value) -> Result<Vec<String>, ToolError> {
    let mut v = argv::from_value(args, "args")?;
    if v.first().map(String::as_str) == Some("gh") {
        v.remove(0);
    }
    group_first(&v, false)
}

/// Flags gh's parser reads with the next word as their value when they come before the
/// group (gh's root command knows only `--help` and `--version`, which take none).
const LEAD_VALUE_FLAGS: [&str; 3] = ["-R", "--repo", "--hostname"];

/// `argv` with the flags before the group moved behind it (D-594). gh's parser (cobra)
/// skips flags to find the command, then hands them to that command in their order, so
/// `--repo=o/r run delete 5` runs `run delete` with `--repo=o/r`, as `run --repo=o/r delete
/// 5` does, and `--hostname h tok` runs the alias `tok` with `--hostname h`. Only the forms
/// cobra reads one way are moved: `--name=value`, and [`LEAD_VALUE_FLAGS`] with the next word
/// as the value. Any other flag first is refused, as is a call that is only flags; a lone
/// `--help` or `--version` is returned as it is. `quiet` (inside an alias's expansion) leaves
/// the flag out of the message.
fn group_first(argv: &[String], quiet: bool) -> Result<Vec<String>, ToolError> {
    // D-598: gh's parser drops empty words, so `'' auth token` or `repo '' delete` runs a
    // command this classifier would not see; an empty word is refused anywhere.
    if argv.iter().any(|t| t.is_empty()) {
        return Err(ToolError::Args(
            "gh: an empty word is not passed to gh (gh drops it, which changes the command); \
             write an empty flag value as --flag=; nothing ran"
                .into(),
        ));
    }
    let is_flag = |t: &str| t.starts_with('-');
    match argv {
        [] => return Ok(Vec::new()),
        [first, ..] if !is_flag(first) => return Ok(argv.to_vec()),
        [one] if one == "--help" || one == "--version" => return Ok(argv.to_vec()),
        _ => {}
    }
    let refuse = |t: &str| {
        let what = if quiet {
            String::new()
        } else {
            format!(" ({})", text::line(t, 40))
        };
        ToolError::Args(format!(
            "gh: a flag before the command{what} is not read here; put the flags after the \
             subcommand, e.g. gh run delete 5 --repo o/r; nothing ran"
        ))
    };
    let mut i = 0;
    while let Some(t) = argv.get(i).filter(|t| is_flag(t)) {
        let one_word = t
            .strip_prefix("--")
            .and_then(|l| l.split_once('='))
            .is_some_and(|(name, _)| !name.is_empty());
        if one_word {
            i += 1;
        } else if LEAD_VALUE_FLAGS.contains(&t.as_str()) && i + 1 < argv.len() {
            i += 2;
        } else {
            return Err(refuse(t));
        }
    }
    let Some(group) = argv.get(i) else {
        return Err(refuse(&argv[0]));
    };
    let mut out = Vec::with_capacity(argv.len());
    out.push(group.clone());
    out.extend_from_slice(&argv[..i]);
    out.extend_from_slice(&argv[i + 1..]);
    Ok(out)
}

/// The gate's view of one `gh` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhCall {
    pub class: Class,
    /// The gate domain: `github.com`, or the GitHub API host an `api` call names (a full
    /// `https://api.github.com/...` endpoint, a configured GHE host, `--hostname`).
    pub domain: String,
}

/// The default gh host, and the gate domain of every call that names no other host.
const GITHUB: &str = "github.com";
/// The only github.com host a full-URL `api` endpoint may name.
const GITHUB_API: &str = "api.github.com";

/// The class of `gh <argv>` with no GHE hosts and no aliases (see [`plan`]).
pub fn classify_args(argv: &[String]) -> Result<Class, ToolError> {
    classify_with(argv, &[]).map(|c| c.class)
}

/// The class and gate domain of `gh <argv>` with no aliases known (see [`plan`]).
/// `ghe_hosts` are the GitHub Enterprise hosts an `api` call may reach besides github.com
/// ([`Config::gh_hosts`]). `Err` for `gh auth`, `gh config`, an empty command, and an `api`
/// call that would reach a host that is not a GitHub API host.
pub fn classify_with(argv: &[String], ghe_hosts: &[String]) -> Result<GhCall, ToolError> {
    plan(argv, ghe_hosts, &Aliases::default()).map(|p| p.call)
}

/// What one `gh` call does and what runs for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhPlan {
    pub call: GhCall,
    /// The gh arguments that run: `argv` itself, or an alias's expansion (D-591).
    pub argv: Vec<String>,
    /// The alias `argv` named, if any.
    pub alias: Option<String>,
}

/// The gate's view of `gh <argv>` given the user's `aliases` (module doc, D-540, D-591).
/// An alias is replaced by its expansion, which is classified, and later run, instead: an
/// alias of a refused command is refused, one of a delete is Delete, one of a read is Read.
/// A shell alias (`!...`), an alias whose value could not be read and one given too few
/// arguments are refused. `alias set` is at least Account and at the class of the command
/// its expansion would run, and refused when that command would be refused.
pub fn plan(argv: &[String], ghe_hosts: &[String], aliases: &Aliases) -> Result<GhPlan, ToolError> {
    plan_at(argv, ghe_hosts, aliases, 0)
}

fn plan_at(
    argv: &[String],
    ghe_hosts: &[String],
    aliases: &Aliases,
    depth: usize,
) -> Result<GhPlan, ToolError> {
    // An expansion may start with a flag too (`alias set x '--repo=o/r run delete'`).
    let argv = &group_first(argv, depth > 0)?;
    if let Some(hit) = aliases.lookup(argv) {
        let name = text::line(&hit.name, 64);
        if depth >= MAX_ALIAS_DEPTH {
            return Err(ToolError::Args(format!(
                "gh {name}: aliases nest more than {MAX_ALIAS_DEPTH} deep; nothing ran"
            )));
        }
        let Some(expansion) = hit.expansion else {
            return Err(ToolError::Args(format!(
                "gh {name} is an alias whose expansion this server cannot read from `gh alias \
                 list`; run it in your own terminal, or call the command it stands for"
            )));
        };
        if expansion.starts_with('!') {
            return Err(shell_alias(&name));
        }
        let expanded = alias::expand(expansion, hit.args)
            .map_err(|e| ToolError::Args(format!("gh {name} (an alias): {e}; nothing ran")))?;
        let inner = plan_at(&expanded, ghe_hosts, aliases, depth + 1)
            .map_err(|e| in_alias(&name, &expanded, e))?;
        return Ok(GhPlan {
            alias: Some(hit.name.clone()),
            ..inner
        });
    }
    if let Some(group) = argv.first() {
        // `gh issue -R o/r mine`: a two-word alias with flags between its words. gh's own
        // lookup may still find it; refuse rather than guess what it runs.
        let rest = &argv[1..];
        let later: Vec<&str> = match subcommand(rest) {
            Sub::Known(i) if i > 0 => vec![rest[i].as_str()],
            Sub::Ambiguous(words) => words,
            _ => Vec::new(),
        };
        if let Some(w) = later.iter().find(|w| aliases.has_pair(group, w)) {
            // Inside an expansion the second word may be far into it: not named.
            let named = if depth > 0 {
                "a two-word alias".to_string()
            } else {
                format!("gh {} {}", text::line(group, 64), text::line(w, 64))
            };
            return Err(ToolError::Args(format!(
                "{named} is an alias: put no flags between its two words; nothing ran"
            )));
        }
    }
    let call = classify_static(argv, ghe_hosts, aliases, depth)?;
    Ok(GhPlan {
        call,
        argv: argv.to_vec(),
        alias: None,
    })
}

fn shell_alias(name: &str) -> ToolError {
    ToolError::Args(format!(
        "gh {name} is a shell alias (`!...` or --shell): it runs through sh, and this server \
         never runs a shell; nothing ran"
    ))
}

/// The message of an error, without its kind.
fn message(e: ToolError) -> String {
    match e {
        ToolError::Args(m) | ToolError::Failed(m) => m,
        other => other.to_string(),
    }
}

/// A refusal of an alias's expansion, naming the alias and the command it stands for by
/// [`command_words`]. The reason is the inner refusal, which names no word of the expansion:
/// every refusal made at depth > 0 leaves its words out (D-596).
fn in_alias(name: &str, expanded: &[String], e: ToolError) -> ToolError {
    ToolError::Args(format!(
        "gh {name} is an alias for gh {} ...: {}",
        command_words(expanded),
        message(e)
    ))
}

/// How a card or an error names an alias's expansion: its first two words, stopping at the
/// first that is not a plain command word (letters, digits, `-`, `_`, not starting with
/// `-`), so a flag, an endpoint, a URL or a value is never shown (D-591, D-596). An
/// expansion can hold anything the user typed.
fn command_words(expanded: &[String]) -> String {
    let plain = |w: &&String| {
        !w.is_empty()
            && w.len() <= 32
            && !w.starts_with('-')
            && w.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    let words: Vec<&str> = expanded
        .iter()
        .take(2)
        .take_while(plain)
        .map(String::as_str)
        .collect();
    if words.is_empty() {
        "(a command)".to_string()
    } else {
        words.join(" ")
    }
}

/// Does classifying `gh <argv>` need the user's aliases? Not for `api`, `auth`, `config`,
/// `status` (built in: an alias cannot shadow a built-in command), a flag, or a command on
/// the read allowlist; any other call may name an alias.
pub fn needs_aliases(argv: &[String]) -> bool {
    let Some(group) = argv.first() else {
        return false;
    };
    if group.starts_with('-') || matches!(group.as_str(), "api" | "auth" | "config" | "status") {
        return false;
    }
    match subcommand(&argv[1..]) {
        Sub::Known(i) => !is_read(group, Some(&argv[1 + i])),
        _ => true,
    }
}

/// The subcommand of `gh <group> ...` (`rest` is everything after the group).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sub<'a> {
    /// No word after the flags.
    None,
    /// `rest[i]` is the subcommand.
    Known(usize),
    /// A flag this module does not know comes first, so a flag value cannot be told from
    /// the subcommand: any listed word (every later word that is not a flag) may be it.
    Ambiguous(Vec<&'a str>),
}

/// The first word of `rest` that is not a flag. `-R/--repo VALUE` (`gh run -R o/r delete 5`
/// runs the delete) and `--help`/`-h` are skipped; any other flag first makes the result
/// [`Sub::Ambiguous`] (D-592).
fn subcommand(rest: &[String]) -> Sub<'_> {
    let is_flag = |t: &str| t.len() > 1 && t.starts_with('-');
    let mut i = 0;
    let mut unsure = false;
    while i < rest.len() {
        let t = rest[i].as_str();
        if t == "-R" || t == "--repo" {
            i += 2;
            continue;
        }
        if is_flag(t) {
            let known = t.starts_with("--repo=")
                || t == "--help"
                || t == "-h"
                || (t.starts_with("-R") && !t.starts_with("--"));
            unsure |= !known;
            i += 1;
            continue;
        }
        if !unsure {
            return Sub::Known(i);
        }
        let words = rest[i..]
            .iter()
            .map(String::as_str)
            .filter(|w| !is_flag(w))
            .collect();
        return Sub::Ambiguous(words);
    }
    Sub::None
}

/// The read allowlist (D-540).
fn is_read(group: &str, sub: Option<&str>) -> bool {
    match (group, sub) {
        ("run", Some("list" | "view"))
        | ("pr", Some("view" | "list" | "diff" | "checks" | "status"))
        | ("issue", Some("view" | "list" | "status"))
        | ("repo", Some("view"))
        | ("workflow", Some("list" | "view"))
        | ("status", _) => true,
        ("search", Some(s)) => !s.starts_with('-'),
        _ => false,
    }
}

/// The class of `gh <group> <sub> ...` from the static rules (D-540) alone.
fn sub_class(group: &str, sub: Option<&str>) -> Class {
    // `remove` and `rm` are gh's own aliases of delete (`secret remove`, `variable remove`).
    if matches!(sub, Some("delete" | "remove" | "rm")) {
        Class::Delete
    } else if is_read(group, sub) {
        Class::Read
    } else {
        Class::Account
    }
}

/// `gh alias set [--clobber] NAME EXPANSION` (D-591): Account at least, and the class of the
/// command the expansion runs (its `$N` placeholders read as plain words; the real arguments
/// are classified again when the alias runs). Refused: a shell alias (`--shell`, `-s`, an
/// expansion starting with `!`), an expansion read from stdin (`-`), a flag this server does
/// not read, and an expansion whose command would be refused (`auth token`, another host).
fn alias_set_class(
    args: &[String],
    ghe_hosts: &[String],
    aliases: &Aliases,
    depth: usize,
) -> Result<Class, ToolError> {
    let mut words = Vec::new();
    let mut positional_only = false;
    for t in args {
        if positional_only || t == "-" || !t.starts_with('-') {
            words.push(t.as_str());
            continue;
        }
        match t.as_str() {
            "--" => positional_only = true,
            "--clobber" | "--help" | "-h" => {}
            s if s.starts_with("--shell") => return Err(shell_alias("alias set")),
            s if short_cluster(s).is_some_and(|c| c.chars().all(|l| l == 's' || l == 'h')) => {
                if s.contains('s') {
                    return Err(shell_alias("alias set"));
                }
            }
            _ => {
                let flag = if depth > 0 {
                    String::new()
                } else {
                    format!(" {}", text::line(t, 40))
                };
                return Err(ToolError::Args(format!(
                    "gh alias set: flag{flag} is not one this server reads (--clobber); nothing ran"
                )));
            }
        }
    }
    let [name, expansion] = words[..] else {
        return Err(ToolError::Args(
            "gh alias set needs exactly a name and an expansion, e.g. alias set pv \"pr view\""
                .into(),
        ));
    };
    // Inside an expansion (`alias set` as an alias) the name is its third word: not shown.
    let name = if depth > 0 {
        "NAME".to_string()
    } else {
        text::line(name, 64)
    };
    if expansion == "-" {
        return Err(ToolError::Args(
            "gh alias set NAME -: an expansion read from stdin cannot be checked; give it as \
             an argument"
                .into(),
        ));
    }
    if expansion.trim_start().starts_with('!') {
        return Err(shell_alias(&name));
    }
    let target = argv::split(&alias::without_placeholders(expansion))?;
    if target.is_empty() {
        return Err(ToolError::Args(
            "gh alias set: the expansion is empty".into(),
        ));
    }
    if depth >= MAX_ALIAS_DEPTH {
        return Err(ToolError::Args(format!(
            "gh alias set: aliases nest more than {MAX_ALIAS_DEPTH} deep; nothing ran"
        )));
    }
    let inner = plan_at(&target, ghe_hosts, aliases, depth + 1).map_err(|e| {
        ToolError::Args(format!(
            "gh alias set {name}: the alias would run a refused command: {}",
            message(e)
        ))
    })?;
    Ok(inner.call.class.max(Class::Account))
}

/// `gh alias import FILE` is refused: it stores every alias in a file (or stdin) with no
/// check of what each would run, so `tok: auth token` would be one Account ask away. Aliases
/// are set one at a time with `alias set`, which checks the expansion (D-597). `alias set`
/// or `import` after an unknown flag is refused too, so the check cannot be skipped.
fn alias_refusals(known: Option<&str>, sub: &Sub<'_>) -> Result<(), ToolError> {
    if known == Some("import") {
        return Err(ToolError::Args(
            "gh alias import is refused: the aliases in a file cannot be checked; set each with \
             gh alias set NAME EXPANSION, which checks what it runs; nothing ran"
                .into(),
        ));
    }
    if let Sub::Ambiguous(words) = sub {
        if words.iter().any(|w| matches!(*w, "set" | "import")) {
            return Err(ToolError::Args(
                "gh alias: a flag before the alias subcommand is not read here; put the flags \
                 after the subcommand, e.g. gh alias set --clobber pv \"pr view\"; nothing ran"
                    .into(),
            ));
        }
    }
    Ok(())
}

/// [`plan`] for a call that is not an alias.
fn classify_static(
    argv: &[String],
    ghe_hosts: &[String],
    aliases: &Aliases,
    depth: usize,
) -> Result<GhCall, ToolError> {
    let group = argv
        .first()
        .ok_or_else(|| ToolError::Args("gh needs a command, e.g. \"run list\"".into()))?;
    if group == "auth" {
        return Err(ToolError::Args(
            "gh auth is refused: `gh auth token` prints a credential the harness cannot scrub. \
             Log in with `gh auth login` in your own terminal."
                .into(),
        ));
    }
    if group == "config" {
        return Err(ToolError::Args(
            "gh config is refused: `gh config get oauth_token` and `gh config list` can print a \
             credential the harness cannot scrub. Change gh settings in your own terminal."
                .into(),
        ));
    }
    let rest = &argv[1..];
    let mut domain = GITHUB.to_string();
    if group == "api" {
        if let Some(host) = api_host(rest, ghe_hosts, depth > 0)? {
            domain = host;
        }
    } else if let Some(host) = repo_host(rest, ghe_hosts, depth > 0)? {
        domain = host;
    }
    let sub = subcommand(rest);
    let class = if group == "api" {
        // `api` has no subcommands: its first word is the endpoint (D-540 kept as it was).
        if rest.first().map(String::as_str) == Some("delete") {
            Class::Delete
        } else {
            api_class(rest)
        }
    } else if group == "status" {
        Class::Read
    } else {
        match &sub {
            Sub::None => sub_class(group, None),
            Sub::Known(i) => sub_class(group, Some(&rest[*i])),
            // Any word may be the subcommand: the highest class any of them gives, and
            // never below Account (a read is a read only when its subcommand is certain).
            Sub::Ambiguous(words) => words
                .iter()
                .map(|w| sub_class(group, Some(w)))
                .fold(Class::Account, Class::max),
        }
    };
    let known_at = match &sub {
        Sub::Known(i) => Some(*i),
        _ => None,
    };
    let known = known_at.map(|i| rest[i].as_str());
    if group == "alias" {
        alias_refusals(known, &sub)?;
    }
    let class = match (group.as_str(), known_at) {
        ("alias", Some(i)) if rest[i] == "set" => {
            class.max(alias_set_class(&rest[i + 1..], ghe_hosts, aliases, depth)?)
        }
        _ => class,
    };
    let class = if opens_web(group, known, rest) {
        class.max(Class::Navigate)
    } else {
        class
    };
    Ok(GhCall { class, domain })
}

/// `--web`, `--web=..`, or `-w` in a single-dash cluster: the call opens a browser. For a
/// command on the read allowlist a cluster is read the way gh's flag parser reads it, with
/// that command's flags ([`read_short_flags`]): letters left to right, and a letter that
/// takes a value ends the cluster (`-Rw/x` names a repo, `run list -w ci.yml` a workflow).
/// For any other command a `w` anywhere counts (stricter). Every token is checked on its
/// own, so a `-w` that is really another flag's value (`-R -w`) also counts.
fn opens_web(group: &str, sub: Option<&str>, rest: &[String]) -> bool {
    let table = read_short_flags(group, sub);
    rest.iter().any(|a| {
        a == "--web"
            || a.starts_with("--web=")
            || short_cluster(a).is_some_and(|c| cluster_opens_web(c, table))
    })
}

fn cluster_opens_web(cluster: &str, table: Option<(&str, bool)>) -> bool {
    let Some((values, w_is_web)) = table else {
        return cluster.contains('w');
    };
    for l in cluster.chars() {
        if l == 'w' && w_is_web {
            return true;
        }
        if values.contains(l) {
            return false;
        }
    }
    false
}

/// For each read-allowlist command: its short flags that take a value (gh 2.93 `--help`,
/// with `-R` where it is accepted), and whether `-w` is `--web` there (on `run list` it is
/// `--workflow`). `None` for every other command.
fn read_short_flags(group: &str, sub: Option<&str>) -> Option<(&'static str, bool)> {
    Some(match (group, sub?) {
        ("run", "list") => ("bceqLstuwR", false),
        ("run", "view") => ("ajqtR", true),
        ("pr", "view" | "status") | ("issue", "view" | "status") => ("qtR", true),
        ("pr", "list") => ("aABHqlLSstR", true),
        ("pr", "diff") => ("eR", true),
        ("pr", "checks") => ("iqtR", true),
        ("issue", "list") => ("aAqlLmSstR", true),
        ("repo", "view") => ("bqt", true),
        ("workflow", "list") => ("qLtR", true),
        ("workflow", "view") => ("rR", true),
        ("search", "repos" | "issues" | "code" | "commits") => ("qLtR", true),
        ("search", "prs") => ("BHqLtR", true),
        _ => return None,
    })
}

/// The letters of a single-dash flag token (`-iXPOST` -> `iXPOST`); `None` for `--long`,
/// `-` and anything that is not a flag.
fn short_cluster(t: &str) -> Option<&str> {
    t.strip_prefix('-')
        .filter(|c| !c.is_empty() && !c.starts_with('-'))
}

/// Short flags of `gh api` that take a value (`--method`, `--raw-field`, `--field`,
/// `--header`, `--preview`, `--jq`, `--template`).
const API_SHORT_VALUE: &str = "XfFHpqt";
/// Boolean short flags of `gh api` (`--include`, `--help`).
const API_SHORT_BOOL: &str = "ih";
/// Long flags of `gh api` that take a value.
const API_LONG_VALUE: [&str; 10] = [
    "method",
    "field",
    "raw-field",
    "header",
    "preview",
    "jq",
    "template",
    "input",
    "hostname",
    "cache",
];
/// Boolean long flags of `gh api`.
const API_LONG_BOOL: [&str; 6] = ["include", "paginate", "slurp", "silent", "verbose", "help"];

/// `gh api ...`: the highest class any method or field flag anywhere in the arguments implies.
/// Every token is checked on its own, including ones that may be another flag's value, so a
/// value cannot hide a write (a value that only looks like a write makes the call stricter).
/// A single-dash token is read letter by letter the way gh's flag parser reads it (`-iXPOST`
/// is `--include --method POST`): a letter that takes a value ends the token, its value being
/// the rest of the token or the next token. An unknown letter or long flag fails closed
/// (Account).
fn api_class(rest: &[String]) -> Class {
    let mut class = Class::Read;
    for (i, t) in rest.iter().enumerate() {
        let next = || rest.get(i + 1).map(String::as_str);
        if let Some(long) = t.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            match name {
                // A missing value fails closed.
                "method" => class = class.max(method_class(inline.or(next()).unwrap_or("?"))),
                "field" | "raw-field" | "input" => class = class.max(Class::Account),
                n if API_LONG_VALUE.contains(&n) || API_LONG_BOOL.contains(&n) => {}
                _ => class = class.max(Class::Account),
            }
            continue;
        }
        let Some(cluster) = short_cluster(t) else {
            continue;
        };
        for (j, c) in cluster.char_indices() {
            if API_SHORT_BOOL.contains(c) {
                continue;
            }
            match c {
                'X' => {
                    let tail = &cluster[j + 1..];
                    let m = if tail.is_empty() {
                        next().unwrap_or("?")
                    } else {
                        tail.strip_prefix('=').unwrap_or(tail)
                    };
                    class = class.max(method_class(m));
                }
                'f' | 'F' => class = class.max(Class::Account),
                c if API_SHORT_VALUE.contains(c) => {}
                _ => class = class.max(Class::Account),
            }
            break;
        }
    }
    class
}

/// The host a `gh api` call reaches when it names one: a full-URL endpoint (`https://...`)
/// or `--hostname`. Only GitHub API hosts are allowed: `api.github.com` over https (or
/// `github.com` as `--hostname`) and the configured GHE hosts. Anything else, a scheme other
/// than https, or an endpoint starting with `//` is refused, so gh cannot be used to fetch an
/// arbitrary page (rule 5) under the wrong gate domain (rule 4).
///
/// Tokens that are the value of a known value-taking flag (`-f body=https://...`) are not
/// endpoints; after an unknown flag the next token is still checked (fails closed).
/// `quiet` (inside an alias's expansion, D-596): the refusal does not quote the token.
fn api_host(
    rest: &[String],
    ghe_hosts: &[String],
    quiet: bool,
) -> Result<Option<String>, ToolError> {
    let ghe = |h: &str| ghe_hosts.iter().any(|g| g.eq_ignore_ascii_case(h));
    let refuse = |what: &str| {
        ToolError::Args(format!(
            "gh api{}: gh api only reaches GitHub API hosts (https://{GITHUB_API}{}); \
             use a path like repos/o/r, or the web tools for other sites",
            quoted(what, quiet),
            if ghe_hosts.is_empty() {
                String::new()
            } else {
                format!(" or {}", ghe_hosts.join(", "))
            }
        ))
    };
    let mut host: Option<String> = None;
    let mut url_host: Option<String> = None;
    let mut positional_only = false;
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i].as_str();
        i += 1;
        if !positional_only {
            if t == "--" {
                positional_only = true;
                continue;
            }
            if let Some(long) = t.strip_prefix("--") {
                let (name, inline) = match long.split_once('=') {
                    Some((n, v)) => (n, Some(v.to_string())),
                    None => (long, None),
                };
                let takes_value = API_LONG_VALUE.contains(&name);
                let value = match inline {
                    Some(v) => Some(v),
                    None if takes_value => {
                        i += 1;
                        rest.get(i - 1).cloned()
                    }
                    None => None,
                };
                if name == "hostname" {
                    let h = value.unwrap_or_default();
                    let h = h.trim();
                    if !(h.eq_ignore_ascii_case(GITHUB) || ghe(h)) {
                        return Err(refuse(&format!("--hostname {h}")));
                    }
                    host = Some(h.to_ascii_lowercase());
                }
                continue;
            }
            if let Some(cluster) = short_cluster(t) {
                for (j, c) in cluster.char_indices() {
                    if API_SHORT_BOOL.contains(c) {
                        continue;
                    }
                    if API_SHORT_VALUE.contains(c) && cluster[j + 1..].is_empty() {
                        // The value is the next token.
                        i += 1;
                    }
                    break;
                }
                continue;
            }
        }
        if t.starts_with("//") {
            return Err(refuse(t));
        }
        if t.contains("://") {
            let h = url::Url::parse(t)
                .ok()
                .filter(|u| u.scheme() == "https" && u.username().is_empty())
                .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
                .filter(|h| h == GITHUB_API || ghe(h))
                .ok_or_else(|| refuse(t))?;
            // api.github.com is github.com's API: one gate domain per host, so a user rule
            // on github.com also covers the full-URL form (the gate matches domains exactly).
            let h = if h == GITHUB_API {
                GITHUB.to_string()
            } else {
                h
            };
            url_host.get_or_insert(h);
        }
    }
    Ok(url_host.or(host))
}

/// The host a non-`api` command names through `-R/--repo [HOST/]OWNER/REPO` or a repo URL
/// argument (`repo view https://host/o/r`). gh sends such a call (and, with
/// `GH_ENTERPRISE_TOKEN` set, a credential) to that host, so it must be github.com or a
/// configured GHE host, and it becomes the gate domain; any other host is refused.
///
/// Fails closed: every token is checked, so a `HOST/OWNER/REPO`-shaped search query or a
/// URL in a free-text argument is refused too. `quiet` as for [`api_host`].
fn repo_host(
    rest: &[String],
    ghe_hosts: &[String],
    quiet: bool,
) -> Result<Option<String>, ToolError> {
    let refuse = |what: &str| {
        ToolError::Args(format!(
            "gh{}: gh only reaches github.com{}; use OWNER/REPO, or the web tools for other \
             sites",
            quoted(what, quiet),
            if ghe_hosts.is_empty() {
                String::new()
            } else {
                format!(" or {}", ghe_hosts.join(", "))
            }
        ))
    };
    let allowed = |h: &str| -> Option<String> {
        let h = h.trim().trim_end_matches('.').to_ascii_lowercase();
        if h == GITHUB || h == "www.github.com" {
            Some(GITHUB.to_string())
        } else if ghe_hosts.iter().any(|g| g.eq_ignore_ascii_case(&h)) {
            Some(h)
        } else {
            None
        }
    };
    let mut found: Option<String> = None;
    for t in rest {
        let v = if let Some(v) = t.strip_prefix("--repo=") {
            v
        } else if let Some(v) = t.strip_prefix("-R").filter(|v| !v.is_empty()) {
            v.trim_start_matches('=')
        } else if let Some((_, v)) = short_cluster(t).and_then(|c| c.split_once('R')) {
            // `-cRHOST/o/r`: gh reads the letters before R as flags and the rest as the
            // repo (`gh pr view -cR127.0.0.1:9/o/r 5` goes to that host). A letter before
            // R may take a value itself, so this can only be stricter than gh.
            v.trim_start_matches('=')
        } else {
            t.as_str()
        };
        let host = if v.contains("://") || v.starts_with("//") {
            let h = url::Url::parse(v)
                .ok()
                .filter(|u| u.username().is_empty())
                .and_then(|u| u.host_str().map(str::to_string))
                .and_then(|h| allowed(&h))
                .ok_or_else(|| refuse(t))?;
            Some(h)
        } else {
            // HOST/OWNER/REPO: a first segment with a dot or a port names a host.
            let segs: Vec<&str> = v.split('/').collect();
            match segs.first() {
                Some(first)
                    if segs.len() >= 3
                        && !first.starts_with('-')
                        && (first.contains('.') || first.contains(':')) =>
                {
                    Some(allowed(first).ok_or_else(|| refuse(t))?)
                }
                _ => None,
            }
        };
        if let Some(h) = host {
            if found.as_ref().is_some_and(|f| f != &h) {
                return Err(refuse(t));
            }
            found = Some(h);
        }
    }
    Ok(found)
}

/// ` "token"` for a host refusal, or nothing when `quiet`.
fn quoted(what: &str, quiet: bool) -> String {
    if quiet {
        " (a host in an alias's expansion)".to_string()
    } else {
        format!(" {:?}", text::line(what, 200))
    }
}

/// GET and HEAD read, DELETE deletes, anything else (POST, PUT, PATCH, unknown) is Account.
fn method_class(m: &str) -> Class {
    match m.trim().to_ascii_uppercase().as_str() {
        "GET" | "HEAD" => Class::Read,
        "DELETE" => Class::Delete,
        _ => Class::Account,
    }
}

/// `run view ... --log` or `--log-failed`.
fn is_run_log(argv: &[String]) -> bool {
    argv.first().map(String::as_str) == Some("run")
        && argv.get(1).map(String::as_str) == Some("view")
        && argv[2..]
            .iter()
            .any(|a| a == "--log" || a == "--log-failed")
}

/// `gh-run-<id>[-job-<job>].log`; ids keep ASCII digits only.
fn log_file_name(argv: &[String]) -> String {
    let digits = |s: &str| -> Option<String> {
        (!s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.to_string())
    };
    let mut id = None;
    let mut job = None;
    let mut i = 2;
    while i < argv.len() {
        let a = argv[i].as_str();
        if a == "--job" || a == "-j" {
            job = argv.get(i + 1).and_then(|j| digits(j));
            i += 2;
            continue;
        }
        if let Some(j) = a.strip_prefix("--job=") {
            job = digits(j);
        } else if id.is_none() && !a.starts_with('-') {
            id = digits(a);
        }
        i += 1;
    }
    let mut name = format!("gh-run-{}", id.unwrap_or_else(|| "latest".into()));
    if let Some(j) = job {
        name.push_str(&format!("-job-{j}"));
    }
    name.push_str(".log");
    name
}

impl Gh {
    /// The user's gh aliases when `argv` may name one ([`needs_aliases`]), from
    /// `gh alias list`. Fails closed: when they cannot be read the call is refused, because
    /// an unknown word may be an alias for anything (D-591).
    async fn aliases(&self, argv: &[String]) -> Result<Aliases, ToolError> {
        if !needs_aliases(argv) {
            return Ok(Aliases::default());
        }
        let mut full = self.cfg.gh.clone();
        full.extend(["alias".to_string(), "list".to_string()]);
        let timeout = self.cfg.proc_timeout.min(ALIAS_TIMEOUT);
        let out = proc::run(&full, &GH_ENV, timeout, ALIAS_MAX_BYTES, "gh").await?;
        let unreadable = |why: String| {
            ToolError::Failed(format!(
                "gh {}: cannot tell whether this names a gh alias ({why}); nothing ran",
                text::line(&argv::show(&argv[..argv.len().min(2)]), 64)
            ))
        };
        if !out.success() {
            if out.stderr_text().contains("no aliases configured") {
                return Ok(Aliases::default());
            }
            return Err(unreadable(format!("gh alias list: {}", out.status_line())));
        }
        if out.stdout_cut() {
            return Err(unreadable("gh alias list printed too much".into()));
        }
        Aliases::parse(&out.stdout_text())
            .map_err(|e| unreadable(format!("gh alias list output: {e}")))
    }

    /// [`plan`] with this server's GHE hosts and the user's aliases.
    async fn plan(&self, argv: &[String]) -> Result<GhPlan, ToolError> {
        let aliases = self.aliases(argv).await?;
        plan(argv, &self.cfg.gh_hosts, &aliases)
    }

    fn planned(&self) -> std::sync::MutexGuard<'_, VecDeque<Planned>> {
        // A panic elsewhere cannot leave a half-written entry: push and remove are whole.
        self.planned.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Keeps classify's plan for this call, replacing one an earlier context at the same
    /// address left (its call never came).
    fn remember(&self, ctx: &ToolCtx, named: &[String], plan: &GhPlan) {
        let key = ctx_key(ctx);
        let mut q = self.planned();
        q.retain(|p| p.ctx != key);
        q.push_back(Planned {
            ctx: key,
            run: ctx.run_id.0.clone(),
            named: named.to_vec(),
            plan: plan.clone(),
        });
        while q.len() > MAX_PLANNED {
            q.pop_front();
        }
    }

    /// Classify's plan for this call, taken out.
    fn take_planned(&self, ctx: &ToolCtx) -> Option<Planned> {
        let key = ctx_key(ctx);
        let mut q = self.planned();
        let i = q.iter().position(|p| p.ctx == key)?;
        q.remove(i)
    }
}

#[async_trait]
impl ToolHandler for Gh {
    async fn classify(&self, ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let argv = gh_argv(args)?;
        let p = self.plan(&argv).await?;
        self.remember(ctx, &argv, &p);
        let shown = text::line(&argv::show(&argv), 300);
        // An alias names the command it runs by its first two words only: an expansion
        // can hold anything the user typed.
        let what = match &p.alias {
            Some(a) => format!(
                "run gh {shown} (alias {} for gh {} ...)",
                text::line(a, 64),
                command_words(&p.argv)
            ),
            None => format!("run gh {shown}"),
        };
        Ok(Classified {
            class: Some(p.call.class),
            domain: Some(p.call.domain),
            what: Some(what),
            values: vec![("args".into(), shown)],
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let named = gh_argv(&args)?;
        let seen = self.take_planned(ctx);
        // Planned again: the aliases may have changed since classify. What runs is the
        // planned argument vector (an alias's expansion), never the alias itself, and only
        // when it is exactly what classify planned: the same words on the same host at the
        // same class (D-595).
        let p = self.plan(&named).await?;
        let same = seen.is_some_and(|s| s.run == ctx.run_id.0 && s.named == named && s.plan == p);
        if p.call.class > ctx.class {
            return Err(ToolError::Failed(
                "gh: the call's class is above what the gate approved; not run".into(),
            ));
        }
        if !same {
            return Err(ToolError::Failed(
                "gh: what this call runs changed after it was classified (a gh alias was set \
                 or changed); nothing ran. Call again to classify it as it is now."
                    .into(),
            ));
        }
        let argv = p.argv;
        let mut full = self.cfg.gh.clone();
        full.extend(argv.iter().cloned());
        let log = is_run_log(&argv);
        let max = if log { LOG_MAX_BYTES } else { OUT_MAX_BYTES };
        let out = proc::run(&full, &GH_ENV, self.cfg.proc_timeout, max, "gh").await?;
        if !out.success() {
            let err = out.stderr_text();
            let err = if err.trim().is_empty() {
                out.stdout_text()
            } else {
                err
            };
            let err = html_page_note(&err).unwrap_or(err);
            return Err(ToolError::Failed(format!(
                "gh {} ({}): {}",
                text::line(&argv::show(&named), 200),
                out.status_line(),
                text::capped(err.trim(), ERR_CHARS, "")
            )));
        }
        if log {
            return save_log(ctx, &argv, &out).await;
        }
        let stdout = out.stdout_text();
        let html_page = argv
            .first()
            .filter(|g| *g == "api")
            .and_then(|_| html_page_note(&stdout));
        let mut t = if stdout.trim().is_empty() {
            "(no output)".to_string()
        } else if let Some(note) = html_page {
            note
        } else {
            text::capped(
                stdout.trim_end(),
                text::MAX_TEXT_CHARS,
                "narrow the call (--limit, --json fields, --jq)",
            )
        };
        if out.stdout_cut() && !t.contains("\ntruncated:") {
            t.push_str(&format!(
                "\ntruncated: gh wrote {} bytes, kept {}",
                out.stdout_total,
                out.stdout.len()
            ));
        }
        let stderr = out.stderr_text();
        if !stderr.trim().is_empty() {
            t.push_str("\nstderr: ");
            t.push_str(&text::line(stderr.trim(), 500));
        }
        Ok(ToolOutput {
            text: t,
            summary: Some(format!("gh {}", text::line(&argv::show(&named), 200))),
            ..ToolOutput::default()
        })
    }
}

/// What the model reads instead of `out` when it is a whole HTML page (rule 5, D-593):
/// `gh api` prints whatever the host answered (a GHE login or proxy page), and a failing
/// call may print it as its error. Only a document counts here, not a few tags, because gh
/// output may quote some (a PR body's `<details>`). JSON is never a page.
fn html_page_note(out: &str) -> Option<String> {
    if crate::http::is_json(out) || !text::looks_like_html_document(out) {
        return None;
    }
    Some(format!(
        "the answer is an HTML page ({} bytes{}), not shown (to read a public page as text \
         use the web tools' fetch)",
        out.len(),
        text::html_title(out, 120)
            .map(|t| format!("; page title \"{t}\""))
            .unwrap_or_default()
    ))
}

/// Saves the scrubbed log to `files/` and returns the first failing step.
async fn save_log(
    ctx: &ToolCtx,
    argv: &[String],
    out: &proc::ProcOut,
) -> Result<ToolOutput, ToolError> {
    // The wrapper scrubs what it returns, not files: scrub before the file is written (H-2).
    let log = harness_core::vault::scrub(&out.stdout_text());
    let name = log_file_name(argv);
    let dir = ctx.run_dir.join("files");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ToolError::Failed(format!("gh: cannot create files/: {e}")))?;
    let path = dir.join(&name);
    tokio::fs::write(&path, log.as_bytes())
        .await
        .map_err(|e| ToolError::Failed(format!("gh: cannot save files/{name}: {e}")))?;
    let shown = format!("files/{name}");
    let mut t = log::summary(&log, &shown);
    if out.stdout_cut() {
        t.push_str(&format!(
            "\ntruncated: the log was {} bytes; kept the first {}",
            out.stdout_total,
            out.stdout.len()
        ));
    }
    let summary = match log::first_failing_step(&log) {
        Some(f) => format!("first failing step: {} / {}", f.job, f.step),
        None => "no failing step in the log".into(),
    };
    Ok(ToolOutput {
        text: t,
        summary: Some(summary),
        consequences: json!({ "saved": shown }),
        files: vec![path],
        stopped: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> Result<Class, ToolError> {
        classify_args(&argv::split(s).unwrap())
    }

    #[test]
    fn classification_table() {
        let table: &[(&str, Class)] = &[
            ("run list", Class::Read),
            ("run view 123 --log", Class::Read),
            ("run view 123 --log-failed", Class::Read),
            ("pr view 5", Class::Read),
            ("pr checks 5", Class::Read),
            ("issue list", Class::Read),
            ("repo view o/r", Class::Read),
            ("search repos tokio", Class::Read),
            ("api repos/o/r", Class::Read),
            ("api -X GET repos/o/r", Class::Read),
            ("api --method=head repos/o/r", Class::Read),
            ("pr view 5 --web", Class::Navigate),
            ("run view 1 -w", Class::Navigate),
            ("api -X POST repos/o/r/issues", Class::Account),
            ("api -XPUT repos/o/r/x", Class::Account),
            ("api --method PATCH repos/o/r", Class::Account),
            ("api --method=post repos/o/r", Class::Account),
            ("api -X GET repos/o/r -f a=b", Class::Account),
            ("api repos/o/r -F a=1", Class::Account),
            ("api repos/o/r --raw-field a=b", Class::Account),
            ("api repos/o/r --input body.json", Class::Account),
            ("api repos/o/r -fa=b", Class::Account),
            ("api -X", Class::Account),
            ("api -X POST repos/o/r -H -XGET", Class::Account),
            ("api -X OPTIONS x", Class::Account),
            ("api -X DELETE repos/o/r", Class::Delete),
            ("pr merge 5", Class::Account),
            ("release list", Class::Account),
            ("release create v1", Class::Account),
            ("workflow run ci.yml", Class::Account),
            ("run rerun 5", Class::Account),
            ("secret set X", Class::Account),
            ("my-extension go", Class::Account),
            ("--version", Class::Account),
            ("repo delete o/r --yes", Class::Delete),
            ("run delete 5", Class::Delete),
            ("release delete v1", Class::Delete),
            // Clustered short flags, read the way gh's flag parser reads them.
            ("api -iXPOST repos/o/r/issues", Class::Account),
            ("api -iX POST repos/o/r", Class::Account),
            ("api -iX=PATCH repos/o/r", Class::Account),
            ("api repos/o/r/issues -iftitle=x", Class::Account),
            ("api repos/o/r -iF a=1", Class::Account),
            ("api -iXDELETE repos/o/r", Class::Delete),
            ("api -iX", Class::Account),
            ("api -iXGET repos/o/r", Class::Read),
            ("api -i repos/o/r", Class::Read),
            ("api -iq .name repos/o/r", Class::Read),
            ("api -H Accept:x -i repos/o/r", Class::Read),
            ("api --paginate --jq .[] repos/o/r", Class::Read),
            ("api -iz repos/o/r", Class::Account),
            ("api -XGET -fa=b repos/o/r", Class::Account),
            ("api --bogus repos/o/r", Class::Account),
            ("api --method", Class::Account),
            ("pr view 5 -cw", Class::Navigate),
            ("pr view 5 -wc", Class::Navigate),
            ("run view 1 --web=true", Class::Navigate),
            ("pr view 5 -c", Class::Read),
        ];
        for (cmd, want) in table {
            assert_eq!(c(cmd).unwrap(), *want, "gh {cmd}");
        }
    }

    #[test]
    fn auth_config_and_empty_are_refused() {
        for bad in [
            "auth token",
            "auth status",
            "config get oauth_token -h github.com",
            "config list",
            "config set editor vim",
            "",
        ] {
            assert!(matches!(c(bad), Err(ToolError::Args(_))), "gh {bad}");
        }
    }

    #[test]
    fn repo_hosts_are_github_hosts_only() {
        let w = |s: &str, hosts: &[&str]| {
            let hosts: Vec<String> = hosts.iter().map(|h| h.to_string()).collect();
            classify_with(&argv::split(s).unwrap(), &hosts)
        };
        for bad in [
            "pr view -R any.host/o/r 5",
            "pr view -Rany.host/o/r 5",
            "issue list --repo any.host/o/r",
            "issue list --repo=any.host/o/r",
            "repo view https://any.host/o/r",
            "repo view //any.host/o/r",
            "repo view https://user@github.com/o/r",
            "run list -R 127.0.0.1:9/o/r",
            "pr view -R github.com.evil.example/o/r 5",
            "pr view -R github.com/o/r --repo ghe.corp/o/r 5",
        ] {
            assert!(matches!(w(bad, &[]), Err(ToolError::Args(_))), "gh {bad}");
        }
        let ok = |s: &str, hosts: &[&str], domain: &str| {
            assert_eq!(w(s, hosts).unwrap().domain, domain, "gh {s}");
        };
        ok("pr view -R o/r 5", &[], "github.com");
        ok("pr view -R github.com/o/r 5", &[], "github.com");
        ok("repo view https://github.com/o/r", &[], "github.com");
        ok("issue list --repo ghe.corp/o/r", &["ghe.corp"], "ghe.corp");
        ok("run view 123 --log", &[], "github.com");
    }

    #[test]
    fn api_hosts_are_github_api_hosts_only() {
        let w = |s: &str, hosts: &[&str]| {
            let hosts: Vec<String> = hosts.iter().map(|h| h.to_string()).collect();
            classify_with(&argv::split(s).unwrap(), &hosts)
        };
        for bad in [
            "api https://any.host/page",
            "api HTTPS://any.host/page",
            "api //any.host/page",
            "api http://api.github.com/repos/o/r",
            "api https://github.com/o/r",
            "api https://user@api.github.com/x",
            "api https://api.github.com.evil.example/x",
            "api -H x https://any.host/page",
            "api --bogus https://any.host/page",
            "api -iz https://any.host/page",
            "api repos/o/r -- https://any.host/page",
            "api 'search/code?q=https://any.host'",
            "api --hostname any.host repos/o/r",
            "api --hostname=any.host repos/o/r",
            "api --hostname",
        ] {
            assert!(matches!(w(bad, &[]), Err(ToolError::Args(_))), "gh {bad}");
        }
        assert!(matches!(
            w("api https://ghe.corp/api/v3/x", &[]),
            Err(ToolError::Args(_))
        ));
        let ok = |s: &str, hosts: &[&str], class: Class, domain: &str| {
            assert_eq!(
                w(s, hosts).unwrap(),
                GhCall {
                    class,
                    domain: domain.into()
                },
                "gh {s}"
            );
        };
        ok("api repos/o/r", &[], Class::Read, "github.com");
        ok("run list", &[], Class::Read, "github.com");
        ok(
            "api https://api.github.com/repos/o/r",
            &[],
            Class::Read,
            "github.com",
        );
        // One gate domain per host: the full-URL and path forms of a write gate the same.
        ok(
            "api https://api.github.com/repos/o/r/issues -X POST",
            &[],
            Class::Account,
            "github.com",
        );
        ok(
            "api -X POST repos/o/r/issues -f body=see-https://any.host/x",
            &[],
            Class::Account,
            "github.com",
        );
        ok(
            "api -iH x:https://any.host repos/o/r",
            &[],
            Class::Read,
            "github.com",
        );
        ok(
            "api --hostname github.com repos/o/r",
            &[],
            Class::Read,
            "github.com",
        );
        ok(
            "api --hostname=GHE.corp repos/o/r",
            &["ghe.corp"],
            Class::Read,
            "ghe.corp",
        );
        ok(
            "api https://ghe.corp/api/v3/repos/o/r -X DELETE",
            &["ghe.corp"],
            Class::Delete,
            "ghe.corp",
        );
    }

    /// T-15b item 3 (D-592): a flag's value is not read as `-w` or as the subcommand, and
    /// every real write keeps (or gains) its class.
    #[test]
    fn flag_values_are_not_subcommands_or_web() {
        let table: &[(&str, Class)] = &[
            // False positives fixed: the value of -R, and run list's -w (--workflow).
            ("pr view 5 -Rw/x", Class::Read),
            ("issue list -Lw", Class::Read),
            ("run list -w ci.yml", Class::Read),
            ("run list -bw x", Class::Read),
            ("pr -R o/r view 5", Class::Read),
            ("pr --repo=o/r view 5", Class::Read),
            ("run -Ro/r list", Class::Read),
            // True positives next to them.
            ("pr view 5 -w", Class::Navigate),
            ("pr view 5 -cw", Class::Navigate),
            ("pr list -dw", Class::Navigate),
            ("pr view 5 -R o/r -w", Class::Navigate),
            ("pr view 5 -R -w", Class::Navigate),
            ("run list --web", Class::Navigate),
            ("pr merge 5 -Rw/x", Class::Account),
            ("pr view 5 -zw", Class::Navigate),
            // Flags before the subcommand: the subcommand still counts (was Account).
            ("run -R o/r delete 5", Class::Delete),
            ("release --repo o/r delete v1 --yes", Class::Delete),
            ("run --repo=o/r delete 5", Class::Delete),
            // An unknown flag first: any later word may be the subcommand, at least Account.
            ("run --bogus delete 5", Class::Delete),
            ("run --bogus x delete", Class::Delete),
            ("pr --bogus view 5", Class::Account),
            ("pr -c view 5", Class::Account),
            ("pr -R o/r", Class::Account),
        ];
        for (cmd, want) in table {
            assert_eq!(c(cmd).unwrap(), *want, "gh {cmd}");
        }
        // An R inside a cluster names a repo, and its host is checked.
        for bad in [
            "pr view -cR127.0.0.1:9/o/r 5",
            "pr view -cR=any.host/o/r 5",
            "issue list -qRany.host/o/r",
        ] {
            assert!(matches!(c(bad), Err(ToolError::Args(_))), "gh {bad}");
        }
        let hosts = vec!["ghe.corp".to_string()];
        let call = classify_with(&argv::split("pr view -cRghe.corp/o/r 5").unwrap(), &hosts);
        assert_eq!(call.unwrap().domain, "ghe.corp");
    }

    const ALIASES: &str = "tok: auth token\ncfg: config get oauth_token\nrmr: repo delete\n\
v: pr view\nrl: run list\nsh1: '!echo hi'\nml: |-\n    pr\n    view\n\
issue mine: issue list --mention @me\nq: api -X \"$1\" repos/o/r\nnest: tok\nloop: loop\n\
far: api https://any.host/x\nt2: v\nrd: --repo=o/r run delete\nlead: --repo o/r --bogus run delete\n\
fr: pr view -R secret.host/o/r\nfa: api --hostname secret.host repos/o/r\n\
as1: alias set zz9 'auth token'\nas2: alias set --bogus x 'pr view'\n";

    fn p(s: &str) -> Result<GhPlan, ToolError> {
        plan(
            &argv::split(s).unwrap(),
            &[],
            &Aliases::parse(ALIASES).unwrap(),
        )
    }

    /// T-15b item 2 (D-591): an alias runs as its expansion, at the expansion's class.
    #[test]
    fn aliases_run_as_their_expansion() {
        let ok = |s: &str, class: Class, runs: &str| {
            let got = p(s).unwrap_or_else(|e| panic!("gh {s}: {e}"));
            assert_eq!(got.call.class, class, "gh {s}");
            assert_eq!(got.argv, argv::split(runs).unwrap(), "gh {s}");
            assert!(got.alias.is_some(), "gh {s}");
        };
        ok("rmr o/r --yes", Class::Delete, "repo delete o/r --yes");
        ok("v 5", Class::Read, "pr view 5");
        ok("v 5 -w", Class::Navigate, "pr view 5 -w");
        ok("t2 5", Class::Read, "pr view 5");
        ok(
            "issue mine -L 5",
            Class::Read,
            "issue list --mention @me -L 5",
        );
        ok("q DELETE", Class::Delete, "api -X DELETE repos/o/r");
        ok("q GET", Class::Read, "api -X GET repos/o/r");
        for bad in [
            "tok",
            "cfg",
            "nest",
            "sh1",
            "ml",
            "q",
            "loop",
            "far",
            "issue -R o/r mine",
        ] {
            assert!(matches!(p(bad), Err(ToolError::Args(_))), "gh {bad}");
        }
        // review-T-15b should-fix 2 (D-596): a refusal inside an alias names the alias and
        // the expansion's command words, never the rest of it (the host, the URL, a flag).
        for (bad, hidden) in [
            ("far", "any.host"),
            ("fr 5", "secret.host"),
            ("fa", "secret.host"),
            ("as1", "zz9"),
            ("as2", "--bogus"),
            ("lead 5", "--bogus"),
        ] {
            let e = p(bad).unwrap_err().to_string();
            assert!(!e.contains(hidden), "gh {bad}: {e}");
            assert!(e.contains(&format!(
                "gh {} is an alias",
                bad.split(' ').next().unwrap()
            )));
        }
        let e = p("fr 5").unwrap_err().to_string();
        assert!(e.contains("is an alias for gh pr view ..."), "{e}");
        let e = p("far").unwrap_err().to_string();
        assert!(e.contains("is an alias for gh api ..."), "{e}");
        // Outside an alias the call's own text is still quoted.
        let e = p("api https://any.host/x").unwrap_err().to_string();
        assert!(e.contains("any.host"), "{e}");
        let words = |s: &str| command_words(&argv::split(s).unwrap());
        assert_eq!(words("pr view 5"), "pr view");
        assert_eq!(words("api https://h/x"), "api");
        assert_eq!(words("issue list --mention @me"), "issue list");
        assert_eq!(words("--repo=o/r run delete"), "(a command)");
        assert_eq!(words("a.b c"), "(a command)");
        // Not an alias: unchanged.
        let got = p("pr merge 5").unwrap();
        assert_eq!((got.call.class, got.alias), (Class::Account, None));
        // Which calls look aliases up.
        let needs = |s: &str| needs_aliases(&argv::split(s).unwrap());
        for no in [
            "run list",
            "pr view 5",
            "api repos/o/r",
            "auth token",
            "status",
        ] {
            assert!(!needs(no), "{no}");
        }
        for yes in [
            "tok",
            "pr merge 5",
            "issue mine",
            "pr --bogus view 5",
            "alias set a b",
        ] {
            assert!(needs(yes), "{yes}");
        }
    }

    /// review-T-15b Blocking (D-594): gh hands a flag before the group to the command, so
    /// it is moved behind the group and classified there, or refused.
    #[test]
    fn a_flag_before_the_group_is_read_where_gh_reads_it() {
        let ok = |s: &str, class: Class, runs: &str| {
            let got = p(s).unwrap_or_else(|e| panic!("gh {s}: {e}"));
            assert_eq!(got.call.class, class, "gh {s}");
            assert_eq!(got.argv, argv::split(runs).unwrap(), "gh {s}");
        };
        ok(
            "--repo=o/r run delete 5",
            Class::Delete,
            "run --repo=o/r delete 5",
        );
        ok("-R o/r run delete 5", Class::Delete, "run -R o/r delete 5");
        ok(
            "--yes=true repo delete o/r",
            Class::Delete,
            "repo --yes=true delete o/r",
        );
        ok("--repo o/r pr view 5", Class::Read, "pr --repo o/r view 5");
        ok(
            "--repo=o/r pr merge 5",
            Class::Account,
            "pr --repo=o/r merge 5",
        );
        // An alias's expansion that starts with a flag is read the same way.
        ok("rd 5", Class::Delete, "run --repo=o/r delete 5");
        // A lone --help or --version stays as it is.
        ok("--help", Class::Account, "--help");
        ok("--version", Class::Account, "--version");
        for bad in [
            // The auth refusal, the alias lookup and the host check still apply.
            "--hostname github.com auth token",
            "--hostname=github.com auth token",
            "--hostname github.com tok",
            "-R o/r tok",
            "--repo=other.host/o/r pr view 5",
            "-R other.host/o/r run delete 5",
            // Forms gh's parser could read another way, and calls that are only flags.
            "--bogus run delete 5",
            "-y repo delete o/r",
            "-Ro/r run delete 5",
            "--help run delete 5",
            "-- run delete 5",
            "-R o/r",
            "--repo=o/r",
            "lead 5",
        ] {
            assert!(matches!(p(bad), Err(ToolError::Args(_))), "gh {bad}");
        }
        // Without the alias list too, and the refusal says where the flags go.
        let e = c("--bogus run delete 5").unwrap_err().to_string();
        assert!(e.contains("put the flags after the subcommand"), "{e}");
        assert!(c("--hostname github.com auth token").is_err());
        assert_eq!(c("--repo=o/r run delete 5").unwrap(), Class::Delete);
        // Inside an alias the flag is not named (D-591: two words of an expansion only).
        let e = p("lead 5").unwrap_err().to_string();
        assert!(!e.contains("--bogus"), "{e}");
        // A flag first is looked up after the move: the alias is found.
        assert!(needs_aliases(
            &group_first(&argv::split("-R o/r tok").unwrap(), false).unwrap()
        ));
    }

    /// Review T-15b round 2 (D-598): gh drops empty words, so none reaches it.
    #[test]
    fn empty_words_are_refused_anywhere() {
        for bad in [
            "'' auth token",
            "auth '' token",
            "'' config get oauth_token",
            "'' tok",
            "'' repo delete --yes",
            "repo '' delete --yes",
            "--repo=o/r '' run delete 5",
            "'' api --hostname other.host /x",
            "pr view 5 ''",
        ] {
            let argv = argv::split(bad).unwrap();
            assert!(
                argv.iter().any(String::is_empty),
                "fixture has an empty word: {bad}"
            );
            let e = p(bad).unwrap_err().to_string();
            assert!(e.contains("empty word"), "gh {bad}: {e}");
            assert!(c(bad).is_err(), "gh {bad} without the alias list");
        }
        // The empty word inside an alias's expansion: `alias set` and running it.
        let e = p("alias set e \"'' auth token\"").unwrap_err().to_string();
        assert!(e.contains("empty word"), "{e}");
        assert!(c("alias set e \"'' auth token\"").is_err());
        // An empty flag value written in one word still works.
        assert_eq!(c("pr view 5 --jq=").unwrap(), Class::Read);
    }

    /// Review T-15b round 2: gh's aliases of delete are a Delete too.
    #[test]
    fn remove_and_rm_are_deletes() {
        for call in [
            "secret remove TOKEN",
            "variable remove X",
            "secret rm TOKEN",
        ] {
            assert_eq!(c(call).unwrap(), Class::Delete, "gh {call}");
        }
    }

    /// T-15b item 2 (D-591): `alias set` is at the class of what the alias would run.
    #[test]
    fn alias_set_is_classified_by_its_expansion() {
        let refused = [
            "alias set tok2 'auth token'",
            "alias set c2 'config list'",
            "alias set n2 tok",
            "alias set far2 'api https://any.host/x'",
            "alias set --shell s 'echo hi'",
            "alias set -s s 'echo hi'",
            "alias set s '!echo hi'",
            "alias set x -",
            "alias set pv",
            "alias set --bogus pv 'pr view'",
            "alias set -- a 'auth token'",
        ];
        for bad in refused {
            assert!(matches!(p(bad), Err(ToolError::Args(_))), "gh {bad}");
        }
        // Without the alias list too.
        assert!(c("alias set tok2 'auth token'").is_err());
        let table: &[(&str, Class)] = &[
            ("alias set rm2 'repo delete'", Class::Delete),
            ("alias set rm3 rmr", Class::Delete),
            ("alias set qq 'api -X DELETE \"repos/$1\"'", Class::Delete),
            ("alias set qq 'api -X $1 repos/o/r'", Class::Account),
            ("alias set pv 'pr view'", Class::Account),
            ("alias set --clobber pv 'pr view'", Class::Account),
            ("alias set m 'pr merge'", Class::Account),
        ];
        for (cmd, want) in table {
            assert_eq!(p(cmd).unwrap().call.class, *want, "gh {cmd}");
        }
        assert_eq!(c("alias delete pv").unwrap(), Class::Delete);
        assert_eq!(c("alias list").unwrap(), Class::Account);
        // review-T-15b should-fix 4 (D-597): `alias import` stores unchecked aliases, and a
        // flag before `set` or `import` would hide them from the check: refused.
        for bad in [
            "alias import x.yml",
            "alias import -",
            "alias import --clobber x.yml",
            "alias -R o/r import x.yml",
            "-R o/r alias import x.yml",
            "alias --bogus import x.yml",
            "alias --bogus set tok2 'auth token'",
            "alias -c set pv 'pr view'",
        ] {
            assert!(matches!(c(bad), Err(ToolError::Args(_))), "gh {bad}");
            assert!(matches!(p(bad), Err(ToolError::Args(_))), "gh {bad}");
        }
        let e = c("alias import x.yml").unwrap_err().to_string();
        assert!(e.contains("gh alias set NAME EXPANSION"), "{e}");
    }

    /// T-15b item 4 (D-593): a whole HTML page from `gh api` is not shown; a PR body that
    /// quotes a few tags, and JSON, are.
    #[test]
    fn html_pages_from_gh_are_not_shown() {
        let page = "<!DOCTYPE html><html><head><title>Sign in</title></head><body>x</body>";
        let note = html_page_note(page).expect("a page");
        assert!(
            !note.contains('<') && note.contains("page title \"Sign in\""),
            "{note}"
        );
        assert!(html_page_note("<details><summary>a</summary><br></details>").is_none());
        assert!(html_page_note(r#"{"body":"<html><body>x</body></html>"}"#).is_none());
        assert!(html_page_note("plain").is_none());
    }

    #[test]
    fn log_file_names() {
        let n = |s: &str| log_file_name(&argv::split(s).unwrap());
        assert_eq!(n("run view 123 --log"), "gh-run-123.log");
        assert_eq!(n("run view --log 123 --job 9"), "gh-run-123-job-9.log");
        assert_eq!(n("run view ../../x --log"), "gh-run-latest.log");
        assert_eq!(n("run view --log --job=77"), "gh-run-latest-job-77.log");
    }
}
