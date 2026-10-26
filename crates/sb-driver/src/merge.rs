//! I-9 `Driver::index` (T-22): the tab's index with every readable frame merged in document
//! order. Each frame's rows come from its own `__sb.index()` (frame-local ids, frame 0 on the
//! wire) and get the registry's frame number, so their refs read `#f2.15`. An iframe row gets
//! `child_frame`: the registry frame its element hosts, found from the parent's side with
//! `DOM.getFrameOwner` (the owner element's backend node), `DOM.resolveNode` into the
//! parent's `sb` world and `__sb.id(this)` there (cached per parent document). The child's
//! rows follow its iframe row. An opaque frame (cross-origin, out of process, or inside one)
//! is that one row with OPAQUE and is never read inside (B-4 #2, B-5); neither is a frame
//! whose row is a CHALLENGE, nor any frame below it (D-5): frames are read level by level
//! and a child is only read once its parent's fresh row and the registry's ancestry say it
//! may be. A frame whose element is not a row (a hidden iframe) is left out with it. The
//! reads of one level and the owner lookups of its children run concurrently.
//!
//! Rows are page strings: every string in them goes through the process-wide vault scrub
//! (H-2, rule 2) and a cap before they leave, as `tabs()` does.

use crate::runtime::CallError;
use crate::{Driver, DriverError, FrameInfo, TabId};
use sb_sheet::{FieldValue, Flags, IndexRow, State};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;
use tokio::time::Instant;

/// Cap of one `index()` call (every frame read and owner lookup in it).
const INDEX_CAP: Duration = Duration::from_secs(3);
/// Frames nested deeper than this are not merged (a page with more is not real).
const MAX_DEPTH: usize = 32;

const INDEX_FN: &str = "function () {
  const sb = globalThis.__sb;
  if (!sb) return { why: 'no runtime' };
  return sb.index();
}";

/// The id of `this` (an iframe element) in its document's `sb` world.
const OWNER_FN: &str = "function () {
  const sb = globalThis.__sb;
  if (!sb) return null;
  const id = sb.id(this);
  return typeof id === 'number' ? id : null;
}";

/// Owner of a child frame, per (tab, child frame): (the parent document's key, its id
/// there; `None` when the element is not a row of that document).
#[derive(Debug, Default)]
pub(crate) struct Owners {
    map: HashMap<(TabId, u32), (String, Option<u32>)>,
}

impl Owners {
    pub(crate) fn forget_tab(&mut self, tab: TabId) {
        self.map.retain(|(t, _), _| *t != tab);
    }
}

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One frame's read: the frame and its rows (`None`: the page gave none).
type FrameRead = (u32, Result<Option<Vec<IndexRow>>, CallError>);

/// Run every future concurrently; outputs in input order.
async fn join_all<T>(futs: Vec<BoxFut<'_, T>>) -> Vec<T> {
    let mut futs: Vec<Option<BoxFut<'_, T>>> = futs.into_iter().map(Some).collect();
    let mut out: Vec<Option<T>> = futs.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (f, o) in futs.iter_mut().zip(out.iter_mut()) {
            if let Some(fut) = f {
                match fut.as_mut().poll(cx) {
                    Poll::Ready(v) => {
                        *o = Some(v);
                        *f = None;
                    }
                    Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
    out.into_iter().flatten().collect()
}

/// Caps of the row strings, in characters, after the scrub (the page caps them too; these
/// hold whatever a page or a changed runtime sends).
const ROLE_CAP: usize = 40;
const TEXT_CAP: usize = 300;
const TOKEN_CAP: usize = 64;
const OPTION_CAP: usize = 120;
const OPTIONS_MAX: usize = 50;

/// `s` scrubbed whole (rule 2), then cut at `cap` characters with `…` (on a char boundary).
fn clean(s: &mut String, cap: usize, scrub: &dyn Fn(&str) -> String) {
    let c = scrub(s);
    *s = if c.chars().count() > cap {
        let mut t: String = c.chars().take(cap.saturating_sub(1)).collect();
        t.push('…');
        t
    } else {
        c
    };
}

/// Every page string of every row through `scrub` (the process-wide vault scrub in
/// `index()`, H-2) and capped. The rows are taken apart field by field with exhaustive
/// patterns, so a string field added to I-5 fails to compile here until it is handled.
fn scrub_rows(rows: &mut [IndexRow], scrub: &dyn Fn(&str) -> String) {
    for r in rows {
        let IndexRow {
            id: _,
            frame: _,
            role,
            name,
            nearby,
            heading,
            state,
            flags:
                Flags {
                    submit: _,
                    challenge: _,
                    overlay: _,
                    opaque: _,
                },
            form,
            landmark,
            child_frame: _,
            bbox: _,
        } = r;
        let State {
            visible: _,
            enabled: _,
            checked: _,
            value,
            submitish: _,
            opens_new_tab: _,
            options,
            covered: _,
            input_type,
            autocomplete,
        } = state;
        clean(role, ROLE_CAP, scrub);
        clean(name, TEXT_CAP, scrub);
        for s in [nearby, heading].into_iter().flatten() {
            clean(s, TEXT_CAP, scrub);
        }
        for s in [form, landmark, input_type, autocomplete]
            .into_iter()
            .flatten()
        {
            clean(s, TOKEN_CAP, scrub);
        }
        match value {
            Some(FieldValue::Text(t)) => clean(t, TEXT_CAP, scrub),
            Some(FieldValue::Empty | FieldValue::Filled) | None => {}
        }
        options.truncate(OPTIONS_MAX);
        for o in options.iter_mut() {
            clean(o, OPTION_CAP, scrub);
        }
    }
}
/// Merge per-frame rows: `rows[f]` in document order, each iframe row of `f` followed by its
/// child frame's rows (depth first). `owners[c]` is the id of frame `c`'s element in its
/// parent; `readable[c]` says whether `c` may be read inside.
pub(crate) fn merge(
    frames: &[FrameInfo],
    rows: &mut BTreeMap<u32, Vec<IndexRow>>,
    owners: &HashMap<u32, u32>,
) -> Vec<IndexRow> {
    let info: HashMap<u32, &FrameInfo> = frames.iter().map(|f| (f.frame, f)).collect();
    // (parent, owner id) -> child frame
    let mut children: HashMap<(u32, u32), u32> = HashMap::new();
    for f in frames {
        if let (Some(p), Some(&id)) = (f.parent, owners.get(&f.frame)) {
            children.insert((p, id), f.frame);
        }
    }
    let mut out = Vec::new();
    emit(0, 0, &info, &children, rows, &mut out);
    out
}

fn emit(
    frame: u32,
    depth: usize,
    info: &HashMap<u32, &FrameInfo>,
    children: &HashMap<(u32, u32), u32>,
    rows: &mut BTreeMap<u32, Vec<IndexRow>>,
    out: &mut Vec<IndexRow>,
) {
    let Some(mine) = rows.remove(&frame) else {
        return;
    };
    for mut row in mine {
        row.frame = frame;
        let child = children.get(&(frame, row.id)).copied();
        let mut inner = None;
        if let Some(c) = child {
            row.child_frame = Some(c);
            let opaque = info.get(&c).is_none_or(|f| f.opaque);
            if opaque {
                row.flags.opaque = true;
            }
            if !opaque && !row.flags.opaque && !row.flags.challenge && depth < MAX_DEPTH {
                inner = Some(c);
            }
        }
        out.push(row);
        if let Some(c) = inner {
            emit(c, depth + 1, info, children, rows, out);
        }
    }
}

impl Driver {
    /// One frame's own index, frame number set; `None` when the page gives none.
    async fn frame_rows(
        &self,
        tab: TabId,
        frame: u32,
        deadline: Instant,
    ) -> Result<Option<Vec<IndexRow>>, CallError> {
        let v = self.call_sb(tab, frame, INDEX_FN, &[], deadline).await?;
        let Ok(mut rows) = Vec::<IndexRow>::deserialize(&v) else {
            return Ok(None);
        };
        for r in &mut rows {
            r.frame = frame;
        }
        Ok(Some(rows))
    }

    /// The id, in its parent's document, of the element hosting `frame` (cached per parent
    /// document). `None` when the element is not a row there, or on any failure: the frame
    /// is then left out. Only a definite answer is cached; a timeout or a failed call is
    /// asked again on the next `index()` (review-T-22 should-fix 4).
    async fn owner_id(
        &self,
        tab: TabId,
        frame: u32,
        parent: u32,
        deadline: Instant,
    ) -> Option<u32> {
        let ctx = self
            .based_ctx(tab, parent, deadline, None, false)
            .await
            .ok()?;
        let key = crate::runtime::doc_key(&ctx);
        if let Some((k, id)) = self.owners().map.get(&(tab, frame)) {
            if *k == key {
                return *id;
            }
        }
        let definite = self.ask_owner(tab, frame, &ctx, deadline).await?;
        self.owners().map.insert((tab, frame), (key, definite));
        definite
    }

    /// `DOM.getFrameOwner` -> `DOM.resolveNode` into the parent's `sb` world ->
    /// `__sb.id(this)`. `Some(answer)` when the page answered (`answer` is `None` for an
    /// element that is not a row), `None` when any step failed or ran out of time.
    async fn ask_owner(
        &self,
        tab: TabId,
        frame: u32,
        ctx: &crate::ExecContext,
        deadline: Instant,
    ) -> Option<Option<u32>> {
        let cdp_id = self.with_registry(|r| r.cdp_frame_id(tab, frame).map(str::to_string))?;
        let owner = self
            .cdp_call_until(
                &ctx.session,
                "DOM.getFrameOwner",
                json!({ "frameId": cdp_id }),
                deadline,
            )
            .await?;
        let node = owner.get("backendNodeId")?.clone();
        let group = format!("sb-driver-owner-{}-{frame}", tab.0);
        let resolved = self
            .cdp_call_until(
                &ctx.session,
                "DOM.resolveNode",
                json!({"backendNodeId": node, "executionContextId": ctx.id, "objectGroup": group}),
                deadline,
            )
            .await;
        let answer = match resolved
            .as_ref()
            .and_then(|o| o.pointer("/object/objectId"))
            .and_then(Value::as_str)
        {
            Some(object) => {
                self.cdp_call_until(
                    &ctx.session,
                    "Runtime.callFunctionOn",
                    json!({"functionDeclaration": OWNER_FN, "objectId": object,
                           "returnByValue": true}),
                    deadline,
                )
                .await
            }
            None => None,
        };
        drop(self.cdp.start(
            Some(&ctx.session),
            "Runtime.releaseObjectGroup",
            json!({ "objectGroup": group }),
        ));
        let v = answer?;
        if v.get("exceptionDetails").is_some() {
            return None;
        }
        Some(
            v.pointer("/result/value")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
        )
    }

    /// One CDP command on `session`, `None` on any error or at `deadline`.
    async fn cdp_call_until(
        &self,
        session: &str,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Option<Value> {
        if Instant::now() >= deadline {
            return None;
        }
        let fut = self.cdp.start(Some(session), method, params);
        tokio::time::timeout_at(deadline, fut).await.ok()?.ok()
    }

    fn owners(&self) -> std::sync::MutexGuard<'_, Owners> {
        self.owners.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// I-9 `index` (module docs). Frames are read level by level: frame 0, then each child
    /// whose iframe row in its parent's fresh rows is neither OPAQUE nor CHALLENGE and that
    /// the registry does not close (opaque, or a CHALLENGE frame or inside one), and so on.
    /// A level's reads run concurrently with the owner lookups of its children, so a page
    /// with one level of frames costs what it did, and nothing inside a CHALLENGE frame or
    /// below one is ever walked or read (D-5, review-T-22 should-fix 3).
    pub(crate) async fn merged_index(&self, tab: TabId) -> Result<Vec<IndexRow>, DriverError> {
        if !self.with_registry(|r| r.has_tab(tab)) {
            return Err(DriverError::TabGone(tab));
        }
        if self.open_dialog(tab).is_some() {
            return Err(DriverError::Cdp(
                "index: a dialog is open on this tab: answer it with dialog() first".into(),
            ));
        }
        let deadline = Instant::now() + INDEX_CAP;
        let mut rows: BTreeMap<u32, Vec<IndexRow>> = BTreeMap::new();
        let mut owners: HashMap<u32, u32> = HashMap::new();
        let mut level: Vec<u32> = vec![0];
        let mut depth = 0usize;
        while !level.is_empty() {
            let frames = self
                .with_registry(|r| r.frames(tab))
                .ok_or(DriverError::TabGone(tab))?;
            // Owners are looked up for every child of a frame read now (in the parent's
            // world, never the child's): an opaque child still gets its one row's
            // child_frame.
            let lookups: Vec<(u32, u32)> = frames
                .iter()
                .filter_map(|f| Some((f.frame, f.parent?)))
                .filter(|(_, p)| level.contains(p))
                .collect();
            let reads: Vec<BoxFut<'_, FrameRead>> = level
                .iter()
                .map(|&f| -> BoxFut<'_, _> {
                    Box::pin(async move { (f, self.frame_rows(tab, f, deadline).await) })
                })
                .collect();
            let owner_futs: Vec<BoxFut<'_, (u32, u32, Option<u32>)>> = lookups
                .iter()
                .map(|&(f, p)| -> BoxFut<'_, _> {
                    Box::pin(async move { (f, p, self.owner_id(tab, f, p, deadline).await) })
                })
                .collect();
            let (reads, owned) = tokio::join!(join_all(reads), join_all(owner_futs));
            for (f, r) in reads {
                match r {
                    Ok(Some(v)) => {
                        rows.insert(f, v);
                    }
                    Ok(None) if f == 0 => {
                        return Err(DriverError::Cdp("index: the page gave no index".into()))
                    }
                    Err(e) if f == 0 => return Err(e.into_driver("index")),
                    _ => {}
                }
            }
            depth += 1;
            let mut next = Vec::new();
            for (c, p, id) in owned {
                let Some(id) = id else { continue };
                owners.insert(c, id);
                let Some(row) = rows.get(&p).and_then(|v| v.iter().find(|r| r.id == id)) else {
                    continue;
                };
                // The parent's fresh row decides whether the frame is a CHALLENGE frame;
                // the registry keeps it so every later call into it, or into a frame below
                // it, is refused by ancestry before anything is sent (D-5).
                let challenge = row.flags.challenge;
                let shut = row.flags.opaque;
                let closed = {
                    let mut r = crate::lock(&self.registry);
                    r.set_challenge(tab, c, challenge);
                    r.closed_frame(tab, c).is_some()
                };
                if !challenge && !shut && !closed && depth < MAX_DEPTH {
                    next.push(c);
                }
            }
            level = next;
        }
        // The registry now: a frame that turned opaque while it was read loses its rows.
        let frames = self
            .with_registry(|r| r.frames(tab))
            .ok_or(DriverError::TabGone(tab))?;
        let mut merged = merge(&frames, &mut rows, &owners);
        scrub_rows(&mut merged, &harness_core::scrub);
        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_sheet::{Flags, State};

    fn row(id: u32, role: &str, name: &str) -> IndexRow {
        IndexRow {
            id,
            frame: 0,
            role: role.into(),
            name: name.into(),
            nearby: None,
            heading: None,
            state: State::default(),
            flags: Flags::default(),
            form: None,
            landmark: None,
            child_frame: None,
            bbox: None,
        }
    }

    fn fi(frame: u32, parent: Option<u32>, opaque: bool) -> FrameInfo {
        FrameInfo {
            frame,
            parent,
            url: String::new(),
            opaque,
        }
    }

    #[test]
    fn child_rows_follow_their_iframe_row_with_rewritten_refs() {
        let frames = [
            fi(0, None, false),
            fi(1, Some(0), false),
            fi(2, Some(0), true),
            fi(3, Some(1), false),
            fi(4, Some(0), false),
        ];
        let mut rows = BTreeMap::from([
            (
                0,
                vec![
                    row(1, "heading", "Top"),
                    row(2, "iframe", "same"),
                    row(3, "iframe", "ads"),
                    row(4, "button", "After"),
                    row(5, "iframe", "hidden-not-a-row"),
                ],
            ),
            (
                1,
                vec![row(1, "textbox", "Inner"), row(2, "iframe", "deep")],
            ),
            (2, vec![row(9, "button", "never read")]),
            (3, vec![row(7, "link", "Deep link")]),
            (4, vec![row(8, "button", "orphan")]),
        ]);
        // Frame 4's element is not a row of frame 0 (no owner): left out.
        let owners = HashMap::from([(1, 2), (2, 3), (3, 2)]);
        let m = merge(&frames, &mut rows, &owners);
        let refs: Vec<String> = m.iter().map(|r| r.elem().to_string()).collect();
        assert_eq!(
            refs,
            ["#1", "#2", "#f1.1", "#f1.2", "#f3.7", "#3", "#4", "#5"]
        );
        assert_eq!(m[1].child_frame, Some(1));
        assert_eq!(m[3].child_frame, Some(3));
        assert_eq!(m[5].child_frame, Some(2));
        assert!(m[5].flags.opaque, "the opaque frame is one OPAQUE row");
        assert!(!m.iter().any(|r| r.name == "never read"));
        assert!(!m.iter().any(|r| r.name == "orphan"));
        assert_eq!(m[7].child_frame, None);
    }

    #[test]
    fn a_challenge_row_is_never_entered() {
        let frames = [fi(0, None, false), fi(1, Some(0), false)];
        let mut cap = row(2, "iframe", "captcha");
        cap.flags.challenge = true;
        let mut rows =
            BTreeMap::from([(0, vec![cap]), (1, vec![row(1, "checkbox", "I am human")])]);
        let m = merge(&frames, &mut rows, &HashMap::from([(1, 2)]));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].child_frame, Some(1));
    }

    /// Rule 2, review-T-22 blocking 3: every string field of a row is scrubbed (role
    /// included) and capped.
    #[test]
    fn every_row_string_is_scrubbed_and_capped() {
        use harness_core::vault::EntryKind;
        let canary = std::env::var("SB_VAULT_CANARY")
            .ok()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "canary-t22-rows-not-a-secret".to_owned());
        let vault =
            harness_core::Vault::in_memory([("pw".to_string(), canary.clone(), EntryKind::Secret)]);
        let scrub = |s: &str| vault.scrub(s);
        let with = |s: &str| format!("{s} {canary} {s}");
        let mut r = row(1, &canary, &with("name"));
        r.nearby = Some(with("near"));
        r.heading = Some(with("heading"));
        r.form = Some(with("f1"));
        r.landmark = Some(with("main"));
        r.state.input_type = Some(canary.clone());
        r.state.autocomplete = Some(with("email"));
        r.state.value = Some(FieldValue::Text(with("value")));
        r.state.options = vec![with("one"), canary.clone()];
        let mut long = row(2, "button", &"n".repeat(1000));
        long.state.options = (0..80).map(|i| format!("o{i}")).collect();
        let mut rows = vec![r, long];
        scrub_rows(&mut rows, &scrub);
        let json = serde_json::to_string(&rows).unwrap();
        let dbg = format!("{rows:?}");
        assert!(!json.contains(&canary), "a row string holds the canary");
        assert!(!dbg.contains(&canary), "a row string holds the canary");
        assert!(rows[0].role.contains("@vault(pw)"), "the role is scrubbed");
        assert!(rows[0].name.starts_with("name @vault(pw)"));
        assert_eq!(rows[1].name.chars().count(), TEXT_CAP);
        assert!(rows[1].name.ends_with('…'));
        assert_eq!(rows[1].state.options.len(), OPTIONS_MAX);
        let mut role = vec![row(3, &"r".repeat(100), "")];
        scrub_rows(&mut role, &scrub);
        assert_eq!(role[0].role.chars().count(), ROLE_CAP);
    }

    #[tokio::test]
    async fn join_all_keeps_input_order() {
        let futs: Vec<BoxFut<'_, u32>> = vec![
            Box::pin(async {
                tokio::task::yield_now().await;
                1
            }),
            Box::pin(async { 2 }),
        ];
        assert_eq!(join_all(futs).await, vec![1, 2]);
    }
}
