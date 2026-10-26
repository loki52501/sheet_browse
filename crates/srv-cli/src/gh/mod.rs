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
//! extensions, aliases, unknown commands) is Account.
//!
//! `run view <id> --log` (or `--log-failed`) saves the whole log, scrubbed, to
//! `runs/<run>/files/gh-run-<id>.log` and returns only the first failing step.

pub mod log;

use crate::{argv, proc, text, Config};
use async_trait::async_trait;
use harness_core::class::Class;
use harness_core::mcp::{Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use serde_json::{json, Value};
use std::sync::Arc;

/// Stdout kept for a run log.
const LOG_MAX_BYTES: usize = 32 * 1024 * 1024;
/// Stdout kept for any other gh call.
const OUT_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Stderr shown on failure, in chars.
const ERR_CHARS: usize = 2_000;

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
        handler: Arc::new(Gh { cfg: cfg.clone() }),
    }]
}

struct Gh {
    cfg: Arc<Config>,
}

/// The gh arguments of a call, without a leading `gh` word.
fn gh_argv(args: &Value) -> Result<Vec<String>, ToolError> {
    let mut v = argv::from_value(args, "args")?;
    if v.first().map(String::as_str) == Some("gh") {
        v.remove(0);
    }
    Ok(v)
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

/// The class of `gh <argv>` with no GHE hosts configured (see [`classify_with`]).
pub fn classify_args(argv: &[String]) -> Result<Class, ToolError> {
    classify_with(argv, &[]).map(|c| c.class)
}

/// The class and gate domain of `gh <argv>` (see the module doc). `ghe_hosts` are the
/// GitHub Enterprise hosts an `api` call may reach besides github.com ([`Config::gh_hosts`]).
/// `Err` for `gh auth`, `gh config`, an empty command, and an `api` call that would reach a
/// host that is not a GitHub API host.
pub fn classify_with(argv: &[String], ghe_hosts: &[String]) -> Result<GhCall, ToolError> {
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
    let sub = rest.first().map(String::as_str);
    let mut domain = GITHUB.to_string();
    if group == "api" {
        if let Some(host) = api_host(rest, ghe_hosts)? {
            domain = host;
        }
    } else if let Some(host) = repo_host(rest, ghe_hosts)? {
        domain = host;
    }
    let class = match (group.as_str(), sub) {
        (_, Some("delete")) => Class::Delete,
        ("api", _) => api_class(rest),
        ("run", Some("list" | "view"))
        | ("pr", Some("view" | "list" | "diff" | "checks" | "status"))
        | ("issue", Some("view" | "list" | "status"))
        | ("repo", Some("view"))
        | ("workflow", Some("list" | "view"))
        | ("status", _) => Class::Read,
        ("search", Some(s)) if !s.starts_with('-') => Class::Read,
        _ => Class::Account,
    };
    let class = if opens_web(rest) {
        class.max(Class::Navigate)
    } else {
        class
    };
    Ok(GhCall { class, domain })
}

/// `--web`, `--web=..`, or a `w` anywhere in a single-dash cluster (`-w`, `-cw`): the call
/// opens a browser. On the read allowlist `-w` is the only short flag that changes the
/// class, so a `w` that is really part of another flag's value (`-Rw`) only makes the call
/// stricter.
fn opens_web(rest: &[String]) -> bool {
    rest.iter().any(|a| {
        a == "--web"
            || a.starts_with("--web=")
            || (short_cluster(a).is_some_and(|c| c.contains('w')))
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
fn api_host(rest: &[String], ghe_hosts: &[String]) -> Result<Option<String>, ToolError> {
    let ghe = |h: &str| ghe_hosts.iter().any(|g| g.eq_ignore_ascii_case(h));
    let refuse = |what: &str| {
        ToolError::Args(format!(
            "gh api {:?}: gh api only reaches GitHub API hosts (https://{GITHUB_API}{}); \
             use a path like repos/o/r, or the web tools for other sites",
            text::line(what, 200),
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
/// URL in a free-text argument is refused too.
fn repo_host(rest: &[String], ghe_hosts: &[String]) -> Result<Option<String>, ToolError> {
    let refuse = |what: &str| {
        ToolError::Args(format!(
            "gh {:?}: gh only reaches github.com{}; use OWNER/REPO, or the web tools for other \
             sites",
            text::line(what, 200),
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

#[async_trait]
impl ToolHandler for Gh {
    async fn classify(&self, _ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let argv = gh_argv(args)?;
        let call = classify_with(&argv, &self.cfg.gh_hosts)?;
        let shown = text::line(&argv::show(&argv), 300);
        Ok(Classified {
            class: Some(call.class),
            domain: Some(call.domain),
            what: Some(format!("run gh {shown}")),
            values: vec![("args".into(), shown)],
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let argv = gh_argv(&args)?;
        if classify_with(&argv, &self.cfg.gh_hosts)?.class > ctx.class {
            return Err(ToolError::Failed(
                "gh: the call's class is above what the gate approved; not run".into(),
            ));
        }
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
            return Err(ToolError::Failed(format!(
                "gh {} ({}): {}",
                text::line(&argv::show(&argv), 200),
                out.status_line(),
                text::capped(err.trim(), ERR_CHARS, "")
            )));
        }
        if log {
            return save_log(ctx, &argv, &out).await;
        }
        let stdout = out.stdout_text();
        let mut t = if stdout.trim().is_empty() {
            "(no output)".to_string()
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
            summary: Some(format!("gh {}", text::line(&argv::show(&argv), 200))),
            ..ToolOutput::default()
        })
    }
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

    #[test]
    fn log_file_names() {
        let n = |s: &str| log_file_name(&argv::split(s).unwrap());
        assert_eq!(n("run view 123 --log"), "gh-run-123.log");
        assert_eq!(n("run view --log 123 --job 9"), "gh-run-123-job-9.log");
        assert_eq!(n("run view ../../x --log"), "gh-run-latest.log");
        assert_eq!(n("run view --log --job=77"), "gh-run-latest-job-77.log");
    }
}
