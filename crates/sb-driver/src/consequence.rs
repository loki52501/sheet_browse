//! B-4 #9: consequence capture. The driver's observer (lib.rs) feeds every event to
//! [`Events`] synchronously, before any subscriber sees it, so nothing is lost to a lagging
//! broadcast: main-frame navigations, JavaScript dialogs (open until answered) and
//! downloads (stopped by `Browser.setDownloadBehavior(deny)` at launch, so nothing lands
//! before [`crate::Driver::download`]). An act step marks the log's sequence number before
//! its input and reads what happened after it.

use crate::{lock, Driver, DriverError, Registry, TabId};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// Log entries kept (a step reads only what came after its mark).
const LOG_CAP: usize = 512;
/// Downloads kept waiting for an answer; the oldest is forgotten first.
const DOWNLOADS_CAP: usize = 64;
/// Raw page strings are cut to this many characters on arrival (escaped later).
const RAW_CAP: usize = 2000;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Kind {
    /// A main-frame document committed, or a same-document navigation of the main frame.
    Navigated {
        url: String,
    },
    /// A main-frame navigation started (it may still become a download or fail).
    NavStarted,
    Dialog {
        text: String,
    },
    Download {
        guid: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct Logged {
    pub seq: u64,
    /// The flattened session it came on; `None` for the browser session.
    pub session: Option<String>,
    pub kind: Kind,
}

/// An open JavaScript dialog, by the session that reported it.
#[derive(Debug, Clone)]
pub(crate) struct OpenDialog {
    pub text: String,
}

/// A download the browser stopped, waiting for [`crate::Driver::download`].
#[derive(Debug, Clone)]
pub(crate) struct Stopped {
    pub guid: String,
    pub url: String,
    /// The page's suggested file name (raw; sanitised when the file is written).
    pub name: String,
    /// The CDP frame id that started it.
    pub frame: String,
    /// The session of `Page.downloadWillBegin`, when that came.
    pub session: Option<String>,
    /// `runs/<id>/files/`, set when an act step reported it (the ask).
    pub files_dir: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub(crate) struct Events {
    seq: u64,
    log: VecDeque<Logged>,
    dialogs: HashMap<String, OpenDialog>,
    downloads: VecDeque<Stopped>,
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn raw(t: &str) -> String {
    t.chars().take(RAW_CAP).collect()
}

impl Events {
    /// The sequence number of the last logged event: a step's mark.
    pub(crate) fn mark(&self) -> u64 {
        self.seq
    }

    fn push(&mut self, session: Option<&str>, kind: Kind) {
        self.seq += 1;
        self.log.push_back(Logged {
            seq: self.seq,
            session: session.map(str::to_string),
            kind,
        });
        while self.log.len() > LOG_CAP {
            self.log.pop_front();
        }
    }

    /// Feed one CDP event (called by the driver's observer for every event).
    pub(crate) fn apply(&mut self, session: Option<&str>, method: &str, p: &Value) {
        match method {
            "Page.frameNavigated" => {
                let Some(frame) = p.get("frame") else { return };
                if frame.get("parentId").is_none() {
                    let url = s(frame, "url").unwrap_or_default();
                    self.push(session, Kind::Navigated { url: raw(url) });
                }
            }
            "Page.navigatedWithinDocument" => {
                // Any frame here: the reader keeps the main frame's by its frame id.
                if let (Some(_), Some(url)) = (s(p, "frameId"), s(p, "url")) {
                    let kind = Kind::Navigated { url: raw(url) };
                    self.push_frame(session, s(p, "frameId"), kind);
                }
            }
            "Page.frameStartedNavigating" => {
                if s(p, "navigationType") != Some("sameDocument") {
                    self.push_frame(session, s(p, "frameId"), Kind::NavStarted);
                }
            }
            "Page.javascriptDialogOpening" => {
                if let Some(sid) = session {
                    let text = raw(s(p, "message").unwrap_or_default());
                    self.dialogs
                        .insert(sid.to_string(), OpenDialog { text: text.clone() });
                    self.push(session, Kind::Dialog { text });
                }
            }
            "Page.javascriptDialogClosed" => {
                if let Some(sid) = session {
                    self.dialogs.remove(sid);
                }
            }
            "Page.downloadWillBegin" | "Browser.downloadWillBegin" => {
                let Some(guid) = s(p, "guid") else { return };
                let page = method.starts_with("Page.");
                if let Some(d) = self.downloads.iter_mut().find(|d| d.guid == guid) {
                    // The second report of the same download: keep the page session.
                    if page && d.session.is_none() {
                        d.session = session.map(str::to_string);
                    }
                    return;
                }
                self.downloads.push_back(Stopped {
                    guid: guid.to_string(),
                    url: s(p, "url").unwrap_or_default().to_string(),
                    name: raw(s(p, "suggestedFilename").unwrap_or_default()),
                    frame: s(p, "frameId").unwrap_or_default().to_string(),
                    session: if page {
                        session.map(str::to_string)
                    } else {
                        None
                    },
                    files_dir: None,
                });
                while self.downloads.len() > DOWNLOADS_CAP {
                    self.downloads.pop_front();
                }
                self.push(
                    if page { session } else { None },
                    Kind::Download {
                        guid: guid.to_string(),
                    },
                );
            }
            "Target.detachedFromTarget" => {
                if let Some(sid) = s(p, "sessionId") {
                    self.dialogs.remove(sid);
                }
            }
            _ => {}
        }
    }

    /// An event that names its frame instead of carrying it in a frame object: logged under
    /// `session<US>frame`, which [`is_main`] matches only for the tab's main frame (its CDP
    /// id is the target id), never as a plain tab session.
    fn push_frame(&mut self, session: Option<&str>, frame: Option<&str>, kind: Kind) {
        let tagged = match (session, frame) {
            (Some(sid), Some(f)) => format!("{sid}\u{1f}{f}"),
            (Some(sid), None) => sid.to_string(),
            (None, _) => return,
        };
        self.push(Some(&tagged), kind);
    }

    /// Entries after `mark`, oldest first.
    pub(crate) fn since(&self, mark: u64) -> impl Iterator<Item = &Logged> {
        self.log.iter().filter(move |l| l.seq > mark)
    }

    /// The open dialog reported on `session`, if any.
    #[cfg(test)]
    pub(crate) fn dialog(&self, session: &str) -> Option<&OpenDialog> {
        self.dialogs.get(session)
    }

    /// Every open dialog, by session.
    pub(crate) fn dialogs(&self) -> impl Iterator<Item = (&String, &OpenDialog)> {
        self.dialogs.iter()
    }

    pub(crate) fn forget_dialog(&mut self, session: &str) {
        self.dialogs.remove(session);
    }

    pub(crate) fn download(&self, guid: &str) -> Option<&Stopped> {
        self.downloads.iter().find(|d| d.guid == guid)
    }

    pub(crate) fn download_mut(&mut self, guid: &str) -> Option<&mut Stopped> {
        self.downloads.iter_mut().find(|d| d.guid == guid)
    }

    pub(crate) fn take_download(&mut self, guid: &str) -> Option<Stopped> {
        let i = self.downloads.iter().position(|d| d.guid == guid)?;
        self.downloads.remove(i)
    }
}

/// Whether a logged event's session slot is this tab's main frame: plain events carry the
/// tab session; frame-scoped ones carry `session<US>frame` and match when the frame is the
/// tab's main frame (its CDP id is the target id).
pub(crate) fn is_main(l: &Logged, session: &str, main_frame: &str) -> bool {
    match l.session.as_deref() {
        Some(s) if s == session => true,
        Some(s) => s
            .split_once('\u{1f}')
            .is_some_and(|(sid, f)| sid == session && f == main_frame),
        None => false,
    }
}

pub(crate) fn lock_events(m: &Mutex<Events>) -> MutexGuard<'_, Events> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Largest file `Driver::download` saves (fail closed past it).
const DOWNLOAD_CAP: u64 = 1 << 30;
/// One `IO.read` chunk.
const READ_CHUNK: u64 = 1 << 20;
/// Cap of a saved file's name, in characters.
const NAME_CAP: usize = 120;

/// A download's name as the page gave it: its suggestion, else the URL's last path segment
/// (percent-decoded). Raw: scrub it, then [`sanitize_name`].
pub(crate) fn raw_name(suggested: &str, url: &str) -> String {
    if !suggested.trim().is_empty() {
        return suggested.to_string();
    }
    let from_url = url
        .split(['?', '#'])
        .next()
        .and_then(|u| u.rsplit('/').next())
        .unwrap_or_default();
    percent_encoding::percent_decode_str(from_url)
        .decode_utf8_lossy()
        .into_owned()
}

/// Windows device names, matched on the part before the first dot (case folded, trailing
/// spaces ignored): `CON`, `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, and `COM`/`LPT` with
/// one digit or one superscript `¹`, `²`, `³`.
fn reserved(stem: &str) -> bool {
    let stem = stem.trim_end().to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let Some(rest) = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
    else {
        return false;
    };
    let mut c = rest.chars();
    matches!(
        (c.next(), c.next()),
        (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None)
    )
}

/// A raw name as a safe file name, which is also the name shown to the model: path
/// separators, control characters and characters Windows refuses become `_`; format (Cf)
/// characters (bidi overrides, zero-width) go; whitespace runs become one space; the
/// harness's own syntax is neutralised as in page text (backticks as `'`, `[`/`]` as
/// `(`/`)`, a `#` that would start a ref as `＃`); leading and trailing dots and spaces go;
/// reserved device names get a `_` prefix; capped at 120 characters keeping the extension.
pub(crate) fn sanitize_name(raw: &str) -> String {
    let chars: Vec<char> = raw
        .chars()
        .filter(|c| !crate::diff::is_format(*c))
        .collect();
    let mut mapped = String::new();
    let mut space = false;
    for (i, &c) in chars.iter().enumerate() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space {
            mapped.push(' ');
            space = false;
        }
        let next = chars.get(i + 1).copied();
        let after = chars.get(i + 2).copied();
        let starts_ref = next.is_some_and(|d| d.is_ascii_digit())
            || (matches!(next, Some('f' | 'F')) && after.is_some_and(|d| d.is_ascii_digit()));
        mapped.push(match c {
            c if c.is_control() || "/\\:*?\"<>|".contains(c) => '_',
            '`' => '\'',
            '[' => '(',
            ']' => ')',
            '#' if starts_ref => '＃',
            c => c,
        });
    }
    let mut name = mapped
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string();
    if name.chars().count() > NAME_CAP {
        let ext: String = match name.rsplit_once('.') {
            Some((_, e)) if !e.is_empty() && e.chars().count() <= 10 => format!(".{e}"),
            _ => String::new(),
        };
        let keep = NAME_CAP - ext.chars().count();
        let stem: String = name.chars().take(keep).collect();
        name = stem.trim_end_matches([' ', '.']).to_string() + &ext;
    }
    if name.is_empty() {
        name = "download".into();
    }
    if reserved(name.split('.').next().unwrap_or_default()) {
        name.insert(0, '_');
    }
    name
}

/// [`raw_name`] then [`sanitize_name`].
#[cfg(test)]
pub(crate) fn safe_name(suggested: &str, url: &str) -> String {
    sanitize_name(&raw_name(suggested, url))
}

/// Claim `dir/name`, or `dir/stem (2).ext` and so on when taken: the file is created
/// with `create_new`, so two downloads never get the same path (no check-then-write race).
fn claim_path(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    for k in 1..10_000u32 {
        let p = if k == 1 {
            dir.join(name)
        } else {
            dir.join(format!("{stem} ({k}){ext}"))
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
        {
            Ok(_) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free file name"))
}

/// Standard or URL-safe base64, padding optional; `None` on anything else.
pub(crate) fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3 + 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b'\r' | b'\n' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn failed(what: impl std::fmt::Display) -> DriverError {
    DriverError::Cdp(format!("download: {what}"))
}

impl Driver {
    /// The log's mark: what a step reads its consequences after.
    pub(crate) fn events_mark(&self) -> u64 {
        lock_events(&self.events).mark()
    }

    /// Read the registry and the event store together (registry first, as everywhere).
    pub(crate) fn with_events<R>(&self, f: impl FnOnce(&Registry, &mut Events) -> R) -> R {
        let r = lock(&self.registry);
        let mut ev = lock_events(&self.events);
        f(&r, &mut ev)
    }

    /// [`Driver::wait_registry`] over the registry and the event store: `f` is re-checked
    /// after every event the observer applies; `None` on the cap.
    pub(crate) async fn wait_state<R>(
        &self,
        cap: Duration,
        mut f: impl FnMut(&Registry, &Events) -> Option<R>,
    ) -> Option<R> {
        let mut changed = self.changed.clone();
        let wait = async {
            loop {
                changed.borrow_and_update();
                if let Some(r) = self.with_events(|r, ev| f(r, ev)) {
                    return Some(r);
                }
                if changed.changed().await.is_err() {
                    return self.with_events(|r, ev| f(r, ev));
                }
            }
        };
        tokio::time::timeout(cap, wait).await.ok().flatten()
    }

    /// The session and text of a dialog open on the tab (its page or one of its frames).
    fn tab_dialog(&self, tab: TabId) -> Option<(String, String)> {
        self.with_events(|r, ev| {
            ev.dialogs()
                .find(|(s, _)| r.tab_of_session(s) == Some(tab))
                .map(|(s, d)| (s.clone(), d.text.clone()))
        })
    }

    /// The text of a dialog open on the tab, if any.
    pub(crate) fn open_dialog(&self, tab: TabId) -> Option<String> {
        self.tab_dialog(tab).map(|d| d.1)
    }

    /// A dialog opened on the tab after `mark`.
    pub(crate) fn dialog_since(&self, tab: TabId, mark: u64) -> bool {
        self.with_events(|r, ev| {
            ev.since(mark).any(|l| {
                matches!(l.kind, Kind::Dialog { .. })
                    && l.session.as_deref().and_then(|s| r.tab_of_session(s)) == Some(tab)
            })
        })
    }

    /// I-9 `dialog`: answer the tab's open JavaScript dialog (`Page.handleJavaScriptDialog`);
    /// `text` is a prompt's answer. The page then continues.
    pub(crate) async fn answer_dialog(
        &self,
        tab: TabId,
        accept: bool,
        text: Option<&str>,
    ) -> Result<(), DriverError> {
        if !self.with_registry(|r| r.has_tab(tab)) {
            return Err(DriverError::TabGone(tab));
        }
        let (session, _) = self
            .tab_dialog(tab)
            .ok_or_else(|| DriverError::Cdp(format!("no dialog is open on tab {}", tab.0)))?;
        let mut params = json!({ "accept": accept });
        if let Some(t) = text {
            params["promptText"] = json!(t);
        }
        self.cdp
            .call(Some(&session), "Page.handleJavaScriptDialog", params)
            .await?;
        lock_events(&self.events).forget_dialog(&session);
        Ok(())
    }

    /// I-9 `download`: the answer to a download an act step reported (stopped by the deny
    /// behaviour, so nothing landed). `allow`: fetch it again with the page's cookies
    /// (`Network.loadNetworkResource`, or decode a `data:` URL) into `runs/<id>/files/`
    /// under a safe, unique name, and return the path; otherwise forget it. Answered once.
    pub(crate) async fn answer_download(
        &self,
        tab: TabId,
        guid: &str,
        allow: bool,
    ) -> Result<Option<PathBuf>, DriverError> {
        let session = self
            .with_registry(|r| r.session(tab).map(str::to_string))
            .ok_or(DriverError::TabGone(tab))?;
        let shown: String = guid
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .take(64)
            .collect();
        let d = self
            .with_events(|r, ev| {
                let d = ev.download(guid)?;
                let ours = d.session.as_deref().and_then(|s| r.tab_of_session(s)) == Some(tab)
                    || r.frame_by_cdp_id(tab, &d.frame).is_some();
                ours.then(|| ev.take_download(guid)).flatten()
            })
            .ok_or_else(|| failed(format!("no stopped download {shown} on tab {}", tab.0)))?;
        if !allow {
            // Denied at the browser already; cancel in case it is still listed.
            drop(
                self.cdp
                    .start(None, "Browser.cancelDownload", json!({ "guid": guid })),
            );
            return Ok(None);
        }
        let dir = d
            .files_dir
            .clone()
            .ok_or_else(|| failed("it was not reported by an act step, so it has no run folder"))?;
        tokio::fs::create_dir_all(&dir).await.map_err(failed)?;
        // The name on disk is the one the act result showed: scrubbed (the process-wide
        // vault, H-2) and sanitised, so the returned path is safe to show as it is.
        let name = sanitize_name(&harness_core::scrub(&raw_name(&d.name, &d.url)));
        let part = dir.join(format!(".{shown}.part"));
        if let Err(e) = self.fetch_into(tab, &session, &d, &part).await {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
        // The name is claimed (an empty file of ours), then replaced by the finished file.
        let path = match claim_path(&dir, &name) {
            Ok(p) => p,
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(failed(e));
            }
        };
        if let Err(e) = tokio::fs::rename(&part, &path).await {
            let _ = tokio::fs::remove_file(&part).await;
            let _ = tokio::fs::remove_file(&path).await;
            return Err(failed(e));
        }
        Ok(Some(path))
    }

    /// The download's bytes into `part`.
    async fn fetch_into(
        &self,
        tab: TabId,
        tab_session: &str,
        d: &Stopped,
        part: &Path,
    ) -> Result<(), DriverError> {
        let mut file = tokio::fs::File::create(part).await.map_err(failed)?;
        let scheme = d
            .url
            .split(':')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if scheme == "data" {
            let (meta, data) = d
                .url
                .split_once(',')
                .ok_or_else(|| failed("bad data: URL"))?;
            let bytes = if meta.to_ascii_lowercase().ends_with(";base64") {
                base64(data).ok_or_else(|| failed("bad base64 in the data: URL"))?
            } else {
                percent_encoding::percent_decode_str(data).collect()
            };
            if bytes.len() as u64 > DOWNLOAD_CAP {
                return Err(failed("larger than 1 GiB"));
            }
            file.write_all(&bytes).await.map_err(failed)?;
            return file.flush().await.map_err(failed);
        }
        // No `file`: a page never needs it, and it would copy a local file into the run.
        if !matches!(scheme.as_str(), "http" | "https") {
            return Err(failed(format!(
                "a {} download cannot be fetched again once stopped (B-608)",
                crate::diff::page_text(&scheme, 20)
            )));
        }
        let (session, frame) = self.with_registry(|r| {
            let s = d
                .session
                .clone()
                .filter(|s| r.tab_of_session(s) == Some(tab))
                .unwrap_or_else(|| tab_session.to_string());
            let f = if r.frame_by_cdp_id(tab, &d.frame).is_some() || d.session.is_some() {
                d.frame.clone()
            } else {
                r.cdp_frame_id(tab, 0).unwrap_or_default().to_string()
            };
            (s, f)
        });
        let res = self
            .cdp
            .call(
                Some(&session),
                "Network.loadNetworkResource",
                json!({"frameId": frame, "url": d.url,
                       "options": {"disableCache": false, "includeCredentials": true}}),
            )
            .await?;
        let res = res.get("resource").cloned().unwrap_or(Value::Null);
        if res.get("success").and_then(Value::as_bool) != Some(true) {
            let why = res
                .get("netErrorName")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| res.get("httpStatusCode").map(|c| format!("HTTP {c}")))
                .unwrap_or_else(|| "failed".into());
            return Err(failed(format!(
                "fetching it again: {}",
                crate::diff::page_text(&why, 60)
            )));
        }
        // A one-time link or a POST-only export answers the second request with an error
        // page: never save that under the download's name.
        if let Some(code) = res.get("httpStatusCode").and_then(Value::as_f64) {
            if !(200.0..300.0).contains(&code) {
                if let Some(h) = res.get("stream").and_then(Value::as_str) {
                    drop(
                        self.cdp
                            .start(Some(&session), "IO.close", json!({ "handle": h })),
                    );
                }
                return Err(failed(format!("fetching it again: HTTP {code}, not saved")));
            }
        }
        let handle = res
            .get("stream")
            .and_then(Value::as_str)
            .ok_or_else(|| failed("no stream"))?
            .to_string();
        let mut total = 0u64;
        let r = async {
            loop {
                let chunk = self
                    .cdp
                    .call(
                        Some(&session),
                        "IO.read",
                        json!({"handle": handle, "size": READ_CHUNK}),
                    )
                    .await?;
                let data = chunk
                    .get("data")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let bytes = if chunk.get("base64Encoded").and_then(Value::as_bool) == Some(true) {
                    base64(data).ok_or_else(|| failed("bad base64 from IO.read"))?
                } else {
                    data.as_bytes().to_vec()
                };
                total += bytes.len() as u64;
                if total > DOWNLOAD_CAP {
                    return Err(failed("larger than 1 GiB"));
                }
                file.write_all(&bytes).await.map_err(failed)?;
                if chunk.get("eof").and_then(Value::as_bool) != Some(false) {
                    break;
                }
            }
            file.flush().await.map_err(failed)
        }
        .await;
        drop(
            self.cdp
                .start(Some(&session), "IO.close", json!({ "handle": handle })),
        );
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn main_frame_navigations_dialogs_and_downloads_are_logged_once() {
        let mut e = Events::default();
        let m = e.mark();
        e.apply(
            Some("S"),
            "Page.frameNavigated",
            &json!({"frame": {"id": "T", "url": "http://a/x"}}),
        );
        e.apply(
            Some("S"),
            "Page.frameNavigated",
            &json!({"frame": {"id": "F", "parentId": "T", "url": "http://a/in"}}),
        );
        e.apply(
            Some("S"),
            "Page.frameStartedNavigating",
            &json!({"frameId": "T", "navigationType": "differentDocument", "url": "http://a/y"}),
        );
        e.apply(
            Some("S"),
            "Page.navigatedWithinDocument",
            &json!({"frameId": "F", "url": "http://a/in#2"}),
        );
        e.apply(
            Some("S"),
            "Page.javascriptDialogOpening",
            &json!({"message": "hi", "type": "alert"}),
        );
        let dl = json!({"guid": "g1", "url": "http://a/f", "suggestedFilename": "f.txt", "frameId": "T"});
        e.apply(Some("S"), "Page.downloadWillBegin", &dl);
        e.apply(None, "Browser.downloadWillBegin", &dl);
        let got: Vec<(bool, Kind)> = e
            .since(m)
            .map(|l| (is_main(l, "S", "T"), l.kind.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    true,
                    Kind::Navigated {
                        url: "http://a/x".into()
                    }
                ),
                (true, Kind::NavStarted),
                (
                    false,
                    Kind::Navigated {
                        url: "http://a/in#2".into()
                    }
                ),
                (true, Kind::Dialog { text: "hi".into() }),
                (true, Kind::Download { guid: "g1".into() }),
            ]
        );
        assert_eq!(e.dialog("S").map(|d| d.text.as_str()), Some("hi"));
        e.apply(
            Some("S"),
            "Page.javascriptDialogClosed",
            &json!({"result": true}),
        );
        assert!(e.dialog("S").is_none());
        let d = e.take_download("g1").unwrap();
        assert_eq!(
            (d.name.as_str(), d.session.as_deref()),
            ("f.txt", Some("S"))
        );
        assert!(e.take_download("g1").is_none());
    }

    #[test]
    fn download_names_are_safe_and_never_collide() {
        assert_eq!(safe_name("re/port:1.txt", "http://x/a"), "re_port_1.txt");
        assert_eq!(
            safe_name("", "http://x/dir/My%20File.pdf?x=1"),
            "My File.pdf"
        );
        assert_eq!(safe_name("..", "http://x/"), "download");
        assert_eq!(safe_name("CON.txt", ""), "_CON.txt");
        assert_eq!(safe_name(r"..\..\evil.exe", ""), "_.._evil.exe");
        // Format characters go: `report<RLO>fdp.exe` would show as `reportexe.pdf`.
        assert_eq!(
            safe_name("report\u{202e}fdp.exe", ""),
            "reportfdp.exe",
            "bidi override"
        );
        assert_eq!(
            safe_name("a\u{200b}b\u{feff}\u{2066}c.txt", ""),
            "abc.txt",
            "zero-width, BOM, isolate"
        );
        for dev in [
            "COM\u{b9}.txt",
            "lpt\u{b3}",
            "CONIN$.log",
            "conout$",
            "COM0.txt",
            "Nul .txt",
        ] {
            assert!(safe_name(dev, "").starts_with('_'), "{dev}");
        }
        for ok in ["COM10.txt", "CONSOLE.txt", "LPT.txt", "COM\u{b9}x.txt"] {
            assert!(!safe_name(ok, "").starts_with('_'), "{ok}");
        }
        // The saved name is the shown name: harness syntax neutralised, spaces collapsed.
        assert_eq!(safe_name("[x](#12) ```  n.txt", ""), "(x)(＃12) ''' n.txt");
        assert_eq!(
            crate::diff::page_text(&safe_name("[x](#12) ```  n.txt", ""), 120),
            safe_name("[x](#12) ```  n.txt", "")
        );
        let long = format!("{}.pdf", "a".repeat(300));
        let n = safe_name(&long, "");
        assert_eq!(n.chars().count(), NAME_CAP);
        assert!(n.ends_with(".pdf"));
        let dir = tempfile::tempdir().unwrap();
        let a = claim_path(dir.path(), "r.txt").unwrap();
        let b = claim_path(dir.path(), "r.txt").unwrap();
        assert_eq!(a, dir.path().join("r.txt"));
        assert_eq!(b, dir.path().join("r (2).txt"));
    }

    #[test]
    fn base64_decodes_standard_and_url_safe() {
        assert_eq!(base64("aGVsbG8gZmlsZQ==").unwrap(), b"hello file");
        assert_eq!(base64("aGVsbG8gZmlsZQ").unwrap(), b"hello file");
        assert_eq!(base64("-_8=").unwrap(), base64("+/8=").unwrap());
        assert!(base64("a$b").is_none());
    }

    #[test]
    fn the_log_and_the_download_list_are_bounded() {
        let mut e = Events::default();
        for i in 0..(LOG_CAP + 10) {
            e.apply(
                Some("S"),
                "Page.downloadWillBegin",
                &json!({"guid": format!("g{i}"), "url": "u", "frameId": "T"}),
            );
        }
        assert_eq!(e.log.len(), LOG_CAP);
        assert_eq!(e.downloads.len(), DOWNLOADS_CAP);
        assert!(e.download("g0").is_none(), "oldest forgotten first");
        let long = "x".repeat(RAW_CAP * 2);
        e.apply(
            Some("S"),
            "Page.javascriptDialogOpening",
            &json!({ "message": long }),
        );
        assert_eq!(e.dialog("S").unwrap().text.chars().count(), RAW_CAP);
    }
}
