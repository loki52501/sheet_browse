//! Run folder (H-4). `RunId` is CONTRACT (it appears in I-1 types); `RunFolder` is
//! owned by area core (T-06).
//!
//! ```text
//! runs/<yyyy-mm-ddThh-mm-ss>-<slug>/
//!   prompt.md  ledger.jsonl  report.md  files/  frames/  handoff/
//! ```

use crate::memory::Ts;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// `runs.id`, also the folder name under `runs/`: `<yyyy-mm-ddThh-mm-ss>-<slug>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub String);

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Longest slug part of a run id, in bytes (ASCII only, so also chars).
pub const SLUG_MAX: usize = 40;

/// How many `-2`, `-3`... suffixes [`RunFolder::create`] tries before giving up.
const MAX_SUFFIX: u32 = 1000;

/// `runs/<id>/` with `prompt.md ledger.jsonl report.md files/ frames/ handoff/` (H-4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFolder {
    pub id: RunId,
    pub dir: PathBuf,
}

impl RunFolder {
    /// Creates `root/<utc ts>-<slug>/`, its subfolders and `prompt.md` (the prompt passed
    /// through the global vault scrub). Same second and slug as an existing run: `-2`,
    /// `-3`... `root` is created if missing.
    pub fn create(root: &Path, slug: &str, prompt: &str) -> io::Result<RunFolder> {
        RunFolder::create_at(root, slug, prompt, chrono::Utc::now())
    }

    /// [`RunFolder::create`] with an explicit start time (tests, scheduled firings).
    pub fn create_at(root: &Path, slug: &str, prompt: &str, at: Ts) -> io::Result<RunFolder> {
        std::fs::create_dir_all(root)?;
        let base = format!("{}-{}", at.format("%Y-%m-%dT%H-%M-%S"), slugify(slug));
        let mut n = 1u32;
        let (id, dir) = loop {
            let id = if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            };
            let dir = root.join(&id);
            // create_dir (not _all) fails on an existing folder, so two processes starting
            // the same run name in the same second cannot both claim it.
            match std::fs::create_dir(&dir) {
                Ok(()) => break (id, dir),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < MAX_SUFFIX => n += 1,
                Err(e) => return Err(e),
            }
        };
        let run = RunFolder { id: RunId(id), dir };
        for sub in [run.files(), run.frames(), run.handoff()] {
            std::fs::create_dir(sub)?;
        }
        std::fs::write(run.prompt_md(), crate::vault::scrub(prompt))?;
        Ok(run)
    }

    /// An existing run's folder (a tool server attaching to the run its session created).
    /// `NotFound` if `root/<id>/` is not a directory; `InvalidInput` if `id` is not a
    /// single plain path component.
    pub fn open(root: &Path, id: &RunId) -> io::Result<RunFolder> {
        let s = id.0.as_str();
        let plain = !s.is_empty()
            && s != "."
            && s != ".."
            && !s.contains(['/', '\\', ':'])
            && Path::new(s).components().count() == 1;
        if !plain {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("run id {s:?} is not a folder name"),
            ));
        }
        let dir = root.join(s);
        if !dir.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("run folder {} does not exist", dir.display()),
            ));
        }
        Ok(RunFolder {
            id: id.clone(),
            dir,
        })
    }

    pub fn prompt_md(&self) -> PathBuf {
        self.dir.join("prompt.md")
    }
    pub fn ledger_jsonl(&self) -> PathBuf {
        self.dir.join("ledger.jsonl")
    }
    pub fn report_md(&self) -> PathBuf {
        self.dir.join("report.md")
    }
    pub fn files(&self) -> PathBuf {
        self.dir.join("files")
    }
    pub fn frames(&self) -> PathBuf {
        self.dir.join("frames")
    }
    pub fn handoff(&self) -> PathBuf {
        self.dir.join("handoff")
    }
}

/// The slug part of a run id: lowercase `[a-z0-9-]`, runs of anything else become one
/// dash, no leading or trailing dash, at most [`SLUG_MAX`] chars, `run` if nothing is left.
/// The input is vault-scrubbed first, so a value in a prompt-derived slug shows as
/// `vault-<name>`.
pub fn slugify(s: &str) -> String {
    let s = crate::vault::scrub(s);
    let mut out = String::with_capacity(s.len().min(SLUG_MAX));
    for c in s.chars().flat_map(char::to_lowercase) {
        let keep = c.is_ascii_lowercase() || c.is_ascii_digit();
        if !keep && (out.is_empty() || out.ends_with('-')) {
            continue;
        }
        if out.len() == SLUG_MAX {
            break;
        }
        out.push(if keep { c } else { '-' });
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("run");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A unique folder under the OS temp dir, removed on drop.
    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Tmp {
            static N: AtomicUsize = AtomicUsize::new(0);
            Tmp(std::env::temp_dir().join(format!(
                "sb-run-test-{}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed),
                chrono::Utc::now().timestamp_micros()
            )))
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn slug_rules() {
        assert_eq!(slugify("Register on Luma!"), "register-on-luma");
        assert_eq!(slugify("  --a__b//c--  "), "a-b-c");
        assert_eq!(slugify(""), "run");
        assert_eq!(slugify("!!!"), "run");
        assert_eq!(slugify("Gr\u{f6}\u{df}e 3"), "gr-e-3");
        let long = slugify(&"abcdefghij ".repeat(10));
        assert!(long.len() <= SLUG_MAX, "{long}");
        assert!(!long.ends_with('-'));
        assert_eq!(slugify(&"x".repeat(100)).len(), SLUG_MAX);
    }

    #[test]
    fn create_layout_collision_and_open() {
        let tmp = Tmp::new();
        let root = tmp.0.join("runs");
        let at = chrono::Utc.with_ymd_and_hms(2026, 10, 23, 9, 5, 7).unwrap();
        let a = RunFolder::create_at(&root, "Say hi", "say hi\n", at).unwrap();
        assert_eq!(a.id.0, "2026-10-23T09-05-07-say-hi");
        assert_eq!(a.dir, root.join(&a.id.0));
        assert_eq!(std::fs::read_to_string(a.prompt_md()).unwrap(), "say hi\n");
        for d in [a.files(), a.frames(), a.handoff()] {
            assert!(d.is_dir(), "{}", d.display());
        }
        let b = RunFolder::create_at(&root, "say hi", "again", at).unwrap();
        let c = RunFolder::create_at(&root, "SAY HI", "again", at).unwrap();
        assert_eq!(b.id.0, "2026-10-23T09-05-07-say-hi-2");
        assert_eq!(c.id.0, "2026-10-23T09-05-07-say-hi-3");

        assert_eq!(RunFolder::open(&root, &a.id).unwrap(), a);
        let missing = RunFolder::open(&root, &RunId("nope".into())).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        for bad in ["", "..", "a/b", "a\\b", "c:x"] {
            let e = RunFolder::open(&root, &RunId(bad.into())).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
    }

    #[test]
    fn create_uses_now() {
        let tmp = Tmp::new();
        let r = RunFolder::create(&tmp.0, "", "p").unwrap();
        assert!(r.id.0.ends_with("-run"), "{}", r.id);
        assert_eq!(r.id.0.len(), "yyyy-mm-ddThh-mm-ss-run".len());
    }
}
