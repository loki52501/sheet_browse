//! sb-driver (B-4). DRAFT signatures for I-9, owned by area driver (T-18, T-20..T-22).
//! No fixed sleeps (rule 6, D-26): every wait is a condition with a cap.
//!
//! T-18: launch (B-2, D-601) over the pipe transport (B-3, D-600) and the target and frame
//! registry (B-4 #2). `tabs` and `frames` are answered from the registry.

use harness_core::gate::StopReason;
use harness_core::ToolCtx;
use sb_cdp::{Cdp, CdpError, CdpSender, Event, Pipe, Transport};
use sb_sheet::{ElemRef, IndexRow, TextBlock};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::watch;

mod launch;
mod registry;

pub use launch::{
    binary_name, launch_args, platform_dir, resolve_chromium, LaunchEnv, CHROMIUM_VERSION,
};
pub use registry::{ExecContext, Registry, SB_WORLD};

use launch::ProfileDir;

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
    /// The browser could not be resolved, prepared or started (B-2, D-601).
    #[error("launch: {0}")]
    Launch(String),
}

impl From<CdpError> for DriverError {
    fn from(e: CdpError) -> DriverError {
        match e {
            CdpError::Timeout { method, .. } => DriverError::Timeout(method),
            CdpError::Closed { .. } => DriverError::Transport(sb_cdp::TransportError::Closed),
            e @ CdpError::Protocol { .. } => DriverError::Cdp(e.to_string()),
        }
    }
}

fn lock(m: &Mutex<Registry>) -> MutexGuard<'_, Registry> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the client's observer runs for every event, before any subscriber sees it: the
/// registry update, the change signal, and the attach follow-up.
fn observe(registry: &Mutex<Registry>, changed: &watch::Sender<u64>, e: &Event, cdp: &CdpSender) {
    lock(registry).apply(e.session.as_deref(), &e.method, &e.params);
    changed.send_modify(|v| *v = v.wrapping_add(1));
    if e.method == "Target.attachedToTarget" {
        on_attached(&e.params, cdp);
    }
}

/// Every auto-attached target is paused (`waitForDebuggerOnStart`). Pages and OOPIFs get
/// Page and Runtime events and auto-attach for their own children; then they run. All four
/// commands are queued at once (pipelined); CDP handles a session's commands in order, so the
/// domains are on before the target runs. Replies are not awaited: their slots are dropped.
fn on_attached(params: &Value, cdp: &CdpSender) {
    let Some(sid) = params.get("sessionId").and_then(Value::as_str) else {
        return;
    };
    let kind = params
        .pointer("/targetInfo/type")
        .and_then(Value::as_str)
        .unwrap_or("");
    if matches!(kind, "page" | "iframe") {
        drop(cdp.start(Some(sid), "Page.enable", json!({})));
        drop(cdp.start(Some(sid), "Runtime.enable", json!({})));
        drop(cdp.start(
            Some(sid),
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
        ));
    }
    drop(cdp.start(Some(sid), "Runtime.runIfWaitingForDebugger", json!({})));
}

/// The driver: one browser, its registry and tabs. Dropping it kills every browser process
/// (waiting up to [`sb_cdp::DROP_KILL_CAP`]) and then removes a `bench` profile.
pub struct Driver {
    cdp: Cdp,
    registry: Arc<Mutex<Registry>>,
    changed: watch::Receiver<u64>,
    pipe: Arc<Pipe>,
    // Last: removed only after the browser is gone.
    profile: ProfileDir,
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.pipe.kill(sb_cdp::DROP_KILL_CAP);
    }
}

impl Driver {
    /// B-2: resolve Chromium (D-601), prepare the profile, launch over the pipe (D-600), and
    /// auto-attach to every target with flattened sessions.
    pub async fn launch(opts: LaunchOpts) -> Result<Driver, DriverError> {
        Driver::launch_with(opts, LaunchEnv::from_process()?).await
    }

    /// [`Driver::launch`] with the environment given explicitly (`SB_CHROMIUM`, `SB_HOME`,
    /// cwd), so tests and embedders do not depend on process-wide state.
    pub async fn launch_with(opts: LaunchOpts, env: LaunchEnv) -> Result<Driver, DriverError> {
        if opts.profile == Profile::Me && opts.headless {
            return Err(DriverError::Launch(
                "profile `me` is never headless (B-2, D-30)".into(),
            ));
        }
        let chromium = resolve_chromium(opts.chromium.as_deref(), &env, Path::is_file)?;
        let profile = ProfileDir::prepare(opts.profile, &env)?;
        let args = launch_args(opts.profile, opts.headless, profile.path())?;
        let pipe = Arc::new(
            Pipe::launch(&chromium, &args)
                .map_err(|e| DriverError::Launch(format!("start {}: {e}", chromium.display())))?,
        );
        let registry = Arc::new(Mutex::new(Registry::new()));
        let (changed_tx, changed) = watch::channel(0u64);
        let reg = registry.clone();
        let cdp = Cdp::with_observer(
            pipe.clone() as Arc<dyn Transport>,
            Box::new(move |e, cdp| observe(&reg, &changed_tx, e, cdp)),
        );
        // From here a failure drops the Driver, which kills the browser and the profile.
        let driver = Driver {
            cdp,
            registry,
            changed,
            pipe,
            profile,
        };
        let discover =
            driver
                .cdp
                .start(None, "Target.setDiscoverTargets", json!({"discover": true}));
        let attach = driver.cdp.start(
            None,
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
        );
        discover.await?;
        attach.await?;
        Ok(driver)
    }

    /// The CDP client (shared; flattened sessions).
    pub fn cdp(&self) -> &Cdp {
        &self.cdp
    }

    /// Read the registry. `f` runs under the registry lock: keep it short.
    pub fn with_registry<R>(&self, f: impl FnOnce(&Registry) -> R) -> R {
        f(&lock(&self.registry))
    }

    /// Wait, at most `cap`, until `f` returns `Some` for the registry. `f` is re-checked
    /// after every event the registry applies (a condition, not a poll). `None` on the cap.
    pub async fn wait_registry<R>(
        &self,
        cap: Duration,
        mut f: impl FnMut(&Registry) -> Option<R>,
    ) -> Option<R> {
        let mut changed = self.changed.clone();
        let wait = async {
            loop {
                changed.borrow_and_update();
                if let Some(r) = f(&lock(&self.registry)) {
                    return Some(r);
                }
                if changed.changed().await.is_err() {
                    return f(&lock(&self.registry));
                }
            }
        };
        tokio::time::timeout(cap, wait).await.ok().flatten()
    }

    /// The browser process id.
    pub fn pid(&self) -> u32 {
        self.pipe.pid()
    }

    /// Every live process of the browser.
    pub fn pids(&self) -> Vec<u32> {
        self.pipe.pids()
    }

    /// The profile directory in use (`profiles/me` or `profiles/bench-*`).
    pub fn profile_dir(&self) -> &Path {
        self.profile.path()
    }

    /// Close the browser gracefully (`Browser.close`, so `me` flushes cookies and session
    /// state), wait at most `cap` for the connection to end, then drop (kill whatever
    /// remains, remove a bench profile).
    pub async fn shutdown(self, cap: Duration) {
        drop(self.cdp.start(None, "Browser.close", json!({})));
        let _ = tokio::time::timeout(cap, self.cdp.closed()).await;
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
        Ok(lock(&self.registry).tabs())
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
        lock(&self.registry)
            .frames(tab)
            .ok_or(DriverError::TabGone(tab))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_in(home: &Path, sb_chromium: Option<PathBuf>) -> LaunchEnv {
        LaunchEnv {
            sb_chromium,
            sb_home: Some(home.to_path_buf()),
            cwd: home.to_path_buf(),
        }
    }

    #[tokio::test]
    async fn me_headless_is_rejected_before_anything_starts() {
        let home = tempfile::tempdir().unwrap();
        let opts = LaunchOpts {
            profile: Profile::Me,
            headless: true,
            chromium: None,
        };
        let Err(e) = Driver::launch_with(opts, env_in(home.path(), None)).await else {
            panic!("me + headless launched");
        };
        assert!(matches!(e, DriverError::Launch(ref m) if m.contains("never headless")));
        assert!(!home.path().join("profiles").exists(), "no profile touched");
    }

    #[tokio::test]
    async fn missing_chromium_names_sb_chromium() {
        let home = tempfile::tempdir().unwrap();
        let env = env_in(home.path(), Some(home.path().join("no-chrome.exe")));
        let opts = LaunchOpts {
            profile: Profile::Bench,
            headless: true,
            chromium: None,
        };
        let Err(e) = Driver::launch_with(opts, env).await else {
            panic!("launched without a browser");
        };
        assert!(e.to_string().contains("SB_CHROMIUM"), "{e}");
    }

    #[test]
    fn cdp_errors_map_to_driver_errors() {
        let t: DriverError = CdpError::Timeout {
            method: "Page.navigate".into(),
            after: Duration::from_secs(10),
        }
        .into();
        assert_eq!(t.to_string(), "command timed out after 10 s: Page.navigate");
        let c: DriverError = CdpError::Closed {
            method: "X.y".into(),
        }
        .into();
        assert!(matches!(
            c,
            DriverError::Transport(sb_cdp::TransportError::Closed)
        ));
        let p: DriverError = CdpError::Protocol {
            method: "DOM.focus".into(),
            code: -32000,
            message: "nope".into(),
        }
        .into();
        assert!(matches!(p, DriverError::Cdp(ref m) if m.contains("DOM.focus")));
    }
}
