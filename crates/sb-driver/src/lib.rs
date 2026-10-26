//! sb-driver (B-4). DRAFT signatures for I-9, owned by area driver (T-18, T-20..T-22).
//! No fixed sleeps (rule 6, D-26): every wait is a condition with a cap. The one timer is
//! the act(id) retry slot (D-606), where the schedule itself is the condition.
//!
//! T-18: launch (B-2, D-601) over the pipe transport (B-3, D-600) and the target and frame
//! registry (B-4 #2). `tabs` and `frames` are answered from the registry.
//! T-20: the page runtime (B-4 #3, D-604), `open`, and actionability with retry (B-4 #6,
//! D-603) behind `act`.
//! T-21: input synthesis (B-4 #7), the per-step gate against `ctx.class` (rule 4, H-1.4),
//! consequence capture (B-4 #9) with `dialog` and `download`, and the B-8 diff.
//! T-22: the registry complete (B-4 #2: `close`, fresh titles in `tabs`, B-601), `settle`
//! with network tracking (B-4 #8, D-620) in `settle`, `open` and the act window (B-614),
//! and `index` with every readable frame merged (I-9).

use harness_core::gate::StopReason;
use harness_core::ToolCtx;
use sb_cdp::{Cdp, CdpError, CdpSender, Event, Observer, Pipe, Transport};
use sb_sheet::{ElemRef, IndexRow, TextBlock};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

mod act;
mod actionable;
#[cfg(test)]
mod chromium_t20;
mod consequence;
mod diff;
mod input;
mod launch;
mod merge;
mod registry;
mod runtime;
mod scrub;
mod settle;

pub use launch::{
    binary_name, launch_args, platform_dir, resolve_chromium, LaunchEnv, CHROMIUM_VERSION,
};
pub use registry::{ExecContext, Registry, Seed, SB_WORLD};

use consequence::{lock_events, Events};
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

/// `open`: how long a new tab may take to commit its first document, and then to settle and
/// walk it.
const OPEN_CAP: Duration = Duration::from_secs(30);
/// `close`: how long the browser may take to report the closed tab detached.
const CLOSE_CAP: Duration = Duration::from_secs(5);
/// `tabs`: cap of the one `Target.getTargets` that refreshes titles (B-601).
const TITLES_CAP: Duration = Duration::from_millis(1000);

/// A page URL as `tabs()` and `frames()` give it: scrubbed whole (rule 2), then escaped and
/// capped (`clean_url`), then scrubbed again (what leaves is the shown form).
fn shown_url(url: &str) -> String {
    harness_core::scrub(&diff::clean_url(&harness_core::scrub(url)))
}

/// A page title as `tabs()` gives it (as `OpenedTab`): scrubbed, escaped, capped.
fn shown_title(title: &str) -> String {
    harness_core::scrub(&diff::page_text(
        &harness_core::scrub(title),
        diff::TEXT_CAP,
    ))
}

fn lock(m: &Mutex<Registry>) -> MutexGuard<'_, Registry> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The client observer for a driver: runs for every event, before any subscriber sees it.
/// It applies the event to the registry and the consequence log (B-4 #9), signals the change
/// and sends the attach follow-up.
fn observer(
    registry: Arc<Mutex<Registry>>,
    events: Arc<Mutex<Events>>,
    changed: Arc<watch::Sender<u64>>,
) -> Observer {
    Box::new(move |e, cdp| observe(&registry, &events, &changed, e, cdp))
}

fn bump(changed: &watch::Sender<u64>) {
    changed.send_modify(|v| *v = v.wrapping_add(1));
}

fn observe(
    registry: &Arc<Mutex<Registry>>,
    events: &Arc<Mutex<Events>>,
    changed: &Arc<watch::Sender<u64>>,
    e: &Event,
    cdp: &CdpSender,
) {
    let attached = (e.method == "Target.attachedToTarget")
        .then(|| e.params.get("sessionId").and_then(Value::as_str))
        .flatten();
    let (tracked, seeds) = {
        let mut r = lock(registry);
        r.apply(e.session.as_deref(), &e.method, &e.params);
        (attached.is_some_and(|sid| r.tracks(sid)), r.take_seeds())
    };
    // Never under the registry lock (readers take the registry, then the log).
    lock_events(events).apply(e.session.as_deref(), &e.method, &e.params);
    // D-612: a popup (target=_blank, window.open) starts its first navigation while paused,
    // and its runtime install cannot reply until it runs (no renderer answers before that:
    // measured on chromium-1248, the popup never loaded). Its navigation has started in the
    // browser, so the install is registered there already: run it now.
    if e.method == "Page.frameStartedNavigating" {
        if let Some(sid) = e.session.as_deref() {
            let waiting = {
                let r = lock(registry);
                r.tracks(sid) && !r.installed(sid)
            };
            if waiting {
                drop(cdp.start(Some(sid), "Runtime.runIfWaitingForDebugger", json!({})));
            }
        }
    }
    if let Some(sid) = attached {
        let seed = seeds.iter().any(|s| s.session == sid && !s.replay_contexts);
        on_attached(registry, changed, sid, tracked, seed, cdp);
    }
    for s in seeds.iter().filter(|s| s.replay_contexts) {
        // An activated prerender: its Runtime.enable ran while it was not tracked, so read
        // the frames first, then make Runtime replay its contexts.
        seed_frames(registry, changed, &s.session, cdp);
        drop(cdp.start(Some(&s.session), "Runtime.disable", json!({})));
        drop(cdp.start(Some(&s.session), "Runtime.enable", json!({})));
    }
    // Last: a registry waiter that wakes now and sends to a new session (open's
    // Page.navigate) queues behind the setup above, so the runtime is installed first.
    bump(changed);
}

/// One `Page.getFrameTree` whose reply is applied in wire order (`start_then`), so frames
/// that existed before `Page.enable` are registered before any later event about them.
fn seed_frames(
    registry: &Arc<Mutex<Registry>>,
    changed: &Arc<watch::Sender<u64>>,
    sid: &str,
    cdp: &CdpSender,
) {
    let (registry, changed, session) = (registry.clone(), changed.clone(), sid.to_string());
    cdp.start_then(Some(sid), "Page.getFrameTree", json!({}), move |reply| {
        // An error (the target went away) leaves the registry as it is.
        if let Ok(v) = reply {
            lock(&registry).seed_frame_tree(&session, &v);
            bump(&changed);
        }
    });
}

/// Every auto-attached target is paused (`waitForDebuggerOnStart`). Sessions the registry
/// tracks (tabs, OOPIFs, pending prerenders) get Page and Runtime events, the page runtime
/// (B-4 #3) and auto-attach for their own children; then every target runs. A target that
/// was already running when we attached also gets one `Page.getFrameTree` (its existing
/// frames are not replayed), placed before `Runtime.enable` so the frames are known when its
/// contexts are replayed. All are
/// queued at once (pipelined); the target runs when the runtime install has replied (B-4 #3),
/// so the domains and the runtime are in place before its first document. Other replies are
/// not awaited.
fn on_attached(
    registry: &Arc<Mutex<Registry>>,
    changed: &Arc<watch::Sender<u64>>,
    sid: &str,
    tracked: bool,
    seed: bool,
    cdp: &CdpSender,
) {
    if tracked {
        drop(cdp.start(Some(sid), "Page.enable", json!({})));
        // B-4 #8: requests in flight, for settle (D-620). No POST bodies in the events: the
        // driver only counts requests, and a body can hold a typed value (H-2).
        drop(cdp.start(Some(sid), "Network.enable", json!({"maxPostDataSize": 0})));
        if seed {
            seed_frames(registry, changed, sid, cdp);
        }
        drop(cdp.start(Some(sid), "Runtime.enable", json!({})));
        drop(cdp.start(
            Some(sid),
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
        ));
        // B-4 #3: the page runtime for every new document of this target. The target runs
        // only once the renderer has replied (one round trip, D-604): a navigation is
        // started in the browser process and does not queue behind renderer-side commands,
        // so a target resumed earlier could commit its first document (a popup's, or one
        // created with a URL) without Page events and without the sb world (seen on
        // chromium-1248). The reply is recorded in wire order for `open`.
        let (r, c, session, run) = (
            registry.clone(),
            changed.clone(),
            sid.to_string(),
            cdp.clone(),
        );
        cdp.start_then(
            Some(sid),
            "Page.addScriptToEvaluateOnNewDocument",
            runtime::install_params(),
            move |reply| {
                if reply.is_ok() {
                    lock(&r).set_installed(&session);
                }
                // Resume on any reply: a target whose install failed still runs.
                drop(run.start(Some(&session), "Runtime.runIfWaitingForDebugger", json!({})));
                bump(&c);
            },
        );
        return;
    }
    drop(cdp.start(Some(sid), "Runtime.runIfWaitingForDebugger", json!({})));
}

/// Closes a target `open` created unless disarmed (`target = None`): sent on drop, not
/// awaited.
struct CloseOnDrop<'a> {
    cdp: &'a Cdp,
    target: Option<String>,
}

impl Drop for CloseOnDrop<'_> {
    fn drop(&mut self) {
        if let Some(t) = self.target.take() {
            drop(
                self.cdp
                    .start(None, "Target.closeTarget", json!({ "targetId": t })),
            );
        }
    }
}

/// The driver: one browser, its registry and tabs. Dropping it kills every browser process
/// (waiting up to [`sb_cdp::DROP_KILL_CAP`], blocking the dropping thread) and then removes a
/// `bench` profile. Dropping skips `Browser.close`, so a `me` driver should end with
/// [`Driver::shutdown`] to let Chromium flush cookies and session state.
pub struct Driver {
    cdp: Cdp,
    registry: Arc<Mutex<Registry>>,
    /// Navigations, dialogs and stopped downloads, as the observer saw them (B-4 #9).
    events: Arc<Mutex<Events>>,
    changed: watch::Receiver<u64>,
    /// The browser's own first tab (about:blank, opened by nobody), kept for the first
    /// `open`, so the model's first tab is tab 1 and no stray window stays (D-610).
    spare: Mutex<Option<String>>,
    /// Element id bases per tab frame (D-604).
    bases: Mutex<runtime::Bases>,
    /// Which element of its parent hosts each frame, per parent document (`index`, T-22).
    owners: Mutex<merge::Owners>,
    /// The launch profile: `me` never brings a tab to front (B-606).
    kind: Profile,
    /// `None` only for the in-crate mock transport tests.
    pipe: Option<Arc<Pipe>>,
    // Last: removed only after the browser is gone.
    profile: ProfileDir,
}

impl Drop for Driver {
    fn drop(&mut self) {
        if let Some(pipe) = &self.pipe {
            pipe.kill(sb_cdp::DROP_KILL_CAP);
        }
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
        let events = Arc::new(Mutex::new(Events::default()));
        let (changed_tx, changed) = watch::channel(0u64);
        let cdp = Cdp::with_observer(
            pipe.clone() as Arc<dyn Transport>,
            observer(registry.clone(), events.clone(), Arc::new(changed_tx)),
        );
        // From here a failure drops the Driver, which kills the browser and the profile.
        let driver = Driver {
            cdp,
            registry,
            events,
            changed,
            spare: Mutex::new(None),
            bases: Mutex::new(runtime::Bases::default()),
            owners: Mutex::new(merge::Owners::default()),
            kind: opts.profile,
            pipe: Some(pipe),
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
        // B-4 #9: every download stops before a byte lands and is reported; the user's
        // answer comes through `download` (D-611).
        let downloads = driver.cdp.start(
            None,
            "Browser.setDownloadBehavior",
            json!({"behavior": "deny", "eventsEnabled": true}),
        );
        discover.await?;
        attach.await?;
        downloads.await?;
        // Targets that already exist are attached before setAutoAttach replies (wire order).
        let spare = driver.with_registry(|r| match r.tabs().as_slice() {
            [(t, url, _)] if (url.is_empty() || url == "about:blank") && r.opener(*t).is_none() => {
                r.target_id(*t).map(str::to_string)
            }
            _ => None,
        });
        *driver.spare.lock().unwrap_or_else(|e| e.into_inner()) = spare;
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
        self.pipe.as_ref().map_or(0, |p| p.pid())
    }

    /// Every live process of the browser.
    pub fn pids(&self) -> Vec<u32> {
        self.pipe.as_ref().map_or_else(Vec::new, |p| p.pids())
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

    /// I-9 `open`: a new tab at `url`. Returns once its document has committed, settled
    /// (B-4 #8: the DOM in the page, and the requests the navigation started, D-620), had its
    /// first paint (D-607) and been walked with a fresh base (D-73): all of that after the
    /// commit ends within 3 s of it (D-626), in one round trip after the commit when the
    /// network is quiet by then. A failed or dropped `open` closes the tab it created.
    /// Scheduling (max 6 live tabs, B-4 #10) is not done yet.
    pub async fn open(&self, url: &str) -> Result<TabId, DriverError> {
        // Its own window, so the tab stays visible: a background tab is hidden, and rAF
        // (which act(id) awaits) never fires there (D-605).
        // about:blank first, then Page.navigate on the tab's session once the renderer has
        // acknowledged the runtime install queued at attach (B-4 #3), so the first document
        // of `url` has `__sb`. Page.navigate is handled in the browser process and does not
        // wait for renderer-side commands queued before it: a target created with the URL, or
        // navigated before that reply, committed without Page events or the sb world (seen
        // on chromium-1248).
        // The browser's untouched first tab is used once instead of a new window (D-610).
        let spare = self
            .spare
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .filter(|t| {
                self.with_registry(|r| {
                    r.tab_by_target(t)
                        .and_then(|tab| r.frame_url(tab, 0))
                        .is_some_and(|u| u.is_empty() || u == "about:blank")
                })
            });
        let target = match &spare {
            Some(t) => t.clone(),
            None => {
                let created = self
                    .cdp
                    .call(
                        None,
                        "Target.createTarget",
                        json!({ "url": "about:blank", "newWindow": true }),
                    )
                    .await?;
                created
                    .get("targetId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DriverError::Cdp("Target.createTarget: no targetId".into()))?
                    .to_string()
            }
        };
        // No half-open tab is left behind: on an error, and when this future is dropped. The
        // browser's first tab is never closed here (it may be its last window).
        let mut guard = CloseOnDrop {
            cdp: &self.cdp,
            target: spare.is_none().then(|| target.clone()),
        };
        let tab = self.open_target(&target, url).await?;
        guard.target = None;
        Ok(tab)
    }

    async fn open_target(&self, target: &str, url: &str) -> Result<TabId, DriverError> {
        let (tab, session, g0) = self
            .wait_registry(OPEN_CAP, |r| {
                let t = r.tab_by_target(target)?;
                let s = r.session(t)?;
                r.installed(s)
                    .then(|| Some((t, s.to_string(), r.generation(t, 0)?)))
                    .flatten()
            })
            .await
            .ok_or_else(|| DriverError::Timeout("open: the new tab did not attach".into()))?;
        let blank = url.trim().is_empty() || url.trim().eq_ignore_ascii_case("about:blank");
        // D-620: the requests of this navigation and of the document it brings count.
        let since = self.events_mark();
        // D-626: everything after the main frame's commit (the D-607 paint wait, the settle,
        // the based walk and its reply, and the network pass) shares one deadline, the commit
        // + the 3 s settle cap (B-4 #8). The navigation itself keeps OPEN_CAP.
        let mut commit = Instant::now();
        if !blank {
            let nav = self
                .cdp
                .call(Some(&session), "Page.navigate", json!({ "url": url }))
                .await?;
            if let Some(err) = nav.get("errorText").and_then(Value::as_str) {
                return Err(DriverError::Cdp(format!("open: {err}")));
            }
            if nav.get("isDownload").and_then(Value::as_bool) == Some(true) {
                return Err(DriverError::Cdp(
                    "open: the URL is a download, not a page".into(),
                ));
            }
            // The conditions: the main frame committed a new document (the instant the
            // driver sees it starts D-626's deadline), and that document has an sb context.
            let committed = |r: &Registry| {
                let moved = r.generation(tab, 0)? != g0;
                let u = r.frame_url(tab, 0)?;
                (moved && u != "about:blank").then_some(())
            };
            let no_commit = || {
                DriverError::Timeout(format!(
                    "open: no document committed within {} s",
                    OPEN_CAP.as_secs()
                ))
            };
            self.wait_registry(OPEN_CAP, committed)
                .await
                .ok_or_else(no_commit)?;
            commit = Instant::now();
            self.wait_registry(OPEN_CAP, |r| {
                committed(r)?;
                r.sb_context(tab, 0).map(|_| ())
            })
            .await
            .ok_or_else(no_commit)?;
        }
        let deadline = Instant::now() + OPEN_CAP;
        let defaults = SettleOpts::default();
        let settle_end = commit + Duration::from_millis(u64::from(defaults.cap_ms));
        let mut retried = false;
        loop {
            let old = self.sb_key(tab, 0);
            let iter = self.events_mark();
            // The in-page settle (and the paint wait under it) gets what is left of the
            // deadline minus the reply margin, as settle_tab gives its call.
            let left = settle_end
                .saturating_duration_since(Instant::now())
                .saturating_sub(settle::REPLY_MARGIN);
            let opts = SettleOpts {
                quiet_ms: defaults.quiet_ms,
                cap_ms: u32::try_from(left.as_millis()).unwrap_or(u32::MAX),
            };
            match self.based_ctx(tab, 0, deadline, Some(opts), false).await {
                Ok(_) => {
                    self.open_net_settle(tab, since, iter, settle_end).await;
                    return Ok(tab);
                }
                // The document was replaced under the walk (a redirect): once more in the new
                // one (B-4 #11).
                Err(runtime::CallError::Lost) if !retried => {
                    retried = true;
                    self.next_ctx(tab, 0, old, deadline).await;
                }
                Err(e) => return Err(e.into_driver("open")),
            }
        }
    }

    /// The network part of `open`'s settle (D-620): when a counted request was in flight
    /// during the in-page settle that came with the walk, or is now, settle again after it,
    /// until `end`. The tab is open either way: a failure here is not `open`'s.
    async fn open_net_settle(&self, tab: TabId, since: u64, iter: u64, end: Instant) {
        let involved = self.with_events(|r, ev| {
            settle::started_since(r, ev, tab, iter)
                || settle::net_busy(r, ev, tab, since, Instant::now()).0
        });
        if !involved || Instant::now() >= end {
            return;
        }
        let pass = settle::Pass {
            decl: settle::SETTLE_FN,
            quiet_ms: SettleOpts::default().quiet_ms,
            per_call: None,
            grace: None,
            at_least_once: false,
            pending_since: since,
            count_gaps: false,
        };
        let mark = self.events_mark();
        let run = self.or_dialog(tab, mark, self.net_settle(tab, 0, since, end, pass));
        let _ = tokio::time::timeout_at(end, run).await;
    }

    /// I-9 `act(tab, program, ctx) -> ActResult`. The gate has already passed the program's
    /// class (`ctx.class`); a CHALLENGE row reached mid-program stops it (H-1.4), and so does
    /// a step whose live class exceeds `ctx.class`, before its input. `@vault(k)` in `t#`
    /// steps is resolved only inside `ctx.resolve_into`, straight into the `Input.insertText`
    /// message. Uploads (`u#`) read from `ctx.run_dir/files/`; downloads are saved there.
    pub async fn act(
        &self,
        tab: TabId,
        program: &str,
        ctx: &ToolCtx,
    ) -> Result<ActResult, DriverError> {
        self.act_steps(tab, program, ctx).await
    }

    /// The tab's current index (I-5), all frames merged, ElemRefs and `child_frame` rewritten:
    /// each readable frame's rows follow its iframe row with the frame's registry number
    /// (`#f2.15`); an opaque (cross-origin) frame is its one OPAQUE row, never read inside;
    /// a CHALLENGE frame, and every frame below it, is never walked or read (D-5): a child is
    /// read only after its parent's row and the registry's ancestry allow it. Every row
    /// string is scrubbed (H-2) and capped.
    pub async fn index(&self, tab: TabId) -> Result<Vec<IndexRow>, DriverError> {
        self.merged_index(tab).await
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

    /// (tab, url, title) for every live tab. Titles are read fresh with one
    /// `Target.getTargets` (B-601: Chromium sends no `Target.targetInfoChanged` when
    /// `document.title` changes; its target list has the current title); if that does not
    /// answer within 1 s the registry's last titles are used. The URL and title are page
    /// strings: a page can echo a typed value into them (a GET form), so they go through the
    /// process-wide vault scrub (H-2, rule 2) before they leave, as defence in depth under
    /// the MCP wrapper's own scrub; then they are escaped and capped as consequence strings
    /// are (`page_text` for the title, `clean_url` for the URL, as `OpenedTab`), so a page
    /// cannot write the harness's own syntax (`#12`, `[x](#3)`) into them.
    pub async fn tabs(&self) -> Result<Vec<(TabId, String, String)>, DriverError> {
        let fresh = tokio::time::timeout(
            TITLES_CAP,
            self.cdp.call(None, "Target.getTargets", json!({})),
        )
        .await;
        if let Ok(Ok(v)) = fresh {
            let infos = v.get("targetInfos").and_then(Value::as_array);
            let mut r = lock(&self.registry);
            for info in infos.into_iter().flatten() {
                if info.get("type").and_then(Value::as_str) != Some("page") {
                    continue;
                }
                if let (Some(t), Some(title)) = (
                    info.get("targetId").and_then(Value::as_str),
                    info.get("title").and_then(Value::as_str),
                ) {
                    r.set_title(t, title);
                }
            }
        }
        let tabs = lock(&self.registry).tabs();
        Ok(tabs
            .into_iter()
            .map(|(t, url, title)| (t, shown_url(&url), shown_title(&title)))
            .collect())
    }

    /// I-9 `close` (B-4 #2): close the tab's target and wait (a registry condition, capped
    /// at 5 s) until the browser reports it detached; then the tab, its sessions (OOPIFs'
    /// included), frames, element id bases, frame owners, dialogs and requests in flight are
    /// gone from the driver. `TabGone` for a tab that is not open.
    pub async fn close(&self, tab: TabId) -> Result<(), DriverError> {
        let (target, sessions) = self
            .with_registry(|r| Some((r.target_id(tab)?.to_string(), r.sessions_of(tab))))
            .ok_or(DriverError::TabGone(tab))?;
        {
            let mut spare = self.spare.lock().unwrap_or_else(|e| e.into_inner());
            if spare.as_deref() == Some(target.as_str()) {
                *spare = None;
            }
        }
        let closed = self
            .cdp
            .call(None, "Target.closeTarget", json!({ "targetId": target }))
            .await;
        if let Err(e) = closed {
            // A protocol error means the target is already going away (a page that closed
            // itself): only the cleanup is left. A dead browser is the caller's error.
            if !matches!(e, CdpError::Protocol { .. }) {
                return Err(e.into());
            }
        }
        let gone = self
            .wait_registry(CLOSE_CAP, |r| (!r.has_tab(tab)).then_some(()))
            .await;
        if gone.is_none() {
            lock(&self.registry).forget_tab(tab);
        }
        self.bases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .forget_tab(tab);
        self.owners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .forget_tab(tab);
        let mut ev = lock_events(&self.events);
        for s in &sessions {
            ev.forget_session(s);
        }
        Ok(())
    }

    /// Answer a paused dialog (B-4 #9): accept or dismiss it; `text` answers a prompt. The
    /// page continues. An error when no dialog is open on the tab.
    pub async fn dialog(
        &self,
        tab: TabId,
        accept: bool,
        text: Option<&str>,
    ) -> Result<(), DriverError> {
        self.answer_dialog(tab, accept, text).await
    }

    /// After the ask: let a stopped download land in `runs/<id>/files/` (returns the path it
    /// was saved to), or cancel it (`None`). `guid` is from [`Consequence::Download`]; each
    /// download is answered once. The browser stopped it before any byte landed, so an
    /// allowed one is fetched again with the page's cookies (B-608: not `blob:` URLs).
    pub async fn download(
        &self,
        tab: TabId,
        guid: &str,
        allow: bool,
    ) -> Result<Option<PathBuf>, DriverError> {
        self.answer_download(tab, guid, allow).await
    }

    /// I-9 `settle` (B-4 #8): settled when frame 0's DOM has had no mutation for
    /// `opts.quiet_ms`, no finite CSS animation runs on changed nodes, and no request that
    /// counts (D-620: started on the tab after this call, not a stream, younger than 3 s) is
    /// in flight. `opts.cap_ms` is a hard deadline: at it, `{settled: false}`. `changed` is
    /// the number of DOM mutations the page saw meanwhile.
    pub async fn settle(&self, tab: TabId, opts: SettleOpts) -> Result<Settled, DriverError> {
        self.settle_tab(tab, opts).await
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
        let mut frames = lock(&self.registry)
            .frames(tab)
            .ok_or(DriverError::TabGone(tab))?;
        // Page strings: scrubbed and escaped as in `tabs()` (H-2, pr-ready 5).
        for f in &mut frames {
            f.url = shown_url(&f.url);
        }
        Ok(frames)
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

    use bytes::Bytes;
    use tokio::sync::mpsc;

    /// In-memory transport: the test reads what the driver's client sent and injects replies
    /// and events.
    struct Mock {
        out: mpsc::UnboundedSender<Bytes>,
        inbound: Mutex<Option<mpsc::Receiver<Bytes>>>,
    }

    #[async_trait::async_trait]
    impl Transport for Mock {
        async fn send(&self, msg: Bytes) -> Result<(), sb_cdp::TransportError> {
            self.out
                .send(msg)
                .map_err(|_| sb_cdp::TransportError::Closed)
        }
        fn recv(&self) -> mpsc::Receiver<Bytes> {
            self.inbound
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .unwrap_or_else(|| mpsc::channel(1).1)
        }
    }

    /// A client with the driver's observer over [`Mock`].
    struct Wired {
        cdp: Cdp,
        registry: Arc<Mutex<Registry>>,
        changed: watch::Receiver<u64>,
        sent: mpsc::UnboundedReceiver<Bytes>,
        inject: mpsc::Sender<Bytes>,
    }

    fn wired() -> Wired {
        let (out, sent) = mpsc::unbounded_channel();
        let (inject, inbound) = mpsc::channel(64);
        let registry = Arc::new(Mutex::new(Registry::new()));
        let (tx, changed) = watch::channel(0u64);
        let mock = Arc::new(Mock {
            out,
            inbound: Mutex::new(Some(inbound)),
        });
        let events = Arc::new(Mutex::new(Events::default()));
        let cdp = Cdp::with_observer(mock, observer(registry.clone(), events, Arc::new(tx)));
        Wired {
            cdp,
            registry,
            changed,
            sent,
            inject,
        }
    }

    impl Wired {
        async fn inject(&self, v: Value) {
            self.inject
                .send(Bytes::from(v.to_string()))
                .await
                .expect("reader alive");
        }

        /// The next `n` messages the client wrote, as (session, method, id).
        async fn sent(&mut self, n: usize) -> Vec<(Option<String>, String, Value)> {
            let mut v = Vec::new();
            for _ in 0..n {
                let raw = tokio::time::timeout(Duration::from_secs(5), self.sent.recv())
                    .await
                    .expect("a message within 5 s")
                    .expect("transport open");
                let m: Value = serde_json::from_slice(&raw).expect("json");
                v.push((
                    m["sessionId"].as_str().map(str::to_string),
                    m["method"].as_str().unwrap_or_default().to_string(),
                    m["id"].clone(),
                ));
            }
            v
        }

        /// The next `n` messages the client wrote, whole.
        async fn sent_raw(&mut self, n: usize) -> Vec<Value> {
            let mut v = Vec::new();
            for _ in 0..n {
                let raw = tokio::time::timeout(Duration::from_secs(5), self.sent.recv())
                    .await
                    .expect("a message within 5 s")
                    .expect("transport open");
                v.push(serde_json::from_slice(&raw).expect("json"));
            }
            v
        }

        fn nothing_more_sent(&mut self) -> bool {
            self.sent.try_recv().is_err()
        }

        /// Wait (capped at 5 s) until the registry satisfies `f`.
        async fn until(&mut self, what: &str, f: impl Fn(&Registry) -> bool) {
            let wait = async {
                loop {
                    self.changed.borrow_and_update();
                    if f(&lock(&self.registry)) {
                        return;
                    }
                    self.changed.changed().await.expect("observer alive");
                }
            };
            tokio::time::timeout(Duration::from_secs(5), wait)
                .await
                .unwrap_or_else(|_| panic!("{what} within 5 s"));
        }
    }

    fn methods(v: &[(Option<String>, String, Value)]) -> Vec<&str> {
        v.iter().map(|(_, m, _)| m.as_str()).collect()
    }

    #[tokio::test]
    async fn running_tab_is_seeded_in_wire_order_with_pipelined_setup() {
        let mut w = wired();
        w.inject(json!({"method": "Target.attachedToTarget", "params": {
            "sessionId": "S1", "waitingForDebugger": false,
            "targetInfo": {"targetId": "T1", "type": "page", "url": "https://a.test/"}}}))
            .await;
        // The setup is on the wire before any reply: no serial round trip. The frame tree is
        // read before Runtime.enable, so replayed contexts find their frames.
        let sent = w.sent(6).await;
        assert_eq!(
            methods(&sent),
            [
                "Page.enable",
                "Network.enable",
                "Page.getFrameTree",
                "Runtime.enable",
                "Target.setAutoAttach",
                "Page.addScriptToEvaluateOnNewDocument",
            ]
        );
        assert!(sent.iter().all(|(s, _, _)| s.as_deref() == Some("S1")));
        let tree_id = sent[2].2.clone();
        w.inject(json!({"id": tree_id, "result": {"frameTree": {
            "frame": {"id": "T1", "url": "https://a.test/"},
            "childFrames": [{"frame": {"id": "F1", "parentId": "T1", "url": "https://a.test/in"}}]}}}))
            .await;
        // Runtime.enable's replay, right after the reply on the wire.
        w.inject(
            json!({"sessionId": "S1", "method": "Runtime.executionContextCreated", "params": {
            "context": {"id": 5, "origin": "https://a.test", "name": "", "uniqueId": "u5",
                "auxData": {"isDefault": true, "type": "default", "frameId": "F1"}}}}),
        )
        .await;
        w.until("child frame with its context", |r| {
            r.tab_by_target("T1")
                .and_then(|t| r.default_context(t, 1))
                .is_some_and(|c| c.id == 5)
        })
        .await;
        assert_eq!(
            w.cdp.pending(),
            1,
            "the seed's slot is gone after its reply; the install's waits for its own"
        );
    }

    #[tokio::test]
    async fn paused_tab_gets_no_seed_and_a_second_session_only_runs() {
        let mut w = wired();
        w.inject(json!({"method": "Target.attachedToTarget", "params": {
            "sessionId": "S1", "waitingForDebugger": true,
            "targetInfo": {"targetId": "T1", "type": "page", "url": "about:blank"}}}))
            .await;
        let sent = w.sent(5).await;
        assert_eq!(
            methods(&sent),
            [
                "Page.enable",
                "Network.enable",
                "Runtime.enable",
                "Target.setAutoAttach",
                "Page.addScriptToEvaluateOnNewDocument",
            ]
        );
        // A second session on the same page (explicit attach): not tracked, nothing enabled.
        w.inject(json!({"method": "Target.attachedToTarget", "params": {
            "sessionId": "S2", "waitingForDebugger": false,
            "targetInfo": {"targetId": "T1", "type": "page", "url": "about:blank"}}}))
            .await;
        let sent = w.sent(1).await;
        assert_eq!(methods(&sent), ["Runtime.runIfWaitingForDebugger"]);
        assert_eq!(sent[0].0.as_deref(), Some("S2"));
        assert!(w.nothing_more_sent());
        let t = lock(&w.registry).tab_by_target("T1").unwrap();
        assert_eq!(lock(&w.registry).session(t), Some("S1"));
    }

    #[tokio::test]
    async fn paused_target_runs_only_after_the_runtime_install_replied() {
        let mut w = wired();
        w.inject(json!({"method": "Target.attachedToTarget", "params": {
            "sessionId": "S1", "waitingForDebugger": true,
            "targetInfo": {"targetId": "T1", "type": "page", "url": "about:blank"}}}))
            .await;
        let sent = w.sent_raw(5).await;
        assert_eq!(sent[1]["method"], "Network.enable");
        assert_eq!(
            sent[1]["params"]["maxPostDataSize"], 0,
            "no POST bodies (H-2)"
        );
        let install = &sent[4];
        assert_eq!(install["method"], "Page.addScriptToEvaluateOnNewDocument");
        assert_eq!(install["sessionId"], "S1");
        assert_eq!(install["params"]["worldName"], SB_WORLD);
        assert_eq!(install["params"]["source"], runtime::INJECTED_JS);
        assert!(
            install["params"].get("runImmediately").is_none(),
            "an sb context in a new tab belongs to a navigated document (D-604)"
        );
        // Still paused: its first document must not start before the install is in.
        assert!(w.nothing_more_sent());
        assert!(!lock(&w.registry).installed("S1"));
        w.inject(
            json!({"id": install["id"].clone(), "sessionId": "S1", "result": {"identifier": "1"}}),
        )
        .await;
        let run = w.sent_raw(1).await;
        assert_eq!(run[0]["method"], "Runtime.runIfWaitingForDebugger");
        assert_eq!(run[0]["sessionId"], "S1");
        w.until("install acknowledged", |r| r.installed("S1")).await;
        // An error reply (the target went away) resumes it but never counts as installed.
        w.inject(json!({"method": "Target.attachedToTarget", "params": {
            "sessionId": "S2", "waitingForDebugger": true,
            "targetInfo": {"targetId": "T2", "type": "page", "url": "about:blank"}}}))
            .await;
        let sent = w.sent_raw(5).await;
        w.inject(json!({"id": sent[4]["id"].clone(), "sessionId": "S2",
            "error": {"code": -32000, "message": "gone"}}))
            .await;
        let run = w.sent_raw(1).await;
        assert_eq!(run[0]["method"], "Runtime.runIfWaitingForDebugger");
        assert_eq!(run[0]["sessionId"], "S2");
        w.inject(json!({"method": "Target.detachedFromTarget", "params": {"sessionId": "S1"}}))
            .await;
        w.until("S1 gone", |r| !r.tracks("S1")).await;
        let r = lock(&w.registry);
        assert!(!r.installed("S2"));
        assert!(!r.installed("S1"), "forgotten with its session");
    }

    #[tokio::test]
    async fn activated_prerender_is_seeded_then_contexts_replayed() {
        let mut w = wired();
        w.inject(json!({"method": "Target.attachedToTarget", "params": {
            "sessionId": "S1", "waitingForDebugger": true,
            "targetInfo": {"targetId": "T1", "type": "page", "url": "about:blank"}}}))
            .await;
        w.sent(5).await;
        w.inject(json!({"sessionId": "S1", "method": "Target.attachedToTarget", "params": {
            "sessionId": "P1", "waitingForDebugger": true,
            "targetInfo": {"targetId": "TP", "type": "page", "subtype": "prerender", "url": "https://r.test/"}}}))
            .await;
        // Tracked (so it has events once it activates), but not a tab.
        assert_eq!(
            methods(&w.sent(5).await),
            [
                "Page.enable",
                "Network.enable",
                "Runtime.enable",
                "Target.setAutoAttach",
                "Page.addScriptToEvaluateOnNewDocument",
            ]
        );
        assert_eq!(lock(&w.registry).tabs().len(), 1);
        w.inject(json!({"method": "Target.targetInfoChanged", "params": {
            "targetInfo": {"targetId": "TP", "type": "page", "url": "https://r.test/", "title": "R"}}}))
            .await;
        let sent = w.sent(3).await;
        assert_eq!(
            methods(&sent),
            ["Page.getFrameTree", "Runtime.disable", "Runtime.enable"]
        );
        assert!(sent.iter().all(|(s, _, _)| s.as_deref() == Some("P1")));
        let t = lock(&w.registry)
            .tab_by_target("TP")
            .expect("tab follows the prerender");
        assert_eq!(t, TabId(1));
        assert_eq!(lock(&w.registry).session(t), Some("P1"));
    }

    // ---- a Driver over the mock transport (no browser) ----

    fn mock_driver(kind: Profile) -> (Driver, Script) {
        let (out, sent) = mpsc::unbounded_channel();
        let (inject, inbound) = mpsc::channel(64);
        let registry = Arc::new(Mutex::new(Registry::new()));
        let (tx, changed) = watch::channel(0u64);
        let mock = Arc::new(Mock {
            out,
            inbound: Mutex::new(Some(inbound)),
        });
        let events = Arc::new(Mutex::new(Events::default()));
        let cdp = Cdp::with_observer(
            mock,
            observer(registry.clone(), events.clone(), Arc::new(tx)),
        );
        let d = Driver {
            cdp,
            registry,
            events,
            changed,
            spare: Mutex::new(None),
            bases: Mutex::new(runtime::Bases::default()),
            owners: Mutex::new(merge::Owners::default()),
            kind,
            pipe: None,
            profile: ProfileDir::Bench(tempfile::tempdir().expect("tempdir")),
        };
        (d, Script { sent, inject })
    }

    /// The browser's side of the mock: reads what the driver sent, answers it.
    struct Script {
        sent: mpsc::UnboundedReceiver<Bytes>,
        inject: mpsc::Sender<Bytes>,
    }

    impl Script {
        async fn send(&self, v: Value) {
            self.inject
                .send(Bytes::from(v.to_string()))
                .await
                .expect("reader alive");
        }

        async fn next(&mut self) -> Value {
            let raw = tokio::time::timeout(Duration::from_secs(5), self.sent.recv())
                .await
                .expect("a message within 5 s")
                .expect("transport open");
            serde_json::from_slice(&raw).expect("json")
        }

        /// The next message, which must be `method`.
        async fn expect(&mut self, method: &str) -> Value {
            let m = self.next().await;
            assert_eq!(m["method"], method, "{m}");
            m
        }

        async fn reply(&self, m: &Value, result: Value) {
            self.send(json!({"id": m["id"].clone(), "sessionId": m["sessionId"].clone(), "result": result}))
                .await;
        }

        async fn fail(&self, m: &Value, message: &str) {
            self.send(
                json!({"id": m["id"].clone(), "sessionId": m["sessionId"].clone(),
                "error": {"code": -32000, "message": message}}),
            )
            .await;
        }

        async fn sb_context(&self, id: i64) {
            self.send(json!({"sessionId": "S1", "method": "Runtime.executionContextCreated", "params": {
                "context": {"id": id, "origin": "https://a.test", "name": SB_WORLD, "uniqueId": format!("u{id}"),
                    "auxData": {"isDefault": false, "type": "isolated", "frameId": "T1"}}}}))
                .await;
        }

        /// A paused page target T1 on session S1, its install acknowledged, resumed.
        async fn attach_paused(&mut self) {
            self.send(json!({"method": "Target.attachedToTarget", "params": {
                "sessionId": "S1", "waitingForDebugger": true,
                "targetInfo": {"targetId": "T1", "type": "page", "url": "about:blank"}}}))
                .await;
            for _ in 0..4 {
                self.next().await;
            }
            let install = self.expect("Page.addScriptToEvaluateOnNewDocument").await;
            self.reply(&install, json!({"identifier": "1"})).await;
            self.expect("Runtime.runIfWaitingForDebugger").await;
        }

        /// A `Runtime.callFunctionOn` whose declaration contains `marker`, in context `ctx`.
        async fn call(&mut self, marker: &str, ctx: i64) -> Value {
            let m = self.expect("Runtime.callFunctionOn").await;
            let decl = m["params"]["functionDeclaration"]
                .as_str()
                .unwrap_or_default();
            assert!(decl.contains(marker), "{marker} expected, got {decl}");
            assert_eq!(m["params"]["executionContextId"], ctx, "{m}");
            m
        }

        async fn value(&self, m: &Value, v: Value) {
            self.reply(m, json!({"result": {"type": "object", "value": v}}))
                .await;
        }

        fn quiet(&mut self) -> bool {
            self.sent.try_recv().is_err()
        }
    }

    const WALK: &str = "sb.walk(";
    const ACT: &str = "sb.act(id)";
    const SETTLE: &str = "sb.settle(opts)";
    const LOST: &str = "Execution context was destroyed.";

    /// A tab with frame 0's sb context 2 in the registry.
    async fn mock_tab(d: &Driver, s: &mut Script) -> TabId {
        s.attach_paused().await;
        s.sb_context(2).await;
        d.wait_registry(Duration::from_secs(5), |r| {
            let t = r.tab_by_target("T1")?;
            r.sb_context(t, 0).map(|_| t)
        })
        .await
        .expect("tab with an sb context")
    }

    /// B-4 #11: one lost context is retried in the next one; a second loss is the answer,
    /// with no third call.
    #[tokio::test]
    async fn a_context_lost_twice_is_not_retried_a_third_time() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        let deadline = Instant::now() + Duration::from_secs(5);
        let (r, ()) = tokio::join!(
            d.call_sb(tab, 0, "function () { return 1; }", &[], deadline),
            async {
                let walk = s.call(WALK, 2).await;
                s.fail(&walk, LOST).await;
                s.sb_context(3).await;
                let walk = s.call(WALK, 3).await;
                s.fail(&walk, LOST).await;
            }
        );
        assert!(matches!(r, Err(runtime::CallError::Lost)), "{r:?}");
        assert!(s.quiet(), "no third call after a second loss");
    }

    /// The same rule in `open`, and a failed `open` closes the tab it created.
    #[tokio::test]
    async fn a_failed_open_is_retried_once_and_closes_its_half_open_tab() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let (r, ()) = tokio::join!(d.open("https://a.test/"), async {
            let create = s.expect("Target.createTarget").await;
            s.reply(&create, json!({"targetId": "T1"})).await;
            s.attach_paused().await;
            let nav = s.expect("Page.navigate").await;
            assert_eq!(nav["sessionId"], "S1");
            s.reply(&nav, json!({"frameId": "T1", "loaderId": "L2"}))
                .await;
            s.send(json!({"sessionId": "S1", "method": "Page.frameNavigated", "params": {
                "frame": {"id": "T1", "url": "https://a.test/", "loaderId": "L2"}, "type": "Navigation"}}))
                .await;
            s.sb_context(2).await;
            let walk = s.call(WALK, 2).await;
            assert!(
                walk["params"]["arguments"][1]["value"].is_object(),
                "open settles"
            );
            s.fail(&walk, LOST).await;
            s.sb_context(3).await;
            let walk = s.call(WALK, 3).await;
            s.fail(&walk, LOST).await;
            let close = s.expect("Target.closeTarget").await;
            assert_eq!(close["params"]["targetId"], "T1");
        });
        let e = r.expect_err("lost twice");
        assert!(e.to_string().contains("context went away"), "{e}");
        assert!(s.quiet(), "no third walk");
    }

    /// D-605 and B-606: a hidden `bench` tab is fronted once; a hidden `me` tab never is
    /// (it would take the user's focus) and fails the step.
    #[tokio::test]
    async fn bring_to_front_is_never_sent_for_profile_me() {
        for kind in [Profile::Me, Profile::Bench] {
            let (d, mut s) = mock_driver(kind);
            let tab = mock_tab(&d, &mut s).await;
            let (r, ()) = tokio::join!(d.actionable(tab, ElemRef::main(5)), async {
                let walk = s.call(WALK, 2).await;
                s.value(&walk, json!({"count": 9, "settled": null})).await;
                let act = s.call(ACT, 2).await;
                s.value(&act, json!({"why": "hidden tab"})).await;
                if kind == Profile::Bench {
                    let front = s.expect("Page.bringToFront").await;
                    assert_eq!(front["sessionId"], "S1");
                    s.reply(&front, json!({})).await;
                    let act = s.call(ACT, 2).await;
                    s.value(&act, json!({"x": 10, "y": 20})).await;
                }
            });
            let r = r.expect("no driver error");
            match kind {
                Profile::Me => {
                    let w = r.expect_err("a hidden me tab fails the step");
                    assert_eq!(w.text, actionable::TAB_HIDDEN_ME);
                    assert_eq!((w.kind, w.attempts), (actionable::WhyKind::Failed, 1));
                    assert!(s.quiet(), "no Page.bringToFront for me");
                }
                Profile::Bench => assert!(r.is_ok(), "{r:?}"),
            }
        }
    }

    /// accept-T-20 note: a `w(d)` whose settle does not answer in time fails its step; the
    /// program's result keeps its steps instead of becoming an error.
    #[tokio::test]
    async fn a_w_step_that_times_out_is_a_failed_step() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        let home = tempfile::tempdir().unwrap();
        let ctx = chromium_t20::tool_ctx(home.path());
        let (r, ()) = tokio::join!(d.act(tab, "w(50ms); c#5", &ctx), async {
            let walk = s.call(WALK, 2).await;
            s.value(&walk, json!({"count": 9, "settled": null})).await;
            // The settle never answers (a busy page under load).
            s.call(SETTLE, 2).await;
        });
        let r = r.expect("a failed step, not an Err for the program");
        assert_eq!(r.steps.len(), 1, "{:?}", r.steps);
        assert_eq!(r.steps[0].step, "w(50ms)");
        assert_eq!(
            r.steps[0].outcome,
            StepOutcome::Failed {
                why: "the page did not answer within 350 ms".into()
            }
        );
        assert!(s.quiet(), "c#5 did not run");
    }

    /// D-606: a CDP failure during the wait between attempts ends the step with its reason;
    /// it is not an instant retry.
    #[tokio::test]
    async fn a_cdp_error_during_the_wait_fails_the_step() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        let (r, ()) = tokio::join!(d.actionable(tab, ElemRef::main(5)), async {
            let walk = s.call(WALK, 2).await;
            s.value(&walk, json!({"count": 9, "settled": null})).await;
            for _ in 0..2 {
                // Attempt 1, then attempt 2 at once (the first slot is 0 ms).
                let act = s.call(ACT, 2).await;
                s.value(&act, json!({"why": "disabled"})).await;
            }
            let wait = s.call(SETTLE, 2).await;
            let cap = wait["params"]["arguments"][0]["value"]["cap"]
                .as_u64()
                .unwrap_or(0);
            assert!((40..=50).contains(&cap), "the 50 ms slot: {wait}");
            s.fail(&wait, "Object reference chain is too long").await;
        });
        let w = r.expect("driver").expect_err("failed step");
        assert_eq!(w.kind, actionable::WhyKind::Failed);
        assert!(
            w.text.contains("Object reference chain is too long"),
            "{}",
            w.text
        );
        assert_eq!(w.attempts, 2);
        assert!(s.quiet(), "no attempt after the failed wait");
    }

    // ---- T-21 over the mock transport: what goes on the wire ----

    const INDEX: &str = "return sb.index();";
    const POST: &str = "const ix = sb.index();";

    /// A ctx approving `class`, with one vault entry `pw` holding `value`.
    fn vault_ctx(dir: &Path, class: harness_core::Class, value: &str) -> ToolCtx {
        use harness_core::testing::{MemStore, ScriptedAsker};
        use harness_core::{Harness, RunId, ToolRegistry, Vault};
        let harness = Harness {
            store: Arc::new(MemStore::new()),
            vault: Arc::new(Vault::in_memory([(
                "pw".to_string(),
                value.to_string(),
                harness_core::vault::EntryKind::Secret,
            )])),
            asker: Arc::new(ScriptedAsker::new()),
            runs_root: dir.to_path_buf(),
            run: None,
        };
        ToolCtx::new(
            RunId("t21".into()),
            dir.join("t21"),
            class,
            &harness,
            ToolRegistry::new(vec![]),
        )
    }

    /// The canary of these tests (never printed).
    fn canary() -> String {
        std::env::var("SB_VAULT_CANARY")
            .ok()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "canary-t21-mock-not-a-secret".to_owned())
    }

    impl Script {
        /// The based walk of the first call into context 2, answered.
        async fn walked(&mut self) {
            let walk = self.call(WALK, 2).await;
            self.value(&walk, json!({"count": 3, "settled": null}))
                .await;
        }
    }

    /// H-2, D-55, B-4 #7: the vault value is resolved into the Input.insertText message and
    /// nowhere else; select-all and insertText are queued together after the focusing act
    /// call (no round trip between them); the result carries no value.
    #[tokio::test]
    async fn a_vault_value_goes_only_into_the_insert_text_message() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        let home = tempfile::tempdir().unwrap();
        let secret = canary();
        let ctx = vault_ctx(home.path(), harness_core::Class::Fill, &secret);
        let pw_row = json!([{"id": 7, "frame": 0, "role": "textbox", "name": "Password",
            "state": {"input_type": "password"}}]);
        let mut wire: Vec<Vec<u8>> = Vec::new();
        let (r, ()) = tokio::join!(d.act(tab, "t#7=@vault(pw)", &ctx), async {
            s.walked().await;
            let ix = s.call(INDEX, 2).await;
            s.value(&ix, pw_row.clone()).await;
            let act = s.call(ACT, 2).await;
            assert_eq!(
                act["params"]["arguments"][1]["value"],
                json!({"focus": true, "text": true})
            );
            s.value(&act, json!({"x": 10, "y": 20, "op": {"focused": true}}))
                .await;
            // All three on the wire before any reply.
            let mut queued = Vec::new();
            for _ in 0..3 {
                let raw = tokio::time::timeout(Duration::from_secs(5), s.sent.recv())
                    .await
                    .expect("queued")
                    .expect("open");
                wire.push(raw.to_vec());
                queued.push(serde_json::from_slice::<Value>(&raw).expect("json"));
            }
            let methods: Vec<&str> = queued
                .iter()
                .map(|m| m["method"].as_str().unwrap_or_default())
                .collect();
            assert_eq!(
                methods,
                [
                    "Input.dispatchKeyEvent",
                    "Input.dispatchKeyEvent",
                    "Input.insertText"
                ]
            );
            assert_eq!(queued[0]["params"]["commands"], json!(["selectAll"]));
            assert_eq!(queued[2]["sessionId"], "S1");
            assert!(
                queued[2]["params"]["text"].as_str() == Some(secret.as_str()),
                "the resolved value is the insertText text"
            );
            for m in &queued {
                s.reply(m, json!({})).await;
            }
            let post = s.call(POST, 2).await;
            // B-4 #9: settle with the 150 ms quiet window, capped at 500 ms.
            assert_eq!(
                post["params"]["arguments"][0]["value"],
                json!({"quiet": 150, "cap": 500})
            );
            let mut after = pw_row.clone();
            after[0]["state"]["value"] = json!({"kind": "filled"});
            s.value(&post, json!({"settled": true, "index": after}))
                .await;
        });
        let r = r.expect("act");
        assert_eq!(r.steps.len(), 1);
        assert_eq!(r.steps[0].outcome, StepOutcome::Ok, "{:?}", r.steps);
        assert!(
            !r.steps[0]
                .consequences
                .contains(&Consequence::NoVisibleChange),
            "filled is a change"
        );
        assert_eq!(r.diff, None, "a value change prints nothing");
        let dbg = format!("{r:?}");
        assert!(!dbg.contains(&secret), "the value is not in the result");
        let insert_only = wire
            .iter()
            .filter(|m| m.windows(secret.len()).any(|w| w == secret.as_bytes()))
            .count();
        assert_eq!(insert_only, 1, "only the insertText message carries it");
        assert!(s.quiet(), "nothing after the window");
    }

    /// Rule 4, H-1.4: a step whose live class is above ctx.class stops before any input; the
    /// steps after it do not run.
    #[tokio::test]
    async fn a_submit_step_under_a_fill_approval_sends_nothing() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        let home = tempfile::tempdir().unwrap();
        let ctx = vault_ctx(home.path(), harness_core::Class::Fill, &canary());
        let rows = json!([
            {"id": 7, "frame": 0, "role": "textbox", "name": "Email", "form": "f1"},
            {"id": 14, "frame": 0, "role": "button", "name": "Sign in", "form": "f1",
             "flags": {"submit": true}, "state": {"submitish": true}}
        ]);
        let (r, ()) = tokio::join!(d.act(tab, "c#14; t#7=x", &ctx), async {
            s.walked().await;
            let ix = s.call(INDEX, 2).await;
            s.value(&ix, rows.clone()).await;
        });
        let r = r.expect("act");
        assert_eq!(r.steps.len(), 1);
        let stop = r.stopped.expect("stopped");
        assert_eq!(stop.class, harness_core::Class::Submit);
        assert_eq!(stop.element.as_deref(), Some("#14"));
        // No act call, no Input.*, and the diff reuses the index the gate read.
        assert!(s.quiet(), "nothing after the gate's index read");
        assert_eq!(r.diff, None);
    }

    /// B-4 #9: a click whose handler opens alert() never gets its mouseReleased reply; the
    /// dialog ends the window, the step reports it, and nothing else is sent to the page.
    #[tokio::test]
    async fn a_dialog_ends_a_click_whose_release_never_answers() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        let home = tempfile::tempdir().unwrap();
        let ctx = vault_ctx(home.path(), harness_core::Class::Navigate, &canary());
        let rows = json!([{"id": 5, "frame": 0, "role": "button", "name": "Alert"}]);
        let (r, ()) = tokio::join!(d.act(tab, "c#5; c#5", &ctx), async {
            s.walked().await;
            let ix = s.call(INDEX, 2).await;
            s.value(&ix, rows.clone()).await;
            let act = s.call(ACT, 2).await;
            s.value(&act, json!({"x": 10, "y": 20})).await;
            let mv = s.expect("Input.dispatchMouseEvent").await;
            let down = s.expect("Input.dispatchMouseEvent").await;
            let up = s.expect("Input.dispatchMouseEvent").await;
            assert_eq!(up["params"]["type"], "mouseReleased");
            s.reply(&mv, json!({})).await;
            s.reply(&down, json!({})).await;
            s.send(json!({"sessionId": "S1", "method": "Page.javascriptDialogOpening",
                "params": {"message": "Sure? [ok](#9)", "type": "confirm", "url": "https://a.test/"}}))
                .await;
        });
        let r = r.expect("act");
        assert_eq!(r.steps.len(), 1, "the program stops at the dialog");
        assert_eq!(r.steps[0].outcome, StepOutcome::Ok);
        assert_eq!(
            r.steps[0].consequences,
            [Consequence::Dialog {
                text: "Sure? (ok)(＃9)".into()
            }]
        );
        assert_eq!(r.diff, None, "the page is paused: no read");
        assert!(s.quiet(), "no in-page call while the dialog is open");
        let (answer, ()) = tokio::join!(d.dialog(tab, false, Some("typed")), async {
            let h = s.expect("Page.handleJavaScriptDialog").await;
            assert_eq!(h["sessionId"], "S1");
            assert_eq!(h["params"], json!({"accept": false, "promptText": "typed"}));
            s.reply(&h, json!({})).await;
        });
        answer.expect("answered");
        assert!(d.dialog(tab, true, None).await.is_err(), "answered once");
    }

    // ---- T-22 over the mock transport: network-aware settle (B-4 #8, D-620) ----

    impl Script {
        async fn request(&self, id: &str) {
            self.send(
                json!({"sessionId": "S1", "method": "Network.requestWillBeSent",
                "params": {"requestId": id, "type": "XHR", "frameId": "T1",
                           "request": {"url": "https://a.test/api", "method": "POST"}}}),
            )
            .await;
        }
    }

    /// B-614, T-21b (4): a Submit click whose XHR is still in flight after the 500 ms
    /// window waits for it before the settle-and-index call, and since it never finished,
    /// the step never reports NoVisibleChange. The whole window ends by input + 3 s: the
    /// wait stops early enough for one index-only call (settle cap 0), which answers before
    /// the deadline (B-4 #8, review-T-22 blocking 2).
    #[tokio::test]
    async fn a_step_whose_request_outlasts_the_window_never_claims_no_change() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        let home = tempfile::tempdir().unwrap();
        let ctx = vault_ctx(home.path(), harness_core::Class::Submit, &canary());
        let rows = json!([{"id": 5, "frame": 0, "role": "button", "name": "Save"}]);
        let (r, (waited, t_input)) = tokio::join!(d.act(tab, "c#5", &ctx), async {
            s.walked().await;
            let ix = s.call(INDEX, 2).await;
            s.value(&ix, rows.clone()).await;
            let act = s.call(ACT, 2).await;
            s.value(&act, json!({"x": 10, "y": 20})).await;
            let mut input = Vec::new();
            for _ in 0..3 {
                input.push(s.expect("Input.dispatchMouseEvent").await);
            }
            // The click handler's XHR, announced before the input's replies (wire order).
            s.request("R1").await;
            let t_input = Instant::now();
            for m in &input {
                s.reply(m, json!({})).await;
            }
            let post = s.call(POST, 2).await;
            let waited = t_input.elapsed();
            assert_eq!(
                post["params"]["arguments"][0]["value"],
                json!({"quiet": 150, "cap": 0}),
                "index-only: the network never went idle"
            );
            s.value(&post, json!({"settled": false, "index": rows.clone()}))
                .await;
            (waited, t_input)
        });
        let total = t_input.elapsed();
        assert!(
            waited >= Duration::from_millis(2500),
            "the window waited for the request first: {waited:?}"
        );
        assert!(
            total <= Duration::from_millis(3040),
            "the window ends by input + 3 s: {total:?}"
        );
        let r = r.expect("act");
        assert_eq!(r.steps.len(), 1);
        assert_eq!(r.steps[0].outcome, StepOutcome::Ok);
        assert!(
            !r.steps[0]
                .consequences
                .contains(&Consequence::NoVisibleChange),
            "{:?}",
            r.steps[0]
        );
        assert_eq!(r.steps[0].settled, Some(false), "the network never settled");
        assert!(s.quiet(), "nothing after the window");
    }

    /// The same step with no request: one call at once, and an empty diff is NoVisibleChange
    /// (the T-21 behaviour is kept when the network is quiet).
    #[tokio::test]
    async fn a_quiet_step_still_reports_no_visible_change_in_one_call() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        let home = tempfile::tempdir().unwrap();
        let ctx = vault_ctx(home.path(), harness_core::Class::Submit, &canary());
        let rows = json!([{"id": 5, "frame": 0, "role": "button", "name": "Save"}]);
        let (r, ()) = tokio::join!(d.act(tab, "c#5", &ctx), async {
            s.walked().await;
            let ix = s.call(INDEX, 2).await;
            s.value(&ix, rows.clone()).await;
            let act = s.call(ACT, 2).await;
            s.value(&act, json!({"x": 10, "y": 20})).await;
            for _ in 0..3 {
                let m = s.expect("Input.dispatchMouseEvent").await;
                s.reply(&m, json!({})).await;
            }
            let post = s.call(POST, 2).await;
            s.value(&post, json!({"settled": true, "index": rows.clone()}))
                .await;
        });
        let r = r.expect("act");
        assert_eq!(r.steps[0].consequences, [Consequence::NoVisibleChange]);
        assert_eq!(r.steps[0].settled, Some(true));
        assert!(s.quiet());
    }

    /// B-4 #8: `settle` is capped as a hard deadline. The page's DOM settles, but a request
    /// that started meanwhile never finishes: `{settled: false}` at the cap, with the
    /// mutations the page counted, and the in-page call got the cap minus the reply margin.
    #[tokio::test]
    async fn settle_waits_for_a_request_and_never_passes_its_cap() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        let opts = SettleOpts {
            quiet_ms: 150,
            cap_ms: 1000,
        };
        let t0 = Instant::now();
        let (r, ()) = tokio::join!(d.settle(tab, opts), async {
            s.walked().await;
            let call = s.call(SETTLE, 2).await;
            let cap = call["params"]["arguments"][0]["value"]["cap"]
                .as_u64()
                .unwrap_or(0);
            assert!((900..=950).contains(&cap), "cap minus the margin: {cap}");
            s.request("R1").await;
            s.value(&call, json!({"settled": true, "changed": 2})).await;
            // The mutation counter for the network wait, sent and not awaited.
            s.gap_mark().await;
        });
        let took = t0.elapsed();
        let r = r.expect("settle");
        assert_eq!(
            r,
            Settled {
                settled: false,
                changed: 2
            }
        );
        assert!(
            took >= Duration::from_millis(950) && took <= Duration::from_millis(1040),
            "{took:?}"
        );
        assert!(s.quiet(), "no second call: the network never went idle");
        // The request finishes: the next settle is one call, settled.
        s.send(
            json!({"sessionId": "S1", "method": "Network.loadingFinished",
            "params": {"requestId": "R1"}}),
        )
        .await;
        let (r, ()) = tokio::join!(d.settle(tab, opts), async {
            let call = s.call(SETTLE, 2).await;
            s.value(&call, json!({"settled": true, "changed": 0})).await;
        });
        assert_eq!(
            r.expect("settle"),
            Settled {
                settled: true,
                changed: 0
            }
        );
    }

    /// B-4 #8: a request started during the in-page settle sends the loop round again once
    /// it finished, so the page's reaction to it is settled too.
    #[tokio::test]
    async fn settle_goes_again_after_a_request_that_started_meanwhile() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        let (r, ()) = tokio::join!(d.settle(tab, SettleOpts::default()), async {
            s.walked().await;
            let call = s.call(SETTLE, 2).await;
            s.request("R2").await;
            s.value(&call, json!({"settled": true, "changed": 1})).await;
            // The DOM changes of the network wait are counted from here (accept-T-22 note
            // 2): the next call carries the mark's token.
            let tok = s.gap_mark().await;
            s.send(json!({"sessionId": "S1", "method": "Network.loadingFailed",
                "params": {"requestId": "R2", "errorText": "net::ERR_FAILED"}}))
                .await;
            let again = s.call(SETTLE, 2).await;
            assert_eq!(again["params"]["arguments"][0]["value"]["gap"], tok);
            s.value(&again, json!({"settled": true, "changed": 4}))
                .await;
        });
        assert_eq!(
            r.expect("settle"),
            Settled {
                settled: true,
                changed: 5
            }
        );
        assert!(s.quiet());
    }

    /// B-7 `w(d)` is a settle too (B-4 #8): a request that starts during it and never ends
    /// waits the step out to `d`, unsettled, with no second in-page call.
    #[tokio::test]
    async fn a_w_step_waits_for_a_request_that_started_meanwhile() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        let home = tempfile::tempdir().unwrap();
        let ctx = chromium_t20::tool_ctx(home.path());
        let t0 = Instant::now();
        let (r, ()) = tokio::join!(d.act(tab, "w(800ms)", &ctx), async {
            s.walked().await;
            let call = s.call(SETTLE, 2).await;
            s.request("R4").await;
            s.value(&call, json!({"settled": true, "changed": 0})).await;
        });
        let took = t0.elapsed();
        let r = r.expect("act");
        assert_eq!(r.steps[0].outcome, StepOutcome::Ok);
        assert_eq!(r.steps[0].settled, Some(false));
        assert!(
            took >= Duration::from_millis(750) && took < Duration::from_millis(1100),
            "{took:?}"
        );
        assert!(s.quiet(), "no second call while the request is in flight");
    }

    /// I-9 `close`: the tab goes from the driver once the browser reports it detached, with
    /// its sessions; a second close is TabGone.
    #[tokio::test]
    async fn close_waits_for_the_detach_and_cleans_up() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        s.request("R3").await;
        let (r, ()) = tokio::join!(d.close(tab), async {
            let close = s.expect("Target.closeTarget").await;
            assert_eq!(close["params"]["targetId"], "T1");
            s.reply(&close, json!({"success": true})).await;
            s.send(json!({"method": "Target.detachedFromTarget",
                "params": {"sessionId": "S1", "targetId": "T1"}}))
                .await;
        });
        r.expect("closed");
        assert!(d.with_registry(|r| r.tabs().is_empty()));
        assert!(!d.with_registry(|r| r.tracks("S1")));
        assert!(d.with_events(|_, ev| ev.net().inflight().next().is_none()));
        assert!(matches!(d.close(tab).await, Err(DriverError::TabGone(t)) if t == tab));
        assert!(matches!(
            d.settle(tab, SettleOpts::default()).await,
            Err(DriverError::TabGone(_))
        ));
    }
    // ---- review-T-22 round 1 fixes over the mock transport ----

    impl Script {
        /// The settle loop's mutation counter for a network wait ([`settle`]'s GAP_FN), sent
        /// and not awaited: its token.
        async fn gap_mark(&mut self) -> Value {
            let m = self.expect("Runtime.callFunctionOn").await;
            let decl = m["params"]["functionDeclaration"]
                .as_str()
                .unwrap_or_default();
            assert!(decl.contains("function (tok, ttl)"), "a gap mark: {decl}");
            assert_eq!(m["params"]["executionContextId"], 2, "{m}");
            m["params"]["arguments"][0]["value"].clone()
        }

        /// Child frame `id` of the main frame T1 at `url`.
        async fn child_frame(&self, id: &str, url: &str) {
            self.send(json!({"sessionId": "S1", "method": "Page.frameAttached",
                "params": {"frameId": id, "parentFrameId": "T1"}}))
                .await;
            self.send(
                json!({"sessionId": "S1", "method": "Page.frameNavigated", "params": {
                "frame": {"id": id, "parentId": "T1", "url": url, "loaderId": format!("L-{id}")},
                "type": "Navigation"}}),
            )
            .await;
        }

        async fn frame_sb_context(&self, frame: &str, id: i64) {
            self.send(json!({"sessionId": "S1", "method": "Runtime.executionContextCreated", "params": {
                "context": {"id": id, "origin": "https://a.test", "name": SB_WORLD, "uniqueId": format!("u{id}"),
                    "auxData": {"isDefault": false, "type": "isolated", "frameId": frame}}}}))
                .await;
        }
    }

    /// Blocking 1: `w(d)` longer than the 3 s age limit on a request that outlives it. The
    /// request stops holding the wait at 3 s (D-620 (3)), but it is still in flight, so the
    /// step is not settled, though the page's own settle said it was.
    #[tokio::test]
    async fn a_w_step_over_3_s_is_unsettled_while_its_request_is_in_flight() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        let home = tempfile::tempdir().unwrap();
        let ctx = chromium_t20::tool_ctx(home.path());
        let t0 = Instant::now();
        let (r, ()) = tokio::join!(d.act(tab, "w(10s)", &ctx), async {
            s.walked().await;
            let call = s.call(SETTLE, 2).await;
            // A 5 s save starts during the first in-page settle.
            s.request("R5").await;
            s.value(&call, json!({"settled": true, "changed": 0})).await;
            // At 3 s it no longer holds the wait: the page is asked again.
            let again = s.call(SETTLE, 2).await;
            s.value(&again, json!({"settled": true, "changed": 0}))
                .await;
        });
        let took = t0.elapsed();
        let r = r.expect("act");
        assert_eq!(r.steps[0].outcome, StepOutcome::Ok);
        assert_eq!(
            r.steps[0].settled,
            Some(false),
            "the 5 s request is still in flight"
        );
        assert!(
            took >= Duration::from_millis(2900) && took < Duration::from_millis(3500),
            "{took:?}"
        );
        assert!(s.quiet());
        // It ends at 5 s; the next w() is settled again.
        s.send(
            json!({"sessionId": "S1", "method": "Network.loadingFinished",
            "params": {"requestId": "R5"}}),
        )
        .await;
        let (r, ()) = tokio::join!(d.act(tab, "w(1s)", &ctx), async {
            let call = s.call(SETTLE, 2).await;
            s.value(&call, json!({"settled": true, "changed": 0})).await;
        });
        assert_eq!(r.expect("act").steps[0].settled, Some(true));
    }

    /// Blocking 1 through `Driver::settle`: a cap of 6 s on a 4 s request. At 3 s the
    /// request stops holding the wait; at the end it is still in flight: not settled.
    #[tokio::test]
    async fn settle_over_3_s_is_unsettled_while_an_aged_request_is_in_flight() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        let opts = SettleOpts {
            quiet_ms: 150,
            cap_ms: 6000,
        };
        let t0 = Instant::now();
        let (r, ()) = tokio::join!(d.settle(tab, opts), async {
            s.walked().await;
            let call = s.call(SETTLE, 2).await;
            s.request("R4").await;
            s.value(&call, json!({"settled": true, "changed": 1})).await;
            let tok = s.gap_mark().await;
            let again = s.call(SETTLE, 2).await;
            assert_eq!(again["params"]["arguments"][0]["value"]["gap"], tok);
            s.value(&again, json!({"settled": true, "changed": 2}))
                .await;
        });
        let took = t0.elapsed();
        assert_eq!(
            r.expect("settle"),
            Settled {
                settled: false,
                changed: 3
            }
        );
        assert!(
            took >= Duration::from_millis(2900) && took < Duration::from_millis(3500),
            "{took:?}"
        );
        assert!(s.quiet());
    }

    /// D-625: a request the tab started before `settle` began is not waited for, but if it
    /// is still in flight when the settle ends, the answer is not settled (fail-safe).
    #[tokio::test]
    async fn a_request_from_before_the_settle_is_not_waited_for_but_unsettles_it() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        s.request("R0").await;
        d.wait_state(Duration::from_secs(5), |_, ev| {
            ev.net().inflight().next().map(|_| ())
        })
        .await
        .expect("R0 in flight");
        let t0 = Instant::now();
        let (r, ()) = tokio::join!(d.settle(tab, SettleOpts::default()), async {
            s.walked().await;
            let call = s.call(SETTLE, 2).await;
            s.value(&call, json!({"settled": true, "changed": 0})).await;
        });
        let took = t0.elapsed();
        assert_eq!(
            r.expect("settle"),
            Settled {
                settled: false,
                changed: 0
            }
        );
        assert!(
            took < Duration::from_millis(1000),
            "not waited for: {took:?}"
        );
        assert!(s.quiet(), "one call");
        // Once it ends, the same settle is settled.
        s.send(
            json!({"sessionId": "S1", "method": "Network.loadingFinished",
            "params": {"requestId": "R0"}}),
        )
        .await;
        let (r, ()) = tokio::join!(d.settle(tab, SettleOpts::default()), async {
            let call = s.call(SETTLE, 2).await;
            s.value(&call, json!({"settled": true, "changed": 0})).await;
        });
        assert!(r.expect("settle").settled);
    }

    /// Should-fix 2, rule 3, B-5: `act` on `#fN.x` for a frame the registry calls opaque
    /// fails before anything is sent: no index read, no walk, no input inside it.
    #[tokio::test]
    async fn act_in_an_opaque_frame_fails_without_reading_or_clicking() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        s.child_frame("F1", "https://b.test/widget").await;
        s.frame_sb_context("F1", 9).await;
        let f = d
            .wait_registry(Duration::from_secs(5), |r| {
                let f = r.frame_by_cdp_id(tab, "F1")?;
                r.sb_context(tab, f).map(|_| f)
            })
            .await
            .expect("frame F1");
        assert!(d
            .frames(tab)
            .await
            .unwrap()
            .iter()
            .any(|i| i.frame == f && i.opaque));
        let home = tempfile::tempdir().unwrap();
        let ctx = vault_ctx(home.path(), harness_core::Class::Submit, &canary());
        let r = d.act(tab, &format!("c#f{f}.5"), &ctx).await.expect("act");
        assert_eq!(r.steps.len(), 1);
        match &r.steps[0].outcome {
            StepOutcome::Failed { why } => assert!(why.contains("OPAQUE"), "{why}"),
            o => panic!("the step must fail: {o:?}"),
        }
        assert!(r.stopped.is_none(), "{:?}", r.stopped);
        assert!(s.quiet(), "nothing was sent into or about the frame");
        let direct = d
            .call_sb(
                tab,
                f,
                "function () { return 1; }",
                &[],
                Instant::now() + Duration::from_secs(1),
            )
            .await;
        assert!(direct.is_err(), "{direct:?}");
        assert!(s.quiet(), "a direct call is refused too");
    }

    /// Should-fix 2: a frame that becomes cross-origin while a call into it runs is refused
    /// on the next call, judged at that call, not from an earlier snapshot.
    #[tokio::test]
    async fn a_frame_that_turns_cross_origin_mid_call_is_refused_on_the_next_call() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        s.child_frame("F2", "about:blank").await;
        s.frame_sb_context("F2", 7).await;
        let f = d
            .wait_registry(Duration::from_secs(5), |r| {
                let f = r.frame_by_cdp_id(tab, "F2")?;
                r.sb_context(tab, f).map(|_| f)
            })
            .await
            .expect("frame F2");
        let deadline = Instant::now() + Duration::from_secs(5);
        let (first, ()) = tokio::join!(
            d.call_sb(tab, f, "function () { return 1; }", &[], deadline),
            async {
                let walk = s.call(WALK, 7).await;
                s.value(&walk, json!({"count": 1, "settled": null})).await;
                let call = s.call("return 1;", 7).await;
                // The frame navigates cross-origin (same process) before the answer.
                s.send(json!({"sessionId": "S1", "method": "Page.frameNavigated", "params": {
                    "frame": {"id": "F2", "parentId": "T1", "url": "https://evil.test/", "loaderId": "L-x"},
                    "type": "Navigation"}}))
                    .await;
                d.wait_registry(Duration::from_secs(5), |r| {
                    r.closed_frame(tab, f).map(|_| ())
                })
                .await
                .expect("the registry sees it");
                s.value(&call, json!(1)).await;
            }
        );
        assert_eq!(first.expect("the first call ran"), json!(1));
        let next = d
            .call_sb(tab, f, "function () { return 2; }", &[], deadline)
            .await;
        match next {
            Err(runtime::CallError::Driver(DriverError::Cdp(why))) => {
                assert!(why.contains("OPAQUE"), "{why}")
            }
            other => panic!("refused: {other:?}"),
        }
        assert!(s.quiet(), "nothing sent to the frame");
    }

    /// Should-fix 6: when `Target.getTargets` does not answer within 1 s, `tabs()` gives the
    /// registry's last titles, escaped and capped like the fresh ones.
    #[tokio::test]
    async fn tabs_falls_back_to_the_registry_titles_after_1_s() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        s.send(
            json!({"method": "Target.targetInfoChanged", "params": {"targetInfo": {
            "targetId": "T1", "type": "page", "title": "Old \"title\" #12 [x](#3)",
            "url": "https://a.test/"}}}),
        )
        .await;
        d.wait_registry(Duration::from_secs(5), |r| {
            r.tabs()
                .first()
                .is_some_and(|t| t.2.starts_with("Old"))
                .then_some(())
        })
        .await
        .expect("title");
        let t0 = Instant::now();
        let (r, ()) = tokio::join!(d.tabs(), async {
            // Never answered.
            s.expect("Target.getTargets").await;
        });
        let took = t0.elapsed();
        let tabs = r.expect("tabs");
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].0, tab);
        assert_eq!(tabs[0].2, "Old 'title' ＃12 (x)(＃3)");
        assert!(
            took >= Duration::from_millis(950) && took < Duration::from_millis(1300),
            "{took:?}"
        );
    }

    /// Row 6c: fresh titles and URLs from `Target.getTargets` are escaped and capped as
    /// `OpenedTab` titles are.
    #[tokio::test]
    async fn tabs_escapes_and_caps_titles_and_urls() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let _tab = mock_tab(&d, &mut s).await;
        let long = "y".repeat(500);
        let (r, ()) = tokio::join!(d.tabs(), async {
            let m = s.expect("Target.getTargets").await;
            s.reply(
                &m,
                json!({"targetInfos": [{"targetId": "T1", "type": "page",
                "title": format!("A\nB `c` #f2.3 [x](#3) {long}"), "url": "https://a.test/"}]}),
            )
            .await;
        });
        let tabs = r.expect("tabs");
        let title = &tabs[0].2;
        assert!(title.starts_with("A B 'c' ＃f2.3 (x)(＃3) yyy"), "{title}");
        assert!(title.ends_with("..."), "capped: {title}");
        assert!(title.chars().count() <= diff::TEXT_CAP + 3);
        assert!(!tabs[0].1.contains(' '));
    }

    /// Should-fix 4: a failed owner lookup is not cached. The first `index()` leaves the
    /// frame out (its owner is unknown), the next asks again and reads it, and the one after
    /// that uses the cached, definite answer.
    #[tokio::test]
    async fn a_failed_owner_lookup_is_asked_again_and_a_definite_one_is_cached() {
        let (d, mut s) = mock_driver(Profile::Bench);
        let tab = mock_tab(&d, &mut s).await;
        s.child_frame("F3", "about:blank").await;
        s.frame_sb_context("F3", 7).await;
        let f = d
            .wait_registry(Duration::from_secs(5), |r| {
                let f = r.frame_by_cdp_id(tab, "F3")?;
                r.sb_context(tab, f).map(|_| f)
            })
            .await
            .expect("frame F3");
        let mut owner_asks = 0usize;
        let mut runs = Vec::new();
        for run in 0..3 {
            let ix = d.index(tab);
            tokio::pin!(ix);
            let rows = loop {
                tokio::select! {
                    r = &mut ix => break r.expect("index"),
                    raw = s.sent.recv() => {
                        let m: Value = serde_json::from_slice(&raw.expect("open")).unwrap();
                        let decl = m["params"]["functionDeclaration"].as_str().unwrap_or_default();
                        let ctx = m["params"]["executionContextId"].as_i64();
                        match m["method"].as_str().unwrap_or_default() {
                            "Runtime.callFunctionOn" if decl.contains(WALK) => {
                                s.value(&m, json!({"count": 1, "settled": null})).await
                            }
                            "Runtime.callFunctionOn" if decl.contains(INDEX) && ctx == Some(2) => {
                                s.value(&m, json!([{"id": 4, "frame": 0, "role": "iframe", "name": "inner"}])).await
                            }
                            "Runtime.callFunctionOn" if decl.contains(INDEX) && ctx == Some(7) => {
                                s.value(&m, json!([{"id": 1, "frame": 0, "role": "button", "name": "Inner"}])).await
                            }
                            "Runtime.callFunctionOn" if decl.contains("sb.id(this)") => {
                                s.reply(&m, json!({"result": {"type": "number", "value": 4}})).await
                            }
                            "DOM.getFrameOwner" => {
                                owner_asks += 1;
                                if run == 0 {
                                    s.fail(&m, "Frame with the given id was not found.").await
                                } else {
                                    s.reply(&m, json!({"backendNodeId": 11})).await
                                }
                            }
                            "DOM.resolveNode" => {
                                s.reply(&m, json!({"object": {"type": "object", "objectId": "o-1"}})).await
                            }
                            "Runtime.releaseObjectGroup" => s.reply(&m, json!({})).await,
                            other => panic!("unexpected {other}: {m}"),
                        }
                    }
                }
            };
            runs.push((owner_asks, rows.iter().any(|r| r.frame == f)));
        }
        assert_eq!(
            runs,
            [(1, false), (2, true), (2, true)],
            "(owner lookups so far, frame read)"
        );
    }
}
