//! Shared plumbing: argument parsing, the classifier verdict, the call-time recheck.

use crate::paths::{Place, Roots};
use harness_core::{Class, Classified, ToolCtx, ToolError};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::Path;

/// Longest ask-card line built here (the gate's clean_card caps it again).
const WHAT_MAX: usize = 400;

/// Parses a tool's arguments. `null` reads as `{}`.
pub fn parse<T: DeserializeOwned>(args: &Value) -> Result<T, ToolError> {
    let v = if args.is_null() {
        Value::Object(Default::default())
    } else {
        args.clone()
    };
    serde_json::from_value(v).map_err(|e| ToolError::Args(e.to_string()))
}

/// What the classifier found: the class and why, per reason the class it implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub class: Class,
    pub reasons: Vec<(Class, String)>,
    /// The shell line runs git (or gh, which runs git).
    pub runs_git: bool,
    /// One of those git or gh commands is above Read on its own (a commit, a push, git
    /// in runs/): the line's git then runs with the user's configuration (D-547).
    pub git_above_read: bool,
}

impl Verdict {
    pub fn new(floor: Class) -> Verdict {
        Verdict {
            class: floor,
            reasons: Vec::new(),
            runs_git: false,
            git_above_read: false,
        }
    }

    /// Every git in the line is Read-class, so it runs sealed: no system or global
    /// config, no hooks, no fsmonitor, no credential helper (D-547).
    pub fn seal_git(&self) -> bool {
        self.runs_git && !self.git_above_read
    }

    /// Raises to `class` (never lowers) and records why.
    pub fn raise(&mut self, class: Class, why: impl Into<String>) {
        self.class = self.class.max(class);
        self.reasons.push((class, why.into()));
    }

    pub fn merge(&mut self, other: Verdict) {
        self.class = self.class.max(other.class);
        self.reasons.extend(other.reasons);
        self.runs_git |= other.runs_git;
        self.git_above_read |= other.git_above_read;
    }

    /// The reasons behind the final class, joined.
    pub fn why(&self) -> String {
        let top: Vec<&str> = self
            .reasons
            .iter()
            .filter(|(c, _)| *c == self.class)
            .map(|(_, w)| w.as_str())
            .collect();
        let s = if top.is_empty() {
            self.class.as_str().to_ascii_lowercase()
        } else {
            top.join("; ")
        };
        crate::text::one_line(&s, WHAT_MAX)
    }

    /// For the gate: the class, a card line, and the values to show.
    pub fn classified(&self, tool: &str, values: Vec<(String, String)>) -> Classified {
        Classified {
            class: Some(self.class),
            what: Some(format!("{tool}: {}", self.why())),
            values: values
                .into_iter()
                .map(|(k, v)| (k, crate::text::one_line(&v, WHAT_MAX)))
                .collect(),
            ..Classified::default()
        }
    }
}

/// Call-time recheck: the world may have changed since classify (a file appeared, a link
/// was swapped). A call whose class is now above the approved one does not run.
pub fn recheck(ctx: &ToolCtx, v: &Verdict) -> Result<(), ToolError> {
    if v.class > ctx.class {
        return Err(ToolError::Failed(format!(
            "not run: this call is now {} ({}), above the {} it was approved as; call it again",
            v.class.as_str(),
            v.why(),
            ctx.class.as_str()
        )));
    }
    Ok(())
}

/// Why writing `p` is Delete when [`crate::paths::runs_code_on_use`] holds.
pub fn code_reason(roots: &Roots, p: &Path) -> String {
    format!(
        "writes {}, configuration, a startup file or a program folder that can make a program run code later",
        roots.shown(p)
    )
}

/// The class of writing the file `p`, for every tool that writes a named file (fs.write,
/// fs.move both ends, fs.mkdir, docs `out`, csv.clean `out`, every shell target; D-525):
/// configuration, startup and PATH files are Delete anywhere; inside runs/ nothing more
/// than `floor`; bookkeeping is Delete; outside, replacing an existing file is Delete and
/// anything else is Fill.
pub fn write_verdict(roots: &Roots, p: &Path, replaces: bool, floor: Class) -> Verdict {
    let mut v = Verdict::new(floor);
    if crate::paths::runs_code_on_use(roots, p) {
        v.raise(Class::Delete, code_reason(roots, p));
        return v;
    }
    if crate::paths::hard_linked(p) {
        // Writing one name writes every other, wherever it is (B7, D-523).
        v.raise(
            Class::Delete,
            format!(
                "writes {}, a file with other hard links (they may be outside runs/)",
                roots.shown(p)
            ),
        );
        return v;
    }
    match roots.place(p) {
        Place::Scratch => {}
        Place::Bookkeeping => v.raise(
            Class::Delete,
            format!("writes the run's own records ({})", roots.shown(p)),
        ),
        Place::Outside if replaces && crate::paths::exists(p) => v.raise(
            Class::Delete,
            format!("replaces {} (outside runs/)", roots.shown(p)),
        ),
        Place::Outside => v.raise(
            Class::Fill,
            format!("writes {} (outside runs/)", roots.shown(p)),
        ),
    }
    v
}

/// The class of writing files whose names are not known in advance into the folder
/// `dir`: a recursive copy or a moved folder, an extracted archive, a clone, a download
/// named by the server (D-525). Inside runs/ it is `floor` (a repository planted there is
/// handled where git runs, see the shell classifier); a folder where configuration or
/// startup files live, bookkeeping, or anywhere outside runs/ is Delete: any file of the
/// tree could replace a file there or be `.git/config`, `.bashrc` or a program on PATH.
pub fn tree_verdict(roots: &Roots, dir: &Path, floor: Class) -> Verdict {
    let mut v = Verdict::new(floor);
    let any_file = dir.join("x");
    if crate::paths::runs_code_on_use(roots, dir)
        || crate::paths::runs_code_on_use(roots, &any_file)
    {
        v.raise(
            Class::Delete,
            format!(
                "writes files it cannot name in advance into {}, where configuration, startup files or programs live",
                roots.shown(dir)
            ),
        );
        return v;
    }
    match roots.place(dir) {
        Place::Scratch => {}
        Place::Bookkeeping => v.raise(
            Class::Delete,
            format!("writes files into the run's own records ({})", roots.shown(dir)),
        ),
        Place::Outside => v.raise(
            Class::Delete,
            format!(
                "writes files it cannot name in advance into {} (outside runs/): any of them could replace a file or be configuration that runs a program",
                roots.shown(dir)
            ),
        ),
    }
    v
}

/// A file this server names itself in this run's `files/` (the shell and diff overflow
/// copies): resolved again (a link planted at `files/` is followed) and allowed only when
/// writing it is plain scratch, the same check as every other write.
pub fn own_output_ok(roots: &Roots, p: &Path) -> Result<(), String> {
    let again = crate::paths::resolve(&roots.base, &crate::paths::plain(p))?;
    let v = write_verdict(roots, &again, true, Class::Read);
    if v.class == Class::Read {
        Ok(())
    } else {
        Err(format!("not written: {}", v.why()))
    }
}

/// Files under this run's folder, for `ToolOutput::files`.
pub fn run_files(roots: &Roots, paths: &[&Path]) -> Vec<std::path::PathBuf> {
    let run = roots.files.parent().unwrap_or(&roots.files);
    paths
        .iter()
        .filter(|p| p.starts_with(run))
        .map(|p| p.to_path_buf())
        .collect()
}

/// A fresh file name in `dir`: `stem.ext`, else `stem-2.ext`, `stem-3.ext`...
pub fn fresh_name(dir: &Path, stem: &str, ext: &str) -> std::path::PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !crate::paths::exists(&first) {
        return first;
    }
    (2u32..)
        .map(|n| dir.join(format!("{stem}-{n}.{ext}")))
        .find(|p| !crate::paths::exists(p))
        .unwrap_or(first)
}

/// Lower-case file extension of `p`.
pub fn ext(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// `n` clamped into `1..=max`; 0 is an argument error, not "nothing" (pr-ready 5).
pub fn limit(name: &str, n: Option<usize>, default: usize, max: usize) -> Result<usize, ToolError> {
    match n {
        None => Ok(default),
        Some(0) => Err(ToolError::Args(format!("{name} must be at least 1"))),
        Some(n) => Ok(n.min(max)),
    }
}
