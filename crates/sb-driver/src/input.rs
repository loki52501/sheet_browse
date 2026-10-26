//! B-4 #7 input synthesis. Every step's commands are queued at once (pipelined on the tab's
//! session) and their replies awaited together, so a step costs one round trip after its
//! actionability answer:
//!
//! * click and check: `Input.dispatchMouseEvent` move, press, release at the act point;
//! * type: the element was focused in the act call itself; then Ctrl+A (Cmd+A on macOS)
//!   with the `selectAll` command and `Input.insertText` (an empty value sends Delete);
//!   `@vault(k)` is resolved only inside `ToolCtx::resolve_into`, straight into the bytes of
//!   the insertText message (H-2, D-55), which `sb_cdp` zeroes once written;
//! * select: in the act call (`op.select`), so no input here;
//! * key: `Input.dispatchKeyEvent` for the chord (modifiers down, key down/up, modifiers up);
//! * upload: `DOM.setFileInputFiles` with a path inside the run's `files/` folder only;
//! * scroll: in the page (`sc(down|up)` scrolls the scroller under the viewport centre by
//!   85 % of its height; `sc(#N)` scrolls the element into the centre).

use crate::actionable::Point;
use crate::{Driver, ToolCtx};
use harness_core::vault::vault_refs;
use sb_cdp::CdpError;
use serde_json::{json, Value};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use zeroize::Zeroizing;

/// A step's queued input commands; resolves when every reply is in. `Err` is the step's
/// failure reason (a CDP error names the method, never its params).
pub(crate) type InputFut = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// Nothing to wait for (the step's effect happened inside its act call).
pub(crate) fn ready() -> InputFut {
    Box::pin(async { Ok(()) })
}

/// Await already-queued commands in order (they are on the wire already: no extra round
/// trip). The first error is the step's reason.
pub(crate) fn join<F>(futs: Vec<F>) -> InputFut
where
    F: Future<Output = Result<Value, CdpError>> + Send + 'static,
{
    Box::pin(async move {
        for f in futs {
            f.await.map_err(|e| e.to_string())?;
        }
        Ok(())
    })
}

/// Move, press, release at `p` (B-4 #7 click; check is a click).
pub(crate) fn click_events(p: Point) -> Vec<Value> {
    vec![
        json!({"type": "mouseMoved", "x": p.x, "y": p.y, "button": "none", "buttons": 0}),
        json!({"type": "mousePressed", "x": p.x, "y": p.y, "button": "left", "buttons": 1, "clickCount": 1}),
        json!({"type": "mouseReleased", "x": p.x, "y": p.y, "button": "left", "buttons": 0, "clickCount": 1}),
    ]
}

/// The platform's select-all chord: Cmd on macOS, Ctrl elsewhere (`Input` modifier bits:
/// Alt 1, Ctrl 2, Meta 4, Shift 8). `commands` makes it select all whatever the platform's
/// key bindings.
pub(crate) fn select_all_events() -> Vec<Value> {
    let m = if cfg!(target_os = "macos") { 4 } else { 2 };
    let base = json!({"modifiers": m, "key": "a", "code": "KeyA",
        "windowsVirtualKeyCode": 65, "nativeVirtualKeyCode": 65});
    let mut down = base.clone();
    down["type"] = json!("rawKeyDown");
    down["commands"] = json!(["selectAll"]);
    let mut up = base;
    up["type"] = json!("keyUp");
    vec![down, up]
}

/// One key: (key, code, Windows key code, text it types).
type KeyDef = (String, String, u32, Option<String>);

fn modifier(name: &str) -> Option<(u32, &'static str, &'static str, u32)> {
    match name {
        "alt" | "option" => Some((1, "Alt", "AltLeft", 18)),
        "control" | "ctrl" => Some((2, "Control", "ControlLeft", 17)),
        "meta" | "cmd" | "command" | "super" | "win" => Some((4, "Meta", "MetaLeft", 91)),
        "shift" => Some((8, "Shift", "ShiftLeft", 16)),
        _ => None,
    }
}

fn named_key(name: &str) -> Option<KeyDef> {
    let k = |key: &str, code: &str, kc: u32, text: Option<&str>| {
        Some((key.into(), code.into(), kc, text.map(str::to_string)))
    };
    match name {
        "enter" | "return" => k("Enter", "Enter", 13, Some("\r")),
        "numpadenter" => k("Enter", "NumpadEnter", 13, Some("\r")),
        "tab" => k("Tab", "Tab", 9, None),
        "escape" | "esc" => k("Escape", "Escape", 27, None),
        "backspace" => k("Backspace", "Backspace", 8, None),
        "delete" | "del" => k("Delete", "Delete", 46, None),
        "space" | "spacebar" => k(" ", "Space", 32, Some(" ")),
        "arrowup" | "up" => k("ArrowUp", "ArrowUp", 38, None),
        "arrowdown" | "down" => k("ArrowDown", "ArrowDown", 40, None),
        "arrowleft" | "left" => k("ArrowLeft", "ArrowLeft", 37, None),
        "arrowright" | "right" => k("ArrowRight", "ArrowRight", 39, None),
        "home" => k("Home", "Home", 36, None),
        "end" => k("End", "End", 35, None),
        "pageup" => k("PageUp", "PageUp", 33, None),
        "pagedown" => k("PageDown", "PageDown", 34, None),
        "insert" => k("Insert", "Insert", 45, None),
        f if f.len() >= 2 && f.starts_with('f') => {
            let n: u32 = f[1..].parse().ok()?;
            (1..=12).contains(&n).then(|| {
                let key = format!("F{n}");
                (key.clone(), key, 111 + n, None)
            })
        }
        _ => None,
    }
}

/// The `Input.dispatchKeyEvent` params for a `k#N=` value: `Enter`, `Shift+Tab`,
/// `Control+a`, one printable character. Unknown names and control characters are refused
/// before anything is sent (fail closed).
pub(crate) fn key_events(value: &str) -> Result<Vec<Value>, String> {
    let unknown = || {
        let shown: String = value
            .chars()
            .take(30)
            .map(|c| if c.is_control() { '?' } else { c })
            .collect();
        format!("unknown key '{shown}'")
    };
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(unknown());
    }
    // Split like sb_act's classifier: the last `+`-separated segment is the key, a trailing
    // `+` is the plus key itself.
    let (mods, last) = match value.strip_suffix('+') {
        Some("") => ("", "+"),
        Some(rest) if rest.ends_with('+') => (rest.trim_end_matches('+'), "+"),
        _ => value.rsplit_once('+').unwrap_or(("", value)),
    };
    let mut bits = 0u32;
    let mut downs = Vec::new();
    for m in mods.split('+').map(|m| m.trim().to_lowercase()) {
        if m.is_empty() {
            continue;
        }
        let (bit, key, code, kc) = modifier(&m).ok_or_else(unknown)?;
        if bits & bit == 0 {
            bits |= bit;
            downs.push((key, code, kc, bits));
        }
    }
    let lname = if !last.is_empty() && last.chars().all(|c| c == ' ') {
        "space".to_string()
    } else {
        last.trim().to_lowercase()
    };
    let main: Option<KeyDef> = if let Some(m) = modifier(&lname) {
        // A modifier alone (`Shift`): pressed and released.
        let (bit, key, code, kc) = m;
        if bits & bit == 0 {
            bits |= bit;
            downs.push((key, code, kc, bits));
        }
        None
    } else if let Some(d) = named_key(&lname) {
        Some(d)
    } else {
        let mut cs = last.chars();
        match (cs.next(), cs.next()) {
            (Some(c), None) => {
                let shifted = bits & 8 != 0;
                let c = if shifted { c.to_ascii_uppercase() } else { c };
                let up = c.to_ascii_uppercase();
                let (code, kc) = if up.is_ascii_alphabetic() {
                    (format!("Key{up}"), up as u32)
                } else if c.is_ascii_digit() {
                    (format!("Digit{c}"), c as u32)
                } else {
                    (String::new(), 0)
                };
                Some((c.to_string(), code, kc, Some(c.to_string())))
            }
            _ => return Err(unknown()),
        }
    };
    let mut out = Vec::new();
    for (key, code, kc, m) in &downs {
        out.push(
            json!({"type": "rawKeyDown", "modifiers": m, "key": key, "code": code,
            "windowsVirtualKeyCode": kc, "nativeVirtualKeyCode": kc}),
        );
    }
    if let Some((key, code, kc, text)) = main {
        // Ctrl, Alt or Meta held: the key is a command, it types nothing.
        let text = text.filter(|_| bits & (1 | 2 | 4) == 0);
        let mut down = json!({"type": if text.is_some() { "keyDown" } else { "rawKeyDown" },
            "modifiers": bits, "key": key, "code": code,
            "windowsVirtualKeyCode": kc, "nativeVirtualKeyCode": kc});
        if let Some(t) = &text {
            down["text"] = json!(t);
            down["unmodifiedText"] = json!(t);
        }
        out.push(down);
        out.push(
            json!({"type": "keyUp", "modifiers": bits, "key": key, "code": code,
            "windowsVirtualKeyCode": kc, "nativeVirtualKeyCode": kc}),
        );
    }
    for (key, code, kc, _) in downs.iter().rev() {
        let after = bits & !modifier_bit(key);
        bits = after;
        out.push(
            json!({"type": "keyUp", "modifiers": after, "key": key, "code": code,
            "windowsVirtualKeyCode": kc, "nativeVirtualKeyCode": kc}),
        );
    }
    Ok(out)
}

fn modifier_bit(key: &str) -> u32 {
    match key {
        "Alt" => 1,
        "Control" => 2,
        "Meta" => 4,
        "Shift" => 8,
        _ => 0,
    }
}

/// Why an upload path is refused; the path itself is not echoed.
pub(crate) const UPLOAD_REFUSED: &str =
    "upload refused: the path must name a file inside the run's files folder (no '..', no absolute path)";

/// `u#N=<path>`: the path, relative to the run's `files/` folder (a leading `files/` is the
/// folder itself, as in B-7's `u#9=files/cv.pdf`), as an existing file that resolves inside
/// that folder after symlinks. Anything else is refused (fail closed).
pub(crate) fn upload_path(files_dir: &Path, value: &str) -> Result<PathBuf, String> {
    let refuse = || UPLOAD_REFUSED.to_string();
    let v = value.trim();
    if v.is_empty() || v.contains('\0') {
        return Err(refuse());
    }
    let rel = Path::new(v);
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(p) => parts.push(p),
            Component::CurDir => {}
            // `..`, a root, a drive or UNC prefix: never.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(refuse())
            }
        }
    }
    if parts.first().is_some_and(|p| *p == "files") && parts.len() > 1 {
        parts.remove(0);
    }
    if parts.is_empty() {
        return Err(refuse());
    }
    let mut joined = files_dir.to_path_buf();
    for p in parts {
        joined.push(p);
    }
    let root = std::fs::canonicalize(files_dir).map_err(|_| refuse())?;
    let real = std::fs::canonicalize(&joined).map_err(|_| refuse())?;
    if !real.starts_with(&root) || !real.is_file() {
        return Err(refuse());
    }
    Ok(real)
}

/// `sc(down|up)` in frame 0 (sb world): the nearest scrollable ancestor of what is under the
/// viewport centre, else the document scroller, by 85 % of its height. Answers whether the
/// scroll position moved.
pub(crate) const SCROLL_FN: &str = "function (dir) {
  const vw = innerWidth;
  const vh = innerHeight;
  const root = document.scrollingElement || document.documentElement;
  const scrollable = (el) => {
    const cs = getComputedStyle(el);
    return /(auto|scroll|overlay)/.test(cs.overflowY) && el.scrollHeight > el.clientHeight + 1;
  };
  let el = document.elementFromPoint(vw / 2, vh / 2);
  while (el && el !== document.body && el !== document.documentElement && !scrollable(el)) {
    el = el.parentElement || (el.getRootNode && el.getRootNode().host) || null;
  }
  const t = el && el !== document.body && el !== document.documentElement ? el : root;
  const h = t === root ? vh : t.clientHeight;
  const before = t.scrollTop;
  t.scrollBy({ top: dir * Math.max(40, Math.round(h * 0.85)), left: 0, behavior: 'instant' });
  return { moved: t.scrollTop !== before };
}";

/// `sc(#N)`: the element into the centre of the viewport.
pub(crate) const SCROLL_TO_FN: &str = "function (id) {
  const sb = globalThis.__sb;
  if (!sb) return { why: 'no runtime' };
  const el = sb.resolve(id);
  if (!el || el.why) return el || { why: 'gone' };
  const r0 = el.getBoundingClientRect();
  el.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' });
  const r1 = el.getBoundingClientRect();
  return { moved: r0.top !== r1.top || r0.left !== r1.left };
}";

/// `u#N`: the file input itself (by reference), or a why string. A file input is usually
/// hidden behind a styled label, so it gets no actionability check (D-609).
pub(crate) const UPLOAD_FN: &str = "function (id) {
  const sb = globalThis.__sb;
  if (!sb) return 'no runtime';
  const el = sb.resolve(id);
  if (!el || el.why) return el ? el.why : 'gone';
  if (el.localName !== 'input' || String(el.type).toLowerCase() !== 'file') return 'not a file input';
  if (el.disabled) return 'disabled';
  return el;
}";

impl Driver {
    /// Queue every event of one `Input.*` method on `session` now; one future for them all.
    pub(crate) fn dispatch(&self, session: &str, method: &str, events: Vec<Value>) -> InputFut {
        let futs: Vec<_> = events
            .into_iter()
            .map(|e| self.cdp.start(Some(session), method, e))
            .collect();
        join(futs)
    }

    /// The type step's commands after focus: select all, then the text. A value with
    /// `@vault(k)` is resolved only inside `ctx.resolve_into`, and the insertText message is
    /// built and queued inside its sink, from a buffer of exact size that is zeroed when the
    /// sink ends (the transport zeroes its own copy). `Err` before anything is queued when a
    /// vault reference is malformed or unknown.
    pub(crate) fn type_text(
        &self,
        session: &str,
        text: &str,
        ctx: &ToolCtx,
    ) -> Result<InputFut, String> {
        type Reply = Pin<Box<dyn Future<Output = Result<Value, CdpError>> + Send>>;
        let refs = vault_refs(text).map_err(|_| "malformed @vault(..) reference in the value")?;
        let key = |e: Value| -> Reply {
            Box::pin(self.cdp.start(Some(session), "Input.dispatchKeyEvent", e))
        };
        if refs.is_empty() {
            let mut futs: Vec<Reply> = select_all_events().into_iter().map(key).collect();
            if text.is_empty() {
                futs.extend(
                    key_events("Delete")
                        .unwrap_or_default()
                        .into_iter()
                        .map(key),
                );
            } else {
                futs.push(Box::pin(self.cdp.start(
                    Some(session),
                    "Input.insertText",
                    json!({ "text": text }),
                )));
            }
            return Ok(join(futs));
        }
        // Everything is queued inside the sink, in order, so the value is resolved once and
        // nothing is sent when a key is unknown.
        let cdp = &self.cdp;
        let futs = ctx
            .resolve_into(text, |r| {
                let v = r.expose();
                // JSON escapes a byte to at most 6 bytes: the buffer never reallocates.
                let cap = v.len() * 6 + 16;
                let mut buf = Zeroizing::new(Vec::with_capacity(cap));
                buf.extend_from_slice(b"{\"text\":");
                serde_json::to_writer(&mut *buf, v).ok()?;
                buf.push(b'}');
                let mut futs: Vec<Reply> = select_all_events().into_iter().map(key).collect();
                futs.push(Box::pin(cdp.start_raw(
                    Some(session),
                    "Input.insertText",
                    &buf,
                )));
                Some(futs)
            })
            .map_err(|e| vault_error(&e))?;
        futs.map(join)
            .ok_or_else(|| "could not build the insertText message".to_string())
    }
}

/// A vault error as a step reason: the entry name or the byte offset, never a value.
fn vault_error(e: &harness_core::vault::VaultError) -> String {
    format!("vault: {e}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(v: &[Value]) -> Vec<String> {
        v.iter()
            .map(|e| {
                format!(
                    "{} {}",
                    e["type"].as_str().unwrap(),
                    e["key"].as_str().unwrap()
                )
            })
            .collect()
    }

    #[test]
    fn enter_types_a_carriage_return() {
        let e = key_events("Enter").unwrap();
        assert_eq!(kinds(&e), ["keyDown Enter", "keyUp Enter"]);
        assert_eq!(e[0]["text"], "\r");
        assert_eq!(e[0]["windowsVirtualKeyCode"], 13);
    }

    #[test]
    fn chords_press_modifiers_around_the_key() {
        let e = key_events("Control+Shift+a").unwrap();
        assert_eq!(
            kinds(&e),
            [
                "rawKeyDown Control",
                "rawKeyDown Shift",
                "rawKeyDown A",
                "keyUp A",
                "keyUp Shift",
                "keyUp Control"
            ]
        );
        assert_eq!(e[2]["modifiers"], 10);
        assert!(e[2].get("text").is_none(), "a Ctrl chord types nothing");
        assert_eq!(e[4]["modifiers"], 2);
        assert_eq!(e[5]["modifiers"], 0);
        let tab = key_events("Shift+Tab").unwrap();
        assert_eq!(
            kinds(&tab),
            [
                "rawKeyDown Shift",
                "rawKeyDown Tab",
                "keyUp Tab",
                "keyUp Shift"
            ]
        );
        let plus = key_events("Control++").unwrap();
        assert_eq!(kinds(&plus)[1], "rawKeyDown +");
        let x = key_events("x").unwrap();
        assert_eq!(x[0]["text"], "x");
        assert_eq!(x[0]["code"], "KeyX");
        assert_eq!(
            kinds(&key_events("F5").unwrap()),
            ["rawKeyDown F5", "keyUp F5"]
        );
    }

    #[test]
    fn unknown_keys_and_control_characters_are_refused() {
        for k in ["", "Hyper+x", "Enterprise", "\r", "a\tb", "F13", "Ctrl+Foo"] {
            assert!(key_events(k).is_err(), "{k:?}");
        }
    }

    #[test]
    fn select_all_selects_with_the_platform_modifier() {
        let e = select_all_events();
        assert_eq!(e[0]["commands"], json!(["selectAll"]));
        let m = if cfg!(target_os = "macos") { 4 } else { 2 };
        assert_eq!(e[0]["modifiers"], m);
        assert_eq!(e[1]["type"], "keyUp");
    }

    #[test]
    fn upload_paths_stay_inside_the_files_folder() {
        let run = tempfile::tempdir().unwrap();
        let files = run.path().join("files");
        std::fs::create_dir_all(files.join("sub")).unwrap();
        std::fs::write(files.join("cv.pdf"), b"%PDF").unwrap();
        std::fs::write(files.join("sub").join("a.txt"), b"a").unwrap();
        std::fs::write(run.path().join("secret.txt"), b"s").unwrap();
        let root = std::fs::canonicalize(&files).unwrap();
        assert_eq!(upload_path(&files, "cv.pdf").unwrap(), root.join("cv.pdf"));
        assert_eq!(
            upload_path(&files, "files/cv.pdf").unwrap(),
            root.join("cv.pdf")
        );
        assert_eq!(
            upload_path(&files, "./sub/a.txt").unwrap(),
            root.join("sub").join("a.txt")
        );
        let abs = run.path().join("secret.txt");
        for bad in [
            "../secret.txt",
            "files/../../secret.txt",
            "sub/../../secret.txt",
            abs.to_str().unwrap(),
            "/etc/passwd",
            "C:\\Windows\\win.ini",
            "\\\\server\\share\\x",
            "missing.pdf",
            "sub",
            "",
            "files",
        ] {
            assert_eq!(
                upload_path(&files, bad),
                Err(UPLOAD_REFUSED.to_string()),
                "{bad}"
            );
        }
    }
}
