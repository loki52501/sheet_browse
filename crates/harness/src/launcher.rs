//! The default [`Launcher`] (D-106, D-116, D-124): a fresh Claude Code session per run.
//!
//! * Scheduled runs (`harnessd`): `claude -p` with the prompt on stdin (not argv, so a prompt
//!   starting with `-` is never read as a flag and Windows quoting cannot split it), after the
//!   run preamble ([`harness_core::sched::run_preamble`], B-563, D-140) and a blank line. stdout,
//!   the session's answer, is scrubbed and written to `files/answer.md`; stderr is inherited
//!   (it reaches harnessd's terminal unscrubbed; tool results in it were scrubbed by the tool
//!   servers already). A session is capped at [`ClaudeLauncher::max_session`] wall-clock time
//!   (default [`DEFAULT_SESSION_CAP`], `SB_SESSION_CAP_SECS` in harnessd): past it the session
//!   is killed (its process tree on Windows) and the launch fails. Its stdout is capped at
//!   [`ClaudeLauncher::max_answer_bytes`]; the rest is read and dropped.
//! * `harness run`: interactive `claude <prompt>` with inherited stdio, no cap (the user is
//!   at the terminal), and no preamble (argv through the `claude.cmd` shim cannot carry its
//!   newlines safely).
//!
//! Every session gets `--disallowedTools` with [`DISALLOWED_TOOLS`] (B-192): a session must
//! not answer its own asks, change gate rules or vault entries through the CLI (rule 4).
//! Both get `SB_RUN_ID` and an absolute `SB_RUNS_ROOT`, which the tool servers read in
//! `core_pg::bootstrap()` to attach to this run. No API key is set or read (rule 1).
//!
//! A launcher with a non-empty [`ClaudeLauncher::allowed_tools`] adds `--allowedTools` with
//! that list: reflect's readers-only replay session runs with only `mcp__harness-web` allowed
//! (G-3, T-16), so in `-p` mode every other tool that needs a permission is refused.

use crate::CliError;
use async_trait::async_trait;
use harness_core::sched::{run_preamble, LaunchError, LaunchExit, LaunchRequest, Launcher};
use harness_core::vault::scrub;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// File under the run's `files/` that holds a `claude -p` session's stdout.
pub const ANSWER_FILE: &str = "answer.md";

/// Claude Code permission rules every session is started with (B-192): the user-only
/// commands of the `harness` CLI.
pub const DISALLOWED_TOOLS: [&str; 3] = [
    "Bash(harness ask:*)",
    "Bash(harness gate:*)",
    "Bash(harness vault:*)",
];

/// Default wall-clock cap of a `claude -p` session (D-124).
pub const DEFAULT_SESSION_CAP: Duration = Duration::from_secs(60 * 60);

/// Environment variable harnessd reads for the session cap, in whole seconds.
pub const SESSION_CAP_VAR: &str = "SB_SESSION_CAP_SECS";

/// Default cap on the answer kept from a session's stdout (D-124).
pub const DEFAULT_MAX_ANSWER_BYTES: usize = 1024 * 1024;

/// Extra stdout read past the cap before scrubbing, then dropped from the end, so a vault
/// value cut by the cap is scrubbed whole or not written at all. Longer than any scrub
/// spelling of a vault value (D-124).
pub const ANSWER_SCRUB_SLACK: usize = 64 * 1024;

/// How long a killed session may take to be reaped before the launch gives up on it.
const KILL_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeLauncher {
    pub program: PathBuf,
    pub interactive: bool,
    /// Wall-clock cap of a `claude -p` session; ignored for interactive sessions.
    pub max_session: Duration,
    /// Bytes of a `claude -p` session's stdout kept in `files/answer.md`.
    pub max_answer_bytes: usize,
    /// Claude Code permission rules passed as `--allowedTools` (none: the flag is left out).
    /// Reflect's replay session uses [`crate::reflect::READERS_ALLOWED_TOOLS`] (G-3, T-16).
    pub allowed_tools: Vec<String>,
}

/// The session cap from `SB_SESSION_CAP_SECS` (unset or blank: [`DEFAULT_SESSION_CAP`]).
pub fn parse_session_cap(value: Option<&str>) -> Result<Duration, CliError> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(DEFAULT_SESSION_CAP),
        Some(v) => match v.parse::<u64>() {
            Ok(n) if n > 0 => Ok(Duration::from_secs(n)),
            _ => Err(CliError::Usage(format!(
                "{SESSION_CAP_VAR} must be a whole number of seconds above 0"
            ))),
        },
    }
}

/// [`parse_session_cap`] of the process environment (harnessd).
pub fn session_cap_from_env() -> Result<Duration, CliError> {
    parse_session_cap(std::env::var(SESSION_CAP_VAR).ok().as_deref())
}

impl ClaudeLauncher {
    /// `claude -p` (harnessd).
    pub fn print(program: impl Into<PathBuf>) -> ClaudeLauncher {
        ClaudeLauncher {
            program: program.into(),
            interactive: false,
            max_session: DEFAULT_SESSION_CAP,
            max_answer_bytes: DEFAULT_MAX_ANSWER_BYTES,
            allowed_tools: Vec::new(),
        }
    }

    /// `claude <prompt>` with the terminal (harness run).
    pub fn interactive(program: impl Into<PathBuf>) -> ClaudeLauncher {
        ClaudeLauncher {
            interactive: true,
            ..ClaudeLauncher::print(program)
        }
    }

    /// The command for `program` (argv, environment, stdio); not spawned.
    fn command(
        &self,
        program: &Path,
        req: &LaunchRequest<'_>,
        root: &Path,
    ) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(program);
        cmd.env("SB_RUN_ID", &req.run.id.0)
            .env("SB_RUNS_ROOT", root);
        if self.interactive {
            // A leading space keeps a prompt such as "-h" from being parsed as a flag. The
            // prompt goes before --disallowedTools, whose list takes every argument after it.
            let p = if req.prompt.starts_with('-') {
                format!(" {}", req.prompt)
            } else {
                req.prompt.to_owned()
            };
            cmd.arg(p);
        } else {
            cmd.arg("-p")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .kill_on_drop(true);
        }
        // Both lists are variadic: each ends where the next flag starts.
        if !self.allowed_tools.is_empty() {
            cmd.arg("--allowedTools").args(&self.allowed_tools);
        }
        cmd.arg("--disallowedTools").args(DISALLOWED_TOOLS);
        cmd
    }

    fn spawn(
        &self,
        req: &LaunchRequest<'_>,
        root: &Path,
    ) -> Result<tokio::process::Child, LaunchError> {
        let err =
            |p: &Path, e: std::io::Error| LaunchError(format!("cannot start {}: {e}", p.display()));
        match self.command(&self.program, req, root).spawn() {
            Ok(c) => Ok(c),
            // npm installs Claude Code as claude.cmd on Windows; std only resolves .exe.
            Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::NotFound => {
                match cmd_shim(&self.program) {
                    Some(shim) => self
                        .command(&shim, req, root)
                        .spawn()
                        .map_err(|e| err(&shim, e)),
                    None => Err(err(&self.program, e)),
                }
            }
            Err(e) => Err(err(&self.program, e)),
        }
    }
}

/// `<program>.cmd` when it exists: next to a program given as a path, else on PATH.
fn cmd_shim(program: &Path) -> Option<PathBuf> {
    if program.extension().is_some() {
        return None;
    }
    let file = program.with_extension("cmd");
    if program.components().count() > 1 {
        return file.is_file().then_some(file);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(&file))
        .find(|p| p.is_file())
}

/// Kills a session past its cap. On Windows the child is often `cmd.exe` running the
/// `claude.cmd` shim, so the whole tree goes (taskkill /T); then the child is reaped,
/// waiting at most [`KILL_WAIT`].
async fn kill_session(child: &mut tokio::process::Child) {
    if cfg!(windows) {
        if let Some(pid) = child.id() {
            let tree = tokio::process::Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status();
            let _ = tokio::time::timeout(KILL_WAIT, tree).await;
        }
    }
    let _ = child.start_kill();
    let _ = tokio::time::timeout(KILL_WAIT, child.wait()).await;
}

/// The text written to `answer.md` from `kept` (the first bytes of stdout) when the session
/// wrote `total` bytes. Scrubbed whole first; when stdout was cut, the last
/// [`ANSWER_SCRUB_SLACK`] bytes are dropped (a value cut at the end is never written in part),
/// then the text is cut to `cap` bytes with a note.
pub fn answer_text(kept: &[u8], total: usize, cap: usize) -> String {
    let text = scrub(&String::from_utf8_lossy(kept));
    if total <= cap {
        return text;
    }
    let mut end = text.len();
    if total > kept.len() {
        end = end.saturating_sub(ANSWER_SCRUB_SLACK);
    }
    end = end.min(cap);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n[answer cut: kept {end} of {total} bytes]\n",
        &text[..end]
    )
}

/// What a `claude -p` session reads on stdin: [`run_preamble`], a blank line, the task.
pub fn session_prompt(req: &LaunchRequest<'_>) -> String {
    format!("{}\n{}", run_preamble(req), req.prompt)
}

#[async_trait]
impl Launcher for ClaudeLauncher {
    async fn launch(&self, req: LaunchRequest<'_>) -> Result<LaunchExit, LaunchError> {
        let root = std::path::absolute(req.runs_root)
            .map_err(|e| LaunchError(format!("runs root: {e}")))?;
        let mut child = self.spawn(&req, &root)?;
        if self.interactive {
            let status = child
                .wait()
                .await
                .map_err(|e| LaunchError(format!("waiting for the session: {e}")))?;
            return Ok(LaunchExit {
                code: status.code(),
            });
        }

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| LaunchError("no stdin pipe".into()))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| LaunchError("no stdout pipe".into()))?;
        let prompt = session_prompt(&req).into_bytes();
        let write = async move {
            let r = stdin.write_all(&prompt).await;
            drop(stdin); // EOF: the session starts.
            r
        };
        let keep = self.max_answer_bytes.saturating_add(ANSWER_SCRUB_SLACK);
        let read = async move {
            let mut kept = Vec::new();
            let mut chunk = vec![0u8; 16 * 1024];
            let mut total = 0usize;
            loop {
                let n = stdout.read(&mut chunk).await?;
                if n == 0 {
                    break;
                }
                total = total.saturating_add(n);
                let room = keep.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
            Ok::<_, std::io::Error>((kept, total))
        };
        // The session's end is the condition waited on; the cap bounds it (rule 6).
        let session = async {
            let (wrote, out) = tokio::join!(write, read);
            let status = child.wait().await;
            (wrote, out, status)
        };
        let (wrote, out, status) = match tokio::time::timeout(self.max_session, session).await {
            Ok(done) => done,
            Err(_) => {
                kill_session(&mut child).await;
                return Err(LaunchError(format!(
                    "session ran past its {}s cap and was killed",
                    self.max_session.as_secs_f32()
                )));
            }
        };
        let status = status.map_err(|e| LaunchError(format!("waiting for the session: {e}")))?;
        // A session that exits before reading its prompt closes the pipe; the exit code says why.
        if let Err(e) = wrote {
            if status.success() {
                return Err(LaunchError(format!("writing the prompt: {e}")));
            }
        }
        let (kept, total) =
            out.map_err(|e| LaunchError(format!("reading the session output: {e}")))?;
        let text = answer_text(&kept, total, self.max_answer_bytes);
        if !text.trim().is_empty() {
            let path = req.run.files().join(ANSWER_FILE);
            std::fs::write(&path, text)
                .map_err(|e| LaunchError(format!("{}: {e}", path.display())))?;
        }
        Ok(LaunchExit {
            code: status.code(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::run::RunFolder;

    fn argv(cmd: &tokio::process::Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[tokio::test]
    async fn missing_program_is_a_launch_error() {
        let tmp = tempfile::tempdir().unwrap();
        let run = RunFolder::create(tmp.path(), "x", "x").unwrap();
        let l = ClaudeLauncher::print(tmp.path().join("no-such-claude"));
        let e = l
            .launch(LaunchRequest {
                run: &run,
                runs_root: tmp.path(),
                prompt: "say hi",
                schedule: None,
            })
            .await
            .unwrap_err();
        assert!(e.0.contains("cannot start"), "{e}");
        assert!(e.0.contains("no-such-claude"), "{e}");
    }

    /// B-192: every session is started with the user-only CLI commands denied; the prompt
    /// is on stdin for -p and before the variadic --disallowedTools list when interactive.
    #[test]
    fn every_session_denies_the_user_only_commands() {
        let tmp = tempfile::tempdir().unwrap();
        let run = RunFolder::create(tmp.path(), "x", "x").unwrap();
        let req = LaunchRequest {
            run: &run,
            runs_root: tmp.path(),
            prompt: "-h say hi",
            schedule: None,
        };
        let deny = [
            "--disallowedTools",
            "Bash(harness ask:*)",
            "Bash(harness gate:*)",
            "Bash(harness vault:*)",
        ];
        let p = ClaudeLauncher::print("claude");
        let mut want = vec!["-p"];
        want.extend(deny);
        assert_eq!(argv(&p.command(&p.program, &req, tmp.path())), want);
        let i = ClaudeLauncher::interactive("claude");
        let mut want = vec![" -h say hi"];
        want.extend(deny);
        assert_eq!(argv(&i.command(&i.program, &req, tmp.path())), want);
    }

    /// B-563 (D-140 (10)): a `-p` session reads the run preamble, one blank line, then the
    /// task; the preamble itself has no blank line, so the task is everything after the first.
    #[test]
    fn session_prompt_is_preamble_then_task() {
        let tmp = tempfile::tempdir().unwrap();
        let run = RunFolder::create(tmp.path(), "x", "x").unwrap();
        let req = LaunchRequest {
            run: &run,
            runs_root: tmp.path(),
            prompt: "-h say hi\n\nsecond paragraph",
            schedule: None,
        };
        let p = session_prompt(&req);
        assert!(p.starts_with(&format!("Harness run {}.\n", run.id)), "{p}");
        assert!(
            p.contains("Cite only what tools returned in this run"),
            "{p}"
        );
        let (pre, task) = p.split_once("\n\n").unwrap();
        assert_eq!(format!("{pre}\n"), run_preamble(&req));
        assert_eq!(task, "-h say hi\n\nsecond paragraph");
    }

    /// G-3, T-16: the reflect replay session allows only harness-web.
    #[test]
    fn allowed_tools_go_before_the_deny_list() {
        let tmp = tempfile::tempdir().unwrap();
        let run = RunFolder::create(tmp.path(), "x", "x").unwrap();
        let req = LaunchRequest {
            run: &run,
            runs_root: tmp.path(),
            prompt: "replay",
            schedule: None,
        };
        let mut p = ClaudeLauncher::print("claude");
        p.allowed_tools = vec!["mcp__harness-web".into()];
        let got = argv(&p.command(&p.program, &req, tmp.path()));
        assert_eq!(
            got[..4],
            [
                "-p",
                "--allowedTools",
                "mcp__harness-web",
                "--disallowedTools"
            ]
        );
    }

    #[test]
    fn session_cap_parses() {
        assert_eq!(parse_session_cap(None).unwrap(), DEFAULT_SESSION_CAP);
        assert_eq!(parse_session_cap(Some(" ")).unwrap(), DEFAULT_SESSION_CAP);
        assert_eq!(
            parse_session_cap(Some("90")).unwrap(),
            Duration::from_secs(90)
        );
        for bad in ["0", "-5", "1.5", "ten"] {
            assert!(parse_session_cap(Some(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn answer_is_cut_at_the_cap() {
        assert_eq!(answer_text(b"short", 5, 10), "short");
        let a = answer_text(b"abcdefghij", 10, 4);
        assert!(
            a.starts_with("abcd\n\n[answer cut: kept 4 of 10 bytes]"),
            "{a}"
        );
        // stdout longer than what was kept: the slack is dropped from the end.
        let kept = vec![b'x'; ANSWER_SCRUB_SLACK + 3];
        let a = answer_text(&kept, kept.len() + 100, 10);
        assert!(a.starts_with("xxx\n\n[answer cut: kept 3 of"), "{a}");
        // Never cut inside a character.
        let a = answer_text("ééé".as_bytes(), 6, 3);
        assert!(a.starts_with("é\n\n"), "{a}");
    }
}
