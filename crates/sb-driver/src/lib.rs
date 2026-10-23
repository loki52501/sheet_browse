//! sb-driver (B-4). DRAFT signatures for I-9, owned by area driver (T-20..T-22).
//! No fixed sleeps (rule 6, D-26): every wait is a condition with a cap.

use harness_core::gate::StopReason;
use harness_core::ToolCtx;
use sb_sheet::{ElemRef, IndexRow, TextBlock};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TabId(pub u32);

/// B-2 profiles. `Me` is never headless (D-30).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    Bench,
    Me,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchOpts {
    pub profile: Profile,
    /// Rejected for `Profile::Me`.
    pub headless: bool,
    /// Override the pinned Chrome for Testing build (D-34).
    pub chromium: Option<PathBuf>,
}

/// What an act step caused (B-4 #9). Serialized into `ToolOutput.consequences`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Consequence {
    Navigated {
        url: String,
    },
    OpenedTab {
        tab: TabId,
        title: String,
    },
    /// Paused on the dialog; continue with [`Driver::dialog`].
    Dialog {
        text: String,
    },
    /// Stopped; asks before the file lands; continue with [`Driver::download`].
    Download {
        name: String,
        guid: String,
    },
    NoVisibleChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepOutcome {
    Ok,
    /// Final actionability or input `why`, e.g. `covered by div.cookie (OVERLAY #31) after 5.0 s`.
    Failed {
        why: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepResult {
    /// 1-based.
    pub n: usize,
    /// The step as written, e.g. `c#19`.
    pub step: String,
    pub outcome: StepOutcome,
    pub consequences: Vec<Consequence>,
    pub settled: Option<bool>,
}

/// I-9 `ActResult` (B-8). Never an empty success.
#[derive(Debug, Clone, PartialEq)]
pub struct ActResult {
    pub steps: Vec<StepResult>,
    /// `+ h1 "Welcome, Lokesh" · − form f2`
    pub diff: Option<String>,
    /// Challenge or gate stop mid-program (H-1.4): nothing after this step ran.
    pub stopped: Option<StopReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettleOpts {
    /// DOM quiet window (150 ms).
    pub quiet_ms: u32,
    /// Hard cap (3 s).
    pub cap_ms: u32,
}

impl Default for SettleOpts {
    fn default() -> SettleOpts {
        SettleOpts {
            quiet_ms: 150,
            cap_ms: 3000,
        }
    }
}

/// B-4 #8 result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settled {
    pub settled: bool,
    pub changed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShotTarget {
    Viewport,
    Full,
    Elem(ElemRef),
    Region(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screenshot {
    /// Under `runs/<id>/frames/`.
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}

/// B-4 #2 registry view of one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameInfo {
    pub frame: u32,
    pub parent: Option<u32>,
    pub url: String,
    /// Cross-origin: one opaque row, never read inside.
    pub opaque: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("tab {0:?} is gone")]
    TabGone(TabId),
    #[error("cdp: {0}")]
    Cdp(String),
    #[error("command timed out after 10 s: {0}")]
    Timeout(String),
    #[error(transparent)]
    Transport(#[from] sb_cdp::TransportError),
    #[error(transparent)]
    Parse(#[from] sb_act::ParseError),
}

/// The driver: one browser, its registry and tabs.
pub struct Driver {
    _private: (),
}

impl Driver {
    pub async fn launch(opts: LaunchOpts) -> Result<Driver, DriverError> {
        let _ = opts;
        todo!("T-18: B-2 launch, B-3 transport")
    }

    /// I-9 `open`.
    pub async fn open(&self, url: &str) -> Result<TabId, DriverError> {
        let _ = url;
        todo!("T-22/T-23")
    }

    /// I-9 `act(tab, program, ctx) -> ActResult`. The gate has already passed the program's
    /// class (`ctx.class`); a CHALLENGE row reached mid-program stops it (H-1.4), and so does
    /// a step whose live class exceeds `ctx.class`. `@vault(k)` in `t#` steps is resolved only
    /// inside `ctx.resolve_into`, straight into the `Input.insertText` message.
    pub async fn act(
        &self,
        tab: TabId,
        program: &str,
        ctx: &ToolCtx,
    ) -> Result<ActResult, DriverError> {
        let _ = (tab, program, ctx);
        todo!("T-20/T-21: B-4 #6 #7 #9")
    }

    /// The tab's current index (I-5), all frames merged, ElemRefs and `child_frame` rewritten.
    pub async fn index(&self, tab: TabId) -> Result<Vec<IndexRow>, DriverError> {
        let _ = tab;
        todo!("T-22/T-23")
    }

    /// Text blocks for a region (`None` = whole page), all frames merged in document order.
    pub async fn text(
        &self,
        tab: TabId,
        region: Option<&str>,
    ) -> Result<Vec<TextBlock>, DriverError> {
        let _ = (tab, region);
        todo!("T-23")
    }

    /// (tab, url, title) for every live tab.
    pub async fn tabs(&self) -> Result<Vec<(TabId, String, String)>, DriverError> {
        todo!("T-22")
    }

    pub async fn close(&self, tab: TabId) -> Result<(), DriverError> {
        let _ = tab;
        todo!("T-22")
    }

    /// Answer a paused dialog (B-4 #9).
    pub async fn dialog(
        &self,
        tab: TabId,
        accept: bool,
        text: Option<&str>,
    ) -> Result<(), DriverError> {
        let _ = (tab, accept, text);
        todo!("T-21")
    }

    /// After the ask: let a stopped download land in `runs/<id>/files/`, or cancel it.
    pub async fn download(
        &self,
        tab: TabId,
        guid: &str,
        allow: bool,
    ) -> Result<Option<PathBuf>, DriverError> {
        let _ = (tab, guid, allow);
        todo!("T-21")
    }

    /// I-9 `settle` (B-4 #8).
    pub async fn settle(&self, tab: TabId, opts: SettleOpts) -> Result<Settled, DriverError> {
        let _ = (tab, opts);
        todo!("T-22: B-4 #8")
    }

    /// I-9 `screenshot` (B-9). The only path, with video.*, that gives the model pixels (rule 5).
    pub async fn screenshot(
        &self,
        tab: TabId,
        target: ShotTarget,
    ) -> Result<Screenshot, DriverError> {
        let _ = (tab, target);
        todo!("T-24: B-9")
    }

    /// I-9 `frames` (B-4 #2).
    pub async fn frames(&self, tab: TabId) -> Result<Vec<FrameInfo>, DriverError> {
        let _ = tab;
        todo!("T-22: B-4 #2")
    }
}
