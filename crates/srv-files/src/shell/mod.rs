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
                sanitize_env(c.as_std_mut());
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
                sanitize_env(c.as_std_mut());
                Ok(c)
            }
        }
    }
}

/// Inherited variables removed before bash starts (D-523 (5), D-525): bash startup files
/// and exported functions, `cd` search paths, xtrace prompts, word splitting; and every
/// variable that points git, curl, wget or gh at configuration, a program or another
/// repository than the classifier checked (a write to the file one names would otherwise
/// be an ordinary write).
const REMOVED_VARS: &[&str] = &[
    "BASH_ENV",
    "ENV",
    "CDPATH",
    "GLOBIGNORE",
    "BASHOPTS",
    "SHELLOPTS",
    "PROMPT_COMMAND",
    "PS4",
    "IFS",
    "XDG_CONFIG_HOME",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_EXEC_PATH",
    "GIT_TEMPLATE_DIR",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_PAGER",
    "GIT_EXTERNAL_DIFF",
    "GIT_ASKPASS",
    "GIT_PROXY_COMMAND",
    "SSH_ASKPASS",
    "PAGER",
    "EDITOR",
    "VISUAL",
    "BROWSER",
    "GH_PAGER",
    "GH_EDITOR",
    "GH_BROWSER",
    "CURL_HOME",
    "WGETRC",
    "NETRC",
];

/// Prefixes of removed variables: exported bash functions (which can shadow `ls`), git
/// configuration given by environment (`GIT_CONFIG`, `_GLOBAL`, `_SYSTEM`, `_NOSYSTEM`,
/// `_COUNT`, `_KEY_n`, `_VALUE_n`, `_PARAMETERS`) and the ssh program (`GIT_SSH*`).
const REMOVED_PREFIXES: &[&str] = &["BASH_FUNC_", "GIT_CONFIG", "GIT_SSH"];

/// Cleans the environment bash starts with (see [`REMOVED_VARS`]) and keeps only
/// absolute PATH entries: a relative or empty one is a folder relative to wherever bash
/// is, so a file written anywhere could become a program (D-525).
pub(crate) fn sanitize_env(c: &mut std::process::Command) {
    sanitize_env_with(c, std::env::var_os("PATH"));
}

/// [`sanitize_env`] with a given harness PATH.
fn sanitize_env_with(c: &mut std::process::Command, path: Option<std::ffi::OsString>) {
    for k in REMOVED_VARS {
        c.env_remove(k);
    }
    for (k, _) in std::env::vars_os() {
        let upper = k.to_string_lossy().to_ascii_uppercase();
        if REMOVED_PREFIXES.iter().any(|p| upper.starts_with(p)) {
            c.env_remove(k);
        }
    }
    if let Some(path) = path {
        let kept: Vec<PathBuf> = std::env::split_paths(&path)
            .filter(|e| e.is_absolute())
            .collect();
        if let Ok(joined) = std::env::join_paths(kept) {
            c.env("PATH", joined);
        }
    }
}

/// The PATH the runner's bash runs with (see [`bash_path_of`]); `None` without bash.
pub fn bash_path() -> Option<BashPath> {
    match Runner::discover() {
        Runner::Bash(b) => Some(bash_path_of(&b)),
        Runner::Fallback(_) => None,
    }
}

/// Folders bash looks programs up in.
#[derive(Debug, Default)]
pub struct BashPath {
    /// Absolute folders, as Windows paths on Windows.
    pub dirs: Vec<PathBuf>,
    /// The PATH had a relative or empty entry (see [`crate::paths::PathDirs`]).
    pub relative: bool,
}

/// Longest wait for the PATH probe; a bash that does not answer by then leaves the Git
/// folders guessed from its location.
const PROBE_CAP: Duration = Duration::from_secs(10);

/// Asks `bash` (started as the runner starts it: `--noprofile --norc`, cleaned
/// environment) for its PATH, converted to Windows folders by `cygpath -w -p` (else by
/// mapping `/c/...` and the MSYS root by hand). Git Bash's `bin/bash.exe` puts
/// `/mingw64/bin`, `/usr/bin` and `$USERPROFILE/bin` in front of the PATH it was given,
/// so the harness PATH alone misses them (D-525). The Git folders next to `bash` are
/// added whatever the probe says.
pub fn bash_path_of(bash: &Path) -> BashPath {
    bash_path_with(bash, std::env::var_os("PATH"))
}

/// [`bash_path_of`] for a given harness PATH.
fn bash_path_with(bash: &Path, harness_path: Option<std::ffi::OsString>) -> BashPath {
    let root = msys_root(bash);
    let mut out = BashPath::default();
    if let Some(r) = &root {
        for sub in ["mingw64/bin", "usr/bin", "usr/local/bin", "bin", "cmd"] {
            out.dirs.push(r.join(sub));
        }
    }
    let mut c = std::process::Command::new(bash);
    c.args([
        "--noprofile",
        "--norc",
        "-c",
        "printf '%s\\n' \"$PATH\"; cygpath -w -p \"$PATH\" 2>/dev/null; cygpath -w / 2>/dev/null",
    ]);
    sanitize_env_with(&mut c, harness_path);
    let Some(text) = probe_output(c) else {
        return out;
    };
    let mut lines = text.lines();
    let posix = lines.next().unwrap_or("");
    out.relative = posix
        .split(':')
        .any(|e| e.is_empty() || !e.starts_with('/'));
    if !cfg!(windows) {
        out.dirs.extend(
            posix
                .split(':')
                .filter(|e| e.starts_with('/'))
                .map(PathBuf::from),
        );
        return out;
    }
    let windows = lines.next().unwrap_or("").trim();
    let root = lines
        .next()
        .map(|r| PathBuf::from(r.trim()))
        .filter(|r| r.is_absolute())
        .or(root);
    if !windows.is_empty() {
        out.dirs.extend(
            windows
                .split(';')
                .map(PathBuf::from)
                .filter(|p| p.is_absolute()),
        );
    } else {
        for e in posix.split(':').filter(|e| e.starts_with('/')) {
            let mapped = paths::msys_drive(e);
            if Path::new(&mapped).is_absolute() {
                out.dirs.push(PathBuf::from(mapped));
            } else if let Some(r) = &root {
                let rest = e.trim_start_matches('/');
                // MSYS mounts /bin on /usr/bin.
                let rest = rest
                    .strip_prefix("bin")
                    .map_or(rest.to_string(), |r| format!("usr/bin{r}"));
                out.dirs.push(r.join(rest));
            }
        }
    }
    out
}

/// The MSYS root of a Git Bash `bash.exe`: `<Git>` for `<Git>/bin/bash.exe` and
/// `<Git>/usr/bin/bash.exe`. `None` elsewhere.
fn msys_root(bash: &Path) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let bin = bash.parent()?;
    if !bin.file_name()?.eq_ignore_ascii_case("bin") {
        return None;
    }
    let up = bin.parent()?;
    if up.file_name()?.eq_ignore_ascii_case("usr") {
        up.parent().map(Path::to_path_buf)
    } else {
        Some(up.to_path_buf())
    }
}

/// Stdout of `c`, read to its end within [`PROBE_CAP`]; the process is killed after.
fn probe_output(mut c: std::process::Command) -> Option<String> {
    use std::io::Read;
    c.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = c.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    let got = rx.recv_timeout(PROBE_CAP).ok();
    let _ = child.kill();
    let _ = child.wait();
    got
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
    // The one write check every sink goes through (D-525): files/ must still be scratch.
    util::own_output_ok(roots, &path)?;
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
        for k in [
            "BASH_ENV",
            "ENV",
            "CDPATH",
            "PS4",
            "IFS",
            // D-525: variables that point git, curl, wget at config or a program.
            "XDG_CONFIG_HOME",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_EXEC_PATH",
            "GIT_EDITOR",
            "GIT_PAGER",
            "PAGER",
            "CURL_HOME",
            "WGETRC",
            "NETRC",
        ] {
            assert!(removed.iter().any(|r| r == k), "{k} not removed");
        }
        assert!(!removed.iter().any(|r| r == "HOME"), "HOME is kept");
    }

    /// D-525: relative and empty PATH entries never reach bash.
    #[test]
    fn bash_path_keeps_only_absolute_entries() {
        let abs = std::env::temp_dir();
        let path =
            std::env::join_paths([abs.clone(), PathBuf::from("rel"), PathBuf::new()]).unwrap();
        let mut c = std::process::Command::new("bash");
        sanitize_env_with(&mut c, Some(path));
        let set = c
            .get_envs()
            .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
            .and_then(|(_, v)| v.map(|v| v.to_os_string()))
            .expect("PATH set");
        let kept: Vec<PathBuf> = std::env::split_paths(&set).collect();
        assert_eq!(kept, vec![abs]);
    }

    /// Review-T-14 closing Blocking 4 (D-525): the PATH check uses the PATH bash really
    /// has. Git Bash puts its own folders and `~/bin` in front of a harness PATH that
    /// lacks them, and a folder on the harness PATH stays on it.
    #[test]
    fn bash_path_includes_what_git_bash_adds_and_a_planted_dir() {
        let Runner::Bash(bash) = Runner::discover() else {
            eprintln!("skipped: no bash on this machine");
            return;
        };
        let planted = tempfile::tempdir().unwrap();
        let system = if cfg!(windows) {
            PathBuf::from(std::env::var_os("SystemRoot").unwrap_or("C:\\Windows".into()))
                .join("System32")
        } else {
            PathBuf::from("/usr/bin")
        };
        let harness = std::env::join_paths([system, planted.path().to_path_buf()]).unwrap();
        let got = bash_path_with(&bash, Some(harness));
        assert!(!got.relative, "{got:?}");
        let dirs = paths::PathDirs::from_entries(&got.dirs, None, got.relative);
        let names = |p: &Path| paths::lower_names(&std::fs::canonicalize(p).unwrap());
        assert!(
            dirs.contains(&names(planted.path())),
            "planted dir missing: {got:?}"
        );
        if cfg!(windows) && msys_root(&bash).is_some() {
            let home_bin = dirs::home_dir().unwrap().join("bin");
            let home_bin_names = paths::lower_names(
                &std::fs::canonicalize(&home_bin)
                    .unwrap_or_else(|_| std::path::absolute(&home_bin).unwrap()),
            );
            assert!(
                dirs.contains(&home_bin_names),
                "Git Bash adds ~/bin: {got:?}"
            );
            let usr_bin = msys_root(&bash).unwrap().join("usr").join("bin");
            assert!(dirs.contains(&names(&usr_bin)), "{got:?}");
        }
        // The process-wide set always has ~/bin, whatever PATH the harness started with.
        let home_bin = dirs::home_dir().unwrap().join("bin");
        let home_bin_names = paths::lower_names(
            &std::fs::canonicalize(&home_bin)
                .unwrap_or_else(|_| std::path::absolute(&home_bin).unwrap()),
        );
        assert!(paths::path_dirs().contains(&home_bin_names));
    }
}
