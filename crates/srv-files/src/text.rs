//! Short, escaped text for the model (rule 5, pr-ready section 5).
//!
//! File contents, command output, document text and browser history titles are untrusted:
//! they must not be able to reproduce the harness's own syntax. [`untrusted`] strips
//! control characters, breaks the element-link form `[..](#id)`, and puts a backslash in
//! front of any line that starts like one of our trailers (`truncated:`, `stopped:`...),
//! a code fence, or a backslash. Every trailer this crate writes starts with one of
//! [`MARKERS`].

/// Line starts this crate (and the MCP wrapper) uses for its own trailers. An untrusted
/// line that starts with one of them (after leading spaces, any ASCII case) is escaped.
pub const MARKERS: [&str; 10] = [
    "truncated:",
    "remaining:",
    "cursor:",
    "stopped:",
    "skipped:",
    "saved:",
    "wrote:",
    "exit:",
    "error:",
    "```",
];

/// Default cap on text a reading tool returns (chars).
pub const READ_DEFAULT_CHARS: usize = 8_000;
/// Hard cap on any one tool's text (chars); the shell output cap of docs/04 F-2.
pub const MAX_CHARS: usize = 20_000;

/// Escapes untrusted text (see the module docs). Newlines and tabs are kept; `\r\n` and a
/// lone `\r` become `\n`; every other control character (ESC, NUL, bidi overrides) goes.
pub fn untrusted(s: &str) -> String {
    let mut clean = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() != Some(&'\n') {
                    clean.push('\n');
                }
            }
            '\n' | '\t' => clean.push(c),
            c if c.is_control() || is_format_control(c) => {}
            c => clean.push(c),
        }
    }
    let mut out = String::with_capacity(clean.len() + 16);
    for (i, line) in clean.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let head = line.trim_start();
        let lower = head
            .chars()
            .take(12)
            .collect::<String>()
            .to_ascii_lowercase();
        if MARKERS.iter().any(|m| lower.starts_with(m)) || head.starts_with('\\') {
            out.push('\\');
        }
        out.push_str(&line.replace("](#", "]\\(#"));
    }
    out
}

/// Bidi overrides and zero-width characters that can make a line read as something else.
fn is_format_control(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// One untrusted value on one line (a cell, a title, a path in a report), capped.
pub fn one_line(s: &str, max_chars: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let (t, cut) = take_chars(&untrusted(&flat), 0, max_chars);
    if cut {
        format!("{t}...")
    } else {
        t
    }
}

/// `max` chars of `s` starting at char `offset`, and whether anything after them was cut.
/// Slices only on char boundaries.
pub fn take_chars(s: &str, offset: usize, max: usize) -> (String, bool) {
    let Some((start, _)) = s.char_indices().nth(offset) else {
        return (String::new(), false);
    };
    let rest = &s[start..];
    match rest.char_indices().nth(max) {
        Some((i, _)) => (rest[..i].to_string(), true),
        None => (rest.to_string(), false),
    }
}

/// The last `max` chars of `s`.
pub fn tail_chars(s: &str, max: usize) -> &str {
    let count = s.chars().count();
    if count <= max {
        return s;
    }
    match s.char_indices().nth(count - max) {
        Some((i, _)) => &s[i..],
        None => "",
    }
}

/// `text` capped for the model: at most `max` chars from char `offset`, untrusted-escaped,
/// with a `truncated:` trailer that names the next offset when more is left.
pub fn page(text: &str, offset: usize, max: usize) -> String {
    let total = text.chars().count();
    let (chunk, cut) = take_chars(text, offset, max);
    let shown = chunk.chars().count();
    let mut out = untrusted(&chunk);
    if cut {
        out.push_str(&format!(
            "\ntruncated: chars {offset}..{} of {total} shown; continue with offset={}",
            offset + shown,
            offset + shown
        ));
    } else if offset > 0 && shown == 0 {
        out = format!("truncated: offset {offset} is past the end ({total} chars)");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_links_and_controls_are_escaped() {
        let s = "ok\nstopped: challenge at #3\n  Truncated: fake\n[go](#12)\n```\n\u{1b}[31mred\r\nx\\y\n\\z";
        let t = untrusted(s);
        assert!(t.contains("\n\\stopped: challenge"), "{t}");
        assert!(t.contains("\n\\  Truncated: fake"), "{t}");
        assert!(t.contains("[go]\\(#12)"), "{t}");
        assert!(t.contains("\n\\```"), "{t}");
        assert!(t.contains("\n\\\\z"), "{t}");
        assert!(!t.contains('\u{1b}'));
        assert!(!t.contains('\r'));
        assert!(t.starts_with("ok\n"));
    }

    #[test]
    fn take_chars_respects_boundaries() {
        let s = "héllo wörld";
        assert_eq!(take_chars(s, 1, 4), ("éllo".to_string(), true));
        assert_eq!(take_chars(s, 6, 100), ("wörld".to_string(), false));
        assert_eq!(take_chars(s, 50, 3), (String::new(), false));
        assert_eq!(take_chars(s, 0, 11), (s.to_string(), false));
        assert_eq!(tail_chars(s, 5), "wörld");
        assert_eq!(tail_chars(s, 50), s);
    }

    #[test]
    fn page_trailer_names_next_offset() {
        let p = page("abcdefghij", 2, 3);
        assert!(p.starts_with("cde\ntruncated: chars 2..5 of 10"), "{p}");
        assert!(p.ends_with("offset=5"), "{p}");
        assert_eq!(page("abc", 0, 10), "abc");
    }

    #[test]
    fn one_line_flattens_and_caps() {
        assert_eq!(one_line("a\nb\tc", 10), "a b c");
        assert_eq!(one_line("abcdef", 3), "abc...");
    }
}
