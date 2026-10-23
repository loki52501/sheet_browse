//! T-2 tool file header (I-12): the `// @key value` comment block at the top of a tool file.
//!
//! The header is the leading run of `//` comment lines and blank lines; the first other
//! line ends it. Unknown `@keys` and plain comments are ignored. Single-valued keys
//! (`@name @kind @gate @expects @forged @phase`) may appear once; `@match` and
//! `@fingerprint` may repeat and accumulate. Missing `@name`, `@kind` or `@gate` is an error.
//! Missing `@match` means `/*`; missing `@phase` means 2 (needs the browser), the safe side.

use harness_core::Class;
use std::fmt;

/// `@kind` (T-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolKind {
    /// Returns JSON; never calls act.
    Reader,
    /// Returns an act program; never gets hands.
    Planner,
}

impl ToolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolKind::Reader => "reader",
            ToolKind::Planner => "planner",
        }
    }
}

/// One `@args` entry: `name:string`, `plan?:string`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgSpec {
    pub name: String,
    pub ty: String,
    pub optional: bool,
}

/// One `@fingerprint` item: `form#f2`, `button"Register"`, `input"Email"`.
///
/// `role` is the short alias or ARIA role as written (D-49). `id` is only meaningful for
/// `form#f2` (matched against `IndexRow.form`, D-304); on other roles it is kept but ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpItem {
    pub role: String,
    pub name: Option<String>,
    pub id: Option<String>,
}

impl fmt::Display for FpItem {
    /// The header spelling: `form#f2`, `button"Register"`, `input"Say \"hi\""`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.role)?;
        if let Some(id) = &self.id {
            write!(f, "#{id}")?;
        }
        if let Some(name) = &self.name {
            f.write_str("\"")?;
            for ch in name.chars() {
                match ch {
                    '"' => f.write_str("\\\"")?,
                    '\\' => f.write_str("\\\\")?,
                    c => write!(f, "{c}")?,
                }
            }
            f.write_str("\"")?;
        }
        Ok(())
    }
}

/// I-12: the parsed header of a tool file (T-2).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolHeader {
    pub name: String,
    pub kind: ToolKind,
    /// `@match` path globs within the site.
    pub matches: Vec<String>,
    pub args: Vec<ArgSpec>,
    /// `@gate`: the class floor (H-1.5).
    pub gate: Class,
    pub fingerprint: Vec<FpItem>,
    /// `@expects`, unparsed (`navigated:/thanks|/confirm OR text"You're registered"`).
    pub expects: Option<String>,
    /// `@forged`, unparsed.
    pub forged: Option<String>,
    /// `@phase`: 1 = works on fetched HTML, 2 = needs the browser.
    pub phase: u8,
}

/// The header lines of `src`: `(line index, key, value)` for every `// @key value` line in
/// the leading comment block.
pub(crate) fn header_lines(src: &str) -> Vec<(usize, &str, &str)> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let Some(c) = t.strip_prefix("//") else {
            break;
        };
        let c = c.trim_start();
        if let Some(rest) = c.strip_prefix('@') {
            let (key, value) = match rest.find(char::is_whitespace) {
                Some(p) => (&rest[..p], rest[p..].trim()),
                None => (rest, ""),
            };
            out.push((i, key, value));
        }
    }
    out
}

/// Byte offset where the body (everything after the header comment block) starts.
pub(crate) fn body_start(src: &str) -> usize {
    let mut off = 0;
    for line in src.split_inclusive('\n') {
        let t = line.trim();
        if !(t.is_empty() || t.starts_with("//")) {
            return off;
        }
        off += line.len();
    }
    off
}

/// Why an `@match` pattern is not a path within the site (G-4, D-307), if it is not.
pub(crate) fn match_violation(pattern: &str) -> Option<&'static str> {
    if pattern.contains("://") {
        Some("has a scheme")
    } else if pattern.contains("//") {
        Some("contains //")
    } else if pattern.split('/').any(|seg| seg == "..") {
        Some("contains ..")
    } else if pattern.contains('\\') {
        // WHATWG URL parsing treats `\` as `/`, so `/\host` is protocol-relative.
        Some("contains a backslash")
    } else if ["%2e", "%2f", "%5c"]
        .iter()
        .any(|e| pattern.to_ascii_lowercase().contains(e))
    {
        Some("contains a percent-encoded dot, slash or backslash")
    } else if !pattern.starts_with('/') {
        Some("does not start with / (a host or relative pattern)")
    } else {
        None
    }
}

fn first_token(v: &str) -> &str {
    v.split_whitespace().next().unwrap_or("")
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn set_once<T>(slot: &mut Option<T>, key: &str, v: T) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("@{key} given twice"));
    }
    *slot = Some(v);
    Ok(())
}

/// I-12. Parses the `// @key value` header of a tool file.
pub fn parse_header(src: &str) -> Result<ToolHeader, String> {
    let mut name = None;
    let mut kind = None;
    let mut gate = None;
    let mut expects = None;
    let mut forged = None;
    let mut phase = None;
    let mut args: Option<Vec<ArgSpec>> = None;
    let mut matches = Vec::new();
    let mut fingerprint = Vec::new();

    for (_, key, value) in header_lines(src) {
        match key {
            "name" => {
                let n = first_token(value);
                if !is_ident(n) {
                    return Err(format!("@name {n:?} is not [A-Za-z0-9_]+"));
                }
                set_once(&mut name, key, n.to_string())?;
            }
            "kind" => {
                let k = match first_token(value) {
                    "reader" => ToolKind::Reader,
                    "planner" => ToolKind::Planner,
                    other => return Err(format!("@kind {other:?} is not reader|planner")),
                };
                set_once(&mut kind, key, k)?;
            }
            "gate" => {
                let g: Class = first_token(value)
                    .parse()
                    .map_err(|e: harness_core::class::UnknownClass| format!("@gate: {e}"))?;
                set_once(&mut gate, key, g)?;
            }
            "phase" => {
                let p = match first_token(value) {
                    "1" => 1u8,
                    "2" => 2u8,
                    other => return Err(format!("@phase {other:?} is not 1|2")),
                };
                set_once(&mut phase, key, p)?;
            }
            "match" => {
                if value.is_empty() {
                    return Err("@match is empty".into());
                }
                for p in value.split_whitespace() {
                    if let Some(why) = match_violation(p) {
                        return Err(format!("@match {p:?} {why}"));
                    }
                    matches.push(p.to_string());
                }
            }
            "args" => set_once(&mut args, key, parse_args(value)?)?,
            "fingerprint" => fingerprint.extend(parse_fingerprint(value)?),
            "expects" => set_once(&mut expects, key, value.to_string())?,
            "forged" => set_once(&mut forged, key, value.to_string())?,
            _ => {}
        }
    }

    if matches.is_empty() {
        matches.push("/*".to_string());
    }
    Ok(ToolHeader {
        name: name.ok_or("missing @name")?,
        kind: kind.ok_or("missing @kind")?,
        gate: gate.ok_or("missing @gate")?,
        matches,
        args: args.unwrap_or_default(),
        fingerprint,
        expects,
        forged,
        phase: phase.unwrap_or(2),
    })
}

/// `name:string email:string plan?:string`. A bare `name` means `name:string`.
pub(crate) fn parse_args(v: &str) -> Result<Vec<ArgSpec>, String> {
    let mut out: Vec<ArgSpec> = Vec::new();
    for tok in v.split_whitespace() {
        let (lhs, ty) = match tok.split_once(':') {
            Some((l, t)) => (l, t),
            None => (tok, "string"),
        };
        let (name, optional) = match lhs.strip_suffix('?') {
            Some(n) => (n, true),
            None => (lhs, false),
        };
        if !is_ident(name) || !is_ident(ty) {
            return Err(format!("@args item {tok:?} is not name:type or name?:type"));
        }
        if out.iter().any(|a| a.name == name) {
            return Err(format!("@args names {name:?} twice"));
        }
        out.push(ArgSpec {
            name: name.to_string(),
            ty: ty.to_string(),
            optional,
        });
    }
    Ok(out)
}

/// `form#f2 button"Register" input"Email"`; a space between role and `"name"` is allowed.
pub fn parse_fingerprint(v: &str) -> Result<Vec<FpItem>, String> {
    let chars: Vec<char> = v.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    let is_role_char = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    loop {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start = i;
        while i < chars.len() && is_role_char(chars[i]) {
            i += 1;
        }
        if i == start {
            return Err(format!(
                "@fingerprint: expected a role at {:?}",
                chars[start..].iter().collect::<String>()
            ));
        }
        let role: String = chars[start..i].iter().collect();
        let mut id = None;
        if i < chars.len() && chars[i] == '#' {
            i += 1;
            let s = i;
            while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '"' {
                i += 1;
            }
            if i == s {
                return Err(format!("@fingerprint: empty id after {role}#"));
            }
            id = Some(chars[s..i].iter().collect::<String>());
        }
        // Optional name, possibly after spaces.
        let mut j = i;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        let mut name = None;
        if j < chars.len() && chars[j] == '"' {
            j += 1;
            let mut s = String::new();
            let mut closed = false;
            while j < chars.len() {
                match chars[j] {
                    '\\' if j + 1 < chars.len() => {
                        s.push(chars[j + 1]);
                        j += 2;
                    }
                    '"' => {
                        closed = true;
                        j += 1;
                        break;
                    }
                    c => {
                        s.push(c);
                        j += 1;
                    }
                }
            }
            if !closed {
                return Err(format!("@fingerprint: unclosed name after {role}"));
            }
            name = Some(s);
            i = j;
        }
        if i < chars.len() && !chars[i].is_whitespace() {
            return Err(format!(
                "@fingerprint: unexpected {:?} after {role}",
                chars[i]
            ));
        }
        out.push(FpItem { role, name, id });
    }
    Ok(out)
}

/// The `@fingerprint` value for `items`.
pub fn render_fingerprint(items: &[FpItem]) -> String {
    items
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Replaces every `@fingerprint` header line in `src` with one line carrying `items`
/// (inserted after `@gate` when the file had none). Nothing else in the file changes.
pub fn patch_fingerprint(src: &str, items: &[FpItem]) -> String {
    let lines = header_lines(src);
    let fp_lines: Vec<usize> = lines
        .iter()
        .filter(|(_, k, _)| *k == "fingerprint")
        .map(|(i, _, _)| *i)
        .collect();
    let anchor = fp_lines.first().copied().or_else(|| {
        lines
            .iter()
            .find(|(_, k, _)| *k == "gate")
            .map(|(i, _, _)| *i + 1)
    });
    let new_line = format!("// @fingerprint {}", render_fingerprint(items));
    let mut out = String::with_capacity(src.len() + new_line.len());
    let mut inserted = false;
    for (i, line) in src.split_inclusive('\n').enumerate() {
        if Some(i) == anchor && !inserted {
            out.push_str(&new_line);
            out.push_str(if line.ends_with("\r\n") { "\r\n" } else { "\n" });
            inserted = true;
        }
        if fp_lines.contains(&i) {
            continue;
        }
        out.push_str(line);
    }
    if !inserted {
        // No header anchor (cannot happen for a parsed header, which has @gate): prepend.
        return format!("{new_line}\n{out}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTER: &str = r#"// @name register
// @kind planner
// @match /*
// @args name:string email:string plan?:string
// @gate Submit
// @fingerprint form#f2 button"Register" input"Email"
// @expects navigated:/thanks|/confirm OR text"You're registered"
// @forged 2026-10-22 task=T-31 by=toolforge score=0.92
// @phase 2
export default function (page, args) {
  const btn = page.find('button "Register" SUBMIT');
  return `c#${btn}; ?s`;
}
"#;

    #[test]
    fn parses_the_t2_example() {
        let h = parse_header(REGISTER).unwrap();
        assert_eq!(h.name, "register");
        assert_eq!(h.kind, ToolKind::Planner);
        assert_eq!(h.matches, vec!["/*"]);
        assert_eq!(h.gate, Class::Submit);
        assert_eq!(h.phase, 2);
        assert_eq!(
            h.args,
            vec![
                ArgSpec {
                    name: "name".into(),
                    ty: "string".into(),
                    optional: false
                },
                ArgSpec {
                    name: "email".into(),
                    ty: "string".into(),
                    optional: false
                },
                ArgSpec {
                    name: "plan".into(),
                    ty: "string".into(),
                    optional: true
                },
            ]
        );
        assert_eq!(
            h.fingerprint,
            vec![
                FpItem {
                    role: "form".into(),
                    name: None,
                    id: Some("f2".into())
                },
                FpItem {
                    role: "button".into(),
                    name: Some("Register".into()),
                    id: None
                },
                FpItem {
                    role: "input".into(),
                    name: Some("Email".into()),
                    id: None
                },
            ]
        );
        assert_eq!(
            h.expects.as_deref(),
            Some("navigated:/thanks|/confirm OR text\"You're registered\"")
        );
        assert!(h.forged.as_deref().unwrap().contains("task=T-31"));
    }

    #[test]
    fn missing_required_keys_are_errors() {
        for key in ["@name", "@kind", "@gate"] {
            let src: String = REGISTER
                .lines()
                .filter(|l| !l.starts_with(&format!("// {key} ")))
                .map(|l| format!("{l}\n"))
                .collect();
            let err = parse_header(&src).unwrap_err();
            assert!(err.contains(key), "{key}: {err}");
        }
    }

    #[test]
    fn defaults_and_garbage() {
        let h = parse_header("// @name x\n// @kind reader\n// @gate Read\nx()").unwrap();
        assert_eq!(h.matches, vec!["/*"]);
        assert_eq!(h.phase, 2);
        assert!(h.args.is_empty());
        assert!(parse_header("// @name x\n// @kind writer\n// @gate Read\n").is_err());
        assert!(parse_header("// @name x\n// @kind reader\n// @gate Huge\n").is_err());
        assert!(parse_header("// @name x y\n// @kind reader\n// @gate Read\n").is_ok());
        assert!(parse_header("// @name x-y\n// @kind reader\n// @gate Read\n").is_err());
        assert!(parse_header("// @name x\n// @name y\n// @kind reader\n// @gate Read\n").is_err());
        assert!(parse_header("// @name x\n// @kind reader\n// @gate Read\n// @phase 3\n").is_err());
        assert!(parse_header(
            "// @name x\n// @kind reader\n// @gate Read\n// @match https://evil.com/*\n"
        )
        .is_err());
        assert!(parse_header("garbage\n// @name x\n// @kind reader\n// @gate Read\n").is_err());
        assert!(parse_header("").is_err());
    }

    #[test]
    fn header_stops_at_first_code_line() {
        let src = "// @name x\n// @kind reader\n\n// @gate Read\nconst a = 1;\n// @gate Pay\n";
        assert_eq!(parse_header(src).unwrap().gate, Class::Read);
    }

    #[test]
    fn fingerprint_forms_and_round_trip() {
        let items =
            parse_fingerprint(r#"button "Sign up" input"Say \"hi\"" link checkbox#c1"#).unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].name.as_deref(), Some("Sign up"));
        assert_eq!(items[1].name.as_deref(), Some("Say \"hi\""));
        assert_eq!(items[2].name, None);
        assert_eq!(items[3].id.as_deref(), Some("c1"));
        let again = parse_fingerprint(&render_fingerprint(&items)).unwrap();
        assert_eq!(again, items);
        assert!(parse_fingerprint(r#"button"open"#).is_err());
        assert!(parse_fingerprint(r#""Register""#).is_err());
        assert!(parse_fingerprint(r#"button"a"x"#).is_err());
    }

    #[test]
    fn patch_fingerprint_changes_only_that_line() {
        let items = parse_fingerprint(r#"form#f3 button"Sign up""#).unwrap();
        let out = patch_fingerprint(REGISTER, &items);
        assert!(out.contains("// @fingerprint form#f3 button\"Sign up\"\n"));
        assert_eq!(out.lines().count(), REGISTER.lines().count());
        let h = parse_header(&out).unwrap();
        assert_eq!(h.fingerprint, items);
        let mut before = parse_header(REGISTER).unwrap();
        before.fingerprint = items.clone();
        assert_eq!(before, h);
        // A file without @fingerprint gets one after @gate.
        let bare = "// @name x\n// @kind reader\n// @gate Read\nf();\n";
        let out = patch_fingerprint(bare, &items);
        assert_eq!(
            out,
            "// @name x\n// @kind reader\n// @gate Read\n// @fingerprint form#f3 button\"Sign up\"\nf();\n"
        );
    }

    #[test]
    fn body_start_skips_header() {
        assert_eq!(&REGISTER[body_start(REGISTER)..][..6], "export");
    }
}
