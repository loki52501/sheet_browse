//! I-5 `IndexRow` and its parts (docs/05 B-5). CONTRACT: change only through `interface_changes`.
//!
//! The serde JSON of these types is the wire format: `injected.js` `__sb.index()` returns
//! `IndexRow[]` in exactly this shape, and `srv-web` builds the same rows from fetched HTML.
//! Field names are snake_case; `bbox` is `"box"` on the wire (D-37).

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

/// Frame number within a tab. `0` is the main frame.
pub type FrameId = u32;

/// An element reference as the model writes it: `#12` in the main frame, `#f2.15` in frame 2
/// (B-4 #4, D-37). Serialized as that string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElemRef {
    pub frame: FrameId,
    pub id: u32,
}

impl ElemRef {
    pub fn main(id: u32) -> ElemRef {
        ElemRef { frame: 0, id }
    }
}

impl fmt::Display for ElemRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.frame == 0 {
            write!(f, "#{}", self.id)
        } else {
            write!(f, "#f{}.{}", self.frame, self.id)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("bad element reference {0:?}: expected #12 or #f2.15")]
pub struct BadElemRef(pub String);

impl FromStr for ElemRef {
    type Err = BadElemRef;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || BadElemRef(s.to_string());
        let rest = s.strip_prefix('#').ok_or_else(bad)?;
        match rest.strip_prefix('f') {
            Some(fr) => {
                let (frame, id) = fr.split_once('.').ok_or_else(bad)?;
                Ok(ElemRef {
                    frame: frame.parse().map_err(|_| bad())?,
                    id: id.parse().map_err(|_| bad())?,
                })
            }
            None => Ok(ElemRef::main(rest.parse().map_err(|_| bad())?)),
        }
    }
}

impl Serialize for ElemRef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ElemRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// CSS pixels in the frame's viewport coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// A field's value as the index may carry it. Passwords are only ever `empty` or `filled` (H-2, B-5).
/// Wire: `{"kind":"empty"}`, `{"kind":"filled"}`, `{"kind":"text","text":"..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "lowercase")]
pub enum FieldValue {
    Empty,
    Filled,
    Text(String),
}

/// Element state (B-5 state).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub visible: bool,
    pub enabled: bool,
    /// Checkbox / radio / switch / aria-checked; `None` when not checkable.
    pub checked: Option<bool>,
    pub value: Option<FieldValue>,
    /// Submits a form (submit button, Enter target).
    pub submitish: bool,
    /// `target=_blank` link, `window.open` handler.
    pub opens_new_tab: bool,
    /// `<select>` option labels, capped by the producer.
    pub options: Vec<String>,
    /// The element's centre hit-tests to something else (B-6.2 "covered": -2). Set by the
    /// live walk; always false for fetched HTML.
    pub covered: bool,
    /// `<input type>` lowercased (`password`, `email`, `tel`, `file`...). B-7 classification
    /// (Account from password fields, Pay from card fields) and the password rule need it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    /// `autocomplete` token (`cc-number`, `new-password`, `email`...), Pay/Account evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autocomplete: Option<String>,
}

impl Default for State {
    fn default() -> State {
        State {
            visible: true,
            enabled: true,
            checked: None,
            value: None,
            submitish: false,
            opens_new_tab: false,
            options: Vec::new(),
            covered: false,
            input_type: None,
            autocomplete: None,
        }
    }
}

/// Row flags (B-5 flags). Wire: an object of booleans, absent = false.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Flags {
    /// SUBMIT: the control submits its form.
    pub submit: bool,
    /// CHALLENGE: recaptcha / hcaptcha / turnstile frame or container (D-5: means stop).
    pub challenge: bool,
    /// OVERLAY: fixed or sticky and covering content.
    pub overlay: bool,
    /// OPAQUE: cross-origin frame, one row, never read inside.
    pub opaque: bool,
}

/// I-5. One element of the page index: `IndexRow { id, frame, role, name, nearby, heading, state, flags, form, box }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexRow {
    /// Stable per element within its frame (B-4 #4).
    pub id: u32,
    pub frame: FrameId,
    /// ARIA role, else tag-derived role, else behaviour (`div` + click + pointer → `button`).
    /// Lowercase ARIA vocabulary (`textbox`, `button`, `checkbox`, `combobox`, `link`, `iframe`...).
    /// Sheet lines, fingerprints and `name:role` queries use short aliases (`input`) that map to
    /// these through sb-sheet's data/roles.txt (D-49).
    pub role: String,
    /// Accessible name from the B-5 name ladder. Empty when none.
    pub name: String,
    /// Text near the element (≤ 200 px above/left) not already consumed as its name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nearby: Option<String>,
    /// Nearest enclosing section heading text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    #[serde(default)]
    pub state: State,
    #[serde(default)]
    pub flags: Flags,
    /// Owning form's id as used in sheets and fingerprints (`f2`), if inside a form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<String>,
    /// Enclosing landmark, same vocabulary as `TextBlock.landmark` (`main nav header footer
    /// aside form region dialog`), so `sheet(region=landmark)` can select rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landmark: Option<String>,
    /// For an iframe row: the frame it hosts (filled by sb-driver when it merges frames).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_frame: Option<FrameId>,
    /// Layout box. `None` for rows built from fetched HTML (srv-web has no layout).
    #[serde(rename = "box", default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<Rect>,
}

impl IndexRow {
    pub fn elem(&self) -> ElemRef {
        ElemRef {
            frame: self.frame,
            id: self.id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elem_ref_round_trips() {
        for (s, r) in [
            ("#12", ElemRef::main(12)),
            ("#f2.15", ElemRef { frame: 2, id: 15 }),
        ] {
            assert_eq!(s.parse::<ElemRef>().unwrap(), r);
            assert_eq!(r.to_string(), s);
            assert_eq!(serde_json::to_string(&r).unwrap(), format!("\"{s}\""));
        }
        for bad in ["12", "#", "#f2", "#fx.1", "#-1", "#f2.", "f2.15"] {
            assert!(bad.parse::<ElemRef>().is_err(), "{bad}");
        }
    }

    /// The wire shape `injected.js` must produce (I-5 / I-11). If this test changes, the JS changes too.
    #[test]
    fn index_row_wire_shape() {
        let json = r#"[
          {"id":7,"frame":0,"role":"textbox","name":"Email","heading":"Create your account",
           "state":{"visible":true,"enabled":true,"value":{"kind":"empty"}},
           "form":"f1","box":{"x":40,"y":210,"w":320,"h":36}},
          {"id":9,"frame":0,"role":"textbox","name":"Password",
           "state":{"value":{"kind":"filled"},"input_type":"password","autocomplete":"new-password"},"form":"f1"},
          {"id":11,"frame":0,"role":"textbox","name":"Card number",
           "state":{"input_type":"text","autocomplete":"cc-number","covered":true},"form":"f3","landmark":"main"},
          {"id":14,"frame":0,"role":"button","name":"Register",
           "state":{"submitish":true},"flags":{"submit":true},"form":"f1"},
          {"id":3,"frame":0,"role":"iframe","name":"reCAPTCHA","flags":{"challenge":true,"opaque":true},"child_frame":4}
        ]"#;
        let rows: Vec<IndexRow> = serde_json::from_str(json).unwrap();
        assert_eq!(rows[0].elem().to_string(), "#7");
        assert_eq!(rows[0].state.value, Some(FieldValue::Empty));
        assert!(rows[1].state.visible && rows[1].bbox.is_none());
        assert_eq!(rows[1].state.value, Some(FieldValue::Filled));
        assert_eq!(rows[1].state.input_type.as_deref(), Some("password"));
        assert!(rows[2].state.covered && rows[2].landmark.as_deref() == Some("main"));
        assert_eq!(rows[2].state.autocomplete.as_deref(), Some("cc-number"));
        assert!(rows[3].flags.submit && !rows[3].flags.overlay && !rows[3].state.covered);
        assert!(rows[4].flags.challenge && rows[4].flags.opaque);
        assert_eq!(rows[4].child_frame, Some(4));

        let back = serde_json::to_value(&rows[0]).unwrap();
        assert_eq!(back["box"]["w"], 320.0);
        assert!(back.get("bbox").is_none() && back.get("nearby").is_none());
    }
}
