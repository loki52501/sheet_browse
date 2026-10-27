//! Untrusted text (CI logs, API bodies, program output) made safe to show the model.
//!
//! The harness writes its own structure into tool text: link forms `[..](#id)`, code fences,
//! and line-start trailers such as `remaining:`, `truncated:`, `stopped:`. Text a site, a CI
//! job or a program produced must not be able to reproduce them (pr-ready section 5), so
//! [`neutral`] drops terminal control sequences and other control characters, breaks the
//! link form, and quotes every line that starts like harness syntax with `| ` (a line that
//! already starts with `|` is quoted too, so the quoting stays unambiguous). [`capped`] then
//! cuts on a char boundary and appends this crate's own `truncated:` trailer.

/// Default cap on text a tool returns, in chars.
pub const MAX_TEXT_CHARS: usize = 12_000;

/// Line starts that the harness itself writes (checked ASCII case-insensitively after
/// leading whitespace).
const SYNTAX_STARTS: [&str; 9] = [
    "```",
    "~~~",
    "remaining:",
    "truncated:",
    "stopped:",
    "skipped:",
    "cursor:",
    "full log:",
    "|",
];

/// `s` with controls removed and harness syntax neutralised (see the module doc).
pub fn neutral(s: &str) -> String {
    let clean = strip_controls(s);
    let mut out = String::with_capacity(clean.len() + 16);
    for (i, line) in clean.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let line = line.replace("](#", "] (#");
        if looks_like_syntax(&line) {
            out.push_str("| ");
        }
        out.push_str(&line);
    }
    out
}

/// [`neutral`], then cut to `max_chars` chars with a `truncated:` trailer naming what was
/// kept. `hint` is appended to the trailer (for example how to narrow the call).
pub fn capped(s: &str, max_chars: usize, hint: &str) -> String {
    let n = neutral(s);
    let total = n.chars().count();
    if total <= max_chars {
        return n;
    }
    let cut = n
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(n.len());
    let mut out = n[..cut].to_string();
    out.push_str(&format!(
        "\ntruncated: kept {max_chars} of {total} chars{}",
        if hint.is_empty() {
            String::new()
        } else {
            format!("; {hint}")
        }
    ));
    out
}

/// One line, neutral and cut to `max_chars` chars (with `...`).
pub fn line(s: &str, max_chars: usize) -> String {
    let n = neutral(s).replace('\n', " ");
    if n.chars().count() <= max_chars {
        return n;
    }
    let mut out: String = n.chars().take(max_chars.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

/// Tags that only an HTML (or XML/SVG) document has: one is enough.
const DOCUMENT_TAGS: [&str; 9] = [
    "html", "head", "body", "script", "style", "iframe", "frameset", "svg", "noscript",
];
/// Tag-shaped tokens (`<p>`, `</div>`, `<a href=..>`, `<br/>`, `<!-- -->`) that make text
/// HTML even without a document tag.
const MIN_TAGS: usize = 3;

/// Is `s` HTML or XML markup, whatever its content type said (rule 5, D-593)? Yes for a
/// document marker (`<!doctype html>`, `<?xml`, or a tag in [`DOCUMENT_TAGS`]) or for
/// [`MIN_TAGS`] or more tag-shaped tokens. Plain text that mentions one `<tag>` is not.
pub fn looks_like_html(s: &str) -> bool {
    let m = markup(s);
    m.document || m.tags >= MIN_TAGS
}

/// Is `s` an HTML or XML document (a document marker only)? For output that may quote a
/// few tags legitimately (a PR body with `<details>`), where only a whole page is refused.
pub fn looks_like_html_document(s: &str) -> bool {
    markup(s).document
}

struct Markup {
    document: bool,
    tags: usize,
}

/// One pass over `s`: document markers and tag-shaped tokens. A tag is `<`, an optional
/// `/`, an ASCII letter, then name characters, then `>`, `/` or whitespace, closed by a `>`
/// before the next `<` or line end.
fn markup(s: &str) -> Markup {
    let b = s.as_bytes();
    let mut m = Markup {
        document: false,
        tags: 0,
    };
    let mut i = 0;
    while let Some(off) = b[i..].iter().position(|&c| c == b'<') {
        let at = i + off + 1;
        i = at;
        let rest = &b[at..];
        let starts = |p: &[u8]| rest.len() >= p.len() && rest[..p.len()].eq_ignore_ascii_case(p);
        if starts(b"!doctype html") || starts(b"?xml") {
            m.document = true;
            return m;
        }
        if starts(b"!--") {
            m.tags += 1;
            continue;
        }
        let name_at = usize::from(rest.first() == Some(&b'/'));
        let name_len = rest[name_at..]
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric() || **c == b'-')
            .count();
        let name = &rest[name_at..name_at + name_len];
        if name.is_empty() || !name[0].is_ascii_alphabetic() {
            continue;
        }
        let after = rest.get(name_at + name_len).copied();
        if !matches!(after, Some(b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n')) {
            continue;
        }
        let closed = rest[name_at + name_len..]
            .iter()
            .take(2_000)
            .take_while(|&&c| c != b'<')
            .any(|&c| c == b'>');
        if !closed {
            continue;
        }
        if DOCUMENT_TAGS
            .iter()
            .any(|t| t.as_bytes().eq_ignore_ascii_case(name))
        {
            m.document = true;
            return m;
        }
        m.tags += 1;
    }
    m
}

/// The text of the first `<title>` in `s`: tags inside dropped, whitespace collapsed,
/// `&amp;` decoded, neutral and cut to `max_chars`. `None` when there is none or it is empty.
pub fn html_title(s: &str, max_chars: usize) -> Option<String> {
    let lower = s.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let start = open + lower[open..].find('>')? + 1;
    let end = start + lower[start..].find("</title")?;
    let mut t = String::new();
    let mut in_tag = false;
    for c in s.get(start..end)?.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => t.push(c),
            _ => {}
        }
    }
    let t = t
        .replace("&amp;", "&")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!t.is_empty()).then(|| line(&t, max_chars))
}

fn looks_like_syntax(line: &str) -> bool {
    let t = line.trim_start().as_bytes();
    SYNTAX_STARTS
        .iter()
        .any(|p| t.len() >= p.len() && t[..p.len()].eq_ignore_ascii_case(p.as_bytes()))
}

/// Drops ANSI escape sequences (CSI, OSC, two-byte), C0 and C1 controls other than `\n` and
/// `\t`, DEL, and the Unicode bidi overrides and isolates. `\r\n` becomes `\n`; a lone `\r`
/// (progress redraws) is dropped.
fn strip_controls(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\n' | '\t' => out.push(c),
            '\u{1b}' => match it.next() {
                // CSI: parameters and intermediates up to a final byte in @..~.
                Some('[') => {
                    for d in it.by_ref() {
                        if ('@'..='~').contains(&d) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ST (ESC \).
                Some(']') => {
                    while let Some(d) = it.next() {
                        if d == '\u{7}' {
                            break;
                        }
                        if d == '\u{1b}' {
                            if it.peek() == Some(&'\\') {
                                it.next();
                            }
                            break;
                        }
                    }
                }
                _ => {}
            },
            c if c.is_control() => {}
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailers_fences_and_links_are_quoted() {
        let s = "ok\nremaining:\n- a\n  Truncated: x\n```rust\n[click](#12)\n| already";
        let n = neutral(s);
        assert_eq!(
            n,
            "ok\n| remaining:\n- a\n|   Truncated: x\n| ```rust\n[click] (#12)\n| | already"
        );
    }

    #[test]
    fn controls_and_ansi_are_dropped() {
        let s = "a\u{1b}[31mred\u{1b}[0m\r\nb\u{1b}]0;title\u{7}c\rd\u{202e}e\u{0}";
        assert_eq!(neutral(s), "ared\nbcde");
    }

    #[test]
    fn capped_cuts_on_char_boundary_with_trailer() {
        let s = "\u{e9}".repeat(10);
        let c = capped(&s, 4, "narrow it");
        assert_eq!(
            c,
            "\u{e9}\u{e9}\u{e9}\u{e9}\ntruncated: kept 4 of 10 chars; narrow it"
        );
        assert_eq!(capped("short", 10, ""), "short");
    }

    #[test]
    fn html_is_recognised_by_its_tags() {
        for html in [
            "<!DOCTYPE html><p>x",
            "  <html lang=en>",
            "<?xml version=\"1.0\"?><a/>",
            "error\n<BODY>\nblocked",
            "<script src=x></script>",
            "<div class=a>one</div><p>two</p>",
            "<!-- a --><br/><span>x</span>",
        ] {
            assert!(looks_like_html(html), "{html}");
        }
        for text in [
            "plain text",
            "use a <tag> here",
            "a < b and c > d",
            "x <b>y</b>",
            "1<2, 3>2, <3",
            "<<< >>>",
            "<",
            "List<String> and Map<K, V>",
        ] {
            assert!(!looks_like_html(text), "{text}");
        }
        assert!(looks_like_html_document("<html><body>x"));
        assert!(!looks_like_html_document(
            "<details><summary>x</summary><br></details>"
        ));
        assert!(looks_like_html(
            "<details><summary>x</summary><br></details>"
        ));
    }

    #[test]
    fn html_titles_are_short_neutral_text() {
        let page = "<html><head><TITLE>\n Just a <b>moment</b> &amp; wait\n</title></head>";
        assert_eq!(
            html_title(page, 80).as_deref(),
            Some("Just a moment & wait")
        );
        assert_eq!(
            html_title("<title>remaining: x</title>", 80).as_deref(),
            Some("| remaining: x")
        );
        assert_eq!(html_title("<title> </title>", 80), None);
        assert_eq!(html_title("<p>no title</p>", 80), None);
        assert_eq!(
            html_title(&format!("<title>{}</title>", "a".repeat(50)), 10).as_deref(),
            Some("aaaaaaa...")
        );
    }

    #[test]
    fn line_is_one_line_and_capped() {
        assert_eq!(line("a\nb", 10), "a b");
        assert_eq!(line("abcdefghij", 6), "abc...");
    }
}
