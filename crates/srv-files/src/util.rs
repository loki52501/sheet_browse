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
}

impl Verdict {
    pub fn new(floor: Class) -> Verdict {
        Verdict {
            class: floor,
            reasons: Vec::new(),
        }
    }

    /// Raises to `class` (never lowers) and records why.
    pub fn raise(&mut self, class: Class, why: impl Into<String>) {
        self.class = self.class.max(class);
        self.reasons.push((class, why.into()));
    }

    pub fn merge(&mut self, other: Verdict) {
        self.class = self.class.max(other.class);
        self.reasons.extend(other.reasons);
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

/// The class of writing to `p`: inside runs/ nothing more than `floor`; bookkeeping is
/// Delete; outside, replacing an existing file is Delete and anything else is Fill.
pub fn write_verdict(roots: &Roots, p: &Path, replaces: bool, floor: Class) -> Verdict {
    let mut v = Verdict::new(floor);
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
