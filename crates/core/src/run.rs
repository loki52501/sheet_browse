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
    /// `InvalidInput` unless `id` has exactly the shape [`RunFolder::create`] makes
    /// (`<yyyy-mm-ddThh-mm-ss>-<slug>` with a lowercase `[a-z0-9]` slug in dash-separated
    /// parts, see [`is_run_id`]). `NotFound` unless `root` holds a directory entry with
    /// exactly that name (not a symlink or junction); this rejects the aliases Windows
    /// would resolve, such as another case or a trailing dot or space.
    pub fn open(root: &Path, id: &RunId) -> io::Result<RunFolder> {
        let s = id.0.as_str();
        if !is_run_id(s) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "run id {:?} is not a run folder name",
                    crate::vault::scrub(s)
                ),
            ));
        }
        let not_found = || {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("run folder {} does not exist", root.join(s).display()),
            )
        };
        let entries = match std::fs::read_dir(root) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(not_found()),
            Err(e) => return Err(e),
        };
        let mut found = false;
        for entry in entries {
            let entry = entry?;
            if entry.file_name().to_str() == Some(s) && entry.file_type()?.is_dir() {
                found = true;
                break;
            }
        }
        if !found {
            return Err(not_found());
        }
        Ok(RunFolder {
            id: id.clone(),
            dir: root.join(s),
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

/// `^\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}-[a-z0-9]+(-[a-z0-9]+)*$`: the ids
/// [`RunFolder::create`] makes (the timestamp is not checked as a date).
pub fn is_run_id(s: &str) -> bool {
    const TS: &[u8] = b"dddd-dd-ddTdd-dd-dd-";
    let b = s.as_bytes();
    if b.len() <= TS.len() {
        return false;
    }
    let ts_ok = TS.iter().zip(b).all(|(&p, &c)| match p {
        b'd' => c.is_ascii_digit(),
        other => c == other,
    });
    ts_ok
        && s[TS.len()..].split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// Lowercase `[a-z0-9]`, runs of anything else become one dash, no leading or trailing
/// dash. No length cap.
fn clean_slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        let keep = c.is_ascii_lowercase() || c.is_ascii_digit();
        if keep {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// The slug part of a run id: lowercase `[a-z0-9-]`, runs of anything else become one
/// dash, no leading or trailing dash, at most [`SLUG_MAX`] chars, `run` if nothing is left.
///
/// Vault values are kept out (H-2): the raw input is scrubbed, then cleaned, then scrubbed
/// again (a value that is itself slug-shaped, written in the prompt with other case or
/// punctuation, only matches after cleaning), then cleaned again and cut. A value in a
/// prompt-derived slug shows as `vault-<name>`. If the final slug still contains a vault
/// value, the slug is `run`.
pub fn slugify(s: &str) -> String {
    use crate::vault::scrub;
    let cleaned = clean_slug(&scrub(s));
    let mut out = clean_slug(&scrub(&cleaned));
    // ASCII only, so the byte cut is a char cut.
    out.truncate(SLUG_MAX);
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() || scrub(&out) != out {
        return "run".to_owned();
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
        assert_eq!(RunFolder::open(&root, &c.id).unwrap(), c);
        let missing =
            RunFolder::open(&root, &RunId("2026-10-23T09-05-07-nope".into())).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        let no_root = RunFolder::open(&root.join("absent"), &a.id).unwrap_err();
        assert_eq!(no_root.kind(), io::ErrorKind::NotFound);
    }

    /// Item 8: only the exact create() id shape is accepted, and only a directory entry
    /// with exactly that name; Windows aliases (case, trailing dot or space) are rejected.
    #[test]
    fn open_rejects_malformed_ids_and_aliases() {
        let tmp = Tmp::new();
        let root = tmp.0.join("runs");
        let at = chrono::Utc.with_ymd_and_hms(2026, 10, 23, 9, 5, 7).unwrap();
        let a = RunFolder::create_at(&root, "Say hi", "p", at).unwrap();
        // A plain file with a run-id name is not a run folder.
        std::fs::write(root.join("2026-10-23T09-05-07-file"), "x").unwrap();
        let file = RunFolder::open(&root, &RunId("2026-10-23T09-05-07-file".into()));
        assert_eq!(file.unwrap_err().kind(), io::ErrorKind::NotFound);
        let id = a.id.0.clone();
        for bad in [
            String::new(),
            "..".into(),
            ".".into(),
            "nope".into(),
            "a/b".into(),
            "a\\b".into(),
            "c:x".into(),
            format!("{id}."),
            format!("{id} "),
            format!("{id}/"),
            format!("{id}\\"),
            format!("{id}-"),
            format!("{id}::$DATA"),
            id.to_uppercase(),
            id.replace('T', "t"),
            id.replace("say-hi", "say--hi"),
            id.replace("say-hi", "Say-hi"),
            "2026-10-23T09-05-07".into(),
            "2026-10-23T09-05-07-".into(),
            "2026-10-23 09-05-07-say-hi".into(),
            "26-10-23T09-05-07-say-hi".into(),
            "2026-10-23T09-05-07-say_hi".into(),
            "2026-10-23T09-05-07-con.txt".into(),
            "2026-10-23T09-05-07-SAY~1".into(),
        ] {
            let e = RunFolder::open(&root, &RunId(bad.clone())).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
        assert!(is_run_id(&id));
        assert!(is_run_id("2026-10-23T09-05-07-say-hi-2"));
        assert!(is_run_id("2026-10-23T09-05-07-run"));
    }

    #[test]
    fn create_uses_now() {
        let tmp = Tmp::new();
        let r = RunFolder::create(&tmp.0, "", "p").unwrap();
        assert!(r.id.0.ends_with("-run"), "{}", r.id);
        assert_eq!(r.id.0.len(), "yyyy-mm-ddThh-mm-ss-run".len());
    }
}
