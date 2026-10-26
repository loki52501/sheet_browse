//! C-4 `cli(cmd)`: a program from `config/cli-allow.toml`, run as an argument vector (no
//! shell). A program not in the file, a denied flag, shell syntax, or a first argument the
//! entry does not list (when its `default` is `refuse`) is refused with
//! [`ToolError::Args`] and nothing runs. The class comes from the entry: a first argument in
//! `delete` is Delete, in `write` Account, in `read` Read; otherwise `default`.
//!
//! ```toml
//! [programs.git]                       # the name the command starts with
//! bin = "git"                          # optional executable, default the name
//! read = ["status", "log", "diff"]     # first arguments that only read
//! write = ["fetch"]                    # first arguments that change state: Account
//! delete = []                          # first arguments that delete: Delete
//! default = "refuse"                   # any other first argument: read|write|delete|refuse
//! deny_flags = ["-c", "--output"]      # refused anywhere (also as --flag=value, -cvalue,
//!                                      # and a short letter anywhere in a cluster: -pc)
//! timeout_secs = 60                    # optional cap, default the server's
//! ```
//!
//! The file is read in `classify` and again in `call`; a change in between that raises the
//! class makes the call fail instead of running.

use crate::{argv, proc, text, Config};
use async_trait::async_trait;
use harness_core::class::Class;
use harness_core::mcp::{Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Stdout kept per run, in bytes.
const OUT_MAX_BYTES: usize = 4 * 1024 * 1024;

/// One allowlisted program.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Program {
    #[serde(default)]
    pub bin: Option<String>,
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
    #[serde(default)]
    pub delete: Vec<String>,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub deny_flags: Vec<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowFile {
    #[serde(default)]
    programs: BTreeMap<String, Program>,
}

/// Parses `cli-allow.toml`. Program names fold to ASCII lowercase (two that fold together
/// are an error); `default` must be read, write, delete or refuse. A missing file allows
/// nothing.
pub fn load_allow(path: &Path) -> Result<BTreeMap<String, Program>, ToolError> {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => {
            return Err(ToolError::Failed(format!(
                "cli: cannot read {}: {e}",
                path.display()
            )))
        }
    };
    parse_allow(&src).map_err(|e| ToolError::Failed(format!("cli: {}: {e}", path.display())))
}

fn parse_allow(src: &str) -> Result<BTreeMap<String, Program>, String> {
    let file: AllowFile = toml::from_str(src).map_err(|e| e.message().to_string())?;
    let mut out = BTreeMap::new();
    for (name, p) in file.programs {
        let key = name.to_ascii_lowercase();
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(format!("program name {name:?} must be [a-z0-9_.-]"));
        }
        if let Some(d) = &p.default {
            if !matches!(d.as_str(), "read" | "write" | "delete" | "refuse") {
                return Err(format!(
                    "program {key}: default must be read, write, delete or refuse"
                ));
            }
        }
        if p.deny_flags
            .iter()
            .any(|f| !f.starts_with('-') || f.len() < 2)
        {
            return Err(format!(
                "program {key}: every deny_flags entry starts with -"
            ));
        }
        if out.insert(key.clone(), p).is_some() {
            return Err(format!(
                "program {key} is listed twice (names fold to lowercase)"
            ));
        }
    }
    Ok(out)
}

/// The command a call runs, checked against the allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// Executable then arguments.
    pub argv: Vec<String>,
    pub class: Class,
    pub timeout: Option<Duration>,
}

/// Checks `cmd` (already split) against `allow`. Refusals are [`ToolError::Args`].
pub fn check(cmd: &[String], allow: &BTreeMap<String, Program>) -> Result<Checked, ToolError> {
    let first = cmd
        .first()
        .ok_or_else(|| ToolError::Args("cli needs a command".into()))?;
    if first.contains(['/', '\\', ':']) {
        return Err(ToolError::Args(
            "cli runs allowlisted programs by name, not by path".into(),
        ));
    }
    let key = first.to_ascii_lowercase();
    let key = key.strip_suffix(".exe").unwrap_or(&key);
    let names = || allow.keys().cloned().collect::<Vec<_>>().join(", ");
    let Some(p) = allow.get(key) else {
        return Err(ToolError::Args(format!(
            "{:?} is not in config/cli-allow.toml (allowed: {}); nothing ran",
            text::line(first, 64),
            if allow.is_empty() {
                "none".to_string()
            } else {
                names()
            }
        )));
    };
    let args = &cmd[1..];
    for a in args {
        // The letters of a single-dash token: short flags may be clustered (`dig -4f x`),
        // and which letters take a value differs per program, so a denied letter anywhere
        // in the cluster (even inside what may be a value) is a hit. Fails closed.
        let cluster = a
            .strip_prefix('-')
            .filter(|c| !c.is_empty() && !c.starts_with('-'));
        for f in &p.deny_flags {
            let short = f.len() == 2 && !f.starts_with("--");
            let hit = a == f
                || a.starts_with(&format!("{f}="))
                || (short && cluster.is_some_and(|c| c.contains(&f[1..])));
            if hit {
                return Err(ToolError::Args(format!(
                    "{key}: flag {f} is denied by config/cli-allow.toml; nothing ran"
                )));
            }
        }
    }
    let sub = args.first().map(String::as_str);
    let listed = |l: &[String]| sub.is_some_and(|s| l.iter().any(|x| x == s));
    let class = if listed(&p.delete) {
        Class::Delete
    } else if listed(&p.write) {
        Class::Account
    } else if listed(&p.read) {
        Class::Read
    } else {
        match p.default.as_deref() {
            Some("read") => Class::Read,
            Some("write") => Class::Account,
            Some("delete") => Class::Delete,
            _ => {
                return Err(ToolError::Args(format!(
                    "{key} {}: not allowed by config/cli-allow.toml (read: {}; write: {}); \
                     nothing ran",
                    text::line(sub.unwrap_or("(no arguments)"), 64),
                    if p.read.is_empty() {
                        "-".to_string()
                    } else {
                        p.read.join(", ")
                    },
                    if p.write.is_empty() {
                        "-".to_string()
                    } else {
                        p.write.join(", ")
                    },
                )))
            }
        }
    };
    let mut argv = vec![p.bin.clone().unwrap_or_else(|| key.to_string())];
    argv.extend(args.iter().cloned());
    Ok(Checked {
        argv,
        class,
        timeout: p.timeout_secs.map(Duration::from_secs),
    })
}

/// `cli` with [`Config::from_env`].
pub fn tools() -> Vec<ToolSpec> {
    tools_with(&Arc::new(Config::from_env()))
}

pub fn tools_with(cfg: &Arc<Config>) -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "cli".into(),
        description: "cli(cmd): run one program from config/cli-allow.toml, without a shell \
                      (no pipes, redirects or chaining), e.g. \"git status\". Anything not \
                      allowlisted is refused and nothing runs."
            .into(),
        class: Class::Read,
        args: json!({
            "type": "object",
            "properties": {
                "cmd": {
                    "description": "the command: one string (quoted like a shell) or an array of strings",
                    "oneOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                }
            },
            "required": ["cmd"]
        }),
        handler: Arc::new(Cli { cfg: cfg.clone() }),
    }]
}

struct Cli {
    cfg: Arc<Config>,
}

impl Cli {
    fn checked(&self, args: &Value) -> Result<Checked, ToolError> {
        let cmd = argv::from_value(args, "cmd")?;
        let allow = load_allow(&self.cfg.cli_allow_file)?;
        check(&cmd, &allow)
    }
}

#[async_trait]
impl ToolHandler for Cli {
    async fn classify(&self, _ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let c = self.checked(args)?;
        let shown = text::line(&argv::show(&c.argv), 300);
        Ok(Classified {
            class: Some(c.class),
            what: Some(format!("run {shown}")),
            values: vec![("cmd".into(), shown)],
            ..Classified::default()
        })
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let c = self.checked(&args)?;
        if c.class > ctx.class {
            return Err(ToolError::Failed(
                "cli: config/cli-allow.toml changed since the call was approved and the class \
                 went up; nothing ran"
                    .into(),
            ));
        }
        let timeout = c.timeout.unwrap_or(self.cfg.proc_timeout);
        let shown = text::line(&argv::show(&c.argv), 200);
        let out = proc::run(&c.argv, &[], timeout, OUT_MAX_BYTES, "cli").await?;
        let stdout = out.stdout_text();
        let stderr = out.stderr_text();
        let mut t = format!("{shown}: {}", out.status_line());
        if !stdout.trim().is_empty() {
            t.push('\n');
            t.push_str(&text::capped(
                stdout.trim_end(),
                text::MAX_TEXT_CHARS,
                "narrow the command",
            ));
        }
        if out.stdout_cut() {
            t.push_str(&format!(
                "\ntruncated: the program wrote {} bytes, kept {}",
                out.stdout_total,
                out.stdout.len()
            ));
        }
        if !stderr.trim().is_empty() {
            t.push_str("\nstderr:\n");
            t.push_str(&text::capped(stderr.trim_end(), 2_000, ""));
        }
        if !out.success() {
            return Err(ToolError::Failed(t));
        }
        Ok(ToolOutput {
            summary: Some(format!("{shown}: {}", out.status_line())),
            text: t,
            ..ToolOutput::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow() -> BTreeMap<String, Program> {
        parse_allow(
            r#"
[programs.git]
read = ["status", "log"]
write = ["fetch"]
delete = ["clean"]
deny_flags = ["-c", "--output"]

[programs.dig]
default = "read"
deny_flags = ["-f"]
"#,
        )
        .unwrap()
    }

    fn chk(s: &str) -> Result<Checked, ToolError> {
        check(&argv::split(s).unwrap(), &allow())
    }

    #[test]
    fn classes_from_the_entry() {
        assert_eq!(chk("git status").unwrap().class, Class::Read);
        assert_eq!(chk("git fetch origin").unwrap().class, Class::Account);
        assert_eq!(chk("git clean -fd").unwrap().class, Class::Delete);
        assert_eq!(chk("dig example.com MX").unwrap().class, Class::Read);
        assert_eq!(chk("dig -4 example.com").unwrap().class, Class::Read);
        assert_eq!(chk("git log -p --stat").unwrap().class, Class::Read);
        assert_eq!(chk("GIT.EXE log").unwrap().argv[0], "git");
    }

    #[test]
    fn refusals() {
        for bad in [
            "rm -rf x",
            "git push",
            "git",
            "git -c core.pager=x log",
            "git log --output=x",
            "git log -cfoo",
            "dig -f names.txt",
            "dig -4f names.txt",
            "dig -46fnames.txt",
            "git log -pc",
            "/usr/bin/git status",
            "C:git status",
        ] {
            assert!(
                matches!(chk(bad), Err(ToolError::Args(_))),
                "{bad} accepted"
            );
        }
    }

    #[test]
    fn shipped_allowlist_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/cli-allow.toml");
        let a = load_allow(&path).unwrap();
        assert!(a.contains_key("git"));
        let git = |s: &str| check(&argv::split(s).unwrap(), &a);
        assert_eq!(git("git status").unwrap().class, Class::Read);
        assert!(git("git push").is_err());
        assert!(git("git -c x=y status").is_err());
        let dig = |s: &str| check(&argv::split(s).unwrap(), &a);
        assert!(dig("dig -4f names.txt").is_err());
        assert!(dig("dig -4k key.conf").is_err());
        assert!(dig("dig -6y hmac:n:s").is_err());
    }
}
