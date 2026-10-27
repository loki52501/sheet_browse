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

    /// The git a line's `git` resolves to: the one in the runner's Git install, else the
    /// first on this server's PATH.
    pub fn git_program(&self) -> Option<PathBuf> {
        if let Runner::Bash(b) = self {
            if let Some(root) = msys_root(b) {
                let found = [
                    root.join("mingw64").join("bin").join("git.exe"),
                    root.join("cmd").join("git.exe"),
                    root.join("bin").join("git.exe"),
                ]
                .into_iter()
                .find(|p| p.is_file());
                if found.is_some() {
                    return found;
                }
            }
        }
        proc::find_program("git", |_| false)
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

/// The MSYS roots of the Git installs this server's commands can reach: the runner's
/// bash and the `git` on this server's PATH (`<Git>/cmd/git.exe`, `<Git>/bin/git.exe`,
/// `<Git>/mingw64/bin/git.exe`). Windows only.
fn msys_roots() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Runner::Bash(b) = Runner::discover() {
        out.extend(msys_root(&b));
    }
    if cfg!(windows) {
        if let Some(git) = proc::find_program("git", |_| false) {
            out.extend(git_root(&git));
        }
    }
    out.dedup();
    out
}

/// `<Git>` for a Git for Windows `git.exe` in `cmd/`, `bin/` or `mingw64/bin/`.
fn git_root(git: &Path) -> Option<PathBuf> {
    let dir = git.parent()?;
    let name = dir.file_name()?.to_string_lossy().to_ascii_lowercase();
    let up = dir.parent()?;
    match name.as_str() {
        "cmd" => Some(up.to_path_buf()),
        "bin" => {
            let upname = up.file_name()?.to_string_lossy().to_ascii_lowercase();
            if upname == "mingw64" || upname == "usr" {
                up.parent().map(Path::to_path_buf)
            } else {
                Some(up.to_path_buf())
            }
        }
        _ => None,
    }
}

/// Folders whose every file is Git's own configuration (D-544): `<msys root>/etc`
/// (`gitconfig`, `gitattributes`, bash's profile files) and `<msys root>/mingw64/etc`
/// for each root of [`msys_roots`], and `%ProgramData%\Git` (older Git for Windows reads
/// `config` there). A write below them is Delete like any other config write.
pub fn git_config_dirs() -> Vec<PathBuf> {
    git_config_dirs_of(&msys_roots())
}

/// [`git_config_dirs`] for given roots.
pub fn git_config_dirs_of(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for r in roots {
        out.push(r.join("etc"));
        out.push(r.join("mingw64").join("etc"));
    }
    if cfg!(windows) {
        if let Some(pd) = std::env::var_os("ProgramData").filter(|p| !p.is_empty()) {
            out.push(PathBuf::from(pd).join("Git"));
        }
    }
    out
}

/// Longest wait for each `git config --list` the seal reads.
const SEAL_CAP: Duration = Duration::from_secs(5);

/// Keys copied from the system and global config into a sealed git (D-547): settings
/// that change what git reads or prints (line endings, symlinks, case, which folders
/// are safe, colours, formats) and never name a program.
const KEPT_KEYS: &[&str] = &[
    "core.autocrlf",
    "core.eol",
    "core.safecrlf",
    "core.symlinks",
    "core.ignorecase",
    "core.fscache",
    "core.longpaths",
    "core.quotepath",
    "core.precomposeunicode",
    "core.checkstat",
    "core.trustctime",
    "core.filemode",
    "core.untrackedcache",
    "core.preloadindex",
    "core.whitespace",
    "core.abbrev",
    "core.commentchar",
    "core.commentstring",
    "core.protecthfs",
    "core.protectntfs",
    "core.bigfilethreshold",
    "core.excludesfile",
    "safe.directory",
    "safe.barerepository",
    "user.name",
    "user.email",
    "init.defaultbranch",
    "http.sslbackend",
    "http.sslcainfo",
    "http.sslcapath",
    "http.schannelcheckrevoke",
    "http.schannelusesslcainfo",
    "windows.appendatomically",
    "i18n.commitencoding",
    "i18n.logoutputencoding",
    "diff.renames",
    "diff.algorithm",
    "diff.mnemonicprefix",
    "diff.noprefix",
    "diff.renamelimit",
    "diff.indentheuristic",
    "diff.colormoved",
    "diff.context",
    "diff.interhunkcontext",
    "diff.relative",
    "diff.statgraphwidth",
    "diff.wserrorhighlight",
    "status.showuntrackedfiles",
    "status.relativepaths",
    "status.short",
    "status.branch",
    "status.renames",
    "status.renamelimit",
    "status.aheadbehind",
    "status.showstash",
    "log.date",
    "log.decorate",
    "log.abbrevcommit",
    "log.follow",
    "log.mailmap",
    "log.showroot",
    "format.pretty",
    "branch.sort",
    "tag.sort",
    "column.ui",
    "grep.linenumber",
    "grep.patterntype",
    "grep.extendedregexp",
    "grep.fullname",
    "grep.column",
    "feature.manyfiles",
    "index.version",
];
/// Key prefixes copied the same way (pure output settings).
const KEPT_PREFIXES: &[&str] = &["color.", "pretty.", "advice."];

/// How a line whose every git command is Read runs git (D-547): without the system and
/// global config files (`GIT_CONFIG_NOSYSTEM`, `GIT_CONFIG_GLOBAL` = an empty file, no
/// system attributes), with only their data settings ([`KEPT_KEYS`]) copied back, and
/// with command-line config, which beats every config file and reaches submodules,
/// switching off what a repository's own config can make a read run: hooks
/// (`core.hooksPath` = an empty folder), `core.fsmonitor`, `core.attributesFile`,
/// `core.sshCommand`, credential helpers, askpass, the `ext::` transport, signature
/// display and submodule recursion. Filter and diff drivers are named per repository,
/// so the classifier makes a Read git in a repository whose config names one Delete
/// instead (`classify::unsealable_config`).
pub struct GitSeal {
    dir: tempfile::TempDir,
    kept: Vec<(String, String)>,
}

impl GitSeal {
    /// `git`: the git whose system and global config is read for [`KEPT_KEYS`] (none:
    /// nothing is copied). `home`: HOME for that read (tests; the runner keeps HOME).
    pub fn new(git: Option<&Path>, home: Option<&Path>) -> Result<GitSeal, String> {
        let dir = tempfile::Builder::new()
            .prefix("sb-gitseal-")
            .tempdir()
            .map_err(|e| format!("temp folder: {e}"))?;
        std::fs::create_dir(dir.path().join("hooks")).map_err(|e| e.to_string())?;
        std::fs::write(dir.path().join("global"), "").map_err(|e| e.to_string())?;
        std::fs::write(dir.path().join("attributes"), "").map_err(|e| e.to_string())?;
        let mut kept = Vec::new();
        if let Some(git) = git {
            for scope in ["--system", "--global"] {
                let mut c = std::process::Command::new(git);
                c.args(["config", scope, "--list", "-z"])
                    .current_dir(dir.path());
                sanitize_env(&mut c);
                if let Some(h) = home {
                    c.env("HOME", h);
                }
                if let Some(out) = probe_output_within(c, SEAL_CAP) {
                    kept.extend(kept_entries(&out));
                }
            }
        }
        Ok(GitSeal { dir, kept })
    }

    fn file(&self, name: &str) -> String {
        paths::plain(&self.dir.path().join(name)).replace('\\', "/")
    }

    /// Sets the sealed environment on `c` (after [`sanitize_env`], which removed every
    /// inherited `GIT_CONFIG*`).
    pub fn apply(&self, c: &mut std::process::Command) {
        let mut pairs: Vec<(String, String)> = self.kept.clone();
        for (k, v) in [
            ("core.hooksPath", self.file("hooks")),
            ("core.fsmonitor", "false".into()),
            ("core.attributesFile", self.file("attributes")),
            ("core.sshCommand", "ssh".into()),
            ("credential.helper", String::new()),
            ("core.askPass", String::new()),
            ("protocol.ext.allow", "never".into()),
            ("diff.ignoreSubmodules", "all".into()),
            ("status.submoduleSummary", "false".into()),
            ("submodule.recurse", "false".into()),
            ("log.showSignature", "false".into()),
        ] {
            pairs.push((k.into(), v));
        }
        c.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.file("global"))
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_COUNT", pairs.len().to_string());
        for (i, (k, v)) in pairs.iter().enumerate() {
            c.env(format!("GIT_CONFIG_KEY_{i}"), k);
            c.env(format!("GIT_CONFIG_VALUE_{i}"), v);
        }
    }
}

/// The [`KEPT_KEYS`] entries of `git config --list -z` output (`key\nvalue\0`...).
fn kept_entries(out: &str) -> Vec<(String, String)> {
    out.split('\0')
        .filter(|e| !e.is_empty())
        .filter_map(|e| {
            let (k, v) = e.split_once('\n').unwrap_or((e, "true"));
            let lower = k.to_ascii_lowercase();
            let kept = KEPT_KEYS.contains(&lower.as_str())
                || KEPT_PREFIXES.iter().any(|p| lower.starts_with(p));
            kept.then(|| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// Stdout of `c`, read to its end within [`PROBE_CAP`]; the process is killed after.
fn probe_output(c: std::process::Command) -> Option<String> {
    probe_output_within(c, PROBE_CAP)
}

/// Stdout of `c`, read to its end within `cap`; the process is killed after.
fn probe_output_within(mut c: std::process::Command, cap: Duration) -> Option<String> {
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
    let got = rx.recv_timeout(cap).ok();
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
        // D-547: a line whose git is all Read runs it without system/global config,
        // hooks or fsmonitor. The seal's folder lives until the process has ended.
        let seal = if v.seal_git() {
            let git = self.runner.git_program();
            let made = tokio::task::spawn_blocking(move || GitSeal::new(git.as_deref(), None))
                .await
                .map_err(|e| ToolError::Failed(format!("git seal: {e}")))?;
            Some(made.map_err(|e| ToolError::Failed(format!("not run: git seal: {e}")))?)
        } else {
            None
        };
        let mut cmd = self.runner.command(&a.cmd)?;
        if let Some(s) = &seal {
            s.apply(cmd.as_std_mut());
        }
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
    crate::safeio::write(&path, s.as_bytes()).map_err(|e| e.to_string())?;
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
        let bin_names = |h: &Path| {
            let b = h.join("bin");
            paths::lower_names(
                &std::fs::canonicalize(&b).unwrap_or_else(|_| std::path::absolute(&b).unwrap()),
            )
        };
        if cfg!(windows) && msys_root(&bash).is_some() {
            // Git Bash adds $HOME/bin, which is the profile folder's bin only when HOME
            // is unset or equal to it (review-T-14 confirm, should-fix 1).
            assert!(
                paths::home_dirs()
                    .iter()
                    .any(|h| dirs.contains(&bin_names(h))),
                "Git Bash adds $HOME/bin: {got:?}"
            );
            let usr_bin = msys_root(&bash).unwrap().join("usr").join("bin");
            assert!(dirs.contains(&names(&usr_bin)), "{got:?}");
        }
        // The process-wide set has <h>/bin for every home folder, whatever PATH the
        // harness started with (D-543).
        for h in paths::home_dirs() {
            assert!(paths::path_dirs().contains(&bin_names(&h)), "{h:?}");
        }
    }

    /// D-543 (review-T-14 confirm, should-fix 1): when the PATH probe cannot run bash,
    /// `<home>/bin` is still a PATH folder for every home folder, HOME included when it
    /// is not the profile folder.
    #[test]
    fn home_bin_is_a_path_folder_when_the_probe_fails() {
        let t = tempfile::tempdir().unwrap();
        let fake_home = t.path().join("fakehome");
        std::fs::create_dir_all(fake_home.join("bin")).unwrap();
        let missing_bash = t.path().join("no-such-git").join("bin").join("bash.exe");
        let probed = bash_path_with(&missing_bash, None);
        assert!(!probed.relative, "{probed:?}");
        let dirs = paths::path_dirs_from(
            None,
            None,
            std::slice::from_ref(&fake_home),
            Some(probed),
            None,
        );
        let jq = fake_home.join("bin").join("jq");
        let run = t
            .path()
            .join("base")
            .join("runs")
            .join("2026-10-25T00-00-00-t");
        std::fs::create_dir_all(run.join("files")).unwrap();
        let roots = Roots::from_run_dir(&run).unwrap();
        let jq = paths::resolve(&roots.base, &paths::plain(&jq)).unwrap();
        assert!(paths::code_path(&roots, &jq, &[], &dirs), "{dirs:?}");
        // Without the home entry the same file is not a PATH folder (the check can fail).
        let none = paths::path_dirs_from(None, None, &[], None, None);
        assert!(!paths::code_path(&roots, &jq, &[], &none));
    }

    /// D-544: Git's own system config folders, from bash's or git's install root.
    #[test]
    fn git_config_dirs_cover_etc_and_mingw64_etc() {
        let root = PathBuf::from(if cfg!(windows) { "C:\\Git" } else { "/opt/git" });
        let got = git_config_dirs_of(std::slice::from_ref(&root));
        assert!(got.contains(&root.join("etc")));
        assert!(got.contains(&root.join("mingw64").join("etc")));
        assert_eq!(
            git_root(&root.join("cmd").join("git.exe")),
            Some(root.clone())
        );
        assert_eq!(
            git_root(&root.join("mingw64").join("bin").join("git.exe")),
            Some(root.clone())
        );
        assert_eq!(git_root(&root.join("bin").join("git.exe")), Some(root));
        if cfg!(windows) {
            if let Runner::Bash(b) = Runner::discover() {
                if let Some(r) = msys_root(&b) {
                    let etc = paths::lower_names(
                        &std::fs::canonicalize(r.join("etc")).unwrap_or(r.join("etc")),
                    );
                    assert!(paths::path_dirs().trees.contains(&etc), "{r:?}");
                }
            }
        }
    }
}
