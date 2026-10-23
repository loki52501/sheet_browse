//! B-6.2 find: BM25 + synonyms + role prior + state + group bonus (D-51, D-54, D-202).
//! Owned by coder A (T-10).
//!
//! Per query part (split on commas), per candidate row:
//!
//! ```text
//! lexical = 3*BM25(name) + 1.5*BM25(nearby) + 0.5*BM25(heading)
//! score   = lexical + 2 (role prior) + (1 visible | -2 covered or disabled) + 1 (group bonus)
//! ```
//!
//! BM25 is k1 = 1.2, b = 0.75 with IDF `ln(1 + (N - n + 0.5) / (n + 0.5))` and the average
//! field length taken per field over every row of the page's index (D-205). Words the caller
//! typed weigh 1; words added by synonym expansion weigh 0.5 (D-205). The role prior fires on
//! the typed words and phrases only, not on synonyms (D-205). A row needs `lexical > 0` to be
//! a Match unless the part is a bare forced role (`:button`) (D-205). The `?` mark compares
//! the lexical part with 3 (D-54). Hidden rows and rows inside CHALLENGE or OPAQUE frames are
//! never candidates (D-202, D-55); ties break by document order (D-202).

use crate::data::{forced_roles, prior_max_words, role_prior, synonym_max_words, synonyms};
use crate::sheet::{sealed_frames, sheet_line};
use crate::tokens::words;
use crate::{ElemRef, FindHit, FindOpts, HitReason, IndexRow, TextBlock};
use std::collections::{HashMap, HashSet};

const K1: f32 = 1.2;
const B: f32 = 0.75;
const W_NAME: f32 = 3.0;
const W_NEARBY: f32 = 1.5;
const W_HEADING: f32 = 0.5;
const ROLE_PRIOR: f32 = 2.0;
const VISIBLE: f32 = 1.0;
const COVERED_OR_DISABLED: f32 = -2.0;
const GROUP_BONUS: f32 = 1.0;
/// Weight of a query word that came from synonym expansion (D-205).
const SYNONYM_WEIGHT: f32 = 0.5;
/// Lexical score below which a hit is weak and printed with `?` (D-54).
const WEAK_BELOW: f32 = 3.0;

/// One BM25 field (name, nearby or heading) over the whole index.
pub(crate) struct Field {
    docs: Vec<Vec<String>>,
    df: HashMap<String, usize>,
    avgdl: f32,
}

impl Field {
    pub(crate) fn new<'a>(texts: impl Iterator<Item = Option<&'a str>>) -> Field {
        let docs: Vec<Vec<String>> = texts.map(|t| t.map(words).unwrap_or_default()).collect();
        let mut df: HashMap<String, usize> = HashMap::new();
        for d in &docs {
            let uniq: HashSet<&String> = d.iter().collect();
            for w in uniq {
                *df.entry(w.clone()).or_default() += 1;
            }
        }
        let total: usize = docs.iter().map(Vec::len).sum();
        let avgdl = if docs.is_empty() {
            0.0
        } else {
            total as f32 / docs.len() as f32
        };
        Field { docs, df, avgdl }
    }

    fn idf(&self, term: &str) -> f32 {
        let n = self.docs.len() as f32;
        let df = self.df.get(term).copied().unwrap_or(0) as f32;
        (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
    }

    /// BM25 of row `i` against weighted query terms.
    pub(crate) fn score(&self, i: usize, terms: &[(String, f32)]) -> f32 {
        let Some(doc) = self.docs.get(i) else {
            return 0.0;
        };
        if doc.is_empty() || self.avgdl <= 0.0 {
            return 0.0;
        }
        let dl = doc.len() as f32;
        let norm = K1 * (1.0 - B + B * dl / self.avgdl);
        terms
            .iter()
            .map(|(t, w)| {
                let tf = doc.iter().filter(|d| *d == t).count() as f32;
                if tf == 0.0 {
                    0.0
                } else {
                    w * self.idf(t) * tf * (K1 + 1.0) / (tf + norm)
                }
            })
            .sum()
    }
}

/// A parsed query part.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Part {
    /// Weighted terms after synonym expansion: typed words 1.0, synonym words 0.5.
    pub(crate) terms: Vec<(String, f32)>,
    /// ARIA roles that get the +2 prior.
    pub(crate) prior: HashSet<String>,
    /// `name:role`: only rows with one of these roles are candidates.
    pub(crate) forced: Option<Vec<String>>,
}

fn ngrams(ws: &[String], max: usize) -> Vec<String> {
    let mut out = Vec::new();
    for n in 1..=max.min(ws.len()) {
        for win in ws.windows(n) {
            out.push(win.join(" "));
        }
    }
    out
}

pub(crate) fn parse_part(raw: &str) -> Part {
    let raw = raw.trim();
    let (text, forced) = match raw.rsplit_once(':') {
        Some((t, r)) => match forced_roles(r) {
            Some(roles) => (t.trim(), Some(roles)),
            None => (raw, None),
        },
        None => (raw, None),
    };
    let typed = words(text);
    let mut terms: Vec<(String, f32)> = Vec::new();
    let mut add = |t: String, w: f32| match terms.iter_mut().find(|(x, _)| *x == t) {
        Some((_, old)) => *old = old.max(w),
        None => terms.push((t, w)),
    };
    for w in &typed {
        add(w.clone(), 1.0);
    }
    for phrase in ngrams(&typed, synonym_max_words()) {
        for syn in synonyms(&phrase) {
            for w in words(syn) {
                add(w, SYNONYM_WEIGHT);
            }
        }
    }
    let prior: HashSet<String> = ngrams(&typed, prior_max_words())
        .iter()
        .flat_map(|p| role_prior(p).iter().cloned())
        .collect();
    Part {
        terms,
        prior,
        forced,
    }
}

struct Scorer<'a> {
    index: &'a [IndexRow],
    name: Field,
    nearby: Field,
    heading: Field,
    candidate: Vec<bool>,
}

impl<'a> Scorer<'a> {
    fn new(index: &'a [IndexRow]) -> Scorer<'a> {
        let sealed = sealed_frames(index);
        Scorer {
            index,
            name: Field::new(index.iter().map(|r| Some(r.name.as_str()))),
            nearby: Field::new(index.iter().map(|r| r.nearby.as_deref())),
            heading: Field::new(index.iter().map(|r| r.heading.as_deref())),
            candidate: index
                .iter()
                .map(|r| r.state.visible && !sealed.contains(&r.frame))
                .collect(),
        }
    }

    fn lexical(&self, i: usize, part: &Part) -> f32 {
        W_NAME * self.name.score(i, &part.terms)
            + W_NEARBY * self.nearby.score(i, &part.terms)
            + W_HEADING * self.heading.score(i, &part.terms)
    }

    /// `(row, lexical, score without group bonus)` for every eligible row of `part`.
    fn scored(&self, part: &Part) -> Vec<(usize, f32, f32)> {
        let bare_role = part.terms.is_empty() && part.forced.is_some();
        let mut out = Vec::new();
        for (i, row) in self.index.iter().enumerate() {
            if !self.candidate[i] {
                continue;
            }
            let role = row.role.to_lowercase();
            if let Some(forced) = &part.forced {
                if !forced.contains(&role) {
                    continue;
                }
            }
            let lex = self.lexical(i, part);
            if lex <= 0.0 && !bare_role {
                continue;
            }
            let mut s = lex;
            if part.prior.contains(&role) {
                s += ROLE_PRIOR;
            }
            s += if row.state.covered || !row.state.enabled {
                COVERED_OR_DISABLED
            } else {
                VISIBLE
            };
            out.push((i, lex, s));
        }
        out
    }
}

/// Sorts by score descending, then document order (D-202).
fn rank(v: &mut [(usize, f32, f32)]) {
    v.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
}

/// The form owning the most part top-1 hits (scored without the bonus); ties go to the
/// form that appears first in the document (D-202).
fn group_form(index: &[IndexRow], top1: &[usize]) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for &i in top1 {
        if let Some(f) = index[i].form.as_deref() {
            *counts.entry(f).or_default() += 1;
        }
    }
    let best = counts.values().copied().max()?;
    index
        .iter()
        .filter_map(|r| r.form.as_deref())
        .find(|f| counts.get(f) == Some(&best))
        .map(str::to_string)
}

pub(crate) fn find(index: &[IndexRow], text_blocks: &[TextBlock], opts: &FindOpts) -> Vec<FindHit> {
    let _ = text_blocks;
    let scorer = Scorer::new(index);
    let parts: Vec<Part> = opts
        .query
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(parse_part)
        .collect();

    let mut ranked: Vec<Vec<(usize, f32, f32)>> = parts.iter().map(|p| scorer.scored(p)).collect();
    for r in &mut ranked {
        rank(r);
    }
    let top1: Vec<usize> = ranked
        .iter()
        .filter_map(|r| r.first().map(|h| h.0))
        .collect();
    let winner = group_form(index, &top1);

    // best hit per row: (score, lexical max across parts, part)
    let mut best: HashMap<usize, (f32, f32, usize)> = HashMap::new();
    for (p, r) in ranked.iter_mut().enumerate() {
        if let Some(w) = &winner {
            for h in r.iter_mut() {
                if index[h.0].form.as_deref() == Some(w.as_str()) {
                    h.2 += GROUP_BONUS;
                }
            }
            rank(r);
        }
        for &(i, lex, s) in r.iter().take(opts.k) {
            let e = best.entry(i).or_insert((s, lex, p));
            if s > e.0 {
                e.0 = s;
                e.2 = p;
            }
            e.1 = e.1.max(lex);
        }
    }

    let mut hits: Vec<(usize, FindHit)> = best
        .into_iter()
        .map(|(i, (score, lex, part))| {
            (
                i,
                FindHit {
                    elem: index[i].elem(),
                    score,
                    weak: lex < WEAK_BELOW,
                    reason: HitReason::Match { part },
                },
            )
        })
        .collect();
    let mut have: HashSet<usize> = hits.iter().map(|h| h.0).collect();
    let mut append = |i: usize, reason: HitReason, hits: &mut Vec<(usize, FindHit)>| {
        if have.insert(i) {
            hits.push((
                i,
                FindHit {
                    elem: index[i].elem(),
                    score: 0.0,
                    weak: false,
                    reason,
                },
            ));
        }
    };
    for (i, row) in index.iter().enumerate() {
        if !scorer.candidate[i] {
            continue;
        }
        if row.flags.challenge {
            append(i, HitReason::Challenge, &mut hits);
        } else if row.flags.overlay {
            append(i, HitReason::Overlay, &mut hits);
        }
    }
    if let Some(w) = &winner {
        let submit = index.iter().enumerate().find(|(i, r)| {
            scorer.candidate[*i]
                && r.form.as_deref() == Some(w.as_str())
                && (r.flags.submit || r.state.submitish)
        });
        if let Some((i, _)) = submit {
            append(i, HitReason::WinningSubmit, &mut hits);
        }
    }
    hits.sort_by_key(|h| h.0);
    hits.into_iter().map(|h| h.1).collect()
}

/// Renders find hits as sheet lines, weak hits prefixed with `?` (D-203).
pub fn find_lines(index: &[IndexRow], hits: &[FindHit]) -> String {
    let by_ref: HashMap<ElemRef, &IndexRow> = index.iter().map(|r| (r.elem(), r)).collect();
    hits.iter()
        .filter_map(|h| {
            by_ref.get(&h.elem).map(|row| {
                let line = sheet_line(row);
                if h.weak {
                    format!("?{line}")
                } else {
                    line
                }
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Flags, State};

    fn row(id: u32, role: &str, name: &str) -> IndexRow {
        IndexRow {
            id,
            frame: 0,
            role: role.into(),
            name: name.into(),
            nearby: None,
            heading: None,
            state: State::default(),
            flags: Flags::default(),
            form: None,
            landmark: None,
            child_frame: None,
            bbox: None,
        }
    }

    fn q(index: &[IndexRow], query: &str, k: usize) -> Vec<FindHit> {
        find(
            index,
            &[],
            &FindOpts {
                query: query.into(),
                k,
            },
        )
    }

    fn ids(h: &[FindHit]) -> Vec<u32> {
        h.iter().map(|h| h.elem.id).collect()
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    /// Hand-computed BM25 on a three-row corpus.
    /// name docs: ["apple"], ["apple","pie"], ["cherry"]: N = 3, avgdl = 4/3.
    /// idf(apple) = ln(1 + (3-2+0.5)/(2+0.5)) = ln(1.6) = 0.470004
    /// row 0: dl = 1, norm = 1.2*(0.25 + 0.75*1/(4/3)) = 0.975
    ///        bm25 = 0.470004 * 2.2 / 1.975 = 0.523549
    /// row 1: dl = 2, norm = 1.2*(0.25 + 0.75*2/(4/3)) = 1.65
    ///        bm25 = 0.470004 * 2.2 / 2.65 = 0.390192
    #[test]
    fn bm25_hand_computed() {
        let f = Field::new(["apple", "apple pie", "cherry"].into_iter().map(Some));
        let t = vec![("apple".to_string(), 1.0)];
        assert!(close(f.idf("apple"), 0.470004), "{}", f.idf("apple"));
        assert!(close(f.score(0, &t), 0.523549), "{}", f.score(0, &t));
        assert!(close(f.score(1, &t), 0.390192), "{}", f.score(1, &t));
        assert_eq!(f.score(2, &t), 0.0);
        // weight scales linearly; unknown terms add nothing
        let half = vec![("apple".to_string(), 0.5), ("zzz".to_string(), 1.0)];
        assert!(close(f.score(0, &half), 0.523549 / 2.0));
        // idf(cherry) = ln(1 + 2.5/1.5) = ln(8/3)
        let c = vec![("cherry".to_string(), 1.0)];
        assert!(close(f.idf("cherry"), (8.0f32 / 3.0).ln()));
        // row 2: dl = 1 => same norm as row 0
        assert!(close(f.score(2, &c), (8.0f32 / 3.0).ln() * 2.2 / 1.975));
    }

    #[test]
    fn full_score_formula() {
        // 3 rows; one part "apple" on a textbox (prior? "apple" has none).
        let mut a = row(1, "textbox", "apple");
        a.nearby = Some("apple".into());
        a.heading = Some("apple".into());
        let idx = vec![a, row(2, "button", "apple pie"), row(3, "link", "cherry")];
        let h = q(&idx, "apple", 1);
        assert_eq!(ids(&h), [1]);
        // name idf over names (df 2 of 3); nearby and heading have df 1 of 3 and avgdl 1/3.
        let name = 0.523549f32;
        let idf1 = (1.0f32 + 2.5 / 1.5).ln();
        let norm1 = 1.2 * (0.25 + 0.75 * 1.0 / (1.0 / 3.0));
        let other = idf1 * 2.2 / (1.0 + norm1);
        let expect = 3.0 * name + 1.5 * other + 0.5 * other + 1.0;
        assert!(close(h[0].score, expect), "{} vs {expect}", h[0].score);
    }

    #[test]
    fn synonyms_expand_at_half_weight() {
        let p = parse_part("e-mail");
        assert!(p.terms.contains(&("e".into(), 1.0)));
        assert!(p.terms.contains(&("email".into(), 0.5)));
        let p = parse_part("email");
        assert!(p.terms.contains(&("email".into(), 1.0)));
        assert!(p.terms.contains(&("mail".into(), 0.5)));
        // a synonym that is also typed keeps weight 1
        let p = parse_part("email mail");
        assert!(p.terms.contains(&("mail".into(), 1.0)));
        // phrase lookup: "sign in" -> login
        let p = parse_part("sign in");
        assert!(p.terms.contains(&("login".into(), 0.5)));
        // a row named only by a synonym is found
        let idx = vec![row(1, "textbox", "Postcode"), row(2, "textbox", "City")];
        assert_eq!(ids(&q(&idx, "zip", 3)), [1]);
    }

    #[test]
    fn role_prior_on_typed_words_only() {
        let p = parse_part("register");
        assert!(p.prior.contains("button"));
        // "agree" expands to "confirm" (a button word) but the prior stays checkbox-only
        let p = parse_part("agree");
        assert!(p.prior.contains("checkbox") && !p.prior.contains("button"));
        // same name, the prior decides
        let idx = vec![row(1, "link", "Register"), row(2, "button", "Register")];
        let h = q(&idx, "register", 1);
        assert_eq!(ids(&h), [2]);
    }

    #[test]
    fn forced_role() {
        let idx = vec![
            row(1, "button", "Name"),
            row(2, "textbox", "Full name"),
            row(3, "link", "Name"),
        ];
        assert_eq!(ids(&q(&idx, "name:input", 1)), [2]);
        assert_eq!(ids(&q(&idx, "name:textbox", 1)), [2]);
        assert_eq!(ids(&q(&idx, "name:link", 3)), [3]);
        // bare forced role lists that role in document order
        assert_eq!(ids(&q(&idx, ":button", 3)), [1]);
        // unknown role suffix is plain text
        let p = parse_part("time: 10:30");
        assert!(p.forced.is_none());
        assert!(p.terms.iter().any(|(t, _)| t == "30"));
    }

    #[test]
    fn weak_mark_uses_lexical_part() {
        // A long name dilutes the match: lexical < 3, but total with prior + visible >= 3.
        let idx = vec![
            row(
                1,
                "textbox",
                "please enter the email you used when you first signed up here",
            ),
            row(2, "textbox", "City"),
            row(3, "textbox", "Street"),
            row(4, "textbox", "Phone"),
            row(5, "textbox", "Zip"),
        ];
        let h = q(&idx, "email", 3);
        assert_eq!(ids(&h), [1]);
        assert!(h[0].weak, "{:?}", h[0]);
        assert!(h[0].score >= 3.0, "{:?}", h[0]);
        assert!(find_lines(&idx, &h).starts_with("?#1 input \"please enter"));
        // a strong single match is not weak (a tiny corpus caps idf: N = 2 gives ln 2)
        let idx = vec![
            row(1, "textbox", "Email"),
            row(2, "button", "Go"),
            row(3, "link", "Home"),
            row(4, "link", "About"),
        ];
        let h = q(&idx, "email", 1);
        assert!(!h[0].weak);
        assert_eq!(find_lines(&idx, &h), "#1 input \"Email\"");
    }

    #[test]
    fn no_lexical_match_no_hit() {
        let idx = vec![row(1, "textbox", "City"), row(2, "textbox", "Street")];
        assert!(q(&idx, "email", 3).is_empty());
        assert!(q(&idx, "", 3).is_empty());
    }

    #[test]
    fn hidden_and_sealed_rows_never_match() {
        let mut hidden = row(1, "textbox", "Email");
        hidden.state.visible = false;
        let mut cap = row(2, "iframe", "captcha");
        cap.flags.challenge = true;
        cap.child_frame = Some(3);
        let mut inside = row(1, "textbox", "Email");
        inside.frame = 3;
        let idx = vec![hidden, cap, inside, row(4, "textbox", "Email")];
        let h = q(&idx, "email", 3);
        assert_eq!(ids(&h), [2, 4]);
        assert_eq!(h[0].reason, HitReason::Challenge);
    }

    #[test]
    fn covered_and_disabled_penalised() {
        let mut a = row(1, "button", "Save");
        a.state.covered = true;
        let mut b = row(2, "button", "Save");
        b.state.enabled = false;
        let c = row(3, "button", "Save");
        let idx = vec![a, b, c];
        let h = q(&idx, "save", 1);
        assert_eq!(ids(&h), [3]);
        let all = q(&idx, "save", 3);
        assert!(close(all[0].score + 3.0, all[2].score));
        assert!(close(all[1].score + 3.0, all[2].score));
    }

    #[test]
    fn ties_break_by_document_order() {
        let idx = vec![
            row(1, "link", "Docs"),
            row(2, "link", "Docs"),
            row(3, "link", "Docs"),
        ];
        assert_eq!(ids(&q(&idx, "docs", 1)), [1]);
        assert_eq!(ids(&q(&idx, "docs", 2)), [1, 2]);
    }

    fn two_forms() -> Vec<IndexRow> {
        let mut v = Vec::new();
        let mut a = row(1, "textbox", "Email");
        a.form = Some("f1".into());
        v.push(a);
        let mut b = row(2, "textbox", "Password");
        b.form = Some("f1".into());
        v.push(b);
        let mut s1 = row(3, "button", "Sign in");
        s1.form = Some("f1".into());
        s1.flags.submit = true;
        v.push(s1);
        let mut c = row(4, "textbox", "Email");
        c.form = Some("f2".into());
        v.push(c);
        let mut s2 = row(5, "button", "Subscribe");
        s2.form = Some("f2".into());
        s2.state.submitish = true;
        v.push(s2);
        v
    }

    #[test]
    fn group_bonus_and_winning_submit() {
        let idx = two_forms();
        // f1 wins (email tie -> #1 by order, password -> #2): bonus lifts #1 above #4
        let h = q(&idx, "email, password", 2);
        assert_eq!(ids(&h), [1, 2, 3, 4]);
        assert_eq!(h[2].reason, HitReason::WinningSubmit);
        let e1 = h.iter().find(|x| x.elem.id == 1).unwrap();
        let e4 = h.iter().find(|x| x.elem.id == 4).unwrap();
        assert!(close(e1.score, e4.score + 1.0));
        // k=1: no newsletter email
        assert_eq!(ids(&q(&idx, "email, password", 1)), [1, 2, 3]);
        // only "subscribe": f2 wins; its submitish button is the match, no duplicate
        let h = q(&idx, "subscribe", 1);
        assert_eq!(ids(&h), [5]);
        assert_eq!(h[0].reason, HitReason::Match { part: 0 });
    }

    #[test]
    fn group_tie_goes_to_first_form() {
        let mut idx = two_forms();
        idx[3].name = "Newsletter".into();
        // email -> f1 (#1), newsletter -> f2 (#4): 1:1, f1 appears first
        let h = q(&idx, "email, newsletter", 1);
        assert_eq!(ids(&h), [1, 3, 4]);
        assert_eq!(h[1].reason, HitReason::WinningSubmit);
    }

    #[test]
    fn dedupe_keeps_match_reason() {
        let mut idx = two_forms();
        idx[2].flags.overlay = true;
        let h = q(&idx, "email, sign in", 1);
        assert_eq!(ids(&h), [1, 3]);
        assert_eq!(h[1].reason, HitReason::Match { part: 1 });
        // same row top in two parts: one hit
        let h = q(&idx, "email, email", 1);
        assert_eq!(ids(&h), [1, 3]);
    }

    #[test]
    fn empty_query_only_appended_rows() {
        let mut idx = two_forms();
        idx[1].flags.overlay = true;
        let mut hidden_overlay = row(9, "region", "Old banner");
        hidden_overlay.flags.overlay = true;
        hidden_overlay.state.visible = false;
        idx.push(hidden_overlay);
        let h = q(&idx, " , ", 3);
        assert_eq!(ids(&h), [2]);
        assert_eq!(h[0].reason, HitReason::Overlay);
        assert_eq!(find_lines(&idx, &h), "#2 input \"Password\" OVERLAY");
    }
}
