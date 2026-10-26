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
use std::path::{Component, Path, PathBuf};

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

    /// `raw` resolved, or this run's `files/<raw>` when `raw` is a bare file name (no
    /// folder part): where documents and reports land by default.
    pub fn resolve_out(&self, raw: &str) -> Result<PathBuf, ToolError> {
        let p = Path::new(raw);
        let mut comps = p.components();
        let bare = matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none();
        if bare {
            self.resolve_in(&self.files, raw)
        } else {
            self.resolve(raw)
        }
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

/// See the module docs. `p` must come from [`resolve`].
pub fn place(roots: &Roots, p: &Path) -> Place {
    let Ok(rest) = p.strip_prefix(&roots.runs) else {
        return Place::Outside;
    };
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
    }
}
