//! Path resolution and the runs/ boundary (F-1, F-2; D-520).
//!
//! Every path a tool touches is resolved the way the OS will resolve it before any class
//! is decided: relative paths join the base folder (the folder that holds `runs/`),
//! Windows paths go through Win32 normalisation (`..`, `/`, trailing dots and spaces),
//! and every existing component is canonicalised, so `runs/../x`, a symlink or junction
//! inside `runs/` that points outside, and another spelling of the same folder all land
//! on the real target. Only the part of a path that does not exist yet is kept as
//! written (it cannot be a link).
//!
//! [`place`] then says whether a resolved path is scratch (inside `runs/`, safe to change
//! without asking), bookkeeping (the runs root, a run folder itself, or a run's `asks/`,
//! `ledger.jsonl`, `report.md`, `prompt.md`: the user's answer channel and the evidence,
//! never scratch) or outside. Writing `asks/<n>.answer` would let a parallel call answer
//! the run's own pending ask, so bookkeeping is treated like outside.

use harness_core::run::is_run_id;
use harness_core::{ToolCtx, ToolError};
use std::io;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::OnceLock;

/// Entries of a run folder that are never scratch (H-3, H-4, the FileAsker channel).
const BOOKKEEPING: [&str; 4] = ["asks", "ledger.jsonl", "report.md", "prompt.md"];

/// The folders a call resolves against, all canonical.
#[derive(Debug, Clone)]
pub struct Roots {
    /// Where relative paths resolve: the folder that holds `runs/`.
    pub base: PathBuf,
    /// The harness runs root (`Harness::runs_root`).
    pub runs: PathBuf,
    /// This run's `files/` (deliverables).
    pub files: PathBuf,
}

impl Roots {
    /// From the call's run folder (`ctx.run_dir` is `<runs root>/<run id>`).
    pub fn of(ctx: &ToolCtx) -> Result<Roots, ToolError> {
        Roots::from_run_dir(&ctx.run_dir).map_err(|e| ToolError::Failed(format!("run folder: {e}")))
    }

    pub fn from_run_dir(run_dir: &Path) -> io::Result<Roots> {
        let run = std::fs::canonicalize(run_dir)?;
        let runs = run
            .parent()
            .ok_or_else(|| io::Error::other("run folder has no parent"))?
            .to_path_buf();
        let base = runs.parent().map(Path::to_path_buf).unwrap_or(runs.clone());
        Ok(Roots {
            base,
            runs,
            files: run.join("files"),
        })
    }

    /// Resolves `raw` against [`Roots::base`].
    pub fn resolve(&self, raw: &str) -> Result<PathBuf, ToolError> {
        self.resolve_in(&self.base, raw)
    }

    /// Resolves `raw` against `dir`.
    pub fn resolve_in(&self, dir: &Path, raw: &str) -> Result<PathBuf, ToolError> {
        resolve(dir, raw).map_err(|e| ToolError::Args(format!("path {raw:?}: {e}")))
    }

    /// Where a file a tool writes goes (B-564, D-570): this run's `files/<raw>` when `raw`
    /// is a bare file name ([`is_bare`]), else `raw` resolved against the base as written
    /// (`./x.csv`, `out/x.csv` and absolute paths keep their meaning). Every tool that
    /// names an output file uses this, in classify and in call alike.
    pub fn resolve_out(&self, raw: &str) -> Result<PathBuf, ToolError> {
        if is_bare(raw) {
            self.resolve_in(&self.files, raw)
        } else {
            self.resolve(raw)
        }
    }

    /// Where a file a tool reads (or moves or deletes) is (B-564, D-571): `raw` resolved
    /// against the base; a bare name that does not exist there but does exist in this
    /// run's `files/` is that file, so a session reads back what it wrote with the same
    /// name. When both exist the base wins (as before T-13c); [`Roots::shadowed`] names
    /// the other one.
    pub fn resolve_existing(&self, raw: &str) -> Result<PathBuf, ToolError> {
        let p = self.resolve(raw)?;
        if !is_bare(raw) || exists(&p) {
            return Ok(p);
        }
        let f = self.resolve_in(&self.files, raw)?;
        Ok(if exists(&f) { f } else { p })
    }

    /// For a bare `raw` that [`Roots::resolve_existing`] resolved to `p` in the base while
    /// this run's `files/<raw>` exists too: that file (D-571).
    pub fn shadowed(&self, raw: &str, p: &Path) -> Option<PathBuf> {
        if !is_bare(raw) || p.starts_with(&self.files) {
            return None;
        }
        let f = self.resolve_in(&self.files, raw).ok()?;
        (f != p && exists(&f)).then_some(f)
    }

    /// For text the model reads: relative to the base with `/`, else the plain path.
    pub fn shown(&self, p: &Path) -> String {
        match p.strip_prefix(&self.base) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().replace('\\', "/"),
            _ => plain(p),
        }
    }

    /// See [`place`].
    pub fn place(&self, p: &Path) -> Place {
        place(self, p)
    }
}

/// True when `raw` is a bare file name: one plain component with no folder part, root,
/// drive, `.`, `..`, `~` or trailing separator (`report.csv`; not `./report.csv`,
/// `out/report.csv`, `archive/`, `C:x`).
pub fn is_bare(raw: &str) -> bool {
    if raw.starts_with('~') || raw.ends_with('/') || raw.ends_with('\\') {
        return false;
    }
    let mut comps = Path::new(raw).components();
    matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none()
}

/// Where a resolved path is, for the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Strictly inside `runs/` and not bookkeeping.
    Scratch,
    /// The runs root, a run folder itself, or a run's asks/ledger/report/prompt.
    Bookkeeping,
    Outside,
}

impl Place {
    pub fn scratch(self) -> bool {
        self == Place::Scratch
    }
}

/// See the module docs. `p` must come from [`resolve`]. A file with more than one hard
/// link is outside: its other names may be outside `runs/`, and writing it writes them
/// (B7, D-523).
pub fn place(roots: &Roots, p: &Path) -> Place {
    let Ok(rest) = p.strip_prefix(&roots.runs) else {
        return Place::Outside;
    };
    if hard_linked(p) {
        return Place::Outside;
    }
    let comps: Vec<String> = rest
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let Some(first) = comps.first() else {
        return Place::Bookkeeping;
    };
    let folder = roots.runs.join(first);
    let is_run = is_run_id(first)
        || exists(&folder.join("ledger.jsonl"))
        || exists(&folder.join("asks"))
        || exists(&folder.join("prompt.md"));
    if is_run {
        match comps.get(1) {
            None => return Place::Bookkeeping,
            Some(second) => {
                let s = second.to_ascii_lowercase();
                if BOOKKEEPING.contains(&s.as_str()) {
                    return Place::Bookkeeping;
                }
            }
        }
    }
    Place::Scratch
}

/// Dot folders that hold configuration a later program runs code from, wherever they are
/// (project-local ones included): git hooks and config, cargo runners and aliases, the
/// XDG config tree (`~/.config/git/config`, `~/.config/git/attributes`), ssh, editors,
/// git hook managers. `start menu` holds the machine-wide Startup folder too.
const CODE_DIRS: &[&str] = &[
    ".git",
    ".cargo",
    ".config",
    ".ssh",
    ".husky",
    ".vscode",
    ".devcontainer",
    ".github",
    ".local",
    "start menu",
];

/// Folders below the home folder whose files a program loads at login or start (D-525):
/// app configuration (`AppData\Roaming`), the per-user programs folder on PATH
/// (`AppData\Local\Microsoft\WindowsApps`).
const CODE_HOME_PREFIXES: &[&[&str]] = &[
    &["appdata", "roaming"],
    &["appdata", "local", "microsoft", "windowsapps"],
];

/// Folder names below the home folder that hold startup items or PowerShell profiles
/// (`Documents\WindowsPowerShell`, `Documents\PowerShell`, a redirected `Documents` too).
const CODE_HOME_NAMES: &[&str] = &["startup", "start menu"];
const PROFILE_DIRS: &[&str] = &["windowspowershell", "powershell"];

/// True when writing `p` can make a program run code later, so the write is Delete
/// wherever it is (D-523 (8), D-524, narrowed by D-525):
/// * any dotfile (`.gitattributes`, `.gitmodules`, `.gitconfig`, `.curlrc`, `.wgetrc`,
///   `.bashrc`, `.profile`, `.zshrc`, `.npmrc`, `.netrc`, `.envrc`, ...) and the Windows
///   `_netrc` / `_curlrc` spellings, anywhere;
/// * anything inside a [`CODE_DIRS`] folder. For a path inside the base folder (the one
///   that holds `runs/`) only the part below the base counts: the user chose where the
///   base is, so a base below `~/.wt` or `~/.local/src` does not make every write ask;
/// * below the home folder, for paths outside the base folder (or every path when the
///   base is the home folder or above it): a top-level dot folder (`~/.x/...`), the
///   [`CODE_HOME_PREFIXES`], a [`CODE_HOME_NAMES`] folder, `Documents\<profile dir>`;
/// * a file in a folder on the PATH that bash, the cmd fallback or this server use
///   ([`path_dirs`]), or any file at all when that PATH has a relative entry.
///
/// Fails closed: a dotfile inside `runs/` is Delete too, rather than keep a list of the
/// startup files each program reads.
pub fn runs_code_on_use(roots: &Roots, p: &Path) -> bool {
    code_path(roots, p, homes(), path_dirs())
}

/// [`runs_code_on_use`] with given home folders ([`lower_names`]) and search folders.
pub fn code_path(roots: &Roots, p: &Path, homes: &[Vec<String>], path: &PathDirs) -> bool {
    let names = lower_names(p);
    let Some((file, dirs)) = names.split_last() else {
        return false;
    };
    if dot_name(file) || matches!(file.as_str(), "_netrc" | "_curlrc") {
        return true;
    }
    let base = lower_names(&roots.base);
    let in_base = names.len() > base.len() && names.starts_with(&base);
    let below_base = if in_base { &dirs[base.len()..] } else { dirs };
    if below_base.iter().any(|d| CODE_DIRS.contains(&d.as_str())) {
        return true;
    }
    for home in homes {
        let base_below_home = base.len() > home.len() && base.starts_with(home);
        let in_home = names.len() > home.len() && names.starts_with(home);
        if in_home && !(in_base && base_below_home) && home_code(&dirs[home.len()..]) {
            return true;
        }
    }
    !dirs.is_empty() && path.contains(dirs)
}

fn dot_name(n: &str) -> bool {
    n.starts_with('.') && n != "." && n != ".."
}

/// `below`: the folders between the home folder and the file.
fn home_code(below: &[String]) -> bool {
    below.first().is_some_and(|d| dot_name(d))
        || CODE_HOME_PREFIXES.iter().any(|pre| {
            below.len() >= pre.len() && below.iter().zip(pre.iter()).all(|(a, b)| a == b)
        })
        || below.iter().any(|d| CODE_HOME_NAMES.contains(&d.as_str()))
        || below
            .windows(2)
            .any(|w| w[0] == "documents" && PROFILE_DIRS.contains(&w[1].as_str()))
}

/// The home folders' names, canonical where they exist: the user's profile folder and,
/// when set to another folder, `HOME` (where git and Git Bash look for `~`).
pub fn homes() -> &'static [Vec<String>] {
    static HOMES: OnceLock<Vec<Vec<String>>> = OnceLock::new();
    HOMES.get_or_init(|| {
        let mut out: Vec<Vec<String>> = Vec::new();
        let cands = [
            dirs::home_dir(),
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(PathBuf::from)
                .filter(|h| h.is_absolute()),
        ];
        for h in cands.into_iter().flatten() {
            let n = lower_names(&canonical_or_absolute(&h));
            if n.len() > 1 && !out.contains(&n) {
                out.push(n);
            }
        }
        out
    })
}

/// The components of `p`, ASCII-lowercased (Windows paths are case-insensitive), the
/// drive as `c:` whether or not the path is verbatim, and `.`/`..`/root dropped.
pub(crate) fn lower_names(p: &Path) -> Vec<String> {
    p.components()
        .filter_map(|c| match c {
            Component::Prefix(pre) => Some(match pre.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                    format!("{}:", (d as char).to_ascii_lowercase())
                }
                Prefix::UNC(s, h) | Prefix::VerbatimUNC(s, h) => format!(
                    "//{}/{}",
                    s.to_string_lossy().to_ascii_lowercase(),
                    h.to_string_lossy().to_ascii_lowercase()
                ),
                _ => pre.as_os_str().to_string_lossy().to_ascii_lowercase(),
            }),
            Component::Normal(n) => Some(n.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

/// `p` canonicalised (8.3 names expanded, junctions and links followed) when it exists,
/// else made absolute (Win32 normalisation of `..`, trailing dots and spaces).
fn canonical_or_absolute(p: &Path) -> PathBuf {
    std::fs::canonicalize(p)
        .or_else(|_| std::path::absolute(p))
        .unwrap_or_else(|_| p.to_path_buf())
}

/// The folders where a bare program name is looked up, by any process this server starts.
#[derive(Debug, Default)]
pub struct PathDirs {
    /// Each folder's [`lower_names`], canonical where it exists.
    pub dirs: Vec<Vec<String>>,
    /// The bash PATH still had a relative or empty entry (a folder relative to wherever
    /// bash is at the time): then a file written anywhere could be run, so every write is
    /// Delete. [`crate::shell`] removes such entries before bash starts, so this only
    /// happens when bash adds one itself.
    pub relative: bool,
}

impl PathDirs {
    /// From folder lists: `absolute` as given, `relative` (from the harness PATH) joined to
    /// `cwd`, the process that searches them being this server, whose folder is fixed.
    pub fn from_entries(entries: &[PathBuf], cwd: Option<&Path>, relative: bool) -> PathDirs {
        let mut dirs: Vec<Vec<String>> = Vec::new();
        for e in entries {
            let e = if e.is_absolute() {
                e.clone()
            } else if let Some(c) = cwd {
                c.join(e)
            } else {
                continue;
            };
            let n = lower_names(&canonical_or_absolute(&e));
            if !n.is_empty() && !dirs.contains(&n) {
                dirs.push(n);
            }
        }
        PathDirs { dirs, relative }
    }

    pub fn contains(&self, dir: &[String]) -> bool {
        self.relative || self.dirs.iter().any(|d| d[..] == *dir)
    }
}

/// The program search folders, computed once (D-525): this server's own PATH (the cmd
/// fallback and the helpers it starts), with a relative or empty entry taken against its
/// working folder; the folder of this executable (Windows searches it first); `~/bin`;
/// and the PATH the runner's bash really has, which Git Bash builds by putting
/// `/mingw64/bin`, `/usr/bin` and `$USERPROFILE/bin` in front ([`crate::shell::bash_path`]).
pub fn path_dirs() -> &'static PathDirs {
    static DIRS: OnceLock<PathDirs> = OnceLock::new();
    DIRS.get_or_init(|| {
        let mut entries: Vec<PathBuf> = Vec::new();
        if let Some(p) = std::env::var_os("PATH") {
            entries.extend(std::env::split_paths(&p));
        }
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf))
        {
            entries.push(dir);
        }
        if let Some(h) = dirs::home_dir() {
            entries.push(h.join("bin"));
        }
        let mut relative = false;
        if let Some(b) = crate::shell::bash_path() {
            entries.extend(b.dirs);
            relative = b.relative;
        }
        let cwd = std::env::current_dir().ok();
        PathDirs::from_entries(&entries, cwd.as_deref(), relative)
    })
}

/// True when `p` is an existing file (links followed) with more than one hard link, or
/// a file whose link count cannot be read (fail closed). Folders are never hard-linked.
pub fn hard_linked(p: &Path) -> bool {
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() => link_count(p, &m).is_none_or(|n| n > 1),
        _ => false,
    }
}

#[cfg(unix)]
fn link_count(_p: &Path, m: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(m.nlink())
}

/// `nNumberOfLinks` from GetFileInformationByHandle (std's `number_of_links` is unstable).
#[cfg(windows)]
fn link_count(p: &Path, _m: &std::fs::Metadata) -> Option<u64> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    #[derive(Default)]
    struct ByHandleFileInformation {
        attributes: u32,
        creation: [u32; 2],
        last_access: [u32; 2],
        last_write: [u32; 2],
        volume_serial: u32,
        size_high: u32,
        size_low: u32,
        number_of_links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(
            file: *mut std::ffi::c_void,
            info: *mut ByHandleFileInformation,
        ) -> i32;
    }
    // Access 0: metadata only, so a file another process holds open still answers; std
    // shares read, write and delete by default.
    let f = std::fs::OpenOptions::new().access_mode(0).open(p).ok()?;
    let mut info = ByHandleFileInformation::default();
    // SAFETY: a live file handle owned by `f` and a correctly laid out out-struct.
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle() as *mut _, &mut info) };
    (ok != 0).then_some(u64::from(info.number_of_links))
}

/// True when `p` exists (a broken link counts).
pub fn exists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

/// True when `p` is the runs root or one of its ancestors: moving or removing it takes
/// `runs/` with it.
pub fn contains_runs(roots: &Roots, p: &Path) -> bool {
    roots.runs.starts_with(p)
}

/// True when `p` is a folder (links followed).
pub fn is_dir(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_dir()).unwrap_or(false)
}

/// Resolves `raw` (see the module docs). Refuses empty paths, NUL, `~user`, Windows
/// device and verbatim paths, and alternate data streams.
pub fn resolve(base: &Path, raw: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("empty path".into());
    }
    if raw.contains('\0') {
        return Err("NUL in path".into());
    }
    let expanded = msys_drive(&expand_tilde(raw)?);
    if cfg!(windows) {
        let norm = expanded.replace('/', "\\");
        if norm.starts_with("\\\\?\\") || norm.starts_with("\\\\.\\") {
            return Err("device and verbatim paths are not accepted".into());
        }
    }
    let p = Path::new(&expanded);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    // From the plain spelling: a `\\?\` path skips Win32 normalisation, so `asks.` or
    // `runs ` would stay distinct names here while the OS (and bash) strip the dot.
    #[cfg(windows)]
    let joined = {
        let abs = std::path::absolute(PathBuf::from(plain(&joined))).map_err(|e| e.to_string())?;
        for c in abs.components() {
            if let Component::Normal(n) = c {
                if n.to_string_lossy().contains(':') {
                    return Err("alternate data streams are not accepted".into());
                }
            }
        }
        abs
    };
    walk(&joined).map_err(|e| e.to_string())
}

/// Canonicalises every existing component in order (links resolved as the OS would);
/// the first missing component and everything after it are appended as written.
fn walk(p: &Path) -> io::Result<PathBuf> {
    let mut cur = PathBuf::new();
    let mut existing = true;
    for comp in p.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => cur.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                cur.pop();
            }
            Component::Normal(n) => {
                let next = cur.join(n);
                if !existing {
                    cur = next;
                    continue;
                }
                match std::fs::symlink_metadata(&next) {
                    Ok(_) => {
                        cur = std::fs::canonicalize(&next).map_err(|e| {
                            io::Error::new(e.kind(), format!("cannot resolve a link: {e}"))
                        })?
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        existing = false;
                        cur = next;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
    Ok(cur)
}

/// `~` and `~/x` become the home folder; `~user` is refused.
fn expand_tilde(raw: &str) -> Result<String, String> {
    let Some(rest) = raw.strip_prefix('~') else {
        return Ok(raw.to_string());
    };
    if !(rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\')) {
        return Err("~user paths are not accepted".into());
    }
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let home = std::env::var_os(var)
        .filter(|h| !h.is_empty())
        .ok_or_else(|| "no home folder".to_string())?;
    Ok(format!("{}{rest}", home.to_string_lossy()))
}

/// Git Bash spelling `/c/x` is `C:/x` on Windows; unchanged elsewhere.
pub fn msys_drive(raw: &str) -> String {
    if !cfg!(windows) {
        return raw.to_string();
    }
    let b = raw.as_bytes();
    if b.len() >= 2 && b[0] == b'/' && b[1].is_ascii_alphabetic() && (b.len() == 2 || b[2] == b'/')
    {
        let drive = (b[1] as char).to_ascii_uppercase();
        return format!("{drive}:/{}", raw.get(3..).unwrap_or(""));
    }
    raw.to_string()
}

/// A path without the Windows verbatim prefix.
pub fn plain(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else if let Some(rest) = s.strip_prefix("\\\\?\\") {
        rest.to_string()
    } else {
        s.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Roots) {
        let t = tempfile::tempdir().unwrap();
        let run = t.path().join("runs").join("2026-10-25T00-00-00-t");
        std::fs::create_dir_all(run.join("files")).unwrap();
        std::fs::create_dir_all(run.join("asks")).unwrap();
        std::fs::write(run.join("ledger.jsonl"), "").unwrap();
        let roots = Roots::from_run_dir(&run).unwrap();
        (t, roots)
    }

    #[test]
    fn places() {
        let (_t, r) = setup();
        let p = |s: &str| place(&r, &r.resolve(s).unwrap());
        assert_eq!(p("runs/x"), Place::Scratch);
        assert_eq!(p("runs/x/../y/z"), Place::Scratch);
        assert_eq!(p("runs"), Place::Bookkeeping);
        assert_eq!(p("runs/."), Place::Bookkeeping);
        assert_eq!(p("runs/../x"), Place::Outside);
        assert_eq!(p("runs/x/../../x"), Place::Outside);
        assert_eq!(p("file.txt"), Place::Outside);
        assert_eq!(p("runs/2026-10-25T00-00-00-t"), Place::Bookkeeping);
        assert_eq!(
            p("runs/2026-10-25T00-00-00-t/asks/1.answer"),
            Place::Bookkeeping
        );
        assert_eq!(
            p("runs/2026-10-25T00-00-00-t/ledger.jsonl"),
            Place::Bookkeeping
        );
        assert_eq!(p("runs/2026-10-25T00-00-00-t/files/a.csv"), Place::Scratch);
        assert_eq!(place(&r, &r.files.join("a")), Place::Scratch);
    }

    #[cfg(windows)]
    #[test]
    fn windows_spellings() {
        let (_t, r) = setup();
        let p = |s: &str| place(&r, &r.resolve(s).unwrap());
        assert_eq!(p("RUNS/x"), Place::Scratch);
        assert_eq!(p("runs\\x"), Place::Scratch);
        assert_eq!(p("runs./x"), Place::Scratch);
        assert_eq!(p("Runs/../x"), Place::Outside);
        assert_eq!(
            p("runs/2026-10-25T00-00-00-t/ASKS/1.answer"),
            Place::Bookkeeping
        );
        assert_eq!(
            p("runs/2026-10-25T00-00-00-t/asks./1.answer"),
            Place::Bookkeeping
        );
        assert!(r.resolve("runs/x:stream").is_err());
        assert!(r.resolve("\\\\?\\C:\\x").is_err());
        assert!(r.resolve("//./C:/x").is_err());
    }

    #[test]
    fn link_out_of_runs_is_outside() {
        let (t, r) = setup();
        let outside = t.path().join("precious");
        std::fs::create_dir_all(&outside).unwrap();
        let link = r.runs.join("link");
        if !make_link(&outside, &link) {
            eprintln!("skipped: cannot create a junction or symlink here");
            return;
        }
        assert_eq!(place(&r, &r.resolve("runs/link").unwrap()), Place::Outside);
        assert_eq!(
            place(&r, &r.resolve("runs/link/f.txt").unwrap()),
            Place::Outside
        );
    }

    /// A junction (Windows) or symlink (Unix) at `link` pointing to `target`.
    pub(crate) fn make_link(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                // mklink takes backslashes only.
                .arg(link.to_string_lossy().replace('/', "\\"))
                .arg(target.to_string_lossy().replace('/', "\\"))
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    /// Review-T-14 closing Blocking 5 (D-525): home rules hold outside the base folder
    /// only, AppData counts only for Roaming and WindowsApps, and a base below a home dot
    /// folder or `AppData\Local\Temp` does not make every write Delete. `home` stands in
    /// for the home folder.
    #[test]
    fn home_rules_are_narrow_and_skip_the_base() {
        let t = tempfile::tempdir().unwrap();
        let mk = |base: &Path| {
            let run = base.join("runs").join("2026-10-25T00-00-00-t");
            std::fs::create_dir_all(run.join("files")).unwrap();
            Roots::from_run_dir(&run).unwrap()
        };
        // Plain (not `\\?\`) so `a/b` joins split on `/` on Windows too.
        let canon_home =
            PathBuf::from(plain(&std::fs::canonicalize(t.path()).unwrap())).join("home");
        let homes = vec![lower_names(&canon_home)];
        let none = PathDirs::default();
        let code = |r: &Roots, p: &Path| code_path(r, p, &homes, &none);
        for base in [
            canon_home.join(".wt").join("repo"),
            canon_home.join(".local").join("src").join("repo"),
            canon_home
                .join("AppData")
                .join("Local")
                .join("Temp")
                .join(".tmpab"),
            canon_home.join("AppData").join("Roaming").join("proj"),
        ] {
            let r = mk(&base);
            for ok in [
                "runs/x/a.txt",
                "runs/x/out/b.csv",
                "notes.txt",
                "src/main.rs",
            ] {
                assert!(
                    !code(&r, &r.resolve(ok).unwrap()),
                    "{} in {}",
                    ok,
                    base.display()
                );
            }
            for bad in [
                ".git/config",
                "runs/x/.git/hooks/pre-commit",
                ".gitattributes",
                "runs/x/r/.config/git/config",
                ".bashrc",
            ] {
                assert!(
                    code(&r, &r.resolve(bad).unwrap()),
                    "{} in {}",
                    bad,
                    base.display()
                );
            }
            // Outside the base the home rules hold.
            for bad in [
                ".config/git/config",
                ".ssh/config",
                ".x/y/z.txt",
                "AppData/Roaming/Code/User/settings.json",
                "AppData/Local/Microsoft/WindowsApps/git.exe",
                "AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup/a.cmd",
                "Documents/PowerShell/Microsoft.PowerShell_profile.ps1",
                "Documents/WindowsPowerShell/profile.ps1",
                "OneDrive/Documents/WindowsPowerShell/profile.ps1",
            ] {
                assert!(
                    code(&r, &canon_home.join(bad)),
                    "{bad} with base {}",
                    base.display()
                );
            }
            for ok in [
                "AppData/Local/Temp/x/a.txt",
                "AppData/Local/Temp/.tmpzz/a.txt",
                "Documents/report.docx",
                "projects/startup-notes.txt",
            ] {
                assert!(
                    !code(&r, &canon_home.join(ok)),
                    "{ok} with base {}",
                    base.display()
                );
            }
        }
        // With the base at the home folder itself the home rules still hold inside it.
        let r = mk(&canon_home);
        assert!(code(&r, &canon_home.join("AppData/Roaming/x/y.json")));
        assert!(code(&r, &canon_home.join(".x/y.txt")));
        assert!(!code(&r, &r.resolve("runs/x/a.txt").unwrap()));
    }

    /// D-525: a PATH folder (and, with a relative PATH entry, any folder) is Delete.
    #[test]
    fn path_folders_and_relative_entries() {
        let (t, r) = setup();
        let bin = t.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let dirs = PathDirs::from_entries(std::slice::from_ref(&bin), None, false);
        let code = |p: &Path, d: &PathDirs| code_path(&r, p, &[], d);
        assert!(code(&r.resolve("bin/jq").unwrap(), &dirs));
        assert!(!code(&r.resolve("runs/x/jq").unwrap(), &dirs));
        // An 8.3 or junction spelling of a PATH folder is the same folder once canonical.
        let link = t.path().join("bin-link");
        if make_link(&bin, &link) {
            let via = PathDirs::from_entries(std::slice::from_ref(&link), None, false);
            assert!(code(&r.resolve("bin/jq").unwrap(), &via));
        }
        let relative = PathDirs::from_entries(&[], None, true);
        assert!(code(&r.resolve("runs/x/a.txt").unwrap(), &relative));
        // A relative harness entry is taken against the server's own folder.
        let from_cwd = PathDirs::from_entries(&[PathBuf::from("bin")], Some(t.path()), false);
        assert!(code(&r.resolve("bin/jq").unwrap(), &from_cwd));
    }

    #[test]
    fn bare_names_go_to_files() {
        let (_t, r) = setup();
        assert_eq!(
            r.resolve_out("report.pdf").unwrap(),
            r.files.join("report.pdf")
        );
        assert_eq!(
            r.resolve_out("x/report.pdf").unwrap(),
            r.base.join("x").join("report.pdf")
        );
        assert_eq!(
            r.resolve_out("./report.pdf").unwrap(),
            r.base.join("report.pdf")
        );
    }

    /// B-564 (D-570): only a plain one-component name is bare.
    #[test]
    fn bare_names() {
        for b in ["report.csv", "a b.txt", "x"] {
            assert!(is_bare(b), "{b}");
        }
        for n in [
            "./report.csv",
            "out/report.csv",
            "archive/",
            "/abs.txt",
            "..",
            ".",
            "~",
            "~/x.txt",
            "",
        ] {
            assert!(!is_bare(n), "{n}");
        }
        #[cfg(windows)]
        for n in ["C:x.txt", "C:\\x.txt", "archive\\", "out\\report.csv"] {
            assert!(!is_bare(n), "{n}");
        }
    }

    /// B-564 (D-571): a bare read falls back to files/ only when the base has no such
    /// file; the base wins when both exist and `shadowed` names the other.
    #[test]
    fn bare_reads_fall_back_to_files() {
        let (_t, r) = setup();
        std::fs::write(r.files.join("a.csv"), "run").unwrap();
        assert_eq!(r.resolve_existing("a.csv").unwrap(), r.files.join("a.csv"));
        assert_eq!(r.shadowed("a.csv", &r.files.join("a.csv")), None);
        assert_eq!(r.resolve_existing("./a.csv").unwrap(), r.base.join("a.csv"));
        assert_eq!(r.resolve_existing("b.csv").unwrap(), r.base.join("b.csv"));
        std::fs::write(r.base.join("a.csv"), "base").unwrap();
        let p = r.resolve_existing("a.csv").unwrap();
        assert_eq!(p, r.base.join("a.csv"));
        assert_eq!(r.shadowed("a.csv", &p), Some(r.files.join("a.csv")));
        assert_eq!(r.shadowed("./a.csv", &p), None);
    }
}
