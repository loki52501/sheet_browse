//! Runs one program as an argument vector (never through a shell), with stdin closed, a
//! wall-clock cap, capped stdout and stderr, and the child killed if the call is dropped
//! or the cap is hit.

use harness_core::mcp::ToolError;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Stderr kept per call, in bytes.
const MAX_STDERR: usize = 64 * 1024;

/// What a finished program produced.
#[derive(Debug)]
pub struct ProcOut {
    pub stdout: Vec<u8>,
    /// Bytes the program wrote to stdout, including the ones past the cap.
    pub stdout_total: u64,
    pub stderr: Vec<u8>,
    /// Exit code; `None` when killed by a signal.
    pub code: Option<i32>,
}

impl ProcOut {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn stdout_cut(&self) -> bool {
        self.stdout_total > self.stdout.len() as u64
    }

    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    /// `exit N` or `killed`.
    pub fn status_line(&self) -> String {
        match self.code {
            Some(c) => format!("exit {c}"),
            None => "killed".into(),
        }
    }
}

/// Runs `argv[0]` with `argv[1..]` and the extra `env`, keeping at most `max_stdout` bytes
/// of stdout. Past `timeout` the child is killed and the call fails. Programs found through
/// `PATH`; `what` names the program in errors.
pub async fn run(
    argv: &[String],
    env: &[(&str, &str)],
    timeout: Duration,
    max_stdout: usize,
    what: &str,
) -> Result<ProcOut, ToolError> {
    let (prog, rest) = argv
        .split_first()
        .ok_or_else(|| ToolError::Args("empty command".into()))?;
    let mut cmd = tokio::process::Command::new(prog);
    cmd.args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ToolError::Failed(format!("{what}: program {prog:?} not found"))
        } else {
            ToolError::Failed(format!("{what}: cannot start {prog:?}: {e}"))
        }
    })?;
    let out = child.stdout.take();
    let err = child.stderr.take();
    let work = async {
        let (o, e, status) = tokio::join!(
            read_capped(out, max_stdout),
            read_capped(err, MAX_STDERR),
            child.wait()
        );
        (o, e, status)
    };
    match tokio::time::timeout(timeout, work).await {
        Ok((o, e, status)) => {
            let status =
                status.map_err(|e| ToolError::Failed(format!("{what}: wait failed: {e}")))?;
            let (stdout, stdout_total) =
                o.map_err(|e| ToolError::Failed(format!("{what}: reading stdout: {e}")))?;
            let (stderr, _) =
                e.map_err(|e| ToolError::Failed(format!("{what}: reading stderr: {e}")))?;
            Ok(ProcOut {
                stdout,
                stdout_total,
                stderr,
                code: status.code(),
            })
        }
        Err(_) => {
            // kill_on_drop also covers this; start_kill makes it happen now. A program
            // started through cmd.exe (a .cmd file) may leave grandchildren behind.
            let _ = child.start_kill();
            Err(ToolError::Failed(format!(
                "{what}: no result within {} s; killed",
                timeout.as_secs()
            )))
        }
    }
}

/// Reads to EOF, keeping the first `cap` bytes and counting the rest.
async fn read_capped<R: AsyncRead + Unpin>(
    r: Option<R>,
    cap: usize,
) -> std::io::Result<(Vec<u8>, u64)> {
    let Some(mut r) = r else {
        return Ok((Vec::new(), 0));
    };
    let mut kept = Vec::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = r.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        total += n as u64;
        let room = cap.saturating_sub(kept.len());
        kept.extend_from_slice(&buf[..n.min(room)]);
    }
    Ok((kept, total))
}
