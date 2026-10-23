//! T-1 / T-7: `tools/_archive/<domain>/<name>.<version>.js`, and the git commit helper
//! (every change to `tools/` is a commit whose message carries a task id).
//!
//! An archived file's mtime is set to the moment it was archived; `sweep` deletes it 90
//! days later (D-303). A fresh clone resets mtimes, which only delays deletion.

use crate::domain::{check_tool_id, ARCHIVE};
use crate::{StoreError, ToolStore};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

impl ToolStore {
    /// `tools/<domain>/<name>.js`, or `Err(BadPath)` when `domain` or `name` is not a single
    /// safe path component (D-310).
    pub fn try_tool_path(&self, domain: &str, name: &str) -> Result<PathBuf, StoreError> {
        check_tool_id(domain, name)?;
        Ok(self.root.join(domain).join(format!("{name}.js")))
    }

    /// `tools/_archive/<domain>/<name>.<version>.js`, or `Err(BadPath)` (D-310).
    pub fn try_archive_path(
        &self,
        domain: &str,
        name: &str,
        version: i32,
    ) -> Result<PathBuf, StoreError> {
        check_tool_id(domain, name)?;
        Ok(self
            .root
            .join(ARCHIVE)
            .join(domain)
            .join(format!("{name}.{version}.js")))
    }

    /// `tools/<domain>/<name>.js`. For a `domain` or `name` that fails the D-310 check this is
    /// the empty path, which no file operation accepts, so it can never point outside the
    /// root. Mutators use [`ToolStore::try_tool_path`] and return `StoreError::BadPath`.
    pub fn tool_path(&self, domain: &str, name: &str) -> PathBuf {
        self.try_tool_path(domain, name).unwrap_or_default()
    }

    /// `tools/_archive/<domain>/<name>.<version>.js`; the empty path on bad input, as
    /// [`ToolStore::tool_path`].
    pub fn archive_path(&self, domain: &str, name: &str, version: i32) -> PathBuf {
        self.try_archive_path(domain, name, version)
            .unwrap_or_default()
    }

    /// Moves `tools/<domain>/<name>.js` to the archive as `version`. If that archive name is
    /// taken (the same version archived twice), `<name>.<version>.<n>.js` is used. Returns
    /// the archive path; `Ok(None)` when the tool file does not exist.
    pub fn archive(
        &self,
        domain: &str,
        name: &str,
        version: i32,
    ) -> Result<Option<PathBuf>, StoreError> {
        let from = self.try_tool_path(domain, name)?;
        if !from.is_file() {
            return Ok(None);
        }
        let mut to = self.try_archive_path(domain, name, version)?;
        let mut n = 2;
        while to.exists() {
            to = self
                .root
                .join(ARCHIVE)
                .join(domain)
                .join(format!("{name}.{version}.{n}.js"));
            n += 1;
        }
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::rename(&from, &to)?;
        fs::File::options()
            .write(true)
            .open(&to)?
            .set_modified(SystemTime::now())?;
        Ok(Some(to))
    }

    /// Stages exactly `paths` (additions, edits, moves and deletions) and commits them with
    /// `message`, which must carry a task id (`T-31`). Every path must lie under the tools
    /// root. Returns the new commit's hash. Runs `git --literal-pathspecs -C <root>`, so `:/`,
    /// `:(top)`, `*` and other pathspec magic are plain names (D-310); a component containing
    /// `:` is refused before git runs.
    pub fn git_commit(&self, paths: &[PathBuf], message: &str) -> Result<String, StoreError> {
        if !has_task_id(message) {
            return Err(StoreError::Git(format!(
                "commit message must carry a task id like T-31: {message:?}"
            )));
        }
        if paths.is_empty() {
            return Err(StoreError::Git("nothing to commit".into()));
        }
        let mut rel = Vec::with_capacity(paths.len());
        for p in paths {
            rel.push(self.under_root(p)?);
        }
        let mut add = self.git();
        add.args(["add", "-A", "--"]).args(&rel);
        run(add)?;
        let mut commit = self.git();
        commit
            .args(["commit", "-q", "-m", message, "--"])
            .args(&rel);
        run(commit)?;
        let mut head = self.git();
        head.args(["rev-parse", "HEAD"]);
        Ok(run(head)?.trim().to_string())
    }

    /// `git --literal-pathspecs -C <root>`: every path argument is a literal path.
    fn git(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("--literal-pathspecs").arg("-C").arg(&self.root);
        cmd
    }

    /// `p` relative to the root, refusing anything outside it.
    fn under_root(&self, p: &Path) -> Result<PathBuf, StoreError> {
        let rel = if p.is_absolute() {
            p.strip_prefix(&self.root)
                .map_err(|_| {
                    StoreError::Git(format!("{} is not under the tools root", p.display()))
                })?
                .to_path_buf()
        } else {
            p.to_path_buf()
        };
        let bad = |c: Component| match c {
            Component::CurDir => false,
            // `:/`, `:(top)x`: git pathspec magic (--literal-pathspecs also disarms it); `a:b`
            // is an NTFS alternate data stream. No tool path has a `:`.
            Component::Normal(n) => n.to_string_lossy().contains(':'),
            _ => true,
        };
        if rel.as_os_str().is_empty() || rel.components().any(bad) {
            return Err(StoreError::Git(format!(
                "{} is not a path under the tools root",
                p.display()
            )));
        }
        Ok(rel)
    }
}

/// `T-` followed by digits, as a whole word.
pub fn has_task_id(message: &str) -> bool {
    let b = message.as_bytes();
    (0..b.len().saturating_sub(2)).any(|i| {
        b[i] == b'T'
            && b[i + 1] == b'-'
            && b[i + 2].is_ascii_digit()
            && (i == 0 || !b[i - 1].is_ascii_alphanumeric())
    })
}

fn run(mut cmd: Command) -> Result<String, StoreError> {
    let out = cmd
        .output()
        .map_err(|e| StoreError::Git(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        return Err(StoreError::Git(format!(
            "git failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_ids() {
        assert!(has_task_id("tools: repair luma.register (T-31)"));
        assert!(has_task_id("T-7"));
        assert!(!has_task_id("tools: repair"));
        assert!(!has_task_id("XT-31"));
        assert!(!has_task_id("T-x"));
    }

    #[test]
    fn archive_moves_and_versions() {
        let dir = tempfile::tempdir().unwrap();
        let ts = ToolStore::new(dir.path());
        fs::create_dir_all(dir.path().join("luma.com")).unwrap();
        fs::write(ts.tool_path("luma.com", "register"), "a").unwrap();
        let to = ts.archive("luma.com", "register", 1).unwrap().unwrap();
        assert_eq!(to, dir.path().join("_archive/luma.com/register.1.js"));
        assert!(!ts.tool_path("luma.com", "register").exists());
        fs::write(ts.tool_path("luma.com", "register"), "b").unwrap();
        let to2 = ts.archive("luma.com", "register", 1).unwrap().unwrap();
        assert_eq!(to2, dir.path().join("_archive/luma.com/register.1.2.js"));
        assert_eq!(ts.archive("luma.com", "register", 3).unwrap(), None);
    }

    #[test]
    fn git_commit_refuses_without_task_id_or_outside_root() {
        let dir = tempfile::tempdir().unwrap();
        let ts = ToolStore::new(dir.path());
        let p = vec![dir.path().join("a.js")];
        assert!(matches!(
            ts.git_commit(&p, "no id"),
            Err(StoreError::Git(_))
        ));
        let outside = vec![PathBuf::from("../x.js")];
        assert!(matches!(
            ts.git_commit(&outside, "x (T-1)"),
            Err(StoreError::Git(m)) if m.contains("not a path under")
        ));
        let abs = vec![std::env::temp_dir().join("elsewhere.js")];
        assert!(ts.git_commit(&abs, "x (T-1)").is_err());
    }

    #[test]
    fn git_commit_stages_only_given_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
        };
        if git(&["init", "-q"]).is_none() {
            eprintln!("skipped: git not available");
            return;
        }
        git(&["config", "user.email", "t@example.invalid"]).unwrap();
        git(&["config", "user.name", "toolstore test"]).unwrap();
        git(&["config", "commit.gpgsign", "false"]).unwrap();
        fs::create_dir_all(root.join("luma.com")).unwrap();
        fs::write(root.join("luma.com/register.js"), "x").unwrap();
        fs::write(root.join("other.txt"), "y").unwrap();
        let ts = ToolStore::new(root);
        let sha = ts
            .git_commit(&[root.join("luma.com/register.js")], "tools: add (T-31)")
            .unwrap();
        assert_eq!(sha.len(), 40);
        let files = git(&["show", "--name-only", "--format=", "HEAD"]).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&files.stdout).trim(),
            "luma.com/register.js"
        );
        let status = git(&["status", "--porcelain"]).unwrap();
        assert!(String::from_utf8_lossy(&status.stdout).contains("?? other.txt"));
        // A move into the archive commits both sides.
        ts.archive("luma.com", "register", 1).unwrap();
        let paths = vec![
            root.join("luma.com/register.js"),
            root.join("_archive/luma.com/register.1.js"),
        ];
        ts.git_commit(&paths, "tools: retire (T-31)").unwrap();
        let files = git(&["show", "--name-only", "--no-renames", "--format=", "HEAD"]).unwrap();
        let s = String::from_utf8_lossy(&files.stdout).to_string();
        assert!(
            s.contains("_archive/luma.com/register.1.js") && s.contains("luma.com/register.js"),
            "{s}"
        );
    }
}
