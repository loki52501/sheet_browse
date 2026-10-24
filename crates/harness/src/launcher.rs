//! The default [`Launcher`] (D-106, D-116): a fresh Claude Code session per run.
//!
//! * Scheduled runs (`harnessd`): `claude -p` with the prompt on stdin (not argv, so a prompt
//!   starting with `-` is never read as a flag and Windows quoting cannot split it). stdout,
//!   the session's answer, is scrubbed and written to `files/answer.md`; stderr is inherited.
//! * `harness run`: interactive `claude <prompt>` with inherited stdio.
//!
//! Both get `SB_RUN_ID` and an absolute `SB_RUNS_ROOT`, which the tool servers read in
//! `core_pg::bootstrap()` to attach to this run. No API key is set or read (rule 1).

use async_trait::async_trait;
use harness_core::sched::{LaunchError, LaunchExit, LaunchRequest, Launcher};
use harness_core::vault::scrub;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// File under the run's `files/` that holds a `claude -p` session's stdout.
pub const ANSWER_FILE: &str = "answer.md";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeLauncher {
    pub program: PathBuf,
    pub interactive: bool,
}

impl ClaudeLauncher {
    /// `claude -p` (harnessd).
    pub fn print(program: impl Into<PathBuf>) -> ClaudeLauncher {
        ClaudeLauncher {
            program: program.into(),
            interactive: false,
        }
    }

    /// `claude <prompt>` with the terminal (harness run).
    pub fn interactive(program: impl Into<PathBuf>) -> ClaudeLauncher {
        ClaudeLauncher {
            program: program.into(),
            interactive: true,
        }
    }

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
            // A leading space keeps a prompt such as "-h" from being parsed as a flag.
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
        let prompt = req.prompt.as_bytes().to_vec();
        let write = async move {
            let r = stdin.write_all(&prompt).await;
            drop(stdin); // EOF: the session starts.
            r
        };
        let read = async move {
            let mut buf = Vec::new();
            stdout.read_to_end(&mut buf).await.map(|_| buf)
        };
        let (wrote, out) = tokio::join!(write, read);
        let status = child
            .wait()
            .await
            .map_err(|e| LaunchError(format!("waiting for the session: {e}")))?;
        // A session that exits before reading its prompt closes the pipe; the exit code says why.
        if let Err(e) = wrote {
            if status.success() {
                return Err(LaunchError(format!("writing the prompt: {e}")));
            }
        }
        let out = out.map_err(|e| LaunchError(format!("reading the session output: {e}")))?;
        let text = String::from_utf8_lossy(&out);
        if !text.trim().is_empty() {
            let path = req.run.files().join(ANSWER_FILE);
            std::fs::write(&path, scrub(&text))
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
            })
            .await
            .unwrap_err();
        assert!(e.0.contains("cannot start"), "{e}");
        assert!(e.0.contains("no-such-claude"), "{e}");
    }
}
