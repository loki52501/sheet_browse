//! B-4 #2: the target and frame registry. A pure state machine fed by CDP events in wire
//! order (the client's synchronous observer calls [`Registry::apply`] before any subscriber
//! sees the event), so every lookup is answered locally with zero round trips.
//!
//! Tabs are page targets, numbered from 1 in attach order. Frames are numbered per tab;
//! frame 0 is the main frame, matching `sb_sheet::FrameId` and element refs `#f2.15`.
//! Each frame carries a generation, bumped on every cross-document navigation, so an id
//! taken from an older document can be rejected as stale.

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
    next_tab: u32,
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
        match s(info, "type") {
            Some("page") => {
                if let Some(&tab) = self.by_target.get(target) {
                    if let Some(t) = self.tabs.get_mut(&tab) {
                        self.sessions.remove(&t.session);
                        t.session = sid.to_string();
                    }
                    self.sessions.insert(sid.to_string(), Owner::Tab(tab));
                    return;
                }
                self.next_tab += 1;
                let tab = TabId(self.next_tab);
                let opener = s(info, "openerId").and_then(|o| self.by_target.get(o).copied());
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
            }
            Some("iframe") => {
                // Auto-attached under the session of the page (or OOPIF) that embeds it.
                let Some((tab, _)) = self.owner(session) else {
                    return;
                };
                if let Some(t) = self.tabs.get_mut(&tab) {
                    t.oopif.insert(target.to_string(), sid.to_string());
                }
                self.sessions
                    .insert(sid.to_string(), Owner::Oopif(tab, target.to_string()));
            }
            _ => {}
        }
    }

    fn detached(&mut self, sid: &str) {
        match self.sessions.remove(sid) {
            Some(Owner::Tab(tab)) => {
                if let Some(t) = self.tabs.remove(&tab) {
                    self.by_target.remove(&t.target_id);
                }
                self.sessions.retain(|_, o| *o != Owner::Tab(tab));
                self.sessions
                    .retain(|_, o| !matches!(o, Owner::Oopif(t, _) if *t == tab));
            }
            Some(Owner::Oopif(tab, frame)) => {
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

    fn info_changed(&mut self, params: &Value) {
        let Some(info) = params.get("targetInfo") else {
            return;
        };
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
}
