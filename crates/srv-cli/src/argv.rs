//! Command strings to argument vectors. Nothing here runs a shell: a program is always
//! started with an argument vector, so shell syntax would be passed through literally and
//! do something other than what the caller meant. It is refused instead.

use harness_core::mcp::ToolError;
use serde_json::Value;

/// Longest command string or argument list accepted, in bytes.
const MAX_CMD_BYTES: usize = 16 * 1024;

/// Splits `cmd` like a POSIX shell word splitter without expansion: whitespace separates
/// words, `'...'` is literal, `"..."` is literal except `\"` and `\\`. A backslash outside
/// quotes is literal (Windows paths). Outside quotes, the shell operators `| ; & < >`,
/// backtick and `$` are refused with [`ToolError::Args`], because no shell runs: quote them
/// to pass them literally.
pub fn split(cmd: &str) -> Result<Vec<String>, ToolError> {
    if cmd.len() > MAX_CMD_BYTES {
        return Err(ToolError::Args(format!(
            "command is longer than {MAX_CMD_BYTES} bytes"
        )));
    }
    if cmd.contains('\0') {
        return Err(ToolError::Args("command contains a NUL character".into()));
    }
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut it = cmd.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match it.next() {
                        Some('\'') => break,
                        Some(d) => cur.push(d),
                        None => return Err(unterminated('\'')),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match it.next() {
                        Some('"') => break,
                        Some('\\') if matches!(it.peek(), Some('"') | Some('\\')) => {
                            if let Some(d) = it.next() {
                                cur.push(d);
                            }
                        }
                        Some(d) => cur.push(d),
                        None => return Err(unterminated('"')),
                    }
                }
            }
            '|' | ';' | '&' | '<' | '>' | '`' | '$' => {
                return Err(ToolError::Args(format!(
                    "shell syntax `{c}` outside quotes: commands run without a shell (no pipes, \
                     redirects, chaining or variables); run one command per call and quote \
                     literal characters"
                )));
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

fn unterminated(q: char) -> ToolError {
    ToolError::Args(format!("unterminated {q} quote"))
}

/// The argument vector in `args[key]`: a string (split by [`split`]) or an array of strings
/// (taken literally). A `@vault(` literal is refused: vault values are never passed to a
/// program (its arguments are visible to the OS), and nothing here would resolve it.
pub fn from_value(args: &Value, key: &str) -> Result<Vec<String>, ToolError> {
    let argv = argv_of(args, key)?;
    if argv
        .iter()
        .any(|a| a.to_ascii_lowercase().contains("@vault("))
    {
        return Err(ToolError::Args(format!(
            "{key} must not contain @vault(..): vault values are never passed to programs"
        )));
    }
    Ok(argv)
}

fn argv_of(args: &Value, key: &str) -> Result<Vec<String>, ToolError> {
    match args.get(key) {
        Some(Value::String(s)) => split(s),
        Some(Value::Array(a)) => {
            let mut out = Vec::with_capacity(a.len());
            let mut total = 0usize;
            for v in a {
                let s = v.as_str().ok_or_else(|| {
                    ToolError::Args(format!("{key} must be a string or an array of strings"))
                })?;
                if s.contains('\0') {
                    return Err(ToolError::Args(format!("{key} contains a NUL character")));
                }
                total += s.len();
                if total > MAX_CMD_BYTES {
                    return Err(ToolError::Args(format!(
                        "{key} is longer than {MAX_CMD_BYTES} bytes"
                    )));
                }
                out.push(s.to_string());
            }
            Ok(out)
        }
        Some(_) => Err(ToolError::Args(format!(
            "{key} must be a string or an array of strings"
        ))),
        None => Err(ToolError::Args(format!("{key} is required"))),
    }
}

/// One line showing an argument vector, for cards and summaries.
pub fn show(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty()
                || a.chars()
                    .any(|c| c.is_whitespace() || c == '"' || c == '\'')
            {
                format!("{a:?}")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn splits_words_and_quotes() {
        assert_eq!(
            split("run view 123 --log").unwrap(),
            s(&["run", "view", "123", "--log"])
        );
        assert_eq!(
            split(r#"api -f body="hello world" -q '.[] | .name' "" C:\x"#).unwrap(),
            s(&[
                "api",
                "-f",
                "body=hello world",
                "-q",
                ".[] | .name",
                "",
                r"C:\x"
            ])
        );
        assert_eq!(split(r#""a\"b\\c""#).unwrap(), s(&[r#"a"b\c"#]));
        assert_eq!(split("  ").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn refuses_shell_syntax_outside_quotes() {
        for bad in [
            "run list | head",
            "a; b",
            "a && b",
            "a > f",
            "a < f",
            "echo $HOME",
            "echo `x`",
            "api repos/o/r/issues?state=open&per_page=5",
        ] {
            assert!(
                matches!(split(bad), Err(ToolError::Args(_))),
                "{bad} was accepted"
            );
        }
        assert!(split("'unterminated").is_err());
    }

    #[test]
    fn array_form_is_literal() {
        let v = serde_json::json!({"args": ["api", "a|b", "$x"]});
        assert_eq!(from_value(&v, "args").unwrap(), s(&["api", "a|b", "$x"]));
        assert!(from_value(&serde_json::json!({"args": [1]}), "args").is_err());
        assert!(from_value(&serde_json::json!({}), "args").is_err());
        for v in [
            serde_json::json!({"args": "api -H 'Authorization: @vault(gh)' x"}),
            serde_json::json!({"args": ["api", "@VAULT(gh)"]}),
        ] {
            assert!(matches!(from_value(&v, "args"), Err(ToolError::Args(_))));
        }
    }
}
