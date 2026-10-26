//! H-2 and rule 2 for page-sourced strings (pr-ready 5, B-152): a page can echo what the
//! program typed (a greeting, a URL after a GET form, a title, a dialog). Every string an
//! act result carries is scrubbed against the values of the vault keys the program typed,
//! resolved inside `ctx.resolve_into` (the values never leave the sink), twice: on the raw
//! page string, before any escaping or cap can cut a value (D-615 is the focus rule; this is
//! D-616), and again on the shown string. Both passes also replace a value prefix of
//! [`PREFIX_MIN`] or more characters that ends where a string was cut: at its end, at `…`
//! (the page index's own cut) or at `...` (the driver's caps). Matching is ASCII
//! case-insensitive, as core's scrubber is (D-102).

use crate::diff::{clean_url, page_text};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use zeroize::Zeroizing;

/// A value prefix this long (in characters) or longer that ends at a cut counts as the value.
pub(crate) const PREFIX_MIN: usize = 8;
/// Cut-prefix rewrites per string before the whole string is withheld (each needs its own
/// cut, so a page string rarely has more than a few; fail closed past this).
const MAX_REWRITES: usize = 64;
/// What a string becomes when it does not settle.
pub(crate) const WITHHELD: &str = "(withheld: it held a vault value)";

/// `encodeURIComponent`'s set, and the form encoding's (space as `+` is added separately).
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');
const FORM: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'*')
    .remove(b' ');

/// The spellings of a typed vault value that can reach a result: as typed, URL-encoded
/// (component and form), each also as the result's own escaping renders it (page text,
/// cleaned URL), and as the actionability whys neutralise page fragments (`_` and `?` for
/// anything but letters and digits). Case is folded by the matcher.
pub(crate) fn spellings(v: &str) -> Vec<Zeroizing<String>> {
    let neutral: String = v
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let why: String = v
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " ._-:,'<>".contains(c) {
                c
            } else {
                '?'
            }
        })
        .collect();
    let bases = [
        v.to_string(),
        utf8_percent_encode(v, URI_COMPONENT).to_string(),
        utf8_percent_encode(v, FORM).to_string().replace(' ', "+"),
    ];
    let mut out: Vec<Zeroizing<String>> = Vec::new();
    let mut add = |s: String| {
        let s = Zeroizing::new(s);
        if !s.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(&s)) {
            out.push(s);
        }
    };
    for b in bases {
        add(page_text(&b, usize::MAX));
        add(clean_url(&b));
        add(b);
    }
    add(neutral);
    add(why);
    out
}

/// ASCII case-insensitive find of `needle` in `hay` from byte `from`, at char boundaries.
fn find_ci(hay: &str, needle: &str, from: usize) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.is_empty() || n.len() > h.len() {
        return None;
    }
    (from..=h.len() - n.len())
        .find(|&i| hay.is_char_boundary(i) && h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// Byte offsets where `s` may have been cut: before each `…` and `...`, and its end.
fn cuts(s: &str) -> Vec<usize> {
    let mut v: Vec<usize> = s.match_indices('…').map(|(i, _)| i).collect();
    v.extend(s.match_indices("...").map(|(i, _)| i));
    v.push(s.len());
    v.sort_unstable();
    v.dedup();
    v
}

/// The longest prefix (at least [`PREFIX_MIN`] characters) of a spelling that `s` has right
/// before a cut: (start, end) in `s`. Anchored on each occurrence of a spelling's first
/// [`PREFIX_MIN`] characters, so a page full of dots costs no more than one without.
fn cut_prefix(s: &str, sp: &[Zeroizing<String>]) -> Option<(usize, usize)> {
    let cuts = cuts(s);
    let mut best: Option<(usize, usize)> = None;
    for x in sp {
        // A spelling of exactly PREFIX_MIN characters has no shorter prefix to look for.
        let Some((head, _)) = x.char_indices().nth(PREFIX_MIN) else {
            continue;
        };
        let mut at = 0usize;
        while let Some(st) = find_ci(s, &x[..head], at) {
            let same = s.as_bytes()[st..]
                .iter()
                .zip(x.as_bytes())
                .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
                .count();
            let end = cuts
                .iter()
                .rev()
                .copied()
                .find(|&m| m >= st + head && m <= st + same && x.is_char_boundary(m - st));
            if let Some(m) = end {
                if best.is_none_or(|(a, b)| b - a < m - st) {
                    best = Some((st, m));
                }
            }
            at = st + 1;
        }
    }
    best
}

/// Replace every spelling in `s`, and every cut prefix of one, with `lit`.
pub(crate) fn scrub_with(s: &mut String, sp: &[Zeroizing<String>], lit: &str) {
    for x in sp {
        let mut at = 0usize;
        while let Some(i) = find_ci(s, x, at) {
            s.replace_range(i..i + x.len(), lit);
            at = i + lit.len();
        }
    }
    let mut rewrites = 0usize;
    while let Some((a, b)) = cut_prefix(s, sp) {
        s.replace_range(a..b, lit);
        rewrites += 1;
        if rewrites > MAX_REWRITES {
            *s = WITHHELD.into();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: &str = "Correct-Horse-battery-staple-9f3a";

    fn scrubbed(s: &str) -> String {
        let mut s = s.to_string();
        scrub_with(&mut s, &spellings(V), "@vault(pw)");
        s
    }

    #[test]
    fn whole_values_go_in_every_spelling_and_case() {
        assert_eq!(scrubbed(&format!("hi {V}!")), "hi @vault(pw)!");
        assert_eq!(
            scrubbed(&format!("hi {}!", V.to_ascii_uppercase())),
            "hi @vault(pw)!"
        );
        let sp = spellings("p@ss w\"rd#1x");
        let all: Vec<&str> = sp.iter().map(|s| s.as_str()).collect();
        for want in [
            "p@ss w\"rd#1x",
            "p%40ss%20w%22rd%231x",
            "p%40ss+w%22rd%231x",
            "p@ss w'rd＃1x",
            "p_ss_w_rd_1x",
        ] {
            assert!(all.contains(&want), "{want} in {all:?}");
        }
    }

    #[test]
    fn a_prefix_that_ends_at_a_cut_goes_too() {
        // The driver's caps end in `...`, the page index's in `…`, a raw cap at the end.
        for cut in ["...", "…", ""] {
            let s = format!("Signed in as {}{cut}", &V[..12]);
            assert_eq!(
                scrubbed(&s),
                format!("Signed in as @vault(pw){cut}"),
                "{cut:?}"
            );
        }
        // Short prefixes, and prefixes that do not end at a cut, are ordinary text.
        assert_eq!(scrubbed("Correct..."), "Correct...");
        let mid = format!("{} and more", &V[..12]);
        assert_eq!(scrubbed(&mid), mid);
        // A neutralised why fragment (`@` and `.` as `_`), cut at 40 with `...`.
        let v = "p@ss.w0rd-long-enough";
        let mut why = "covered by div#p_ss_w0rd-lo...".to_string();
        scrub_with(&mut why, &spellings(v), "@vault(pw)");
        assert_eq!(why, "covered by div#@vault(pw)...");
    }

    #[test]
    fn many_cuts_stay_cheap_and_many_prefixes_are_withheld() {
        let dots = ".".repeat(2000);
        let t = std::time::Instant::now();
        assert_eq!(scrubbed(&dots), dots);
        assert!(t.elapsed() < std::time::Duration::from_millis(200));
        // More cut prefixes than any page string needs: withheld whole (fail closed).
        let many = format!("{}...", &V[..10]).repeat(MAX_REWRITES + 2);
        assert_eq!(scrubbed(&many), WITHHELD);
    }

    #[test]
    fn non_ascii_text_is_matched_at_char_boundaries() {
        let v = "pässwörd-long-value";
        let mut s = format!("é {v}… ü{}", &v[..12]);
        scrub_with(&mut s, &spellings(v), "@vault(k)");
        assert_eq!(s, "é @vault(k)… ü@vault(k)");
    }
}
