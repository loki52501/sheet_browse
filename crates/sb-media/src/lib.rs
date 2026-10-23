//! sb-media (B-9). SKELETON owned by area driver (T-24). Captcha frames are never captured (D-5).

use sb_driver::{Driver, DriverError, TabId};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    File,
    Mse,
    /// Reported as protected; no frames.
    Drm,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoInfo {
    pub id: String,
    pub duration: Option<Duration>,
    pub source: SourceKind,
    pub captions: bool,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameSet {
    pub frames: Vec<PathBuf>,
    pub contact_sheets: Vec<PathBuf>,
    pub manifest: PathBuf,
}

/// `video.list(tab)`.
pub async fn list(driver: &Driver, tab: TabId) -> Result<Vec<VideoInfo>, DriverError> {
    let _ = (driver, tab);
    todo!("T-24: B-9")
}

/// `video.frames(tab, v, from, to, every, size, dedupe)`.
pub async fn frames(
    driver: &Driver,
    tab: TabId,
    video: &str,
    from: Duration,
    to: Duration,
    every: Duration,
) -> Result<FrameSet, DriverError> {
    let _ = (driver, tab, video, from, to, every);
    todo!("T-24: B-9")
}
