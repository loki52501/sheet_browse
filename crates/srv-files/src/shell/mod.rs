//! F-2 `shell(cmd, cwd, timeout)`: class from the command ([`classify`]), run through
//! bash (Git Bash on Windows) with a cleaned environment. Without bash on Windows only a
//! single plain command runs, as argv without cmd.exe; anything else is refused with
//! "shell needs Git Bash; set SB_BASH" (B9, D-523). The process tree is killed at the
//! timeout. The model gets at most 20 000 chars (head and tail); when more was
//! printed, the whole output, scrubbed, is saved in this run's `files/` and its path is in
//! the text.

pub mod classify;
pub mod lex;

use crate::paths::{self, Roots};
use crate::proc::{self, Tap};
use crate::text::{self, MAX_CHARS};
use crate::util::{self, parse, recheck, Verdict};
use async_trait::async_trait;
use harness_core::{Class, Classified, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolSpec};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Default and largest timeout, seconds.
pub const TIMEOUT_DEFAULT_SECS: u64 = 120;
pub const TIMEOUT_MAX_SECS: u64 = 600;
/// Chars of the start and of the end shown when the output is cut (sum: [`MAX_CHARS`]).
const HEAD_CHARS: usize = 4_000;
const TAIL_CHARS: usize = MAX_CHARS - HEAD_CHARS;
/// Bytes kept in memory from each end of the output (enough for the chars above).
const KEEP_BYTES: usize = 4 * MAX_CHARS;
/// Largest output saved to `files/`.
const SAVE_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Which shell runs the command.
#[derive(Debug, Clone)]
pub enum Runner {
    Bash(PathBuf),
    /// No bash: on Windows (the path is cmd.exe, never used to run a line) only plain
    /// argv commands run; elsewhere `sh`.
    Fallback(PathBuf),
}

impl Runner {
    /// SB_BASH, else Git Bash (never WSL's System32 bash), else bash on PATH, else
    /// cmd.exe / sh.
    pub fn discover() -> Runner {
        if let Some(p) = std::env::var_os("SB_BASH").filter(|p| !p.is_empty()) {
            return Runner::Bash(PathBuf::from(p));
        }
        if cfg!(windows) {
            let mut cands: Vec<PathBuf> = Vec::new();
            for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
                if let Some(pf) = std::env::var_os(var) {
                    cands.push(PathBuf::from(pf).join("Git").join("bin").join("bash.exe"));
                }
            }
            if let Some(git) = proc::find_program("git", |_| false) {
                // <Git>/cmd/git.exe or <Git>/bin/git.exe or <Git>/mingw64/bin/git.exe
                let mut d = git.parent().map(Path::to_path_buf);
                for _ in 0..3 {
                    if let Some(dir) = d {
                        cands.push(dir.join("bin").join("bash.exe"));
                        d = dir.parent().map(Path::to_path_buf);
                    }
                }
            }
            if let Some(b) = cands.into_iter().find(|c| c.is_file()) {
                return Runner::Bash(b);
            }
            if let Some(b) = proc::find_program("bash", |p| {
                let s = p.to_string_lossy().to_ascii_lowercase();
                s.contains("system32") || s.contains("windowsapps")
            }) {
                return Runner::Bash(b);
            }
            let comspec = std::env::var_os("ComSpec")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("cmd.exe"));
            Runner::Fallback(comspec)
        } else {
            match proc::find_program("bash", |_| false) {
                Some(b) => Runner::Bash(b),
                None => Runner::Fallback(PathBuf::from("/bin/sh")),
            }
        }
    }

    fn is_cmd_exe(&self) -> bool {
        matches!(self, Runner::Fallback(_)) && cfg!(windows)
    }

    /// The process for `line`. Without bash on Windows nothing goes through cmd.exe,
    /// whose quoting this classifier does not read (B9): a single plain command runs as
    /// argv directly, anything else is refused.
    fn command(&self, line: &str) -> Result<tokio::process::Command, ToolError> {
        match self {
            Runner::Bash(b) => {
                let mut c = tokio::process::Command::new(b);
                c.args(["--noprofile", "--norc", "-c", line]);
                sanitize_env(&mut c);
                Ok(c)
            }
            Runner::Fallback(_) if cfg!(windows) => {
                let argv = classify::plain_argv(line)
                    .ok_or_else(|| ToolError::Failed(classify::CMD_EXE_REFUSED.into()))?;
                let mut c = tokio::process::Command::new(&argv[0]);
                c.args(&argv[1..]);
                Ok(c)
            }
            Runner::Fallback(s) => {
                let mut c = tokio::process::Command::new(s);
                c.args(["-c", line]);
                sanitize_env(&mut c);
                Ok(c)
            }
        }
    }
}

/// Removes inherited variables that make bash run code or resolve names other than the
/// classifier assumed: a startup file (BASH_ENV, ENV), exported functions (BASH_FUNC_*,
/// which can shadow `ls`), `cd` search paths, xtrace prompts and word splitting.
fn sanitize_env(c: &mut tokio::process::Command) {
    for k in [
        "BASH_ENV",
        "ENV",
        "CDPATH",
        "GLOBIGNORE",
        "BASHOPTS",
        "SHELLOPTS",
        "PROMPT_COMMAND",
        "PS4",
        "IFS",
    ] {
        c.env_remove(k);
    }
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("BASH_FUNC_") {
            c.env_remove(k);
        }
    }
}

struct Shell {
    runner: Runner,
}

/// `shell` with the shell found on this machine.
pub fn tools() -> Vec<ToolSpec> {
    tools_with(Runner::discover())
}

/// `shell` with a given runner (tests).
pub fn tools_with(runner: Runner) -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "shell".into(),
        description: format!(
            "Run a bash command line (cwd defaults to the folder that holds runs/). Its class comes from what it does: changes inside runs/ just run; deleting or replacing files elsewhere, git push, curl -X POST and commands the classifier does not know ask first. Output is capped at {MAX_CHARS} chars; the whole output is saved in files/ when cut. timeout in seconds (default {TIMEOUT_DEFAULT_SECS}, max {TIMEOUT_MAX_SECS})."
        ),
        class: Class::Read,
        args: json!({"type":"object","properties":{
            "cmd":{"type":"string"},
            "cwd":{"type":"string"},
            "timeout":{"type":"integer","minimum":1,"maximum":TIMEOUT_MAX_SECS}
        },"required":["cmd"],"additionalProperties":false}),
        handler: Arc::new(Shell { runner }),
    }]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    cmd: String,
    cwd: Option<String>,
    timeout: Option<u64>,
}

impl Shell {
    fn plan(&self, roots: &Roots, a: &Args) -> Result<(PathBuf, Verdict), ToolError> {
        if a.cmd.trim().is_empty() {
            return Err(ToolError::Args("empty cmd".into()));
        }
        let cwd = match &a.cwd {
            Some(c) => roots.resolve(c)?,
            None => roots.base.clone(),
        };
        if !paths::is_dir(&cwd) {
            return Err(ToolError::Args(format!(
                "cwd {} is not a folder",
                text::one_line(&roots.shown(&cwd), 300)
            )));
        }
        if self.runner.is_cmd_exe() && classify::plain_argv(&a.cmd).is_none() {
            return Err(ToolError::Failed(classify::CMD_EXE_REFUSED.into()));
        }
        let v = classify::classify(roots, &cwd, &a.cmd, self.runner.is_cmd_exe());
        Ok((cwd, v))
    }
}

#[async_trait]
impl ToolHandler for Shell {
    async fn classify(&self, ctx: &ToolCtx, args: &Value) -> Result<Classified, ToolError> {
        let roots = Roots::of(ctx)?;
        let a: Args = parse(args)?;
        let (cwd, v) = self.plan(&roots, &a)?;
        Ok(v.classified(
            "shell",
            vec![
                ("cmd".into(), a.cmd.clone()),
                ("cwd".into(), roots.shown(&cwd)),
            ],
        ))
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let roots = Roots::of(ctx)?;
        let a: Args = parse(&args)?;
        let (cwd, v) = self.plan(&roots, &a)?;
        recheck(ctx, &v)?;
        let secs = match a.timeout {
            None => TIMEOUT_DEFAULT_SECS,
            Some(0) => return Err(ToolError::Args("timeout must be at least 1".into())),
            Some(s) => s.min(TIMEOUT_MAX_SECS),
        };

        // The raw stream goes to a temp file outside runs/; only a scrubbed copy is saved.
        let raw = tempfile::NamedTempFile::new()
            .map_err(|e| ToolError::Failed(format!("temp file: {e}")))?;
        let log = raw
            .reopen()
            .map_err(|e| ToolError::Failed(format!("temp file: {e}")))?;
        let tap = Tap::shared(KEEP_BYTES, Some(log));
        let mut cmd = self.runner.command(&a.cmd)?;
        cmd.current_dir(paths::plain(&cwd));
        let started = Instant::now();
        let exit = proc::run(
            cmd,
            None,
            Duration::from_secs(secs),
            tap.clone(),
            tap.clone(),
        )
        .await
        .map_err(|e| ToolError::Failed(format!("could not start the shell: {e}")))?;
        let elapsed = started.elapsed().as_secs_f64();

        let mut tap = tap.lock().unwrap_or_else(|e| e.into_inner());
        let log_error = tap.finish_log();
        let total_bytes = tap.total();
        let (head, tail) = tap.parts();
        let bytes_cut = tap.cut();
        drop(tap);

        let status = if exit.timed_out {
            format!("exit: killed after the {secs} s timeout ({elapsed:.1} s)")
        } else {
            match exit.code {
                Some(c) => format!("exit: {c} ({elapsed:.1} s)"),
                None => format!("exit: ended by a signal ({elapsed:.1} s)"),
            }
        };
        // Trailing line ends carry nothing for the model.
        let tail = tail.trim_end_matches(['\n', '\r']).to_string();
        let whole = if bytes_cut {
            None
        } else {
            Some(
                format!("{head}{tail}")
                    .trim_end_matches(['\n', '\r'])
                    .to_string(),
            )
        };
        let fits = whole
            .as_ref()
            .is_some_and(|w| w.chars().count() <= MAX_CHARS);
        let mut files = Vec::new();
        let body = if total_bytes == 0 {
            "(no output)".to_string()
        } else if fits {
            text::untrusted(whole.as_deref().unwrap_or(""))
        } else {
            let (h, _) = text::take_chars(whole.as_deref().unwrap_or(&head), 0, HEAD_CHARS);
            let t = text::tail_chars(whole.as_deref().unwrap_or(&tail), TAIL_CHARS);
            let saved = save_whole(&roots, raw.path(), total_bytes, log_error.as_deref());
            let note = match &saved {
                Ok(p) => {
                    files.push(p.clone());
                    format!(
                        "truncated: output cut in the middle ({total_bytes} bytes in all); saved: {}",
                        text::one_line(&roots.shown(p), 300)
                    )
                }
                Err(e) => format!(
                    "truncated: output cut in the middle ({total_bytes} bytes in all); not saved: {e}"
                ),
            };
            format!("{}\n{note}\n{}", text::untrusted(&h), text::untrusted(t))
        };
        Ok(ToolOutput {
            text: format!("{status}\n{body}"),
            summary: Some(format!(
                "shell: {} -> {}",
                text::one_line(&a.cmd, 120),
                status.trim_start_matches("exit: ")
            )),
            consequences: json!({
                "exit": exit.code,
                "timed_out": exit.timed_out,
                "output_bytes": total_bytes,
            }),
            files: util::run_files(
                &roots,
                &files.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            ),
            stopped: None,
        })
    }
}

/// Copies the raw output into `files/shell-<n>.log`, scrubbed of vault values (rule 2).
fn save_whole(
    roots: &Roots,
    raw: &Path,
    total: u64,
    log_error: Option<&str>,
) -> Result<PathBuf, String> {
    use std::io::Read;
    if let Some(e) = log_error {
        return Err(format!("the output log failed: {e}"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(raw)
        .and_then(|f| f.take(SAVE_MAX_BYTES).read_to_end(&mut bytes))
        .map_err(|e| e.to_string())?;
    let mut s = harness_core::scrub(&String::from_utf8_lossy(&bytes));
    if total > SAVE_MAX_BYTES {
        s.push_str(&format!(
            "\n[saved the first {SAVE_MAX_BYTES} of {total} bytes]\n"
        ));
    }
    std::fs::create_dir_all(&roots.files).map_err(|e| e.to_string())?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let path = util::fresh_name(&roots.files, &format!("shell-{stamp}"), "log");
    std::fs::write(&path, s).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn cmd_exe_fallback_runs_only_plain_argv_b9() {
        let r = Runner::Fallback(PathBuf::from("cmd.exe"));
        for line in [
            "echo ' & del file.txt & '",
            "dir && del file.txt",
            "echo %PATH%",
        ] {
            let e = r.command(line).expect_err(line).to_string();
            assert!(e.contains("Git Bash") && e.contains("SB_BASH"), "{e}");
        }
        let c = r.command("git status --short").unwrap();
        let std = c.as_std();
        assert_eq!(std.get_program(), "git");
        let args: Vec<_> = std.get_args().collect();
        assert_eq!(args, ["status", "--short"]);
    }

    #[test]
    fn bash_env_is_cleaned() {
        let r = Runner::Bash(PathBuf::from("bash"));
        let c = r.command("ls").unwrap();
        let removed: Vec<String> = c
            .as_std()
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for k in ["BASH_ENV", "ENV", "CDPATH", "PS4", "IFS"] {
            assert!(removed.iter().any(|r| r == k), "{k} not removed");
        }
    }
}
