//! C-1 `gh(args)`: the GitHub CLI, run as an argument vector (no shell).
//!
//! Classification (D-540, fails closed): reads on an allowlist are Read (`run list`,
//! `run view [--log]`, `pr view|list|diff|checks|status`, `issue view|list|status`,
//! `repo view`, `workflow list|view`, `search ...`, `status`, and `api` with GET or HEAD);
//! `--web`/`-w` on a read opens a browser and is Navigate. `api` with `-X`/`--method` POST,
//! PUT, PATCH or any other method is Account, DELETE is Delete, and any `-f`, `-F`,
//! `--field`, `--raw-field` or `--input` makes it Account even with `-X GET`. Any
//! `<group> delete` is Delete. `gh auth` is refused outright: `gh auth token` prints a
//! credential the vault does not know, so the scrubber cannot catch it. Everything else
//! (`pr merge`, `release ...`, `workflow run`, extensions, aliases, unknown commands) is
//! Account.
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

/// The class of `gh <argv>` (see the module doc). `Err` for `gh auth` and an empty command.
pub fn classify_args(argv: &[String]) -> Result<Class, ToolError> {
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
    let sub = argv.get(1).map(String::as_str);
    let class = match (group.as_str(), sub) {
        (_, Some("delete")) => Class::Delete,
        ("api", _) => api_class(&argv[1..]),
        ("run", Some("list" | "view"))
        | ("pr", Some("view" | "list" | "diff" | "checks" | "status"))
        | ("issue", Some("view" | "list" | "status"))
        | ("repo", Some("view"))
        | ("workflow", Some("list" | "view"))
        | ("status", _) => Class::Read,
        ("search", Some(s)) if !s.starts_with('-') => Class::Read,
        _ => Class::Account,
    };
    let web = argv[1..]
        .iter()
        .any(|a| a == "--web" || a == "-w" || a.starts_with("--web="));
    Ok(if web {
        class.max(Class::Navigate)
    } else {
        class
    })
}

/// `gh api ...`: the highest class any method or field flag anywhere in the arguments implies.
/// Every token is checked, including ones that may be another flag's value, so a value
/// cannot hide a write (a value that only looks like a write makes the call stricter).
fn api_class(rest: &[String]) -> Class {
    let mut class = Class::Read;
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i].as_str();
        let method: Option<&str> = if t == "-X" || t == "--method" {
            // A missing value fails closed.
            Some(rest.get(i + 1).map(String::as_str).unwrap_or("?"))
        } else if let Some(m) = t.strip_prefix("--method=") {
            Some(m)
        } else if let Some(m) = t.strip_prefix("-X") {
            (!m.is_empty()).then(|| m.strip_prefix('=').unwrap_or(m))
        } else {
            None
        };
        if let Some(m) = method {
            class = class.max(method_class(m));
        }
        let field = matches!(t, "-f" | "-F" | "--field" | "--raw-field" | "--input")
            || t.starts_with("--field=")
            || t.starts_with("--raw-field=")
            || t.starts_with("--input=")
            || ((t.starts_with("-f") || t.starts_with("-F")) && !t.starts_with("--"));
        if field {
            class = class.max(Class::Account);
        }
        i += 1;
    }
    class
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
        let class = classify_args(&argv)?;
        let shown = text::line(&argv::show(&argv), 300);
        Ok(Classified {
            class: Some(class),
            domain: Some("github.com".into()),
            what: Some(format!("run gh {shown}")),
            values: vec![("args".into(), shown)],
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let argv = gh_argv(&args)?;
        if classify_args(&argv)? > ctx.class {
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
        ];
        for (cmd, want) in table {
            assert_eq!(c(cmd).unwrap(), *want, "gh {cmd}");
        }
    }

    #[test]
    fn auth_and_empty_are_refused() {
        assert!(matches!(c("auth token"), Err(ToolError::Args(_))));
        assert!(matches!(c("auth status"), Err(ToolError::Args(_))));
        assert!(matches!(c(""), Err(ToolError::Args(_))));
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
