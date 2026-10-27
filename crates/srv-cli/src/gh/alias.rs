//! gh aliases (D-591). `gh alias set NAME EXPANSION` makes `gh NAME ...` run
//! `gh EXPANSION ...`, so an alias may stand for any command, `gh auth token` included. The
//! server reads the user's aliases with `gh alias list` (gh prints them as a YAML map), and
//! classifies and runs the expansion itself, never the alias: what the gate saw is what runs.
//!
//! Only the YAML gh 2.x writes is understood: one `name: value` line per alias, the value
//! plain, single-quoted or double-quoted. A value in any other form (a block scalar, a
//! multi-line plain value, an unknown escape) is kept as unreadable, and calling that alias
//! is refused (fails closed). Alias values can hold anything the user typed, so nothing here
//! puts an expansion into an error message.

use std::collections::BTreeMap;

/// The user's gh aliases: name (one or two words, `issue mine`) to expansion; `None` when
/// the value could not be read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Aliases {
    map: BTreeMap<String, Option<String>>,
}

/// An alias a call names.
#[derive(Debug, PartialEq, Eq)]
pub struct Hit<'a> {
    pub name: String,
    /// `None`: the value could not be read from `gh alias list`.
    pub expansion: Option<&'a str>,
    /// The words after the alias name.
    pub args: &'a [String],
}

impl Aliases {
    /// The output of `gh alias list`. `Err` names the first line that is not `name: value`
    /// (by number only: values may hold credentials).
    pub fn parse(out: &str) -> Result<Aliases, String> {
        let mut map = BTreeMap::new();
        let mut last: Option<String> = None;
        for (n, raw) in out.lines().enumerate() {
            let line = raw.trim_end_matches('\r');
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            if line.starts_with([' ', '\t']) {
                // A continuation (block scalar or wrapped plain value): the last value is not
                // what gh runs as we read it.
                match &last {
                    Some(k) => {
                        map.insert(k.clone(), None);
                        continue;
                    }
                    None => return Err(format!("line {} is not `name: expansion`", n + 1)),
                }
            }
            let (key, value) = split_entry(line)
                .ok_or_else(|| format!("line {} is not `name: expansion`", n + 1))?;
            last = Some(key.clone());
            map.insert(key, value);
        }
        Ok(Aliases { map })
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The alias `argv` starts with: a two-word name (`issue mine`) first, then one word.
    pub fn lookup<'a>(&'a self, argv: &'a [String]) -> Option<Hit<'a>> {
        let first = argv.first()?;
        if let Some(second) = argv.get(1) {
            let two = format!("{first} {second}");
            if let Some(v) = self.map.get(&two) {
                return Some(Hit {
                    name: two,
                    expansion: v.as_deref(),
                    args: &argv[2..],
                });
            }
        }
        self.map.get(first).map(|v| Hit {
            name: first.clone(),
            expansion: v.as_deref(),
            args: &argv[1..],
        })
    }

    /// Is `group word` a two-word alias (`issue mine`)?
    pub fn has_pair(&self, group: &str, word: &str) -> bool {
        self.map.contains_key(&format!("{group} {word}"))
    }
}

/// One top-level `key: value` line; the value is `None` when it is not a form we read.
fn split_entry(line: &str) -> Option<(String, Option<String>)> {
    let (key, rest) = if line.starts_with(['\'', '"']) {
        let (k, used) = quoted(line)?;
        let rest = line[used..].strip_prefix(':')?;
        (k, rest)
    } else {
        // A plain key ends at the first `: ` (or a final `:`).
        match line.find(": ") {
            Some(i) => (line[..i].to_string(), &line[i + 1..]),
            None => (line.strip_suffix(':')?.to_string(), ""),
        }
    };
    if key.is_empty() {
        return None;
    }
    let v = rest.trim();
    let value = if v.is_empty() || v.starts_with(['|', '>', '&', '*', '!', '[', '{']) {
        None
    } else if v.starts_with(['\'', '"']) {
        match quoted(v) {
            Some((s, used)) if v[used..].trim().is_empty() => Some(s),
            _ => None,
        }
    } else if v.contains(" #") || v.contains('\t') {
        None
    } else {
        Some(v.to_string())
    };
    Some((key, value))
}

/// A YAML single- or double-quoted scalar at the start of `s`: its value and the bytes it
/// used. `None` for an unterminated scalar or an escape we do not read.
fn quoted(s: &str) -> Option<(String, usize)> {
    let mut it = s.char_indices();
    let (_, q) = it.next()?;
    let mut out = String::new();
    while let Some((i, c)) = it.next() {
        match (q, c) {
            ('\'', '\'') => {
                if s[i + 1..].starts_with('\'') {
                    it.next();
                    out.push('\'');
                } else {
                    return Some((out, i + 1));
                }
            }
            ('"', '"') => return Some((out, i + 1)),
            ('"', '\\') => {
                let (_, e) = it.next()?;
                out.push(match e {
                    '\\' => '\\',
                    '"' => '"',
                    '/' => '/',
                    't' => '\t',
                    'n' => '\n',
                    'r' => '\r',
                    ' ' => ' ',
                    _ => return None,
                });
            }
            _ => out.push(c),
        }
    }
    None
}

/// The words `gh NAME args...` runs for an alias whose value is `expansion`, the way gh
/// 2.x expands it: when the expansion contains `$`, `$1`, `$2`... are replaced by the
/// arguments in order (every one is used that way); otherwise the arguments are appended.
/// A `$N` left over means too few arguments. The result is split by [`crate::argv::split`],
/// which refuses shell syntax, so it may differ from gh's own splitting only by refusing.
pub fn expand(expansion: &str, args: &[String]) -> Result<Vec<String>, String> {
    let mut e = expansion.to_string();
    let mut extra = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if e.contains('$') {
            e = e.replace(&format!("${}", i + 1), a);
        } else {
            extra.push(a.clone());
        }
    }
    if has_placeholder(&e) {
        return Err("needs more arguments than were given".into());
    }
    let mut words = crate::argv::split(&e).map_err(|err| match err {
        harness_core::mcp::ToolError::Args(m) => m,
        other => other.to_string(),
    })?;
    words.extend(extra);
    Ok(words)
}

/// `$` followed by a digit anywhere in `s`.
pub fn has_placeholder(s: &str) -> bool {
    s.as_bytes()
        .windows(2)
        .any(|w| w[0] == b'$' && w[1].is_ascii_digit())
}

/// `s` with every `$N` placeholder replaced by `ARG`: the shape `alias set` classifies.
pub fn without_placeholders(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '$' && it.peek().is_some_and(|d| d.is_ascii_digit()) {
            while it.peek().is_some_and(|d| d.is_ascii_digit()) {
                it.next();
            }
            out.push_str("ARG");
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// What gh 2.93 printed for these aliases (`gh alias list`, scratch GH_CONFIG_DIR).
    const LISTED: &str = "bang: '!gh issue list'\nco: pr checkout\ncolon: 'pr view: x #y'\n\
dq: pr view \"x\" 'y' \\z\nissue mine: issue list --mention @me\nml: |-\n    pr\n    view\n\
q: api -X POST \"repos/$1/x\" -f a=\"b c\"\nsh1: '!echo hi | cat'\ntab: \"pr\\tview\"\n\
tok: auth token\nit: 'it''s'\n";

    #[test]
    fn parses_what_gh_alias_list_prints() {
        let a = Aliases::parse(LISTED).unwrap();
        let get = |k: &str| a.map.get(k).cloned();
        assert_eq!(get("co"), Some(Some("pr checkout".into())));
        assert_eq!(get("tok"), Some(Some("auth token".into())));
        assert_eq!(get("bang"), Some(Some("!gh issue list".into())));
        assert_eq!(get("colon"), Some(Some("pr view: x #y".into())));
        assert_eq!(get("dq"), Some(Some("pr view \"x\" 'y' \\z".into())));
        assert_eq!(
            get("issue mine"),
            Some(Some("issue list --mention @me".into()))
        );
        assert_eq!(get("ml"), Some(None), "block scalars are unreadable");
        assert_eq!(get("tab"), Some(Some("pr\tview".into())));
        assert_eq!(get("it"), Some(Some("it's".into())));
        assert!(Aliases::parse("").unwrap().is_empty());
        for bad in ["  indented first", "no separator here", ": empty key"] {
            assert!(Aliases::parse(bad).is_err(), "{bad}");
        }
        // Unknown escapes and trailing text after a quoted value are unreadable.
        let a = Aliases::parse("a: \"x\\qy\"\nb: 'x' y\nc: x # comment").unwrap();
        assert_eq!(a.map.values().filter(|v| v.is_none()).count(), 3);
    }

    #[test]
    fn lookup_prefers_two_word_names() {
        let a = Aliases::parse(LISTED).unwrap();
        let argv = s(&["issue", "mine", "-L", "5"]);
        let hit = a.lookup(&argv).unwrap();
        assert_eq!(hit.name, "issue mine");
        assert_eq!(hit.args, &argv[2..]);
        let argv = s(&["tok"]);
        assert_eq!(a.lookup(&argv).unwrap().expansion, Some("auth token"));
        assert!(a.lookup(&s(&["issue", "list"])).is_none());
        assert!(a.has_pair("issue", "mine"));
    }

    #[test]
    fn expands_like_gh() {
        let x = |e: &str, a: &[&str]| expand(e, &s(a));
        assert_eq!(
            x("pr view", &["5", "-w"]).unwrap(),
            s(&["pr", "view", "5", "-w"])
        );
        assert_eq!(
            x("api -X POST \"repos/$1/x\"", &["o/r"]).unwrap(),
            s(&["api", "-X", "POST", "repos/o/r/x"])
        );
        // gh replaces into the string, then splits: an argument can add words.
        assert_eq!(
            x("issue list --author=\"$1\" $2", &["me", "-X DELETE"]).unwrap(),
            s(&["issue", "list", "--author=me", "-X", "DELETE"])
        );
        assert!(x("issue list --author=\"$1\"", &[]).is_err());
        assert!(x("pr view | cat", &[]).is_err(), "shell syntax is refused");
        assert_eq!(without_placeholders("a $1 b$23"), "a ARG bARG");
        assert!(has_placeholder("x $9") && !has_placeholder("x $ y"));
    }
}
