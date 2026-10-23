//! Run folder (H-4). `RunId` is CONTRACT (it appears in I-1 types); `RunFolder` is a
//! SKELETON owned by area core (T-06): change freely.

use serde::{Deserialize, Serialize};
use std::fmt;
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

/// `runs/<id>/` with `prompt.md ledger.jsonl report.md files/ frames/ handoff/` (H-4).
#[derive(Debug, Clone)]
pub struct RunFolder {
    pub id: RunId,
    pub dir: PathBuf,
}

impl RunFolder {
    /// Creates `root/<ts>-<slug>/` and its subfolders and writes `prompt.md`.
    pub fn create(root: &Path, slug: &str, prompt: &str) -> std::io::Result<RunFolder> {
        let _ = (root, slug, prompt);
        todo!("T-06: H-4 run folder")
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
