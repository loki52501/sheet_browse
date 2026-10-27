//! W-3 extension (D-566, B-560): `api(..., pick)` projects a JSON answer onto a few dotted
//! paths before it is pretty-printed and capped, so a 2 MB job board fits in one call; and
//! [`shape`], the line a cut answer ends with, naming the top-level keys and array lengths
//! so the model's next call can pick.
//!
//! A path is keys joined by `.`; `[]` after a key (or alone, for a top-level array) marks an
//! array whose items the rest of the path applies to: `jobs[].title`, `[].name`,
//! `data.items[].id`. A key step met by an array maps over its items anyway, so `jobs.title`
//! means the same as `jobs[].title`. Items that hold none of the picked keys stay in place as
//! `null`, so the arrays of two picks line up.

use crate::escape;
use serde_json::{Map, Value};

/// Paths one `pick` may list.
pub const MAX_PATHS: usize = 40;
/// Characters in one path.
const MAX_PATH_CHARS: usize = 200;
/// Keys a [`shape`] line names per object.
const SHAPE_KEYS: usize = 25;

/// A set of picked paths as a tree of keys.
#[derive(Debug, Default, PartialEq)]
pub struct Pick {
    /// Keep the whole value here (a path ended at this node).
    whole: bool,
    keys: Vec<(String, Pick)>,
}

impl Pick {
    /// Parses the `pick` argument: a list of path strings, or one string. `None` when absent,
    /// null or an empty list.
    pub fn parse(arg: Option<&Value>) -> Result<Option<Pick>, String> {
        let paths: Vec<&str> = match arg {
            None | Some(Value::Null) => return Ok(None),
            Some(Value::String(s)) => vec![s.as_str()],
            Some(Value::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    match it {
                        Value::String(s) => out.push(s.as_str()),
                        _ => return Err("pick must be a list of path strings".into()),
                    }
                }
                out
            }
            Some(_) => return Err("pick must be a list of path strings".into()),
        };
        if paths.is_empty() {
            return Ok(None);
        }
        if paths.len() > MAX_PATHS {
            return Err(format!("pick takes at most {MAX_PATHS} paths"));
        }
        let mut root = Pick::default();
        for p in paths {
            root.insert(&steps(p)?);
        }
        Ok(Some(root))
    }

    fn insert(&mut self, steps: &[String]) {
        if self.whole {
            return;
        }
        let Some((first, rest)) = steps.split_first() else {
            self.whole = true;
            self.keys.clear();
            return;
        };
        match self.keys.iter_mut().find(|(k, _)| k == first) {
            Some((_, child)) => child.insert(rest),
            None => {
                let mut child = Pick::default();
                child.insert(rest);
                self.keys.push((first.clone(), child));
            }
        }
    }

    /// `v` projected onto the picked paths; `None` when nothing picked is in `v`.
    pub fn apply(&self, v: &Value) -> Option<Value> {
        if self.whole {
            return Some(v.clone());
        }
        match v {
            Value::Array(items) => {
                if items.is_empty() {
                    return Some(Value::Array(Vec::new()));
                }
                let out: Vec<Option<Value>> = items.iter().map(|e| self.apply(e)).collect();
                if out.iter().all(Option::is_none) {
                    return None;
                }
                Some(Value::Array(
                    out.into_iter().map(|e| e.unwrap_or(Value::Null)).collect(),
                ))
            }
            Value::Object(m) => {
                let mut o = Map::new();
                for (k, child) in &self.keys {
                    if let Some(x) = m.get(k).and_then(|x| child.apply(x)) {
                        o.insert(k.clone(), x);
                    }
                }
                (!o.is_empty()).then_some(Value::Object(o))
            }
            _ => None,
        }
    }
}

/// The key steps of one path; `[]` marks are dropped (arrays are mapped wherever they are).
fn steps(path: &str) -> Result<Vec<String>, String> {
    let bad = || {
        format!(
            "pick path {:?} is not keys joined by dots with [] for arrays, like jobs[].title",
            escape::name(path, 80)
        )
    };
    let p = path.trim();
    if p.is_empty() || p.chars().count() > MAX_PATH_CHARS {
        return Err(bad());
    }
    let mut out = Vec::new();
    for seg in p.split('.') {
        let mut key = seg.trim();
        let mut marked = false;
        while let Some(k) = key.strip_suffix("[]") {
            key = k.trim_end();
            marked = true;
        }
        if key.contains(['[', ']']) || (key.is_empty() && !marked) {
            return Err(bad());
        }
        if !key.is_empty() {
            out.push(key.to_string());
        }
    }
    Ok(out)
}

/// What kind of value `v` is, with array lengths: `array of 66 objects`, `string`.
fn kind(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(_) => "boolean".into(),
        Value::Number(_) => "number".into(),
        Value::String(_) => "string".into(),
        Value::Object(_) => "object".into(),
        Value::Array(a) => {
            let objects = a.iter().filter(|x| x.is_object()).count();
            if !a.is_empty() && objects == a.len() {
                format!("array of {} objects", a.len())
            } else {
                format!("array of {}", a.len())
            }
        }
    }
}

/// Up to [`SHAPE_KEYS`] keys of `m`, escaped, comma-joined, with `...` when there are more.
fn key_list(m: &Map<String, Value>) -> Vec<String> {
    m.keys()
        .take(SHAPE_KEYS)
        .map(|k| escape::name(k, 60))
        .collect()
}

fn joined(keys: &[String], total: usize) -> String {
    let mut s = keys.join(", ");
    if total > keys.len() {
        s.push_str(&format!(", ... ({total} keys)"));
    }
    s
}

/// The first object item of the largest array of objects among `m`'s values: (key, length,
/// item).
fn largest_array(m: &Map<String, Value>) -> Option<(&str, usize, &Map<String, Value>)> {
    m.iter()
        .filter_map(|(k, v)| {
            let a = v.as_array()?;
            let first = a.iter().find_map(Value::as_object)?;
            Some((k.as_str(), a.len(), first))
        })
        .max_by_key(|(_, n, _)| *n)
}

/// Up to three keys of `item` for a `pick` example: short scalar fields, name-like ones
/// (`title`, `name`, `url`...) first, so the example is a pick worth making.
fn example_keys(item: &Map<String, Value>) -> Vec<String> {
    const LIKELY: [&str; 6] = ["title", "name", "url", "jobUrl", "id", "location"];
    let short = |v: &Value| match v {
        Value::String(t) => t.chars().count() <= 200,
        Value::Number(_) | Value::Bool(_) => true,
        _ => false,
    };
    let mut keys: Vec<&String> = item
        .iter()
        .filter(|(_, v)| short(v))
        .map(|(k, _)| k)
        .collect();
    keys.sort_by_key(|k| LIKELY.iter().position(|l| l == k).unwrap_or(LIKELY.len()));
    keys.into_iter()
        .take(3)
        .map(|k| escape::name(k, 60))
        .collect()
}

/// `; items ... have keys: ...; call again with pick ..., e.g. pick: [...]` for the items of
/// the array at `prefix` (`jobs` or empty for a top-level array).
fn items_note(prefix: &str, item: &Map<String, Value>) -> String {
    let keys = key_list(item);
    let at = if prefix.is_empty() {
        "the items".to_string()
    } else {
        format!("items of {prefix}")
    };
    let mut s = format!("; {at} have keys: {}", joined(&keys, item.len()));
    let ex: Vec<String> = example_keys(item)
        .iter()
        .map(|f| format!("\"{prefix}[].{f}\""))
        .collect();
    s.push_str(PICK_HINT);
    if !ex.is_empty() {
        s.push_str(&format!(", e.g. pick: [{}]", ex.join(", ")));
    }
    s
}

const PICK_HINT: &str = "; call again with pick to get only the fields you need";

/// One line describing `v`'s top level, for the note of a cut answer (D-566): the top-level
/// keys with their kinds and array lengths, the keys of the items of the largest array, and a
/// `pick` example built from them. Empty for a scalar.
pub fn shape(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let tops: Vec<String> = m
                .iter()
                .take(SHAPE_KEYS)
                .map(|(k, x)| format!("{} ({})", escape::name(k, 60), kind(x)))
                .collect();
            let mut s = format!("top-level keys: {}", joined(&tops, m.len()));
            match largest_array(m) {
                Some((k, _, item)) => s.push_str(&items_note(&escape::name(k, 60), item)),
                None => s.push_str(PICK_HINT),
            }
            s
        }
        Value::Array(a) => {
            let mut s = format!("top level: {}", kind(v));
            if let Some(item) = a.iter().find_map(Value::as_object) {
                s.push_str(&items_note("", item));
            }
            s
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pick(paths: Value) -> Pick {
        Pick::parse(Some(&paths)).unwrap().unwrap()
    }

    fn board() -> Value {
        json!({
            "apiVersion": "1",
            "jobs": [
                {"title": "ML Engineer", "location": "Berlin", "jobUrl": "https://x/1",
                 "descriptionHtml": "<p>long</p>", "address": {"city": "Berlin", "zip": "10115"}},
                {"title": "Designer", "location": "Paris", "jobUrl": "https://x/2",
                 "descriptionHtml": "<p>long</p>"}
            ]
        })
    }

    #[test]
    fn projects_array_fields_and_drops_the_rest() {
        let p = pick(json!(["jobs[].title", "jobs[].location", "jobs[].jobUrl"]));
        assert_eq!(
            p.apply(&board()).unwrap(),
            json!({"jobs": [
                {"title": "ML Engineer", "location": "Berlin", "jobUrl": "https://x/1"},
                {"title": "Designer", "location": "Paris", "jobUrl": "https://x/2"}
            ]})
        );
    }

    #[test]
    fn a_key_step_maps_over_an_array_and_nested_paths_work() {
        let a = pick(json!(["jobs.title", "jobs[].address.city"]));
        assert_eq!(
            a.apply(&board()).unwrap(),
            json!({"jobs": [
                {"title": "ML Engineer", "address": {"city": "Berlin"}},
                {"title": "Designer"}
            ]})
        );
    }

    #[test]
    fn a_whole_subtree_wins_over_a_deeper_path() {
        let p = pick(json!([
            "jobs[].address.city",
            "jobs[].address",
            "apiVersion"
        ]));
        assert_eq!(
            p.apply(&board()).unwrap(),
            json!({"apiVersion": "1", "jobs": [
                {"address": {"city": "Berlin", "zip": "10115"}},
                null
            ]})
        );
    }

    #[test]
    fn top_level_arrays_and_single_strings() {
        let v = json!([{"name": "a", "x": 1}, {"name": "b", "x": 2}]);
        assert_eq!(
            pick(json!("[].name")).apply(&v).unwrap(),
            json!([{"name": "a"}, {"name": "b"}])
        );
        assert_eq!(
            pick(json!(["name"])).apply(&v).unwrap(),
            json!([{"name": "a"}, {"name": "b"}])
        );
    }

    #[test]
    fn nothing_matched_is_none() {
        assert!(pick(json!(["nope"])).apply(&board()).is_none());
        assert!(pick(json!(["jobs[].nope"])).apply(&board()).is_none());
        assert_eq!(
            pick(json!(["jobs[].title"])).apply(&json!({"jobs": []})),
            Some(json!({"jobs": []}))
        );
    }

    #[test]
    fn bad_paths_are_refused_with_the_form() {
        for bad in [
            json!(["jobs[0].title"]),
            json!([""]),
            json!(["a..b"]),
            json!(["a]b"]),
        ] {
            let e = Pick::parse(Some(&bad)).unwrap_err();
            assert!(e.contains("like jobs[].title"), "{e}");
        }
        assert!(Pick::parse(Some(&json!([1]))).is_err());
        assert!(Pick::parse(Some(&json!({"a": 1}))).is_err());
        let many: Vec<String> = (0..=MAX_PATHS).map(|i| format!("k{i}")).collect();
        assert!(Pick::parse(Some(&json!(many))).is_err());
        assert_eq!(Pick::parse(None).unwrap(), None);
        assert_eq!(Pick::parse(Some(&json!([]))).unwrap(), None);
        assert_eq!(Pick::parse(Some(&json!(null))).unwrap(), None);
    }

    #[test]
    fn shape_names_keys_lengths_and_a_pick_example() {
        let s = shape(&board());
        assert!(s.contains("apiVersion (string)"), "{s}");
        assert!(s.contains("jobs (array of 2 objects)"), "{s}");
        assert!(s.contains("items of jobs have keys: "), "{s}");
        for k in ["title", "location", "jobUrl", "descriptionHtml", "address"] {
            assert!(s.contains(k), "{k}: {s}");
        }
        // Short name-like fields make the example; long text and objects do not.
        assert!(
            s.contains(r#"pick: ["jobs[].title", "jobs[].jobUrl", "jobs[].location"]"#),
            "{s}"
        );
        let s = shape(&json!([{"id": 1}, {"id": 2}, {"id": 3}]));
        assert!(s.contains("top level: array of 3 objects"), "{s}");
        assert!(s.contains("the items have keys: id"), "{s}");
        assert!(s.contains(r#"pick: ["[].id"]"#), "{s}");
        assert_eq!(shape(&json!("x")), "");
    }

    #[test]
    fn shape_escapes_page_keys() {
        let s = shape(&json!({"[x](#12)": [{"a": 1}]}));
        assert!(!s.contains("[x](#12)"), "{s}");
    }
}
