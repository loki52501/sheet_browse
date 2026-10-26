//! B-4 #2: the target and frame registry. A pure state machine fed by CDP events in wire
//! order (the client's synchronous observer calls [`Registry::apply`] before any subscriber
//! sees the event), so every lookup is answered locally with zero round trips.
//!
//! Tabs are page targets, numbered from 1 in attach order. Frames are numbered per tab;
//! frame 0 is the main frame, matching `sb_sheet::FrameId` and element refs `#f2.15`.
//! Each frame carries a generation, bumped on every cross-document navigation, so an id
//! taken from an older document can be rejected as stale.
//!
//! Only the session that created a tab owns it: a second session attached to the same page
//! (an explicit `Target.attachToTarget`) is not tracked, and its detach changes nothing.
//! Page targets with a subtype (`prerender`) are not tabs; a prerender that activates takes
//! over the tab whose session it was attached under.
//!
//! `Page.enable` does not replay frames that already exist, so a target attached running
//! (the initial tab, tabs a `me` profile restores, iframes already out of process, an
//! activated prerender) is queued as a [`Seed`]: the driver sends one `Page.getFrameTree`
//! for it, pipelined with its setup commands, and feeds the reply to
//! [`Registry::seed_frame_tree`] in wire order.

use crate::{FrameInfo, TabId};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// An execution context the driver can call into: `Runtime.callFunctionOn` with
/// `executionContextId: id` on `session`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecContext {
    pub session: String,
    pub id: i64,
    pub unique_id: Option<String>,
}

/// The isolated world `injected.js` runs in (B-4 #3, B-5).
pub const SB_WORLD: &str = "sb";

/// Frames read from one `Page.getFrameTree` at most (a page with more is not real).
const MAX_SEED_FRAMES: usize = 4096;

#[derive(Debug, Clone)]
struct Frame {
    cdp_id: String,
    parent: Option<u32>,
    url: String,
    generation: u32,
    default_ctx: Option<ExecContext>,
    sb_ctx: Option<ExecContext>,
}

impl Frame {
    fn new(cdp_id: &str, parent: Option<u32>, url: &str) -> Frame {
        Frame {
            cdp_id: cdp_id.to_string(),
            parent,
            url: url.to_string(),
            generation: 0,
            default_ctx: None,
            sb_ctx: None,
        }
    }
}

#[derive(Debug, Clone)]
struct Tab {
    target_id: String,
    session: String,
    title: String,
    opener: Option<TabId>,
    frames: BTreeMap<u32, Frame>,
    by_cdp: HashMap<String, u32>,
    /// Out-of-process iframes: CDP frame id -> their own session.
    oopif: HashMap<String, String>,
    next_frame: u32,
}

impl Tab {
    fn frame_mut(&mut self, cdp_id: &str) -> Option<&mut Frame> {
        let n = *self.by_cdp.get(cdp_id)?;
        self.frames.get_mut(&n)
    }

    fn attach_frame(&mut self, cdp_id: &str, parent_cdp: &str) -> Option<u32> {
        if let Some(&n) = self.by_cdp.get(cdp_id) {
            return Some(n);
        }
        let parent = *self.by_cdp.get(parent_cdp)?;
        let n = self.next_frame;
        self.next_frame += 1;
        self.frames.insert(n, Frame::new(cdp_id, Some(parent), ""));
        self.by_cdp.insert(cdp_id.to_string(), n);
        Some(n)
    }

    /// Remove `n` and every frame below it; returns their CDP ids.
    fn remove_subtree(&mut self, n: u32) -> Vec<String> {
        let mut doomed = vec![n];
        let mut i = 0;
        while i < doomed.len() {
            let p = doomed[i];
            doomed.extend(
                self.frames
                    .iter()
                    .filter(|(_, f)| f.parent == Some(p))
                    .map(|(&c, _)| c),
            );
            i += 1;
        }
        let mut gone = Vec::new();
        for d in doomed {
            if let Some(f) = self.frames.remove(&d) {
                self.by_cdp.remove(&f.cdp_id);
                self.oopif.remove(&f.cdp_id);
                gone.push(f.cdp_id);
            }
        }
        gone
    }

    fn clear_contexts(&mut self, session: &str, id: Option<i64>) {
        for f in self.frames.values_mut() {
            for slot in [&mut f.default_ctx, &mut f.sb_ctx] {
                if slot
                    .as_ref()
                    .is_some_and(|c| c.session == session && id.is_none_or(|i| c.id == i))
                {
                    *slot = None;
                }
            }
        }
    }
}

/// A session whose existing frames must be read once with `Page.getFrameTree` (see the
/// module docs). `replay_contexts`: its `Runtime.enable` already ran while it was untracked
/// (an activated prerender), so contexts must be replayed after the seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seed {
    pub session: String,
    pub replay_contexts: bool,
}

/// A page target with a subtype, by its session: not a tab until it activates.
#[derive(Debug, Clone)]
struct Pending {
    target: String,
    /// The tab whose page session it was attached under.
    parent: Option<TabId>,
}

/// Who a flattened session belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Owner {
    Tab(TabId),
    /// An out-of-process iframe of the tab, by its CDP frame id (= its target id).
    Oopif(TabId, String),
}

#[derive(Debug, Clone, Default)]
pub struct Registry {
    tabs: BTreeMap<TabId, Tab>,
    by_target: HashMap<String, TabId>,
    sessions: HashMap<String, Owner>,
    /// Prerendered (subtyped) page targets, by session.
    pending: HashMap<String, Pending>,
    seeds: Vec<Seed>,
    next_tab: u32,
    /// Sessions whose runtime install (B-4 #3) the renderer has acknowledged.
    installed: std::collections::HashSet<String>,
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// Where a frame's document comes from, for the cross-origin (opaque) test.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    /// about:blank, about:srcdoc, empty: the parent's origin.
    Inherit,
    /// data:, unparsable: never same-origin with anything.
    Opaque,
    Tuple(String),
}

fn origin(url: &str) -> Origin {
    let lower = url.trim().to_ascii_lowercase();
    if lower.is_empty() || lower.starts_with("about:") {
        return Origin::Inherit;
    }
    if let Some(inner) = lower.strip_prefix("blob:") {
        return origin(inner);
    }
    let Some((scheme, rest)) = lower.split_once("://") else {
        return Origin::Opaque;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    if host.is_empty() || scheme == "file" {
        return Origin::Opaque;
    }
    let default_port = match scheme {
        "http" | "ws" => Some(":80"),
        "https" | "wss" => Some(":443"),
        _ => None,
    };
    let host = match default_port {
        Some(p) => host.strip_suffix(p).unwrap_or(host),
        None => host,
    };
    Origin::Tuple(format!("{scheme}://{host}"))
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    /// Feed one CDP event. `session` is the flattened session it arrived on (`None` for the
    /// browser). Events the registry does not track are ignored.
    pub fn apply(&mut self, session: Option<&str>, method: &str, params: &Value) {
        match method {
            "Target.attachedToTarget" => self.attached(session, params),
            "Target.detachedFromTarget" => {
                if let Some(sid) = s(params, "sessionId") {
                    self.detached(sid);
                }
            }
            "Target.targetInfoChanged" => self.info_changed(params),
            "Target.targetDestroyed" => {
                if let Some(target) = s(params, "targetId") {
                    self.target_gone(target);
                }
            }
            "Page.frameAttached" => {
                if let (Some(tab), Some(id), Some(parent)) = (
                    self.page_session(session),
                    s(params, "frameId"),
                    s(params, "parentFrameId"),
                ) {
                    if let Some(t) = self.tabs.get_mut(&tab) {
                        t.attach_frame(id, parent);
                    }
                }
            }
            "Page.frameNavigated" => self.navigated(session, params),
            "Page.navigatedWithinDocument" => {
                if let (Some((tab, scope)), Some(id), Some(url)) =
                    (self.owner(session), s(params, "frameId"), s(params, "url"))
                {
                    if scope.as_deref().is_none_or(|root| root == id) {
                        if let Some(f) = self.tabs.get_mut(&tab).and_then(|t| t.frame_mut(id)) {
                            f.url = url.to_string();
                        }
                    }
                }
            }
            "Page.frameDetached" => {
                if let (Some(tab), Some(id)) = (self.page_session(session), s(params, "frameId")) {
                    // "swap": the frame moves to another process (an OOPIF target attaches
                    // for it next); it stays in the tree.
                    if s(params, "reason") != Some("swap") {
                        self.remove_frame(tab, id);
                    }
                }
            }
            "Runtime.executionContextCreated" => self.context_created(session, params),
            "Runtime.executionContextDestroyed" => {
                if let (Some(sid), Some((tab, _)), Some(id)) = (
                    session,
                    self.owner(session),
                    params.get("executionContextId").and_then(Value::as_i64),
                ) {
                    if let Some(t) = self.tabs.get_mut(&tab) {
                        t.clear_contexts(sid, Some(id));
                    }
                }
            }
            "Runtime.executionContextsCleared" => {
                if let (Some(sid), Some((tab, _))) = (session, self.owner(session)) {
                    if let Some(t) = self.tabs.get_mut(&tab) {
                        t.clear_contexts(sid, None);
                    }
                }
            }
            _ => {}
        }
    }

    /// The tab and, for an OOPIF session, the CDP id of the frame it is rooted at.
    fn owner(&self, session: Option<&str>) -> Option<(TabId, Option<String>)> {
        match self.sessions.get(session?)? {
            Owner::Tab(t) => Some((*t, None)),
            Owner::Oopif(t, f) => Some((*t, Some(f.clone()))),
        }
    }

    /// The tab whose page session this is. Frame-tree events from an OOPIF session describe
    /// frames inside an opaque frame and are not tracked (never read inside, B-4 #2).
    fn page_session(&self, session: Option<&str>) -> Option<TabId> {
        match self.sessions.get(session?)? {
            Owner::Tab(t) => Some(*t),
            Owner::Oopif(..) => None,
        }
    }

    fn attached(&mut self, session: Option<&str>, params: &Value) {
        let (Some(sid), Some(info)) = (s(params, "sessionId"), params.get("targetInfo")) else {
            return;
        };
        let Some(target) = s(info, "targetId") else {
            return;
        };
        // A target that was already running when we attached: its frames are not replayed.
        let running = params.get("waitingForDebugger").and_then(Value::as_bool) != Some(true);
        match s(info, "type") {
            Some("page") => {
                if s(info, "subtype").is_some_and(|st| !st.is_empty()) {
                    // A prerender (or any other non-tab page): kept aside until it activates.
                    let parent = self.page_session(session);
                    self.pending.insert(
                        sid.to_string(),
                        Pending {
                            target: target.to_string(),
                            parent,
                        },
                    );
                    return;
                }
                if self.by_target.contains_key(target) {
                    // A second session on a known tab: the tab keeps its own session, and
                    // this one is not tracked (its events would duplicate the owner's).
                    return;
                }
                let opener = s(info, "openerId").and_then(|o| self.by_target.get(o).copied());
                self.new_tab(sid, target, info, opener);
                if running {
                    self.seeds.push(Seed {
                        session: sid.to_string(),
                        replay_contexts: false,
                    });
                }
            }
            Some("iframe") => {
                // Auto-attached under the session of the page (or OOPIF) that embeds it.
                let Some((tab, _)) = self.owner(session) else {
                    return;
                };
                let Some(t) = self.tabs.get_mut(&tab) else {
                    return;
                };
                if t.oopif
                    .get(target)
                    .is_some_and(|o| self.sessions.contains_key(o))
                {
                    // A second session on a tracked OOPIF: the first one keeps it.
                    return;
                }
                t.oopif.insert(target.to_string(), sid.to_string());
                self.sessions
                    .insert(sid.to_string(), Owner::Oopif(tab, target.to_string()));
                if running {
                    self.seeds.push(Seed {
                        session: sid.to_string(),
                        replay_contexts: false,
                    });
                }
            }
            _ => {}
        }
    }

    fn new_tab(&mut self, sid: &str, target: &str, info: &Value, opener: Option<TabId>) -> TabId {
        self.next_tab += 1;
        let tab = TabId(self.next_tab);
        let mut frames = BTreeMap::new();
        frames.insert(0, Frame::new(target, None, s(info, "url").unwrap_or("")));
        self.tabs.insert(
            tab,
            Tab {
                target_id: target.to_string(),
                session: sid.to_string(),
                title: s(info, "title").unwrap_or("").to_string(),
                opener,
                frames,
                by_cdp: HashMap::from([(target.to_string(), 0)]),
                oopif: HashMap::new(),
                next_frame: 1,
            },
        );
        self.by_target.insert(target.to_string(), tab);
        self.sessions.insert(sid.to_string(), Owner::Tab(tab));
        tab
    }

    /// Remove a tab with every session that belongs to it.
    fn remove_tab(&mut self, tab: TabId) {
        if let Some(t) = self.tabs.remove(&tab) {
            if self.by_target.get(&t.target_id) == Some(&tab) {
                self.by_target.remove(&t.target_id);
            }
        }
        self.sessions.retain(|_, o| match o {
            Owner::Tab(t) | Owner::Oopif(t, _) => *t != tab,
        });
    }

    fn detached(&mut self, sid: &str) {
        self.pending.remove(sid);
        self.installed.remove(sid);
        match self.sessions.get(sid).cloned() {
            Some(Owner::Tab(tab)) => {
                if self.tabs.get(&tab).is_some_and(|t| t.session == sid) {
                    self.remove_tab(tab);
                } else {
                    self.sessions.remove(sid);
                }
            }
            Some(Owner::Oopif(tab, frame)) => {
                self.sessions.remove(sid);
                if let Some(t) = self.tabs.get_mut(&tab) {
                    if t.oopif.get(&frame).is_some_and(|s| s == sid) {
                        t.oopif.remove(&frame);
                    }
                    t.clear_contexts(sid, None);
                }
            }
            None => {}
        }
    }

    /// `Target.targetDestroyed`: the tab goes, whatever session it had.
    fn target_gone(&mut self, target: &str) {
        self.pending.retain(|_, p| p.target != target);
        if let Some(&tab) = self.by_target.get(target) {
            self.remove_tab(tab);
        }
    }

    /// A pending (prerender) page lost its subtype: it is the page now. It takes over the tab
    /// it was attached under, or becomes a new tab when that one is gone.
    fn activate(&mut self, sid: String, p: Pending, info: &Value) {
        let url = s(info, "url").unwrap_or("");
        match p.parent.filter(|t| self.tabs.contains_key(t)) {
            Some(tab) => {
                // The old page's sessions (and its OOPIFs) no longer speak for this tab.
                self.sessions.retain(|_, o| match o {
                    Owner::Tab(t) | Owner::Oopif(t, _) => *t != tab,
                });
                let Some(t) = self.tabs.get_mut(&tab) else {
                    return;
                };
                if self.by_target.get(&t.target_id) == Some(&tab) {
                    self.by_target.remove(&t.target_id);
                }
                let generation = t.frames.get(&0).map_or(0, |f| f.generation.wrapping_add(1));
                let mut main = Frame::new(&p.target, None, url);
                main.generation = generation;
                t.target_id = p.target.clone();
                t.session = sid.clone();
                if let Some(title) = s(info, "title") {
                    t.title = title.to_string();
                }
                // Every old frame went with the old document; numbers are not reused.
                t.frames = BTreeMap::from([(0, main)]);
                t.by_cdp = HashMap::from([(p.target.clone(), 0)]);
                t.oopif.clear();
                self.by_target.insert(p.target, tab);
                self.sessions.insert(sid.clone(), Owner::Tab(tab));
            }
            None => {
                if self.by_target.contains_key(&p.target) {
                    return;
                }
                self.new_tab(&sid, &p.target, info, None);
            }
        }
        self.seeds.push(Seed {
            session: sid,
            replay_contexts: true,
        });
    }

    /// Sessions whose existing frames must be read once (see [`Seed`]); drained.
    pub fn take_seeds(&mut self) -> Vec<Seed> {
        std::mem::take(&mut self.seeds)
    }

    /// True for a session the registry follows: a tab's, an OOPIF's or a pending
    /// prerender's. Only those need Page and Runtime events.
    pub fn tracks(&self, session: &str) -> bool {
        self.sessions.contains_key(session) || self.pending.contains_key(session)
    }

    /// Apply a `Page.getFrameTree` reply for `session` (a [`Seed`]). Run it in wire order (the
    /// driver uses `CdpSender::start_then`): the tree is then the truth at that point, so
    /// unknown frames are added under their parents and every listed frame's URL is set.
    /// Generations are not bumped (no navigation happened). For an OOPIF session only its
    /// root frame is used, attached under its parent in the embedding page.
    pub fn seed_frame_tree(&mut self, session: &str, reply: &Value) {
        let Some((tab, scope)) = self.owner(Some(session)) else {
            return;
        };
        let Some(tree) = reply.get("frameTree") else {
            return;
        };
        let Some(t) = self.tabs.get_mut(&tab) else {
            return;
        };
        let url_of = |f: &Value| {
            format!(
                "{}{}",
                s(f, "url").unwrap_or(""),
                s(f, "urlFragment").unwrap_or("")
            )
        };
        if let Some(root) = scope {
            let Some(f) = tree.get("frame") else {
                return;
            };
            if s(f, "id") != Some(root.as_str()) {
                return;
            }
            if !t.by_cdp.contains_key(&root) {
                let Some(parent) = s(f, "parentId") else {
                    return;
                };
                if t.attach_frame(&root, parent).is_none() {
                    return;
                }
            }
            if let Some(fr) = t.frame_mut(&root) {
                fr.url = url_of(f);
            }
            return;
        }
        // Depth-first with an explicit stack; a parent is always handled before its children.
        let mut stack: Vec<(&Value, Option<&str>)> = vec![(tree, None)];
        let mut budget = MAX_SEED_FRAMES;
        while let Some((node, parent)) = stack.pop() {
            if budget == 0 {
                return;
            }
            budget -= 1;
            let Some(f) = node.get("frame") else {
                continue;
            };
            let Some(id) = s(f, "id") else {
                continue;
            };
            let known = match parent {
                None => {
                    if !t.by_cdp.contains_key(id) {
                        // The main frame under a new CDP id: frame 0 follows it.
                        if let Some(main) = t.frames.get_mut(&0) {
                            t.by_cdp.remove(&main.cdp_id);
                            main.cdp_id = id.to_string();
                            t.by_cdp.insert(id.to_string(), 0);
                        }
                    }
                    true
                }
                Some(p) => t.attach_frame(id, p).is_some(),
            };
            if !known {
                continue;
            }
            if let Some(fr) = t.frame_mut(id) {
                fr.url = url_of(f);
            }
            if let Some(children) = node.get("childFrames").and_then(Value::as_array) {
                stack.extend(children.iter().rev().map(|c| (c, Some(id))));
            }
        }
    }
    fn info_changed(&mut self, params: &Value) {
        let Some(info) = params.get("targetInfo") else {
            return;
        };
        let activated =
            s(info, "type") == Some("page") && s(info, "subtype").is_none_or(str::is_empty);
        if activated {
            if let Some(target) = s(info, "targetId") {
                let sid = self
                    .pending
                    .iter()
                    .find(|(_, p)| p.target == target)
                    .map(|(sid, _)| sid.clone());
                if let Some(sid) = sid {
                    if let Some(p) = self.pending.remove(&sid) {
                        self.activate(sid, p, info);
                    }
                    return;
                }
            }
        }
        let Some(tab) = s(info, "targetId")
            .and_then(|t| self.by_target.get(t))
            .copied()
        else {
            return;
        };
        if let Some(t) = self.tabs.get_mut(&tab) {
            if let Some(title) = s(info, "title") {
                t.title = title.to_string();
            }
        }
    }

    fn navigated(&mut self, session: Option<&str>, params: &Value) {
        let Some((tab, scope)) = self.owner(session) else {
            return;
        };
        let Some(frame) = params.get("frame") else {
            return;
        };
        let (Some(id), Some(url)) = (s(frame, "id"), s(frame, "url")) else {
            return;
        };
        let url = format!("{url}{}", s(frame, "urlFragment").unwrap_or(""));
        let Some(t) = self.tabs.get_mut(&tab) else {
            return;
        };
        match scope {
            // Inside an OOPIF only its own root frame is tracked.
            Some(root) if root != id => return,
            Some(_) => {}
            None => {
                if !t.by_cdp.contains_key(id) {
                    match s(frame, "parentId") {
                        Some(parent) => {
                            if t.attach_frame(id, parent).is_none() {
                                return;
                            }
                        }
                        None => {
                            // The main frame under a new CDP id: frame 0 follows it.
                            if let Some(main) = t.frames.get_mut(&0) {
                                t.by_cdp.remove(&main.cdp_id);
                                main.cdp_id = id.to_string();
                                t.by_cdp.insert(id.to_string(), 0);
                            }
                        }
                    }
                }
            }
        }
        if let Some(f) = t.frame_mut(id) {
            f.url = url;
            f.generation = f.generation.wrapping_add(1);
        }
    }

    fn remove_frame(&mut self, tab: TabId, cdp_id: &str) {
        let Some(t) = self.tabs.get_mut(&tab) else {
            return;
        };
        let Some(&n) = t.by_cdp.get(cdp_id) else {
            return;
        };
        if n == 0 {
            // The main frame leaves only with its target (Target.detachedFromTarget).
            return;
        }
        let gone = t.remove_subtree(n);
        self.sessions
            .retain(|_, o| !matches!(o, Owner::Oopif(t, f) if *t == tab && gone.contains(f)));
    }

    fn context_created(&mut self, session: Option<&str>, params: &Value) {
        let (Some(sid), Some((tab, scope))) = (session, self.owner(session)) else {
            return;
        };
        let Some(ctx) = params.get("context") else {
            return;
        };
        let Some(id) = ctx.get("id").and_then(Value::as_i64) else {
            return;
        };
        let aux = ctx.get("auxData");
        let Some(frame_id) = aux.and_then(|a| s(a, "frameId")) else {
            return;
        };
        if scope.as_deref().is_some_and(|root| root != frame_id) {
            return;
        }
        let is_default = aux
            .and_then(|a| a.get("isDefault"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let name = s(ctx, "name").unwrap_or("");
        let Some(f) = self.tabs.get_mut(&tab).and_then(|t| t.frame_mut(frame_id)) else {
            return;
        };
        let ec = ExecContext {
            session: sid.to_string(),
            id,
            unique_id: s(ctx, "uniqueId").map(str::to_string),
        };
        if is_default {
            f.default_ctx = Some(ec);
        } else if name == SB_WORLD {
            f.sb_ctx = Some(ec);
        }
    }

    // ---- lookups: all local, zero round trips ----

    /// (tab, url, title) for every live tab, in tab order.
    pub fn tabs(&self) -> Vec<(TabId, String, String)> {
        self.tabs
            .iter()
            .map(|(id, t)| {
                let url = t.frames.get(&0).map(|f| f.url.clone()).unwrap_or_default();
                (*id, url, t.title.clone())
            })
            .collect()
    }

    pub fn has_tab(&self, tab: TabId) -> bool {
        self.tabs.contains_key(&tab)
    }

    /// The tab's flattened CDP session.
    pub fn session(&self, tab: TabId) -> Option<&str> {
        self.tabs.get(&tab).map(|t| t.session.as_str())
    }

    pub fn target_id(&self, tab: TabId) -> Option<&str> {
        self.tabs.get(&tab).map(|t| t.target_id.as_str())
    }

    pub fn tab_by_target(&self, target_id: &str) -> Option<TabId> {
        self.by_target.get(target_id).copied()
    }

    pub fn tab_by_session(&self, session: &str) -> Option<TabId> {
        match self.sessions.get(session)? {
            Owner::Tab(t) => Some(*t),
            Owner::Oopif(..) => None,
        }
    }

    /// "opened by tab N": the tab whose page opened this one (`openerId`).
    pub fn opener(&self, tab: TabId) -> Option<TabId> {
        self.tabs.get(&tab)?.opener
    }

    /// Record the reply to the session's `Page.addScriptToEvaluateOnNewDocument` (applied in
    /// wire order by the driver). Only tracked sessions are kept.
    pub(crate) fn set_installed(&mut self, session: &str) {
        if self.tracks(session) {
            self.installed.insert(session.to_string());
        }
    }

    /// The tab a session belongs to: its own page session or one of its out-of-process
    /// iframes' (a dialog or download reported there is the tab's).
    pub(crate) fn tab_of_session(&self, session: &str) -> Option<TabId> {
        self.owner(Some(session)).map(|(t, _)| t)
    }

    /// True once the renderer has acknowledged the session's runtime install: a navigation
    /// started after this gets `__sb` in its document (B-4 #3).
    pub(crate) fn installed(&self, session: &str) -> bool {
        self.installed.contains(session)
    }

    /// The registry's frame number for a CDP frame id.
    pub fn frame_by_cdp_id(&self, tab: TabId, cdp_id: &str) -> Option<u32> {
        self.tabs.get(&tab)?.by_cdp.get(cdp_id).copied()
    }

    pub fn cdp_frame_id(&self, tab: TabId, frame: u32) -> Option<&str> {
        Some(self.tabs.get(&tab)?.frames.get(&frame)?.cdp_id.as_str())
    }

    /// The frame's current document generation.
    pub fn generation(&self, tab: TabId, frame: u32) -> Option<u32> {
        Some(self.tabs.get(&tab)?.frames.get(&frame)?.generation)
    }

    /// False for a frame that is gone or has navigated since `generation` was read: ids taken
    /// from that document are stale.
    pub fn is_current(&self, tab: TabId, frame: u32, generation: u32) -> bool {
        self.generation(tab, frame) == Some(generation)
    }

    /// The frame's `sb` world context, where `globalThis.__sb` lives.
    pub fn sb_context(&self, tab: TabId, frame: u32) -> Option<&ExecContext> {
        self.tabs.get(&tab)?.frames.get(&frame)?.sb_ctx.as_ref()
    }

    /// The `sb` or default context with this id on `session`, wherever it is registered.
    pub(crate) fn context_by_id(&self, session: &str, id: i64) -> Option<&ExecContext> {
        self.tabs
            .values()
            .flat_map(|t| t.frames.values())
            .flat_map(|f| [f.sb_ctx.as_ref(), f.default_ctx.as_ref()])
            .flatten()
            .find(|c| c.session == session && c.id == id)
    }

    /// The frame's current URL.
    pub(crate) fn frame_url(&self, tab: TabId, frame: u32) -> Option<&str> {
        Some(self.tabs.get(&tab)?.frames.get(&frame)?.url.as_str())
    }

    /// The frame's main-world context.
    pub fn default_context(&self, tab: TabId, frame: u32) -> Option<&ExecContext> {
        self.tabs
            .get(&tab)?
            .frames
            .get(&frame)?
            .default_ctx
            .as_ref()
    }

    /// The session that owns the frame's document: the OOPIF's own, else the tab's.
    pub fn frame_session(&self, tab: TabId, frame: u32) -> Option<&str> {
        let t = self.tabs.get(&tab)?;
        let f = t.frames.get(&frame)?;
        Some(
            t.oopif
                .get(&f.cdp_id)
                .map(String::as_str)
                .unwrap_or(t.session.as_str()),
        )
    }

    /// B-4 #2 view: every frame of the tab, frame 0 first, with parent and opacity. A frame
    /// is opaque when it is out of process, cross-origin to its parent, or inside an opaque
    /// frame.
    pub fn frames(&self, tab: TabId) -> Option<Vec<FrameInfo>> {
        let t = self.tabs.get(&tab)?;
        let mut memo: HashMap<u32, (Origin, bool)> = HashMap::new();
        Some(
            t.frames
                .keys()
                .map(|&n| {
                    let f = &t.frames[&n];
                    let (_, opaque) = Self::origin_of(t, n, &mut memo, 0);
                    FrameInfo {
                        frame: n,
                        parent: f.parent,
                        url: f.url.clone(),
                        opaque,
                    }
                })
                .collect(),
        )
    }

    /// (effective origin, opaque) of frame `n`, memoized; depth-capped against a cycle.
    fn origin_of(
        t: &Tab,
        n: u32,
        memo: &mut HashMap<u32, (Origin, bool)>,
        depth: usize,
    ) -> (Origin, bool) {
        if let Some(v) = memo.get(&n) {
            return v.clone();
        }
        let Some(f) = t.frames.get(&n) else {
            return (Origin::Opaque, true);
        };
        let own = origin(&f.url);
        let v = match f.parent {
            None => (own, false),
            Some(_) if depth > 64 => (Origin::Opaque, true),
            Some(p) => {
                let (parent_origin, parent_opaque) = Self::origin_of(t, p, memo, depth + 1);
                let (eff, cross) = if own == Origin::Inherit {
                    (parent_origin, false)
                } else {
                    let same = matches!((&own, &parent_origin),
                        (Origin::Tuple(a), Origin::Tuple(b)) if a == b);
                    (own, !same)
                };
                let opaque = parent_opaque || cross || t.oopif.contains_key(&f.cdp_id);
                (eff, opaque)
            }
        };
        memo.insert(n, v.clone());
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn attach_page(r: &mut Registry, sid: &str, target: &str, opener: Option<&str>) -> TabId {
        let mut info =
            json!({"targetId": target, "type": "page", "url": "about:blank", "title": ""});
        if let Some(o) = opener {
            info["openerId"] = json!(o);
        }
        r.apply(
            None,
            "Target.attachedToTarget",
            &json!({"sessionId": sid, "targetInfo": info, "waitingForDebugger": true}),
        );
        r.tab_by_target(target).expect("tab registered")
    }

    fn navigate(r: &mut Registry, sid: &str, id: &str, parent: Option<&str>, url: &str) {
        let mut frame = json!({"id": id, "url": url, "loaderId": "L"});
        if let Some(p) = parent {
            frame["parentId"] = json!(p);
        }
        r.apply(
            Some(sid),
            "Page.frameNavigated",
            &json!({"frame": frame, "type": "Navigation"}),
        );
    }

    fn attach_frame(r: &mut Registry, sid: &str, id: &str, parent: &str) {
        r.apply(
            Some(sid),
            "Page.frameAttached",
            &json!({"frameId": id, "parentFrameId": parent}),
        );
    }

    fn context(r: &mut Registry, sid: &str, id: i64, frame: &str, name: &str, default: bool) {
        r.apply(
            Some(sid),
            "Runtime.executionContextCreated",
            &json!({"context": {
                "id": id, "origin": "https://a.test", "name": name, "uniqueId": format!("u{id}"),
                "auxData": {"isDefault": default, "type": if default {"default"} else {"isolated"}, "frameId": frame}
            }}),
        );
    }

    #[test]
    fn page_attach_makes_a_tab_with_main_frame_zero() {
        let mut r = Registry::new();
        let t = attach_page(&mut r, "S1", "T1", None);
        assert_eq!(t, TabId(1));
        assert_eq!(r.session(t), Some("S1"));
        assert_eq!(r.tab_by_session("S1"), Some(t));
        assert_eq!(r.frame_by_cdp_id(t, "T1"), Some(0));
        let frames = r.frames(t).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].frame, 0);
        assert_eq!(frames[0].parent, None);
        assert!(!frames[0].opaque);
        assert_eq!(r.tabs(), vec![(t, "about:blank".into(), "".into())]);
    }

    #[test]
    fn popup_records_its_opener_tab() {
        let mut r = Registry::new();
        let a = attach_page(&mut r, "S1", "T1", None);
        let b = attach_page(&mut r, "S2", "T2", Some("T1"));
        assert_eq!(b, TabId(2));
        assert_eq!(r.opener(b), Some(a));
        assert_eq!(r.opener(a), None);
        // An opener the registry never saw is not invented.
        let c = attach_page(&mut r, "S3", "T3", Some("T-unknown"));
        assert_eq!(r.opener(c), None);
    }

    #[test]
    fn child_frame_has_its_parent() {
        let mut r = Registry::new();
        let t = attach_page(&mut r, "S1", "T1", None);
        navigate(&mut r, "S1", "T1", None, "https://a.test/");
        attach_frame(&mut r, "S1", "F1", "T1");
        attach_frame(&mut r, "S1", "F2", "F1");
        navigate(&mut r, "S1", "F1", Some("T1"), "https://a.test/inner");
        let frames = r.frames(t).unwrap();
        let by: HashMap<u32, &FrameInfo> = frames.iter().map(|f| (f.frame, f)).collect();
        assert_eq!(by[&1].parent, Some(0));
        assert_eq!(by[&2].parent, Some(1));
        assert_eq!(by[&1].url, "https://a.test/inner");
        assert!(!by[&1].opaque);
        assert_eq!(r.cdp_frame_id(t, 2), Some("F2"));
    }

    #[test]
    fn navigation_bumps_generation_so_old_ids_are_stale() {
        let mut r = Registry::new();
        let t = attach_page(&mut r, "S1", "T1", None);
        let g0 = r.generation(t, 0).unwrap();
        navigate(&mut r, "S1", "T1", None, "https://a.test/one");
        let g1 = r.generation(t, 0).unwrap();
        assert_ne!(g0, g1);
        assert!(!r.is_current(t, 0, g0));
        assert!(r.is_current(t, 0, g1));
        // Same-document navigation updates the URL, not the generation.
        r.apply(
            Some("S1"),
            "Page.navigatedWithinDocument",
            &json!({"frameId": "T1", "url": "https://a.test/one#x"}),
        );
        assert!(r.is_current(t, 0, g1));
        assert_eq!(r.tabs()[0].1, "https://a.test/one#x");
        assert!(!r.is_current(t, 7, g1), "unknown frame is never current");
    }

    #[test]
    fn detach_removes_the_frame_and_its_subtree() {
        let mut r = Registry::new();
        let t = attach_page(&mut r, "S1", "T1", None);
        attach_frame(&mut r, "S1", "F1", "T1");
        attach_frame(&mut r, "S1", "F2", "F1");
        attach_frame(&mut r, "S1", "F3", "T1");
        r.apply(
            Some("S1"),
            "Page.frameDetached",
            &json!({"frameId": "F1", "reason": "remove"}),
        );
        let ids: Vec<u32> = r.frames(t).unwrap().iter().map(|f| f.frame).collect();
        assert_eq!(ids, vec![0, 3]);
        assert_eq!(r.frame_by_cdp_id(t, "F2"), None);
        assert_eq!(r.generation(t, 1), None);
        // Frame numbers are never reused for a new frame.
        attach_frame(&mut r, "S1", "F4", "T1");
        assert_eq!(r.frame_by_cdp_id(t, "F4"), Some(4));
    }

    #[test]
    fn swap_detach_keeps_the_frame_and_the_oopif_makes_it_opaque() {
        let mut r = Registry::new();
        let t = attach_page(&mut r, "S1", "T1", None);
        navigate(&mut r, "S1", "T1", None, "https://a.test/");
        attach_frame(&mut r, "S1", "F1", "T1");
        navigate(&mut r, "S1", "F1", Some("T1"), "https://a.test/same");
        r.apply(
            Some("S1"),
            "Page.frameDetached",
            &json!({"frameId": "F1", "reason": "swap"}),
        );
        assert_eq!(r.frame_by_cdp_id(t, "F1"), Some(1));
        r.apply(
            Some("S1"),
            "Target.attachedToTarget",
            &json!({"sessionId": "S9", "targetInfo": {"targetId": "F1", "type": "iframe", "url": "https://b.test/"}, "waitingForDebugger": true}),
        );
        assert_eq!(
            r.tab_by_session("S9"),
            None,
            "an OOPIF session is not a tab"
        );
        assert_eq!(r.frame_session(t, 1), Some("S9"));
        navigate(&mut r, "S9", "F1", None, "https://b.test/x");
        let f1 = r
            .frames(t)
            .unwrap()
            .into_iter()
            .find(|f| f.frame == 1)
            .unwrap();
        assert_eq!(f1.url, "https://b.test/x");
        assert!(f1.opaque);
        assert_eq!(r.tabs().len(), 1);
    }

    #[test]
    fn cross_origin_child_is_opaque_and_about_blank_inherits() {
        let mut r = Registry::new();
        let t = attach_page(&mut r, "S1", "T1", None);
        navigate(&mut r, "S1", "T1", None, "https://a.test/");
        attach_frame(&mut r, "S1", "F1", "T1");
        navigate(&mut r, "S1", "F1", Some("T1"), "https://ads.other.test/");
        attach_frame(&mut r, "S1", "F2", "T1");
        navigate(&mut r, "S1", "F2", Some("T1"), "about:srcdoc");
        attach_frame(&mut r, "S1", "F3", "F1");
        navigate(&mut r, "S1", "F3", Some("F1"), "about:blank");
        attach_frame(&mut r, "S1", "F4", "T1");
        navigate(&mut r, "S1", "F4", Some("T1"), "https://A.test:443/p");
        let o: HashMap<u32, bool> = r
            .frames(t)
            .unwrap()
            .iter()
            .map(|f| (f.frame, f.opaque))
            .collect();
        assert!(!o[&0]);
        assert!(o[&1], "cross-origin");
        assert!(!o[&2], "srcdoc inherits");
        assert!(o[&3], "inside an opaque frame");
        assert!(!o[&4], "same origin, default port and case folded");
    }

    #[test]
    fn sb_world_context_tracked_per_frame_and_destroyed() {
        let mut r = Registry::new();
        let t = attach_page(&mut r, "S1", "T1", None);
        attach_frame(&mut r, "S1", "F1", "T1");
        context(&mut r, "S1", 1, "T1", "", true);
        context(&mut r, "S1", 2, "T1", SB_WORLD, false);
        context(&mut r, "S1", 3, "F1", SB_WORLD, false);
        context(&mut r, "S1", 4, "T1", "some-extension", false);
        assert_eq!(r.default_context(t, 0).map(|c| c.id), Some(1));
        assert_eq!(r.sb_context(t, 0).map(|c| c.id), Some(2));
        assert_eq!(r.sb_context(t, 0).unwrap().session, "S1");
        assert_eq!(r.sb_context(t, 1).map(|c| c.id), Some(3));
        r.apply(
            Some("S1"),
            "Runtime.executionContextDestroyed",
            &json!({"executionContextId": 2, "executionContextUniqueId": "u2"}),
        );
        assert_eq!(r.sb_context(t, 0), None);
        assert_eq!(r.default_context(t, 0).map(|c| c.id), Some(1));
        assert_eq!(r.sb_context(t, 1).map(|c| c.id), Some(3));
        r.apply(Some("S1"), "Runtime.executionContextsCleared", &json!({}));
        assert_eq!(r.default_context(t, 0), None);
        assert_eq!(r.sb_context(t, 1), None);
    }

    #[test]
    fn target_detach_removes_the_tab_and_its_sessions() {
        let mut r = Registry::new();
        let a = attach_page(&mut r, "S1", "T1", None);
        let b = attach_page(&mut r, "S2", "T2", Some("T1"));
        r.apply(
            None,
            "Target.detachedFromTarget",
            &json!({"sessionId": "S1", "targetId": "T1"}),
        );
        assert!(!r.has_tab(a));
        assert_eq!(r.tab_by_target("T1"), None);
        assert_eq!(r.tab_by_session("S1"), None);
        assert_eq!(r.frames(a), None);
        assert_eq!(r.tabs().len(), 1);
        assert_eq!(r.opener(b), Some(a), "opener history is kept");
        // Events for the gone session are ignored.
        navigate(&mut r, "S1", "T1", None, "https://x.test/");
        assert_eq!(r.tabs().len(), 1);
    }

    #[test]
    fn title_follows_target_info_changes() {
        let mut r = Registry::new();
        let t = attach_page(&mut r, "S1", "T1", None);
        r.apply(
            None,
            "Target.targetInfoChanged",
            &json!({"targetInfo": {"targetId": "T1", "type": "page", "title": "Inbox (3)", "url": "https://mail.test/"}}),
        );
        assert_eq!(r.tabs()[0], (t, "about:blank".into(), "Inbox (3)".into()));
    }

    #[test]
    fn malformed_events_are_ignored() {
        let mut r = Registry::new();
        r.apply(None, "Target.attachedToTarget", &json!({}));
        r.apply(Some("S?"), "Page.frameNavigated", &json!({"frame": 3}));
        r.apply(Some("S?"), "Runtime.executionContextCreated", &json!(null));
        assert!(r.tabs().is_empty());
    }

    #[test]
    fn origin_parsing() {
        assert_eq!(
            origin("https://a.test/x"),
            Origin::Tuple("https://a.test".into())
        );
        assert_eq!(
            origin("https://u:p@a.test:443/"),
            Origin::Tuple("https://a.test".into())
        );
        assert_eq!(
            origin("http://a.test:8080/"),
            Origin::Tuple("http://a.test:8080".into())
        );
        assert_eq!(origin("about:blank"), Origin::Inherit);
        assert_eq!(origin("data:text/html,hi"), Origin::Opaque);
        assert_eq!(
            origin("blob:https://a.test/uuid"),
            Origin::Tuple("https://a.test".into())
        );
    }

    fn attach(r: &mut Registry, parent: Option<&str>, sid: &str, info: Value, waiting: bool) {
        r.apply(
            parent,
            "Target.attachedToTarget",
            &json!({"sessionId": sid, "targetInfo": info, "waitingForDebugger": waiting}),
        );
    }

    #[test]
    fn second_session_on_a_tab_does_not_take_it_over() {
        let mut r = Registry::new();
        let t = attach_page(&mut r, "S1", "T1", None);
        // An explicit Target.attachToTarget{flatten} on the same page.
        attach(
            &mut r,
            None,
            "S2",
            json!({"targetId": "T1", "type": "page", "url": "about:blank"}),
            false,
        );
        assert_eq!(r.session(t), Some("S1"));
        assert_eq!(r.tab_by_session("S2"), None);
        assert!(!r.tracks("S2"));
        assert!(
            r.take_seeds().is_empty(),
            "no seed for an untracked session"
        );
        // Events on the second session do not touch the tab.
        let g = r.generation(t, 0).unwrap();
        navigate(&mut r, "S2", "T1", None, "https://x.test/");
        assert!(r.is_current(t, 0, g));
        r.apply(
            None,
            "Target.detachedFromTarget",
            &json!({"sessionId": "S2", "targetId": "T1"}),
        );
        assert!(r.has_tab(t), "the second session's detach keeps the tab");
        assert_eq!(r.session(t), Some("S1"));
        assert_eq!(r.tab_by_session("S1"), Some(t));
        r.apply(
            None,
            "Target.detachedFromTarget",
            &json!({"sessionId": "S1", "targetId": "T1"}),
        );
        assert!(!r.has_tab(t), "the owner's detach removes it");
    }

    #[test]
    fn second_session_on_an_oopif_does_not_take_it_over() {
        let mut r = Registry::new();
        let t = attach_page(&mut r, "S1", "T1", None);
        attach_frame(&mut r, "S1", "F1", "T1");
        let iframe = json!({"targetId": "F1", "type": "iframe", "url": "https://b.test/"});
        attach(&mut r, Some("S1"), "O1", iframe.clone(), true);
        attach(&mut r, Some("S1"), "O2", iframe, false);
        assert_eq!(r.frame_session(t, 1), Some("O1"));
        r.apply(
            Some("S1"),
            "Target.detachedFromTarget",
            &json!({"sessionId": "O2"}),
        );
        assert_eq!(r.frame_session(t, 1), Some("O1"));
    }

    #[test]
    fn target_destroyed_removes_the_tab() {
        let mut r = Registry::new();
        let t = attach_page(&mut r, "S1", "T1", None);
        r.apply(None, "Target.targetDestroyed", &json!({"targetId": "T1"}));
        assert!(!r.has_tab(t));
        assert_eq!(r.tab_by_session("S1"), None);
        assert!(r.tabs().is_empty());
    }

    #[test]
    fn prerender_is_not_a_tab_until_it_activates() {
        let mut r = Registry::new();
        let t = attach_page(&mut r, "S1", "T1", None);
        navigate(&mut r, "S1", "T1", None, "https://search.test/?q=x");
        attach_frame(&mut r, "S1", "F1", "T1");
        let g = r.generation(t, 0).unwrap();
        // Page-level auto-attach delivers the prerender under the tab's session.
        attach(
            &mut r,
            Some("S1"),
            "P1",
            json!({"targetId": "TP", "type": "page", "subtype": "prerender", "url": "https://result.test/"}),
            true,
        );
        assert_eq!(r.tabs().len(), 1, "a prerender is not a tab");
        assert_eq!(r.tab_by_target("TP"), None);
        assert_eq!(r.tab_by_session("P1"), None);
        assert!(r.tracks("P1"), "its events are wanted once it activates");
        // Its frame events do not leak into the tab before activation.
        navigate(&mut r, "P1", "TP", None, "https://result.test/");
        assert_eq!(r.tabs()[0].1, "https://search.test/?q=x");
        // Any other subtype is not a tab either.
        attach(
            &mut r,
            None,
            "Q1",
            json!({"targetId": "TQ", "type": "page", "subtype": "something-new"}),
            true,
        );
        assert_eq!(r.tabs().len(), 1);
        assert!(r.take_seeds().is_empty());

        // Activation: the target loses its subtype and becomes the tab's page.
        r.apply(
            None,
            "Target.targetInfoChanged",
            &json!({"targetInfo": {"targetId": "TP", "type": "page", "url": "https://result.test/", "title": "Result"}}),
        );
        assert_eq!(
            r.tabs(),
            vec![(t, "https://result.test/".into(), "Result".into())]
        );
        assert_eq!(r.session(t), Some("P1"));
        assert_eq!(r.tab_by_target("TP"), Some(t));
        assert_eq!(r.tab_by_target("T1"), None);
        assert_eq!(r.tab_by_session("S1"), None);
        assert!(
            !r.is_current(t, 0, g),
            "ids from the old document are stale"
        );
        assert_eq!(r.frame_by_cdp_id(t, "F1"), None, "old frames went with it");
        assert_eq!(
            r.take_seeds(),
            vec![Seed {
                session: "P1".into(),
                replay_contexts: true
            }]
        );
        // The old page going away does not remove the tab.
        r.apply(
            Some("S1"),
            "Target.detachedFromTarget",
            &json!({"sessionId": "S1", "targetId": "T1"}),
        );
        r.apply(None, "Target.targetDestroyed", &json!({"targetId": "T1"}));
        assert!(r.has_tab(t));
        // New frames get fresh numbers.
        attach_frame(&mut r, "P1", "F9", "TP");
        assert_eq!(r.frame_by_cdp_id(t, "F9"), Some(2));
    }

    #[test]
    fn running_attach_is_seeded_from_the_frame_tree() {
        let mut r = Registry::new();
        // The initial tab (or one `me` restored): attached running, frames already loaded.
        attach(
            &mut r,
            None,
            "S1",
            json!({"targetId": "T1", "type": "page", "url": "https://a.test/"}),
            false,
        );
        let t = r.tab_by_target("T1").unwrap();
        assert_eq!(
            r.take_seeds(),
            vec![Seed {
                session: "S1".into(),
                replay_contexts: false
            }]
        );
        // A paused (new) target needs no seed.
        attach_page(&mut r, "S2", "T2", None);
        assert!(r.take_seeds().is_empty());

        r.seed_frame_tree(
            "S1",
            &json!({"frameTree": {
            "frame": {"id": "T1", "url": "https://a.test/", "urlFragment": "#top"},
            "childFrames": [
                {"frame": {"id": "F1", "parentId": "T1", "url": "https://a.test/in"},
                 "childFrames": [{"frame": {"id": "F2", "parentId": "F1", "url": "about:srcdoc"}}]},
                {"frame": {"id": "F3", "parentId": "T1", "url": "https://ads.test/"}}
            ]}}),
        );
        let frames = r.frames(t).unwrap();
        let by: HashMap<u32, &FrameInfo> = frames.iter().map(|f| (f.frame, f)).collect();
        assert_eq!(frames.len(), 4);
        assert_eq!(by[&0].url, "https://a.test/#top");
        assert_eq!(r.frame_by_cdp_id(t, "F1"), Some(1));
        assert_eq!(r.frame_by_cdp_id(t, "F2"), Some(2));
        assert_eq!(r.frame_by_cdp_id(t, "F3"), Some(3));
        assert_eq!(by[&2].parent, Some(1));
        assert!(!by[&1].opaque && !by[&2].opaque);
        assert!(by[&3].opaque, "cross-origin child");
        assert_eq!(r.generation(t, 1), Some(0), "seeding is not a navigation");
        // Contexts Runtime.enable replays after the seed now find their frames.
        context(&mut r, "S1", 7, "F2", "", true);
        assert_eq!(r.default_context(t, 2).map(|c| c.id), Some(7));
        // Seeding again changes nothing that is already known.
        r.seed_frame_tree(
            "S1",
            &json!({"frameTree": {"frame": {"id": "T1", "url": "https://a.test/"},
                "childFrames": [{"frame": {"id": "F1", "parentId": "T1", "url": "https://a.test/in"}}]}}),
        );
        assert_eq!(r.frame_by_cdp_id(t, "F1"), Some(1));
        // An unknown session or a malformed reply is ignored.
        r.seed_frame_tree("S?", &json!({"frameTree": {}}));
        r.seed_frame_tree("S1", &json!({"nope": 1}));
        assert_eq!(r.frames(t).unwrap().len(), 4);
    }

    #[test]
    fn running_oopif_is_seeded_under_its_parent() {
        let mut r = Registry::new();
        let t = attach_page(&mut r, "S1", "T1", None);
        r.take_seeds();
        navigate(&mut r, "S1", "T1", None, "https://a.test/");
        // An iframe that was already out of process: the page never reported it.
        attach(
            &mut r,
            Some("S1"),
            "O1",
            json!({"targetId": "F1", "type": "iframe", "url": "https://b.test/"}),
            false,
        );
        assert_eq!(
            r.take_seeds(),
            vec![Seed {
                session: "O1".into(),
                replay_contexts: false
            }]
        );
        r.seed_frame_tree(
            "O1",
            &json!({"frameTree": {"frame": {"id": "F1", "parentId": "T1", "url": "https://b.test/x"},
                "childFrames": [{"frame": {"id": "G1", "parentId": "F1", "url": "https://b.test/inner"}}]}}),
        );
        let n = r.frame_by_cdp_id(t, "F1").expect("OOPIF frame registered");
        assert_eq!(r.frame_session(t, n), Some("O1"));
        let f = r
            .frames(t)
            .unwrap()
            .into_iter()
            .find(|f| f.frame == n)
            .unwrap();
        assert_eq!(f.parent, Some(0));
        assert_eq!(f.url, "https://b.test/x");
        assert!(f.opaque);
        assert_eq!(
            r.frame_by_cdp_id(t, "G1"),
            None,
            "never read inside an OOPIF"
        );
    }
}
