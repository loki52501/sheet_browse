//! Shared test doubles (D-108): [`MemStore`], an in-memory [`MemoryStore`] seeded like
//! `migrations/0001_init.sql`, and [`ScriptedAsker`], an [`Asker`] that answers from a
//! queue and records every card.
//!
//! Compiled for core's own tests and, in other crates, through the `testing` feature:
//! `harness-core = { workspace = true, features = ["testing"] }` under `[dev-dependencies]`.
//! Nothing here talks to Postgres, a model, or the network.

use crate::class::Mode;
use crate::gate::{rule_class_key, rule_domain, rule_key, AskCard, AskOutcome, Asker, OnTimeout};
use crate::memory::{
    contact_key, ts_now, Baseline, Contact, Fact, FactId, Filter, GateRule, Hit, LedgerEvent,
    LedgerId, MemoryError, MemoryStore, Node, NodeId, Result, Review, ReviewId, Run, RunStatus,
    Schedule, Source, ToolHealth, Ts, Why,
};
use crate::run::RunId;
use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Mutex, MutexGuard};

/// `neighbors` caps (M-5).
const MAX_DEPTH: u8 = 4;
const MAX_NEIGHBORS: usize = 500;

/// The `gate.rules` rows migration 0001 inserts: `(domain, class, mode, note)`.
pub const SEED_RULES: [(&str, &str, Mode, &str); 6] = [
    ("*", "Pay", Mode::Ask, ""),
    ("*", "Account", Mode::Ask, ""),
    ("*", "Delete", Mode::Ask, ""),
    ("*", "Challenge", Mode::Stop, ""),
    (
        "*",
        "Submit:job_application",
        Mode::Ask,
        "job submissions are mine (H-1.2)",
    ),
    (
        "*",
        "Submit:signature",
        Mode::Ask,
        "signatures are mine (H-1.2)",
    ),
];

#[derive(Debug, Clone)]
struct Edge {
    src: NodeId,
    rel: String,
    dst: NodeId,
    #[allow(dead_code)]
    source: Source,
}

#[derive(Default)]
struct Inner {
    next_fact: i64,
    facts: BTreeMap<i64, Fact>,
    next_node: i64,
    nodes: BTreeMap<i64, Node>,
    edges: Vec<Edge>,
    contacts: HashMap<String, Contact>,
    next_review: i64,
    reviews: BTreeMap<i64, Review>,
    baselines: HashMap<String, Baseline>,
    next_ledger: i64,
    ledger: Vec<(LedgerId, LedgerEvent)>,
    tools: BTreeMap<(String, String), ToolHealth>,
    /// `(domain, class key)` → (rule, note).
    rules: HashMap<(String, String), (GateRule, String)>,
    runs: BTreeMap<String, Run>,
    next_schedule: i32,
    schedules: BTreeMap<i32, Schedule>,
}

/// In-memory [`MemoryStore`]. Same observable rules as the Postgres store where tests
/// depend on them: seeded gate rules, gate rules stored under the canonical key
/// ([`rule_key`], D-110) with Challenge rules `stop`, contacts keyed by [`contact_key`],
/// increasing ledger ids, append-only ledger, SM-2 reviews, every stored timestamp
/// truncated to microseconds (what `timestamptz` keeps), `neighbors` walking both edge
/// directions with M-5's caps ordered by (depth, node id), and the same `Invalid` checks
/// (empty node kind/name, edge relation, tool domain/name, contact). Timestamps are
/// truncated toward zero from 2000-01-01 as sqlx sends them, on writes and on the
/// `since` / `now` bounds. `gate_rule` canonicalises its lookup. `recall` is a
/// case-insensitive substring match over subject, key and value (no embeddings, every hit
/// `Why::Fts`); `search` follows D-115 with pg_trgm's `word_similarity` ported; SM-2
/// rounds a fractional interval up. A blank query finds nothing.
pub struct MemStore {
    inner: Mutex<Inner>,
}

impl Default for MemStore {
    fn default() -> Self {
        MemStore::new()
    }
}

impl MemStore {
    /// A store with migration 0001's gate rules ([`SEED_RULES`]).
    pub fn new() -> MemStore {
        let store = MemStore::empty();
        {
            let mut g = store.lock();
            for (domain, class, mode, note) in SEED_RULES {
                g.rules.insert(
                    (domain.to_string(), class.to_string()),
                    (
                        GateRule {
                            mode,
                            strict: false,
                        },
                        note.to_string(),
                    ),
                );
            }
        }
        store
    }

    /// A store with no rows at all, not even the seed rules.
    pub fn empty() -> MemStore {
        MemStore {
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Adds (or bumps) a contact synchronously; same effect as `touch_contact(p, "test", "")`.
    /// Panics on a blank `person`, which `touch_contact` (and PgStore) refuse as `Invalid`:
    /// a test must not build a contact the real store could never hold.
    pub fn add_contact(&self, person: &str) {
        assert!(
            !contact_key(person).is_empty(),
            "MemStore::add_contact: blank person {person:?} (touch_contact refuses it)"
        );
        touch(&mut self.lock(), person, "test", "");
    }

    /// Sets a gate rule synchronously; same checks and canonical key as `gate_rule_set`.
    pub fn add_rule(&self, domain: &str, class: &str, mode: Mode, strict: bool) -> Result<()> {
        set_rule(
            &mut self.lock(),
            domain,
            class,
            GateRule { mode, strict },
            "",
        )
    }

    /// Writes a rule row verbatim without any validation, as a hand-edited `gate.rules`
    /// table could. Exists to prove the gate does not trust the table for D-5 (a Challenge
    /// `allow`).
    pub fn force_rule(&self, domain: &str, class: &str, rule: GateRule) {
        self.lock().rules.insert(
            (domain.to_string(), class.to_string()),
            (rule, String::new()),
        );
    }

    /// Every ledger event in insertion order, across runs (including `run_id = None`).
    pub fn ledger(&self) -> Vec<(LedgerId, LedgerEvent)> {
        self.lock().ledger.clone()
    }

    /// The note stored with a gate rule, if the rule exists. The key is canonicalised
    /// first when it can be (so `rule_note("Bank.com", "submit")` finds `(bank.com, Submit)`).
    pub fn rule_note(&self, domain: &str, class: &str) -> Option<String> {
        let d = rule_domain(domain).unwrap_or_else(|_| domain.to_string());
        let k = rule_class_key(class).unwrap_or_else(|_| class.to_string());
        self.lock().rules.get(&(d, k)).map(|(_, n)| n.clone())
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Truncated to the microsecond precision Postgres stores (`timestamptz`), the way sqlx
/// sends it: microseconds since 2000-01-01 truncated toward zero, so before 2000 a
/// sub-microsecond remainder rounds up, not down.
fn micros(t: Ts) -> Ts {
    let pg_epoch = DateTime::from_timestamp(946_684_800, 0).unwrap_or_default();
    (t - pg_epoch)
        .num_microseconds()
        .map(|us| pg_epoch + ChronoDuration::microseconds(us))
        .unwrap_or(t)
}

fn touch(g: &mut Inner, person: &str, channel: &str, note: &str) {
    let key = contact_key(person);
    let c = g.contacts.entry(key.clone()).or_insert_with(|| Contact {
        person: key,
        channel: None,
        last: None,
        count: 0,
        note: None,
    });
    c.count = c.count.saturating_add(1);
    c.last = Some(ts_now());
    if !channel.is_empty() {
        c.channel = Some(channel.to_string());
    }
    if !note.is_empty() {
        c.note = Some(note.to_string());
    }
}

/// Stores a rule under its canonical key ([`rule_key`]: canonical domain and class key,
/// D-5), exactly as every `gate_rule_set` backend must (D-110 (1)).
fn set_rule(g: &mut Inner, domain: &str, class: &str, rule: GateRule, note: &str) -> Result<()> {
    let key = rule_key(domain, class, &rule)?;
    g.rules.insert(key, (rule, note.to_string()));
    Ok(())
}

fn matches_filter(f: &Fact, filter: &Option<Filter>) -> bool {
    let Some(filter) = filter else { return true };
    if let Some(p) = &filter.subject_prefix {
        if !f.subject.starts_with(p.as_str()) {
            return false;
        }
    }
    if let Some(since) = filter.since {
        // PgStore binds `since` truncated to micros, like every stored timestamp.
        if f.seen_at < micros(since) {
            return false;
        }
    }
    if let Some(kind) = filter.source_kind {
        if f.source.kind != kind {
            return false;
        }
    }
    true
}

fn contains_ci(f: &Fact, q: &str) -> bool {
    [&f.subject, &f.key, &f.value]
        .iter()
        .any(|s| s.to_lowercase().contains(q))
}

/// D-115's case- and punctuation-insensitive form, as PgStore computes it in SQL:
/// lowercased, every run of non-alphanumeric characters one space, trimmed.
fn normalised(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut gap = false;
    for c in s.to_lowercase().chars() {
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

/// The cut-off of `<%`: `pg_trgm.word_similarity_threshold` as PgStore sets it on every
/// session (D-125), not pg_trgm's default 0.6.
const WORD_SIMILARITY_THRESHOLD: f32 = 0.5;

/// pg_trgm's trigrams of `s` in order (`generate_trgm_only`): each word (a run of
/// alphanumeric characters), lowercased and padded with two spaces before and one after,
/// gives one trigram per window of three characters. Repeats are kept.
fn trigrams(s: &str) -> Vec<[char; 3]> {
    let mut out = Vec::new();
    for word in s
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let padded: Vec<char> = "  ".chars().chain(word.chars()).chain([' ']).collect();
        out.extend(padded.windows(3).map(|w| [w[0], w[1], w[2]]));
    }
    out
}

/// pg_trgm's `word_similarity(a, b)`, ported from its non-strict `calc_word_similarity` and
/// `iterate_word_similarity`: the similarity `count / (|trgm(a)| + |extent| - count)`
/// between the trigram set of `a` and a continuous extent of `b`'s ordered trigrams, with
/// the extent found by pg_trgm's own greedy walk, so scores equal the server's.
fn word_similarity(a: &str, b: &str) -> f32 {
    let t1 = trigrams(a);
    let t2 = trigrams(b);
    if t1.is_empty() || t2.is_empty() {
        return 0.0;
    }
    // Unique trigram ids over both strings; `found[id]`: the trigram occurs in `a`.
    let mut ids: BTreeMap<[char; 3], usize> = BTreeMap::new();
    for t in t1.iter().chain(&t2) {
        let next = ids.len();
        ids.entry(*t).or_insert(next);
    }
    let mut found = vec![false; ids.len()];
    for t in &t1 {
        found[ids[t]] = true;
    }
    let ulen1 = found.iter().filter(|f| **f).count();
    let idx: Vec<usize> = t2.iter().map(|t| ids[t]).collect();
    let sml = |count: usize, ulen2: usize| count as f32 / (ulen1 + ulen2 - count) as f32;

    let mut lastpos: Vec<Option<usize>> = vec![None; ids.len()];
    let (mut ulen2, mut count) = (0usize, 0usize);
    let mut lower: Option<usize> = None;
    let mut best = 0.0f32;
    for (i, &t) in idx.iter().enumerate() {
        if lower.is_some() || found[t] {
            if lastpos[t].is_none() {
                ulen2 += 1;
                if found[t] {
                    count += 1;
                }
            }
            lastpos[t] = Some(i);
        }
        if !found[t] {
            continue;
        }
        let prev_lower = match lower {
            Some(l) => l,
            None => {
                ulen2 = 1;
                i
            }
        };
        let mut cur = sml(count, ulen2);
        // Try moving the lower bound up for a greater similarity.
        let (mut tmp_count, mut tmp_ulen2) = (count, ulen2);
        let mut new_lower = prev_lower;
        for (tmp_lower, &ti) in idx.iter().enumerate().take(i + 1).skip(prev_lower) {
            let s = sml(tmp_count, tmp_ulen2);
            if s > cur {
                cur = s;
                ulen2 = tmp_ulen2;
                new_lower = tmp_lower;
                count = tmp_count;
            }
            if lastpos[ti] == Some(tmp_lower) {
                tmp_ulen2 -= 1;
                if found[ti] {
                    tmp_count -= 1;
                }
            }
        }
        best = best.max(cur);
        // Trigrams last seen behind the new lower bound leave the extent.
        for (tmp_lower, &ti) in idx.iter().enumerate().take(new_lower).skip(prev_lower) {
            if lastpos[ti] == Some(tmp_lower) {
                lastpos[ti] = None;
            }
        }
        lower = Some(new_lower);
    }
    best
}

/// D-115: SM-2 rounds a fractional interval up. The small slack keeps float noise in
/// `interval * ease` (15.000001) from adding a day; never below one day.
fn ceil_days(days: f32) -> f32 {
    (days - 1e-4).ceil().max(1.0)
}

#[async_trait]
impl MemoryStore for MemStore {
    async fn remember(&self, f: Fact) -> Result<FactId> {
        if !(0.0..=1.0).contains(&f.confidence) {
            return Err(MemoryError::Invalid(format!(
                "confidence {} outside 0..=1",
                f.confidence
            )));
        }
        let mut f = f;
        f.seen_at = micros(f.seen_at);
        let mut g = self.lock();
        g.next_fact += 1;
        let id = g.next_fact;
        g.facts.insert(id, f);
        Ok(FactId(id))
    }

    async fn forget(&self, id: FactId) -> Result<()> {
        self.lock()
            .facts
            .remove(&id.0)
            .map(|_| ())
            .ok_or_else(|| MemoryError::NotFound(format!("fact {}", id.0)))
    }

    async fn link(&self, src: NodeId, rel: &str, dst: NodeId, source: Source) -> Result<()> {
        if rel.trim().is_empty() {
            return Err(MemoryError::Invalid("edge relation is empty".into()));
        }
        let mut g = self.lock();
        for n in [src, dst] {
            if !g.nodes.contains_key(&n.0) {
                return Err(MemoryError::NotFound(format!("node {}", n.0)));
            }
        }
        if !g
            .edges
            .iter()
            .any(|e| e.src == src && e.rel == rel && e.dst == dst)
        {
            g.edges.push(Edge {
                src,
                rel: rel.to_string(),
                dst,
                source,
            });
        }
        Ok(())
    }

    async fn node(&self, kind: &str, name: &str) -> Result<NodeId> {
        if kind.trim().is_empty() || name.trim().is_empty() {
            return Err(MemoryError::Invalid(
                "node kind and name are required".into(),
            ));
        }
        let mut g = self.lock();
        if let Some(n) = g.nodes.values().find(|n| n.kind == kind && n.name == name) {
            return Ok(n.id);
        }
        g.next_node += 1;
        let id = NodeId(g.next_node);
        g.nodes.insert(
            id.0,
            Node {
                id,
                kind: kind.to_string(),
                name: name.to_string(),
            },
        );
        Ok(id)
    }

    async fn neighbors(&self, n: NodeId, rel: Option<&str>, depth: u8) -> Result<Vec<Node>> {
        // Breadth-first, so each node is found at its minimum depth; then ordered by
        // (depth, node id) and capped, like PgStore's `order by d, n.id limit 500`.
        let g = self.lock();
        let mut seen: HashSet<NodeId> = HashSet::from([n]);
        let mut frontier = vec![n];
        let mut found: Vec<(u8, NodeId)> = Vec::new();
        for d in 1..=depth.min(MAX_DEPTH) {
            let mut next = Vec::new();
            for cur in &frontier {
                for e in g.edges.iter().filter(|e| rel.is_none_or(|r| e.rel == r)) {
                    let other = if e.src == *cur {
                        e.dst
                    } else if e.dst == *cur {
                        e.src
                    } else {
                        continue;
                    };
                    if seen.insert(other) {
                        found.push((d, other));
                        next.push(other);
                    }
                }
            }
            frontier = next;
        }
        found.sort();
        Ok(found
            .into_iter()
            .filter_map(|(_, id)| g.nodes.get(&id.0).cloned())
            .take(MAX_NEIGHBORS)
            .collect())
    }

    async fn recall(&self, q: &str, k: usize, filter: Option<Filter>) -> Result<Vec<Hit>> {
        let q = q.trim().to_lowercase();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let g = self.lock();
        Ok(g.facts
            .iter()
            .rev()
            .filter(|(_, f)| matches_filter(f, &filter) && contains_ci(f, &q))
            .take(k)
            .enumerate()
            .map(|(rank, (id, f))| Hit {
                id: FactId(*id),
                fact: f.clone(),
                score: 1.0 / (rank as f32 + 1.0),
                why: Why::Fts,
            })
            .collect())
    }

    async fn search(&self, q: &str, k: usize) -> Result<Vec<Hit>> {
        // D-115, as PgStore's SEARCH_SQL: exact tier = normalised equality on subject (1.0)
        // or a case-insensitive substring of subject/key/value (0.9); trigram tier =
        // `q <% subject` (word_similarity >= 0.6) with that score, for what exact missed.
        // Ordered by score, then tier, then newest seen_at, then id.
        let q = q.trim();
        if q.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let lower = q.to_lowercase();
        let norm_q = normalised(q);
        let g = self.lock();
        let mut hits: Vec<(u8, Hit)> = Vec::new();
        for (id, f) in &g.facts {
            let (tier, score) = if !norm_q.is_empty() && normalised(&f.subject) == norm_q {
                (0, 1.0)
            } else if contains_ci(f, &lower) {
                (0, 0.9)
            } else {
                let sim = word_similarity(q, &f.subject);
                if sim < WORD_SIMILARITY_THRESHOLD {
                    continue;
                }
                (1, sim)
            };
            hits.push((
                tier,
                Hit {
                    id: FactId(*id),
                    fact: f.clone(),
                    score,
                    why: if tier == 0 { Why::Exact } else { Why::Trigram },
                },
            ));
        }
        hits.sort_by(|(ta, a), (tb, b)| {
            b.score
                .total_cmp(&a.score)
                .then(ta.cmp(tb))
                .then(b.fact.seen_at.cmp(&a.fact.seen_at))
                .then(a.id.cmp(&b.id))
        });
        Ok(hits.into_iter().take(k).map(|(_, h)| h).collect())
    }

    async fn contact(&self, person: &str) -> Result<Option<Contact>> {
        Ok(self.lock().contacts.get(&contact_key(person)).cloned())
    }

    async fn touch_contact(&self, person: &str, channel: &str, note: &str) -> Result<()> {
        if contact_key(person).is_empty() {
            return Err(MemoryError::Invalid("contact person is empty".into()));
        }
        touch(&mut self.lock(), person, channel, note);
        Ok(())
    }

    async fn due_reviews(&self, now: Ts, k: usize) -> Result<Vec<Review>> {
        let g = self.lock();
        let mut due: Vec<Review> = g
            .reviews
            .values()
            .filter(|r| r.due <= micros(now))
            .cloned()
            .collect();
        due.sort_by_key(|r| (r.due, r.id));
        due.truncate(k);
        Ok(due)
    }

    async fn grade_review(&self, id: ReviewId, grade: u8) -> Result<()> {
        if grade > 5 {
            return Err(MemoryError::Invalid(format!("grade {grade} outside 0..=5")));
        }
        let mut g = self.lock();
        let r = g
            .reviews
            .get_mut(&id.0)
            .ok_or_else(|| MemoryError::NotFound(format!("review {}", id.0)))?;
        // SM-2; a fractional interval rounds up (D-115), as in PgStore's `sm2`.
        let q = f32::from(grade);
        if grade >= 3 {
            r.interval_days = match r.reps {
                0 => 1.0,
                1 => 6.0,
                _ => ceil_days(r.interval_days * r.ease),
            };
            r.reps += 1;
        } else {
            r.reps = 0;
            r.interval_days = 1.0;
        }
        r.ease = (r.ease + (0.1 - (5.0 - q) * (0.08 + (5.0 - q) * 0.02))).max(1.3);
        let secs = (f64::from(r.interval_days) * 86_400.0) as i64;
        r.due = ts_now() + ChronoDuration::seconds(secs);
        Ok(())
    }

    async fn baseline_get(&self, watch: &str) -> Result<Option<Baseline>> {
        Ok(self.lock().baselines.get(watch).cloned())
    }

    async fn baseline_set(&self, watch: &str, b: Baseline) -> Result<()> {
        let mut b = b;
        b.updated = micros(b.updated);
        self.lock().baselines.insert(watch.to_string(), b);
        Ok(())
    }

    async fn append(&self, e: LedgerEvent) -> Result<LedgerId> {
        if e.duration_ms < 0 {
            return Err(MemoryError::Invalid(format!(
                "duration_ms {} is negative",
                e.duration_ms
            )));
        }
        let mut e = e;
        e.ts = micros(e.ts);
        let mut g = self.lock();
        g.next_ledger += 1;
        let id = LedgerId(g.next_ledger);
        g.ledger.push((id, e));
        Ok(id)
    }

    async fn tool_health(&self, domain: &str) -> Result<Vec<ToolHealth>> {
        Ok(self
            .lock()
            .tools
            .values()
            .filter(|h| h.domain == domain)
            .cloned()
            .collect())
    }

    async fn tool_health_set(&self, h: ToolHealth) -> Result<()> {
        if h.domain.trim().is_empty() || h.name.trim().is_empty() {
            return Err(MemoryError::Invalid(
                "tool domain and name are required".into(),
            ));
        }
        let mut h = h;
        h.last_ok = h.last_ok.map(micros);
        h.last_fail = h.last_fail.map(micros);
        self.lock()
            .tools
            .insert((h.domain.clone(), h.name.clone()), h);
        Ok(())
    }

    async fn gate_rule(&self, domain: &str, class: &str) -> Result<Option<GateRule>> {
        // Like PgStore: the lookup is canonicalised too, and one that does not canonicalise
        // matches nothing (D-110 (1), D-113).
        let (Ok(domain), Ok(class)) = (rule_domain(domain), rule_class_key(class)) else {
            return Ok(None);
        };
        Ok(self.lock().rules.get(&(domain, class)).map(|(r, _)| *r))
    }

    async fn run_start(&self, run: Run) -> Result<()> {
        let mut run = run;
        run.started = micros(run.started);
        run.ended = run.ended.map(micros);
        let mut g = self.lock();
        if g.runs.contains_key(&run.id.0) {
            return Err(MemoryError::Invalid(format!("run {} exists", run.id)));
        }
        g.runs.insert(run.id.0.clone(), run);
        Ok(())
    }

    async fn run_end(&self, id: &RunId, status: RunStatus, ended: Ts) -> Result<()> {
        let mut g = self.lock();
        let r = g
            .runs
            .get_mut(&id.0)
            .ok_or_else(|| MemoryError::NotFound(format!("run {id}")))?;
        r.status = status;
        r.ended = Some(micros(ended));
        Ok(())
    }

    async fn run_get(&self, id: &RunId) -> Result<Option<Run>> {
        Ok(self.lock().runs.get(&id.0).cloned())
    }

    async fn schedule_add(&self, mut s: Schedule) -> Result<i32> {
        let mut g = self.lock();
        g.next_schedule += 1;
        s.id = g.next_schedule;
        s.last_run = s.last_run.map(micros);
        g.schedules.insert(s.id, s);
        Ok(g.next_schedule)
    }

    async fn schedule_list(&self) -> Result<Vec<Schedule>> {
        Ok(self.lock().schedules.values().cloned().collect())
    }

    async fn schedule_rm(&self, id: i32) -> Result<()> {
        self.lock()
            .schedules
            .remove(&id)
            .map(|_| ())
            .ok_or_else(|| MemoryError::NotFound(format!("schedule {id}")))
    }

    async fn schedule_fired(&self, id: i32, at: Ts) -> Result<()> {
        let mut g = self.lock();
        let s = g
            .schedules
            .get_mut(&id)
            .ok_or_else(|| MemoryError::NotFound(format!("schedule {id}")))?;
        s.last_run = Some(micros(at));
        Ok(())
    }

    async fn gate_rule_set(
        &self,
        domain: &str,
        class: &str,
        rule: GateRule,
        note: &str,
    ) -> Result<()> {
        set_rule(&mut self.lock(), domain, class, rule, note)
    }

    async fn review_add(&self, item: &str, answer: &str) -> Result<ReviewId> {
        let mut g = self.lock();
        g.next_review += 1;
        let id = ReviewId(g.next_review);
        g.reviews.insert(
            id.0,
            Review {
                id,
                item: item.to_string(),
                answer: answer.to_string(),
                due: ts_now(),
                interval_days: 1.0,
                ease: 2.5,
                reps: 0,
            },
        );
        Ok(id)
    }

    async fn ledger_events(&self, run: &RunId) -> Result<Vec<(LedgerId, LedgerEvent)>> {
        Ok(self
            .lock()
            .ledger
            .iter()
            .filter(|(_, e)| e.run_id.as_ref() == Some(run))
            .cloned()
            .collect())
    }
}

/// An [`Asker`] for tests. Answers from a queue of outcomes in order; once the queue is
/// empty it behaves like an unanswered card and returns the card's own timeout outcome
/// (`TimeoutProceed` / `TimeoutSkip` from `card.on_timeout`) immediately, without waiting
/// out `card.timeout`. A queued `Timeout*` outcome must equal what the card says: one that
/// contradicts `card.on_timeout` panics (a scripted timeout cannot override the gate).
/// Every card it is shown is recorded for assertions.
#[derive(Default)]
pub struct ScriptedAsker {
    outcomes: Mutex<VecDeque<AskOutcome>>,
    cards: Mutex<Vec<AskCard>>,
}

impl ScriptedAsker {
    /// No scripted answers: every card times out per its `on_timeout`.
    pub fn new() -> ScriptedAsker {
        ScriptedAsker::default()
    }

    /// Answers with `outcomes` in order, then times out.
    pub fn with(outcomes: impl IntoIterator<Item = AskOutcome>) -> ScriptedAsker {
        ScriptedAsker {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            cards: Mutex::new(Vec::new()),
        }
    }

    /// Queues one more answer.
    pub fn push(&self, outcome: AskOutcome) {
        self.outcomes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(outcome);
    }

    /// Every card shown so far, in order.
    pub fn cards(&self) -> Vec<AskCard> {
        self.cards.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl Asker for ScriptedAsker {
    async fn ask(&self, card: &AskCard) -> AskOutcome {
        self.cards
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(card.clone());
        let scripted = self
            .outcomes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front();
        let timeout = match card.on_timeout {
            OnTimeout::Proceed => AskOutcome::TimeoutProceed,
            OnTimeout::Skip => AskOutcome::TimeoutSkip,
        };
        match scripted {
            Some(o @ (AskOutcome::TimeoutProceed | AskOutcome::TimeoutSkip)) if o != timeout => {
                panic!(
                    "ScriptedAsker: scripted {o:?} contradicts the card's on_timeout {:?}",
                    card.on_timeout
                )
            }
            Some(o) => o,
            None => timeout,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::Class;
    use crate::memory::{node_kind, rel, SourceKind};
    use chrono::Utc;
    use serde_json::json;
    use std::time::Duration;

    fn src() -> Source {
        Source {
            kind: SourceKind::User,
            reference: "test".into(),
            run_id: None,
        }
    }

    fn event(run: Option<&str>) -> LedgerEvent {
        LedgerEvent {
            run_id: run.map(|r| RunId(r.into())),
            ts: crate::memory::ts_now(),
            server: "core".into(),
            tool: "echo.send".into(),
            class: Some(Class::Send),
            mode: None,
            args: json!({}),
            result_summary: String::new(),
            consequences: json!([]),
            duration_ms: 0,
            ok: true,
            why: None,
        }
    }

    #[tokio::test]
    async fn seeded_rules_and_challenge_invariant() {
        let s = MemStore::new();
        let job = s.gate_rule("*", "Submit:job_application").await.unwrap();
        assert_eq!(
            job,
            Some(GateRule {
                mode: Mode::Ask,
                strict: false
            })
        );
        assert!(s
            .gate_rule("*", "Submit:signature")
            .await
            .unwrap()
            .is_some());
        assert!(MemStore::empty()
            .gate_rule("*", "Pay")
            .await
            .unwrap()
            .is_none());

        let allow = GateRule {
            mode: Mode::Allow,
            strict: false,
        };
        assert!(s
            .gate_rule_set("x.com", "Challenge", allow, "")
            .await
            .is_err());
        assert!(s
            .gate_rule_set("x.com", "Challenge:turnstile", allow, "")
            .await
            .is_err());
        assert!(s.gate_rule_set("x.com", "Shout", allow, "").await.is_err());
        s.gate_rule_set("luma.com", "Account", allow, "events")
            .await
            .unwrap();
        assert_eq!(
            s.gate_rule("luma.com", "Account").await.unwrap(),
            Some(allow)
        );
        assert_eq!(
            s.rule_note("luma.com", "Account").as_deref(),
            Some("events")
        );
    }

    #[tokio::test]
    async fn contacts_use_contact_key() {
        let s = MemStore::new();
        s.touch_contact("  Vasanth@Example.COM ", "mail", "met at luma")
            .await
            .unwrap();
        s.add_contact("vasanth@example.com");
        let c = s.contact("VASANTH@example.com").await.unwrap().unwrap();
        assert_eq!(c.person, "vasanth@example.com");
        assert_eq!(c.count, 2);
        assert_eq!(c.note.as_deref(), Some("met at luma"));
        assert!(s.contact("other@example.com").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn ledger_ids_increase_and_filter_by_run() {
        let s = MemStore::new();
        let a = s.append(event(Some("r1"))).await.unwrap();
        let b = s.append(event(Some("r2"))).await.unwrap();
        let c = s.append(event(Some("r1"))).await.unwrap();
        assert!(a < b && b < c);
        let r1 = s.ledger_events(&RunId("r1".into())).await.unwrap();
        assert_eq!(r1.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [a, c]);
        assert_eq!(s.ledger().len(), 3);
        let mut neg = event(None);
        neg.duration_ms = -1;
        assert!(s.append(neg).await.is_err());
    }

    #[tokio::test]
    async fn runs_and_schedules() {
        let s = MemStore::new();
        let id = RunId("2026-10-23T10-00-00-hi".into());
        let run = Run {
            id: id.clone(),
            started: Utc::now(),
            ended: None,
            prompt: "say hi".into(),
            status: RunStatus::Running,
            folder: format!("runs/{id}"),
            parent_run: None,
            strict: false,
        };
        s.run_start(run.clone()).await.unwrap();
        assert!(s.run_start(run).await.is_err());
        s.run_end(&id, RunStatus::Done, Utc::now()).await.unwrap();
        assert_eq!(
            s.run_get(&id).await.unwrap().unwrap().status,
            RunStatus::Done
        );

        let sched = Schedule {
            id: 0,
            cron: "*/1 * * * *".into(),
            prompt: "say hi".into(),
            kind: crate::memory::ScheduleKind::Cron,
            watch_target: None,
            enabled: true,
            last_run: None,
            run_id: Some(id),
        };
        let a = s.schedule_add(sched.clone()).await.unwrap();
        let b = s.schedule_add(sched).await.unwrap();
        assert!(a < b);
        s.schedule_fired(a, Utc::now()).await.unwrap();
        s.schedule_rm(b).await.unwrap();
        let list = s.schedule_list().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, a);
        assert!(list[0].last_run.is_some());
    }

    #[tokio::test]
    async fn graph_walks_both_directions() {
        let s = MemStore::new();
        let p = s.node(node_kind::PERSON, "vasanth").await.unwrap();
        let e = s.node(node_kind::EVENT, "rust meetup").await.unwrap();
        let me = s.node(node_kind::PERSON, "me").await.unwrap();
        let co = s.node(node_kind::COMPANY, "acme").await.unwrap();
        assert_eq!(s.node(node_kind::PERSON, "vasanth").await.unwrap(), p);
        s.link(p, rel::MET_AT, e, src()).await.unwrap();
        s.link(me, rel::ATTENDED, e, src()).await.unwrap();
        s.link(p, rel::WORKS_AT, co, src()).await.unwrap();
        let from_me: Vec<String> = s
            .neighbors(me, None, 2)
            .await
            .unwrap()
            .into_iter()
            .map(|n| n.name)
            .collect();
        assert_eq!(from_me, ["rust meetup", "vasanth"]);
        let only_works: Vec<NodeId> = s
            .neighbors(co, Some(rel::WORKS_AT), 4)
            .await
            .unwrap()
            .into_iter()
            .map(|n| n.id)
            .collect();
        assert_eq!(only_works, [p]);
    }

    #[tokio::test]
    async fn facts_recall_search_forget() {
        let s = MemStore::new();
        let id = s
            .remember(Fact::new("vasanth", "works_at", "Acme", src()))
            .await
            .unwrap();
        s.remember(Fact::new("luma", "event", "Rust meetup", src()))
            .await
            .unwrap();
        let hits = s.recall("acme", 5, None).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
        let hits = s.search("Acme", 5).await.unwrap();
        assert_eq!(hits[0].why, Why::Exact);
        let filtered = s
            .recall(
                "rust",
                5,
                Some(Filter {
                    subject_prefix: Some("vas".into()),
                    ..Filter::default()
                }),
            )
            .await
            .unwrap();
        assert!(filtered.is_empty());
        s.forget(id).await.unwrap();
        assert!(s.forget(id).await.is_err());
    }

    #[tokio::test]
    async fn reviews_follow_sm2() {
        let s = MemStore::new();
        let id = s.review_add("capital of France", "Paris").await.unwrap();
        assert_eq!(s.due_reviews(Utc::now(), 10).await.unwrap().len(), 1);
        s.grade_review(id, 5).await.unwrap();
        assert!(s.due_reviews(Utc::now(), 10).await.unwrap().is_empty());
        assert!(s.grade_review(id, 6).await.is_err());
    }

    #[tokio::test]
    async fn scripted_asker_answers_then_times_out() {
        let asker = ScriptedAsker::with([AskOutcome::AnsweredSkip]);
        let card = AskCard {
            class: Class::Send,
            what: "send".into(),
            site: None,
            values: Vec::new(),
            timeout: Duration::from_secs(60),
            on_timeout: OnTimeout::Proceed,
        };
        assert_eq!(asker.ask(&card).await, AskOutcome::AnsweredSkip);
        assert_eq!(asker.ask(&card).await, AskOutcome::TimeoutProceed);
        let skip = AskCard {
            on_timeout: OnTimeout::Skip,
            ..card
        };
        assert_eq!(asker.ask(&skip).await, AskOutcome::TimeoutSkip);
        asker.push(AskOutcome::AnsweredProceed);
        assert_eq!(asker.ask(&skip).await, AskOutcome::AnsweredProceed);
        // A scripted timeout that agrees with the card is fine.
        asker.push(AskOutcome::TimeoutSkip);
        assert_eq!(asker.ask(&skip).await, AskOutcome::TimeoutSkip);
        assert_eq!(asker.cards().len(), 5);
    }

    fn skip_card() -> AskCard {
        AskCard {
            class: Class::Pay,
            what: "pay".into(),
            site: None,
            values: Vec::new(),
            timeout: Duration::from_secs(60),
            on_timeout: OnTimeout::Skip,
        }
    }

    #[tokio::test]
    #[should_panic(expected = "contradicts")]
    async fn scripted_timeout_proceed_on_a_skip_card_panics() {
        let asker = ScriptedAsker::with([AskOutcome::TimeoutProceed]);
        asker.ask(&skip_card()).await;
    }

    #[tokio::test]
    #[should_panic(expected = "contradicts")]
    async fn scripted_timeout_skip_on_a_proceed_card_panics() {
        let asker = ScriptedAsker::with([AskOutcome::TimeoutSkip]);
        let card = AskCard {
            on_timeout: OnTimeout::Proceed,
            ..skip_card()
        };
        asker.ask(&card).await;
    }

    #[tokio::test]
    async fn rules_are_stored_under_the_canonical_key() {
        // D-110 (1): stored verbatim before, so the gate's canonical lookup never hit.
        let s = MemStore::empty();
        let ask = GateRule {
            mode: Mode::Ask,
            strict: false,
        };
        for (d, k, want_d, want_k) in [
            ("Bank.com.", "submit", "bank.com", "Submit"),
            (" shop2.com", "Submit", "shop2.com", "Submit"),
            (" * ", "Submit: Wire", "*", "Submit:wire"),
        ] {
            s.gate_rule_set(d, k, ask, "n").await.unwrap();
            assert_eq!(
                s.gate_rule(want_d, want_k).await.unwrap(),
                Some(ask),
                "{d:?} {k:?}"
            );
            // The lookup canonicalises too, as PgStore's does, so the verbatim key finds it.
            assert_eq!(s.gate_rule(d, k).await.unwrap(), Some(ask), "{d:?} {k:?}");
        }
        for (d, k) in [
            ("", "Submit"),
            ("a b", "Submit"),
            ("*", "Submit:"),
            ("*", "Nope"),
        ] {
            assert!(
                matches!(
                    s.gate_rule_set(d, k, ask, "").await,
                    Err(MemoryError::Invalid(_))
                ),
                "{d:?} {k:?}"
            );
        }
        // Seed rules are already canonical.
        for (d, k, mode, _) in SEED_RULES {
            let rule = GateRule {
                mode,
                strict: false,
            };
            assert_eq!(
                rule_key(d, k, &rule).unwrap(),
                (d.to_string(), k.to_string())
            );
        }
    }

    #[tokio::test]
    async fn stored_timestamps_are_truncated_to_micros() {
        // PgStore keeps microseconds; the fake used to keep nanoseconds.
        let ns = DateTime::from_timestamp(1_700_000_000, 123_456_789).unwrap();
        let us = DateTime::from_timestamp(1_700_000_000, 123_456_000).unwrap();
        let s = MemStore::new();

        let mut e = event(Some("r1"));
        e.ts = ns;
        s.append(e).await.unwrap();
        assert_eq!(s.ledger()[0].1.ts, us);

        let id = RunId("r1".into());
        s.run_start(Run {
            id: id.clone(),
            started: ns,
            ended: Some(ns),
            prompt: "p".into(),
            status: RunStatus::Running,
            folder: "runs/r1".into(),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
        let r = s.run_get(&id).await.unwrap().unwrap();
        assert_eq!((r.started, r.ended), (us, Some(us)));
        s.run_end(&id, RunStatus::Done, ns).await.unwrap();
        assert_eq!(s.run_get(&id).await.unwrap().unwrap().ended, Some(us));

        let sid = s
            .schedule_add(Schedule {
                id: 0,
                cron: "* * * * *".into(),
                prompt: "p".into(),
                kind: crate::memory::ScheduleKind::Cron,
                watch_target: None,
                enabled: true,
                last_run: Some(ns),
                run_id: None,
            })
            .await
            .unwrap();
        assert_eq!(s.schedule_list().await.unwrap()[0].last_run, Some(us));
        s.schedule_fired(sid, ns).await.unwrap();
        assert_eq!(s.schedule_list().await.unwrap()[0].last_run, Some(us));

        let mut f = Fact::new("a", "b", "c", src());
        f.seen_at = ns;
        s.remember(f).await.unwrap();
        assert_eq!(s.search("a", 1).await.unwrap()[0].fact.seen_at, us);

        s.baseline_set(
            "w",
            Baseline {
                hash: "h".into(),
                snapshot: json!({}),
                updated: ns,
            },
        )
        .await
        .unwrap();
        assert_eq!(s.baseline_get("w").await.unwrap().unwrap().updated, us);

        s.add_contact("x@y.z");
        let last = s.contact("x@y.z").await.unwrap().unwrap().last.unwrap();
        assert_eq!(last.timestamp_subsec_nanos() % 1_000, 0);
    }

    #[tokio::test]
    async fn neighbors_are_ordered_by_depth_then_id() {
        // Insertion order used to decide; PgStore orders by (depth, node id).
        let s = MemStore::new();
        let a = s.node(node_kind::PERSON, "a").await.unwrap();
        let b = s.node(node_kind::PERSON, "b").await.unwrap();
        let c = s.node(node_kind::PERSON, "c").await.unwrap();
        let d = s.node(node_kind::PERSON, "d").await.unwrap();
        let root = s.node(node_kind::PERSON, "root").await.unwrap();
        // depth 1: d then b (inserted in that order); depth 2: c then a.
        s.link(root, rel::EMAILED, d, src()).await.unwrap();
        s.link(b, rel::EMAILED, root, src()).await.unwrap();
        s.link(d, rel::EMAILED, c, src()).await.unwrap();
        s.link(b, rel::EMAILED, a, src()).await.unwrap();
        let ids: Vec<NodeId> = s
            .neighbors(root, None, 2)
            .await
            .unwrap()
            .into_iter()
            .map(|n| n.id)
            .collect();
        assert_eq!(ids, [b, d, a, c]);
        assert!(s.neighbors(root, None, 0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn validation_matches_pgstore() {
        let s = MemStore::new();
        for (k, n) in [("", "x"), ("person", " "), (" ", "")] {
            assert!(
                matches!(s.node(k, n).await, Err(MemoryError::Invalid(_))),
                "{k:?} {n:?}"
            );
        }
        let a = s.node(node_kind::PERSON, "a").await.unwrap();
        assert!(matches!(
            s.link(a, " ", a, src()).await,
            Err(MemoryError::Invalid(_))
        ));
        let health = |domain: &str, name: &str| ToolHealth {
            domain: domain.into(),
            name: name.into(),
            version: 1,
            status: crate::memory::ToolStatus::Saved,
            runs: 0,
            fails: 0,
            last_ok: None,
            last_fail: None,
            why: None,
            pinned: false,
        };
        for (d, n) in [("", "t"), ("x.com", ""), (" ", " ")] {
            assert!(
                matches!(
                    s.tool_health_set(health(d, n)).await,
                    Err(MemoryError::Invalid(_))
                ),
                "{d:?} {n:?}"
            );
        }
        s.tool_health_set(health("x.com", "t")).await.unwrap();
        assert!(matches!(
            s.touch_contact("  ", "mail", "").await,
            Err(MemoryError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn blank_queries_find_nothing() {
        // A blank query used to match every fact (empty substring).
        let s = MemStore::new();
        s.remember(Fact::new("a", "b", "c", src())).await.unwrap();
        for q in ["", "   "] {
            assert!(s.recall(q, 5, None).await.unwrap().is_empty(), "{q:?}");
            assert!(s.search(q, 5).await.unwrap().is_empty(), "{q:?}");
        }
    }

    // ---- Round-2 review regressions: fake parity with PgStore (each failed on c22ece0) ----

    #[tokio::test]
    async fn gate_rule_canonicalises_its_lookup_like_pgstore() {
        let s = MemStore::empty();
        let stop = GateRule {
            mode: Mode::Stop,
            strict: false,
        };
        s.gate_rule_set("bank.com", "Submit:wire", stop, "")
            .await
            .unwrap();
        for (d, k) in [
            ("Bank.COM.", "submit : WIRE"),
            (" bank.com", "SUBMIT:wire"),
            ("BANK.com", "Submit:Wire"),
        ] {
            assert_eq!(s.gate_rule(d, k).await.unwrap(), Some(stop), "{d:?} {k:?}");
        }
        // A lookup that does not canonicalise matches nothing (PgStore: Ok(None)).
        for (d, k) in [("bank.com:443", "Submit:wire"), ("bank.com", "Nope")] {
            assert_eq!(s.gate_rule(d, k).await.unwrap(), None, "{d:?} {k:?}");
        }
    }

    #[tokio::test]
    async fn recall_since_compares_micros_on_both_sides() {
        // seen_at was truncated on write but `since` kept its nanoseconds, so a fact seen
        // in the same microsecond as `since` was dropped; PgStore truncates the bind too.
        let ns = DateTime::from_timestamp(1_700_000_000, 123_456_789).unwrap();
        let s = MemStore::new();
        let mut f = Fact::new("acme", "k", "v", src());
        f.seen_at = ns;
        s.remember(f).await.unwrap();
        let since = Filter {
            since: Some(DateTime::from_timestamp(1_700_000_000, 123_456_999).unwrap()),
            ..Filter::default()
        };
        assert_eq!(s.recall("acme", 5, Some(since)).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn timestamps_truncate_toward_zero_from_2000_like_sqlx() {
        // sqlx sends (t - 2000-01-01).num_microseconds(): toward zero, so before 2000 a
        // sub-microsecond remainder rounds *up*; flooring Unix micros rounded it down.
        let s = MemStore::new();
        for (t, want) in [
            (
                DateTime::from_timestamp(946_684_799, 999_999_500).unwrap(),
                DateTime::from_timestamp(946_684_800, 0).unwrap(),
            ),
            (
                DateTime::from_timestamp(-1, 999_999_999).unwrap(),
                DateTime::from_timestamp(0, 0).unwrap(),
            ),
            (
                DateTime::from_timestamp(1_700_000_000, 123_456_789).unwrap(),
                DateTime::from_timestamp(1_700_000_000, 123_456_000).unwrap(),
            ),
        ] {
            let mut e = event(None);
            e.ts = t;
            s.append(e).await.unwrap();
            assert_eq!(s.ledger().last().unwrap().1.ts, want, "{t:?}");
        }
    }

    #[tokio::test]
    async fn search_follows_d115() {
        let s = MemStore::new();
        let t = |secs| DateTime::from_timestamp(secs, 0).unwrap();
        let put = |subject: &str, key: &str, value: &str, at: i64| {
            let mut f = Fact::new(subject, key, value, src());
            f.seen_at = t(at);
            f
        };
        let company = s
            .remember(put("Foo-Bar Inc", "kind", "company", 1_000))
            .await
            .unwrap();
        let mention = s
            .remember(put("note", "text", "we met Foo-Bar Inc staff", 2_000))
            .await
            .unwrap();
        let person = s
            .remember(put("Vasanth Raghavan", "works_at", "acme", 3_000))
            .await
            .unwrap();

        // Case/punctuation-insensitive equality on subject: Exact 1.0, ahead of a substring.
        let hits = s.search("foo bar INC", 5).await.unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(
            (hits[0].id, hits[0].why, hits[0].score),
            (company, Why::Exact, 1.0)
        );
        let hits = s.search("Foo-Bar Inc", 5).await.unwrap();
        let got: Vec<(FactId, Why, f32)> = hits.iter().map(|h| (h.id, h.why, h.score)).collect();
        assert_eq!(
            got,
            [(company, Why::Exact, 1.0), (mention, Why::Exact, 0.9)]
        );
        // Equality on key or value is only a substring hit.
        let hits = s.search("acme", 5).await.unwrap();
        assert_eq!(
            (hits[0].id, hits[0].why, hits[0].score),
            (person, Why::Exact, 0.9)
        );

        // A typo inside a longer subject: word-trigram tier with pg_trgm's word_similarity.
        let hits = s.search("Vasanh", 5).await.unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!((hits[0].id, hits[0].why), (person, Why::Trigram));
        assert!(
            (hits[0].score - 5.0 / 7.0).abs() < 1e-6,
            "{}",
            hits[0].score
        );
        // D-125: D-115's own example scores exactly the 0.5 cut-off and hits.
        let hits = s.search("Vasnth", 5).await.unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!((hits[0].id, hits[0].why), (person, Why::Trigram));
        assert!((hits[0].score - 0.5).abs() < 1e-6, "{}", hits[0].score);
        assert!(s.search("Zebra", 5).await.unwrap().is_empty());

        // Ordered by score; tier only on ties; newest seen_at breaks the remaining ties.
        let older = s.remember(put("old", "k", "acme", 10)).await.unwrap();
        let ids: Vec<FactId> = s
            .search("acme", 5)
            .await
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        assert_eq!(ids, [person, older]);
        let exact_word = s.remember(put("Vasanh", "k", "v", 5)).await.unwrap();
        let ids: Vec<FactId> = s
            .search("vasanh", 5)
            .await
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        assert_eq!(ids, [exact_word, person]);
    }

    #[test]
    fn word_similarity_matches_pg_trgm() {
        // Values from the pg_trgm documentation and by hand from its algorithm.
        assert!((word_similarity("word", "two words") - 0.8).abs() < 1e-6);
        assert!((word_similarity("vasnth", "Vasanth Raghavan") - 0.5).abs() < 1e-6);
        assert_eq!(word_similarity("", "x"), 0.0);
        assert_eq!(word_similarity("acme", "acme"), 1.0);
    }

    #[test]
    fn word_similarity_equals_the_server() {
        // `select word_similarity(a, b)` on the compose Postgres 16 + pg_trgm (2026-10-23).
        for (a, b, want) in [
            ("Vasanh", "Vasanth Raghavan", 0.714_285_73),
            ("foo bar", "Foo-Bar Inc", 1.0),
            ("rapidflair", "Rapidflare", 0.727_272_75),
            ("acme corp", "the acme corporation ltd", 0.9),
            ("abcab", "xxabcabxx abab", 0.555_555_6),
            ("banana", "bananas and ban ana", 0.833_333_3),
            ("ab cd ef", "ef cd ab cd", 1.0),
            (
                "Rust CDP crate",
                "chromiumoxide crate for rust cdp",
                0.777_777_8,
            ),
        ] {
            let got = word_similarity(a, b);
            assert!((got - want).abs() < 1e-6, "{a:?} {b:?}: {got} vs {want}");
        }
    }

    #[tokio::test]
    async fn sm2_rounds_the_interval_up() {
        // D-115: 6 days x ease 2.7 = 16.2 days -> 17 (round() gave 16).
        let s = MemStore::new();
        let id = s.review_add("q", "a").await.unwrap();
        for _ in 0..3 {
            s.grade_review(id, 5).await.unwrap();
        }
        let far = Utc::now() + ChronoDuration::days(400);
        let r = &s.due_reviews(far, 1).await.unwrap()[0];
        assert_eq!(r.interval_days, 17.0);
    }
}
