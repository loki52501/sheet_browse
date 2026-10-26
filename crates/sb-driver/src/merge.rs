//! I-9 `Driver::index` (T-22): the tab's index with every readable frame merged in document
//! order. Each frame's rows come from its own `__sb.index()` (frame-local ids, frame 0 on the
//! wire) and get the registry's frame number, so their refs read `#f2.15`. An iframe row gets
//! `child_frame`: the registry frame its element hosts, found from the parent's side with
//! `DOM.getFrameOwner` (the owner element's backend node), `DOM.resolveNode` into the
//! parent's `sb` world and `__sb.id(this)` there (cached per parent document). The child's
//! rows follow its iframe row. An opaque frame (cross-origin, out of process, or inside one)
//! is that one row with OPAQUE and is never read inside (B-4 #2, B-5); neither is a frame
//! whose row is a CHALLENGE (D-5). A frame whose element is not a row (a hidden iframe) is
//! left out with it. All reads and owner lookups of one call run concurrently.
//!
//! Rows are page strings: every string in them goes through the process-wide vault scrub
//! (H-2, rule 2) before they leave, as `tabs()` does.

use crate::runtime::CallError;
use crate::{Driver, DriverError, FrameInfo, TabId};
use sb_sheet::{FieldValue, IndexRow};
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

/// Every page string of the rows through the process-wide scrub (H-2).
fn scrub_rows(rows: &mut [IndexRow]) {
    let scrub = |s: &mut String| {
        let c = harness_core::scrub(s);
        if c != *s {
            *s = c;
        }
    };
    for r in rows {
        scrub(&mut r.name);
        for o in [
            &mut r.nearby,
            &mut r.heading,
            &mut r.form,
            &mut r.landmark,
            &mut r.state.input_type,
            &mut r.state.autocomplete,
        ]
        .into_iter()
        .flatten()
        {
            scrub(o);
        }
        if let Some(FieldValue::Text(t)) = &mut r.state.value {
            scrub(t);
        }
        for o in &mut r.state.options {
            scrub(o);
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
    /// document). `None` on any failure: the frame is then left out.
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
        let id = match resolved
            .as_ref()
            .and_then(|o| o.pointer("/object/objectId"))
            .and_then(Value::as_str)
        {
            Some(object) => self
                .cdp_call_until(
                    &ctx.session,
                    "Runtime.callFunctionOn",
                    json!({"functionDeclaration": OWNER_FN, "objectId": object,
                           "returnByValue": true}),
                    deadline,
                )
                .await
                .and_then(|v| v.pointer("/result/value").and_then(Value::as_u64))
                .and_then(|n| u32::try_from(n).ok()),
            None => None,
        };
        drop(self.cdp.start(
            Some(&ctx.session),
            "Runtime.releaseObjectGroup",
            json!({ "objectGroup": group }),
        ));
        resolved?;
        self.owners().map.insert((tab, frame), (key, id));
        id
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

    /// I-9 `index` (module docs).
    pub(crate) async fn merged_index(&self, tab: TabId) -> Result<Vec<IndexRow>, DriverError> {
        let frames = self
            .with_registry(|r| r.frames(tab))
            .ok_or(DriverError::TabGone(tab))?;
        if self.open_dialog(tab).is_some() {
            return Err(DriverError::Cdp(
                "index: a dialog is open on this tab: answer it with dialog() first".into(),
            ));
        }
        let deadline = Instant::now() + INDEX_CAP;
        let readable: Vec<u32> = frames
            .iter()
            .filter(|f| !f.opaque)
            .map(|f| f.frame)
            .collect();
        // Owners are looked up for every frame whose parent is read: an opaque child still
        // gets its one row's child_frame.
        let lookups: Vec<(u32, u32)> = frames
            .iter()
            .filter_map(|f| Some((f.frame, f.parent?)))
            .filter(|(_, p)| readable.contains(p))
            .collect();
        let reads: Vec<BoxFut<'_, FrameRead>> = readable
            .iter()
            .map(|&f| -> BoxFut<'_, _> {
                Box::pin(async move { (f, self.frame_rows(tab, f, deadline).await) })
            })
            .collect();
        let owner_futs: Vec<BoxFut<'_, (u32, Option<u32>)>> = lookups
            .iter()
            .map(|&(f, p)| -> BoxFut<'_, _> {
                Box::pin(async move { (f, self.owner_id(tab, f, p, deadline).await) })
            })
            .collect();
        let (reads, owned) = tokio::join!(join_all(reads), join_all(owner_futs));
        let mut rows = BTreeMap::new();
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
        let owners: HashMap<u32, u32> = owned
            .into_iter()
            .filter_map(|(f, id)| Some((f, id?)))
            .collect();
        let mut merged = merge(&frames, &mut rows, &owners);
        scrub_rows(&mut merged);
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
