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
//! typed weigh 1; words added by synonym expansion weigh 0.5 (D-205). A multi-word synonym is
//! one phrase term: it matches only where its words appear consecutively, and weighs 0.5 per
//! word (D-213). The role prior fires on the typed words and phrases only, not on synonyms
//! (D-205). A row needs query evidence, `lexical > 0` or the role prior, to be a Match; a
//! bare forced role (`:button`) lists every row of that role (D-214). The `?` mark compares
//! the lexical part with 3 (D-54). Hidden rows and rows inside CHALLENGE or OPAQUE frames are
//! never candidates (D-202, D-55); ties break by document order (D-202). CHALLENGE rows are
//! always appended, hidden or not (D-215).

use crate::data::{
    forced_roles, prior_max_words, role_noun, role_prior, synonym_max_words, synonyms,
};
use crate::sheet::{sealed_frames, sheet_line};
use crate::tokens::words;
use crate::{ElemRef, FindHit, FindOpts, FrameId, HitReason, IndexRow, TextBlock};
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

    /// Document frequency; a phrase term counts the docs that hold its words consecutively
    /// (D-213).
    fn df(&self, term: &[String]) -> usize {
        match term {
            [w] => self.df.get(w).copied().unwrap_or(0),
            _ => self.docs.iter().filter(|d| tf(d, term) > 0).count(),
        }
    }

    fn idf(&self, term: &[String]) -> f32 {
        let n = self.docs.len() as f32;
        let df = self.df(term) as f32;
        (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
    }

    /// IDF of every query term, computed once per part and field so a phrase's df scan is not
    /// repeated per row.
    pub(crate) fn idfs(&self, q: &Query) -> Vec<f32> {
        q.words.iter().map(|t| self.idf(t)).collect()
    }

    /// BM25 of row `i` against the weighted query terms. Terms of one synonym group are
    /// alternatives: the row keeps the best of them, and groups add (D-217).
    pub(crate) fn score(&self, i: usize, q: &Query, idfs: &[f32]) -> f32 {
        let Some(doc) = self.docs.get(i) else {
            return 0.0;
        };
        if doc.is_empty() || self.avgdl <= 0.0 {
            return 0.0;
        }
        let dl = doc.len() as f32;
        let norm = K1 * (1.0 - B + B * dl / self.avgdl);
        let mut best = vec![0.0f32; q.groups];
        for (t, ws) in q.words.iter().enumerate() {
            let tf = tf(doc, ws) as f32;
            if tf == 0.0 {
                continue;
            }
            let (Some(w), Some(idf), Some(g)) = (q.weights.get(t), idfs.get(t), q.group.get(t))
            else {
                continue;
            };
            let c = w * idf * tf * (K1 + 1.0) / (tf + norm);
            if let Some(b) = best.get_mut(*g) {
                *b = b.max(c);
            }
        }
        best.iter().sum()
    }
}

/// Occurrences of `term` (one word, or a phrase as consecutive words) in `doc`.
fn tf(doc: &[String], term: &[String]) -> usize {
    if term.is_empty() || term.len() > doc.len() {
        return 0;
    }
    doc.windows(term.len()).filter(|win| *win == term).count()
}

/// A part's terms in scoring form: split words, weights and synonym groups (D-217).
pub(crate) struct Query {
    words: Vec<Vec<String>>,
    weights: Vec<f32>,
    group: Vec<usize>,
    groups: usize,
}

impl Query {
    pub(crate) fn new(part: &Part) -> Query {
        Query {
            words: part
                .terms
                .iter()
                .map(|(t, _)| t.split(' ').map(str::to_string).collect())
                .collect(),
            weights: part.terms.iter().map(|(_, w)| *w).collect(),
            group: part.groups.clone(),
            groups: part.groups.iter().map(|g| g + 1).max().unwrap_or(0),
        }
    }
}

/// A parsed query part.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Part {
    /// Weighted terms after synonym expansion: typed words 1.0, synonym words 0.5, a synonym
    /// phrase (words joined by one space, matched as consecutive words) 0.5 per word (D-213),
    /// a spelling variant of the typed n-gram it expands 1.0 (D-217).
    pub(crate) terms: Vec<(String, f32)>,
    /// Group of each term, parallel to `terms`: each typed word is its own group; the
    /// synonyms of one typed n-gram share a group and do not add up (D-217).
    pub(crate) groups: Vec<usize>,
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

/// Roles named by role nouns in the typed words, longest noun first: a word inside a
/// matched longer noun (`radio button`) does not also count alone (`button`).
fn noun_roles(ws: &[String], max: usize) -> HashSet<String> {
    let mut covered = vec![false; ws.len()];
    let mut out = HashSet::new();
    for n in (1..=max.min(ws.len())).rev() {
        for start in 0..=ws.len() - n {
            if covered[start..start + n].iter().any(|c| *c) {
                continue;
            }
            let roles = role_noun(&ws[start..start + n].join(" "));
            if !roles.is_empty() {
                out.extend(roles.iter().cloned());
                covered[start..start + n].iter_mut().for_each(|c| *c = true);
            }
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
    let mut groups: Vec<usize> = Vec::new();
    let mut next_group = 0usize;
    // A term already present keeps its group and the higher weight.
    let mut add = |t: String, w: f32, g: usize| match terms.iter_mut().find(|(x, _)| *x == t) {
        Some((_, old)) => *old = old.max(w),
        None => {
            terms.push((t, w));
            groups.push(g);
        }
    };
    for w in &typed {
        add(w.clone(), 1.0, next_group);
        next_group += 1;
    }
    let grams = ngrams(&typed, synonym_max_words().max(1));
    let typed_grams: HashSet<&str> = grams.iter().map(String::as_str).collect();
    for phrase in &grams {
        let group = next_group;
        next_group += 1;
        let squashed = phrase.replace(' ', "");
        for syn in synonyms(phrase) {
            let ws = words(syn);
            let n = ws.len();
            let term = ws.join(" ");
            // A phrase the caller typed is already scored through its typed words.
            if n == 0 || (n > 1 && typed_grams.contains(term.as_str())) {
                continue;
            }
            // `e mail` for `email` (or the reverse) is the typed word respelled (D-217).
            let weight = if term.replace(' ', "") == squashed {
                1.0
            } else {
                SYNONYM_WEIGHT * n as f32
            };
            add(term, weight, group);
        }
    }
    // A role the caller named (`help button`) replaces the task-word priors (D-214).
    let named = noun_roles(&typed, prior_max_words());
    let prior: HashSet<String> = if named.is_empty() {
        ngrams(&typed, prior_max_words())
            .iter()
            .flat_map(|p| role_prior(p).iter().cloned())
            .collect()
    } else {
        named
    };
    Part {
        terms,
        groups,
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
    sealed: HashSet<FrameId>,
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
            sealed,
        }
    }

    /// `(row, lexical, score without group bonus)` for every eligible row of `part`.
    fn scored(&self, part: &Part) -> Vec<(usize, f32, f32)> {
        let bare_role = part.terms.is_empty() && part.forced.is_some();
        let query = Query::new(part);
        let fields = [
            (W_NAME, &self.name, self.name.idfs(&query)),
            (W_NEARBY, &self.nearby, self.nearby.idfs(&query)),
            (W_HEADING, &self.heading, self.heading.idfs(&query)),
        ];
        let lexical = |i: usize| -> f32 {
            fields
                .iter()
                .map(|(w, f, idfs)| w * f.score(i, &query, idfs))
                .sum()
        };
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
            let lex = lexical(i);
            let prior = part.prior.contains(&role);
            // Query evidence (D-214): text, or the role the query names. The state term alone
            // says nothing about the query.
            if lex <= 0.0 && !prior && !bare_role {
                continue;
            }
            let mut s = lex;
            if prior {
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
    // `k = 0` is the documented default, not "drop every Match" (D-221).
    let k = if opts.k == 0 {
        crate::DEFAULT_FIND_K
    } else {
        opts.k
    };
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
        for &(i, lex, s) in r.iter().take(k) {
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
        // Every CHALLENGE row outside a sealed frame is reported, hidden or not: producer
        // visibility is approximate and a challenge means stop (D-5, D-215).
        if row.flags.challenge && !scorer.sealed.contains(&row.frame) {
            append(i, HitReason::Challenge, &mut hits);
        } else if row.flags.overlay && scorer.candidate[i] {
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
    /// Scores `terms` on row `i`, each term its own group.
    fn bm25(f: &Field, i: usize, terms: &[(&str, f32)]) -> f32 {
        let part = Part {
            terms: terms.iter().map(|(t, w)| (t.to_string(), *w)).collect(),
            groups: (0..terms.len()).collect(),
            prior: HashSet::new(),
            forced: None,
        };
        let q = Query::new(&part);
        f.score(i, &q, &f.idfs(&q))
    }

    fn idf(f: &Field, t: &str) -> f32 {
        f.idf(&words(t))
    }

    #[test]
    fn bm25_hand_computed() {
        let f = Field::new(["apple", "apple pie", "cherry"].into_iter().map(Some));
        let t = [("apple", 1.0)];
        assert!(close(idf(&f, "apple"), 0.470004), "{}", idf(&f, "apple"));
        assert!(close(bm25(&f, 0, &t), 0.523549), "{}", bm25(&f, 0, &t));
        assert!(close(bm25(&f, 1, &t), 0.390192), "{}", bm25(&f, 1, &t));
        assert_eq!(bm25(&f, 2, &t), 0.0);
        // weight scales linearly; unknown terms add nothing
        let half = [("apple", 0.5), ("zzz", 1.0)];
        assert!(close(bm25(&f, 0, &half), 0.523549 / 2.0));
        // idf(cherry) = ln(1 + 2.5/1.5) = ln(8/3)
        let c = [("cherry", 1.0)];
        assert!(close(idf(&f, "cherry"), (8.0f32 / 3.0).ln()));
        // row 2: dl = 1 => same norm as row 0
        assert!(close(bm25(&f, 2, &c), (8.0f32 / 3.0).ln() * 2.2 / 1.975));
        // phrase df counts docs holding the words consecutively
        assert!(close(idf(&f, "apple pie"), (8.0f32 / 3.0).ln()));
        assert_eq!(f.df(&words("pie apple")), 0);
    }

    #[test]
    fn synonym_group_is_alternatives_not_a_sum() {
        let f = Field::new(["e mail", "email"].into_iter().map(Some));
        let one = Part {
            terms: vec![("e mail".into(), 1.0), ("mail".into(), 0.5)],
            groups: vec![0, 0],
            prior: HashSet::new(),
            forced: None,
        };
        let q = Query::new(&one);
        let only_phrase = bm25(&f, 0, &[("e mail", 1.0)]);
        assert!(close(f.score(0, &q, &f.idfs(&q)), only_phrase));
        // separate groups add
        let two = bm25(&f, 0, &[("e mail", 1.0), ("mail", 0.5)]);
        assert!(two > only_phrase);
    }

    #[test]
    fn phrase_df_is_not_rescanned_per_row() {
        // audit round 2 item 10: 5000 rows holding a synonym phrase. With the df scan per
        // row this was quadratic (seconds in release); now it is one scan per term and field.
        let idx: Vec<IndexRow> = (1..=5000)
            .map(|i| row(i, "textbox", &format!("item {i} postal code area label")))
            .collect();
        let h = q(&idx, "zip", 1);
        assert_eq!(ids(&h), [1]);
    }

    #[test]
    fn exact_typed_name_beats_synonym_spelling() {
        // audit round 2 item 4: `email` must pick `Email`, not the newsletter `E-mail`
        let mut idx = vec![
            row(1, "textbox", "Email"),
            row(2, "textbox", "Password"),
            row(3, "button", "Sign in"),
        ];
        for r in &mut idx {
            r.form = Some("f1".into());
        }
        idx[2].flags.submit = true;
        for (i, name) in [
            "Home", "About", "Pricing", "Blog", "Careers", "Press", "Terms", "Privacy", "Help",
            "Status", "Docs", "Contact",
        ]
        .iter()
        .enumerate()
        {
            idx.push(row(10 + i as u32, "link", name));
        }
        let mut e = row(40, "textbox", "E-mail");
        e.form = Some("f2".into());
        idx.push(e);
        let mut s = row(41, "button", "Subscribe");
        s.form = Some("f2".into());
        s.flags.submit = true;
        idx.push(s);
        for query in ["email", "Email"] {
            let h = q(&idx, query, 1);
            assert_eq!(ids(&h), [1, 3], "{query}: {}", find_lines(&idx, &h));
            assert!(!h[0].weak);
        }
        // the respelled query still finds the other spelling strongly
        let h = q(&idx, "e-mail", 2);
        assert_eq!(ids(&h)[..2], [1, 40], "{}", find_lines(&idx, &h));
        assert!(h.iter().take(2).all(|x| !x.weak), "{h:?}");
        let top = q(&idx, "e-mail", 1);
        assert!(ids(&top).contains(&40), "{}", find_lines(&idx, &top));
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
        // a respelling of the typed n-gram weighs as typed (D-217)
        assert!(p.terms.contains(&("email".into(), 1.0)));
        assert!(p.terms.contains(&("email address".into(), 1.0)));
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
        let h = q(&idx, "zip", 1);
        assert_eq!(ids(&h), [1]);
        // City has only the textbox prior: it ranks below, weak (D-214)
        let h = q(&idx, "zip", 3);
        assert_eq!(ids(&h), [1, 2]);
        assert!(h[1].weak && h[0].score > h[1].score);
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
        let h = q(&idx, "email", 1);
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
    fn no_query_evidence_no_hit() {
        let idx = vec![row(1, "textbox", "City"), row(2, "link", "Street")];
        // no text match and no role named: the state term alone is not evidence (D-214)
        assert!(q(&idx, "banana", 3).is_empty());
        assert!(q(&idx, "", 3).is_empty());
        // `email` names the textbox role: prior + visible = 3, lexical 0, so `?` (D-54)
        let h = q(&idx, "email", 3);
        assert_eq!(ids(&h), [1]);
        assert!(h[0].weak && close(h[0].score, 3.0), "{:?}", h[0]);
    }

    #[test]
    fn role_prior_alone_finds_an_unnamed_searchbox() {
        let idx = vec![
            row(1, "searchbox", ""),
            row(2, "link", "Home"),
            row(3, "link", "About"),
        ];
        let h = q(&idx, "search", 1);
        assert_eq!(ids(&h), [1]);
        assert_eq!(find_lines(&idx, &h), "?#1 input");
    }

    #[test]
    fn role_nouns_pick_the_role() {
        let idx = vec![
            row(1, "link", "Help"),
            row(2, "button", "Help"),
            row(3, "link", "Home"),
            row(4, "link", "About"),
        ];
        assert_eq!(ids(&q(&idx, "help button", 1)), [2]);
        assert_eq!(ids(&q(&idx, "help link", 1)), [1]);
        let idx = vec![row(1, "link", "Details"), row(2, "button", "Details")];
        assert_eq!(ids(&q(&idx, "details button", 1)), [2]);
        // the longest noun wins: a radio button is a radio
        let p = parse_part("yes radio button");
        assert_eq!(p.prior, HashSet::from(["radio".to_string()]));
        let p = parse_part("check box");
        assert_eq!(p.prior, HashSet::from(["checkbox".to_string()]));
        // no noun: task-word priors as before
        assert!(parse_part("register").prior.contains("button"));
    }

    #[test]
    fn synonym_phrases_match_as_phrases() {
        let p = parse_part("zip");
        assert!(p.terms.contains(&("postal code".into(), 1.0)), "{p:?}");
        assert!(!p.terms.iter().any(|(t, _)| t == "code"));
        let p = parse_part("subscribe");
        assert!(!p.terms.iter().any(|(t, _)| t == "sign" || t == "in"));
        // a phrase the caller typed is not added again as a synonym phrase
        let p = parse_part("phone number");
        assert!(!p.terms.iter().any(|(t, _)| t == "phone number"));

        // login form: "subscribe" must not point at the login submit
        let mut idx = vec![
            row(1, "textbox", "Email"),
            row(2, "textbox", "Password"),
            row(3, "button", "Sign in"),
            row(4, "checkbox", "Send me product news"),
            row(5, "link", "Forgot password"),
            row(6, "link", "Privacy"),
        ];
        for r in idx.iter_mut().take(4) {
            r.form = Some("f1".into());
        }
        idx[2].flags.submit = true;
        let h = q(&idx, "subscribe", 1);
        assert!(
            h.iter()
                .all(|x| x.elem.id != 3 || x.reason == HitReason::WinningSubmit),
            "{h:?}"
        );

        let idx = vec![
            row(1, "textbox", "Card number"),
            row(2, "textbox", "Promo code"),
            row(3, "button", "Sign in"),
            row(4, "textbox", "City"),
            row(5, "textbox", "Street"),
        ];
        for (query, wrong) in [("phone", 1), ("zip", 2), ("register", 3)] {
            let h = q(&idx, query, 1);
            let line = find_lines(&idx, &h);
            assert!(
                h.iter().all(|x| x.elem.id != wrong || x.weak),
                "{query}: {line}"
            );
            // whatever comes back has no text match, so it is marked weak
            assert!(h.iter().all(|x| x.weak), "{query}: {line}");
        }

        // a real phrase match still scores
        let idx = vec![
            row(1, "textbox", "Postal code"),
            row(2, "textbox", "Promo code"),
        ];
        assert_eq!(ids(&q(&idx, "zip", 1)), [1]);
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
    fn hidden_challenge_is_still_appended() {
        let mut cap = row(2, "iframe", "reCAPTCHA");
        cap.flags.challenge = true;
        cap.state.visible = false;
        let idx = vec![row(1, "link", "Home"), cap];
        let h = q(&idx, "home", 1);
        assert_eq!(ids(&h), [1, 2]);
        assert_eq!(h[1].reason, HitReason::Challenge);
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

    #[test]
    fn k_defaults_to_three_and_zero_means_the_default() {
        // B-32 item 5, D-221
        assert_eq!(FindOpts::default().k, 3);
        assert!(FindOpts::default().query.is_empty());
        let idx: Vec<IndexRow> = [
            "Save draft",
            "Save copy",
            "Save all",
            "Save page",
            "Save as",
        ]
        .iter()
        .enumerate()
        .map(|(i, n)| row(i as u32 + 1, "button", n))
        .collect();
        let matches = |h: &[FindHit]| {
            h.iter()
                .filter(|h| matches!(h.reason, HitReason::Match { .. }))
                .count()
        };
        let three = q(&idx, "save", 3);
        assert_eq!(matches(&three), 3, "{three:?}");
        let zero = q(&idx, "save", 0);
        assert_eq!(zero, three, "k = 0 must behave as k = 3");
        let dflt = find(
            &idx,
            &[],
            &FindOpts {
                query: "save".into(),
                ..FindOpts::default()
            },
        );
        assert_eq!(dflt, three);
        assert_eq!(matches(&q(&idx, "save", 1)), 1);
        assert_eq!(matches(&q(&idx, "save", 10)), 5);
    }
}
