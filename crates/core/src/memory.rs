//! I-1 `MemoryStore` (docs/02 M-2) and the types it moves. CONTRACT: change only through
//! `agent-memory/blockers.yaml: interface_changes` and a supreme decision.
//!
//! Every harness component holds `Arc<dyn MemoryStore>`; nothing else talks to the DB.
//! The trait uses `#[async_trait]` because native `async fn` in traits is not dyn-compatible (D-40).

use crate::class::{Class, Mode};
use crate::run::RunId;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Timestamps everywhere in the harness (`timestamptz` in Postgres).
pub type Ts = DateTime<Utc>;

pub type Result<T, E = MemoryError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    /// The backend failed (connection, SQL, embedding). The message must already be scrubbed.
    #[error("memory backend: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Row id of `memory.facts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FactId(pub i64);

/// Row id of `memory.nodes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub i64);

/// Row id of `memory.reviews`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReviewId(pub i64);

/// Where a fact or edge came from (M-3 `source jsonb`: `{kind, ref, run_id}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub kind: SourceKind,
    /// Message id, URL, file path, tool name: whatever lets a human find the origin.
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Email,
    Page,
    User,
    Tool,
}

/// A fact with provenance and time (M-1 job 1). Stored in `memory.facts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    pub subject: String,
    pub key: String,
    pub value: String,
    pub source: Source,
    pub seen_at: Ts,
    /// 0.0..=1.0, default 1.0.
    pub confidence: f32,
}

impl Fact {
    pub fn new(
        subject: impl Into<String>,
        key: impl Into<String>,
        value: impl Into<String>,
        source: Source,
    ) -> Self {
        Fact {
            subject: subject.into(),
            key: key.into(),
            value: value.into(),
            source,
            seen_at: Utc::now(),
            confidence: 1.0,
        }
    }
}

/// A graph node (M-5). Unique on `(kind, name)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: String,
    pub name: String,
}

/// Node kinds used by Phase 1 (M-5). Plain strings so new kinds need no contract change.
pub mod node_kind {
    pub const PERSON: &str = "person";
    pub const COMPANY: &str = "company";
    pub const EVENT: &str = "event";
    pub const THREAD: &str = "thread";
    pub const ROLE: &str = "role";
    pub const SITE: &str = "site";
    pub const TOOL: &str = "tool";
}

/// Edge relations used by Phase 1 (M-5).
pub mod rel {
    pub const WORKS_AT: &str = "works_at";
    pub const MET_AT: &str = "met_at";
    pub const ATTENDED: &str = "attended";
    pub const APPLIED_TO: &str = "applied_to";
    pub const EMAILED: &str = "emailed";
    pub const ABOUT_TOOL: &str = "about_tool";
}

/// `recall` filters (M-4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Ts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<SourceKind>,
}

/// Which retrieval path produced a hit. `recall` yields vector|fts|both (M-4);
/// `search` yields exact|trigram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Why {
    Vector,
    Fts,
    Both,
    Exact,
    Trigram,
}

/// One retrieval result. `id` is carried so a hit can be forgotten or cited.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub id: FactId,
    pub fact: Fact,
    /// Reciprocal-rank-fusion score for `recall`; similarity for `search`. Higher is better.
    pub score: f32,
    pub why: Why,
}

/// `memory.contacts`: who has been contacted, how, how often (gate H-1.1 Send rule).
/// `person` is the recipient exactly as the classifier puts it in `Call.recipients`,
/// lowercased and trimmed (an address or handle); `contact`, `touch_contact` and every
/// classifier normalise with [`contact_key`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub person: String,
    pub channel: Option<String>,
    pub last: Option<Ts>,
    pub count: i32,
    pub note: Option<String>,
}

/// `memory.reviews`: one spaced-repetition item (SM-2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub id: ReviewId,
    pub item: String,
    pub answer: String,
    pub due: Ts,
    pub interval_days: f32,
    pub ease: f32,
    pub reps: i32,
}

/// `memory.baselines`: last observed state of a watch target (H-5 watch). Keyed by watch name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub hash: String,
    pub snapshot: Value,
    pub updated: Ts,
}

/// The `mode` column of a ledger event: a gate decision, or how an ask ended (H-1.3, D-41).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerMode {
    Allow,
    Ask,
    Stop,
    AnsweredProceed,
    AnsweredSkip,
    TimeoutProceed,
    TimeoutSkip,
}

impl From<Mode> for LedgerMode {
    fn from(m: Mode) -> Self {
        match m {
            Mode::Allow => LedgerMode::Allow,
            Mode::Ask => LedgerMode::Ask,
            Mode::Stop => LedgerMode::Stop,
        }
    }
}

/// Normalised `memory.contacts.person` key (see [`Contact`]).
pub fn contact_key(person: &str) -> String {
    person.trim().to_lowercase()
}

/// `ledger.events.id`, returned by `append` so the jsonl mirror can carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LedgerId(pub i64);

/// One `ledger.events` row (H-3). Append-only. Every string field is already scrubbed
/// (H-2) before it is built; `MemoryStore::append` does not scrub for you.
/// `ts` must be truncated to microseconds (what `timestamptz` keeps) before the event is
/// written anywhere, so the jsonl mirror matches the table row-for-row (use [`ts_now`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerEvent {
    pub run_id: Option<RunId>,
    pub ts: Ts,
    /// Tool server name, or `core` for gate decisions, ask answers, timeouts, stops, repairs, forges.
    pub server: String,
    pub tool: String,
    pub class: Option<Class>,
    pub mode: Option<LedgerMode>,
    /// Scrubbed arguments: vault values appear only as `@vault(name)`.
    pub args: Value,
    pub result_summary: String,
    pub consequences: Value,
    /// `int` column; never negative.
    pub duration_ms: i32,
    pub ok: bool,
    pub why: Option<String>,
}

/// Now, truncated to the microsecond precision Postgres stores.
pub fn ts_now() -> Ts {
    let now = Utc::now();
    let micros = now.timestamp_micros();
    DateTime::from_timestamp_micros(micros).unwrap_or(now)
}

/// A `gate.rules` row's effect (H-1.2, H-1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GateRule {
    pub mode: Mode,
    /// Every ask under this rule times out to skip (H-1.3).
    pub strict: bool,
}

/// `runs.status` (Q-4 `Result:` line, plus running).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Done,
    Partial,
    Stopped,
    Failed,
}

/// One `runs` row (H-4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    pub started: Ts,
    pub ended: Option<Ts>,
    pub prompt: String,
    pub status: RunStatus,
    /// `runs/<id>/`, relative to the repo root.
    pub folder: String,
    /// For a scheduled firing: the run that created the schedule (`schedules.run_id`).
    pub parent_run: Option<RunId>,
    /// H-1.3: every ask in this run times out to skip.
    pub strict: bool,
}

/// `schedules.kind` (H-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleKind {
    Cron,
    Watch,
    Reflect,
}

/// One `schedules` row (H-5). `id` is ignored by `schedule_add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: i32,
    pub cron: String,
    pub prompt: String,
    pub kind: ScheduleKind,
    /// For `watch`: the `memory.baselines.watch` key (replaces H-5's baseline_id, D-45).
    pub watch_target: Option<String>,
    pub enabled: bool,
    pub last_run: Option<Ts>,
    /// The run that created the schedule; a firing's `parent_run` (H-4). `None` from the CLI.
    pub run_id: Option<RunId>,
}

/// Lifecycle status of a site tool (docs/03 T-7). `pinned` is a separate flag, not a status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Ephemeral,
    Saved,
    Repaired,
    NeedsRepair,
    Retired,
    /// T-5: a re-forge raised the gate floor; held until the user confirms.
    Ask,
}

/// One `tools.health` row (docs/03 T-4..T-7). Primary key `(domain, name)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolHealth {
    /// Registrable domain folder (eTLD+1) or `_generic`.
    pub domain: String,
    pub name: String,
    pub version: i32,
    pub status: ToolStatus,
    pub runs: i32,
    pub fails: i32,
    pub last_ok: Option<Ts>,
    pub last_fail: Option<Ts>,
    pub why: Option<String>,
    pub pinned: bool,
}

/// I-1. The first block is docs/02 M-2 with `#[async_trait]` and the error type added, and
/// two wave-0 changes (D-48): `append` returns the row id, and `gate_rule` returns the
/// rule's `strict` flag with its mode. The second block covers the M-3 tables M-2 has no
/// methods for (runs, schedules, rule writes, review creation, ledger reads), since
/// nothing but the store talks to the DB.
#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn remember(&self, f: Fact) -> Result<FactId>;
    /// User-requested only.
    async fn forget(&self, id: FactId) -> Result<()>;
    async fn link(&self, src: NodeId, rel: &str, dst: NodeId, source: Source) -> Result<()>;
    /// Get-or-create on `(kind, name)`.
    async fn node(&self, kind: &str, name: &str) -> Result<NodeId>;
    /// Recursive CTE, depth capped at 4 and 500 rows (M-5). Edges are traversed in both
    /// directions (`met_at → attended → works_at` paths mix them); `rel = None` means any
    /// relation, `Some(r)` restricts every hop to `r`. `n` itself is not returned.
    async fn neighbors(&self, n: NodeId, rel: Option<&str>, depth: u8) -> Result<Vec<Node>>;
    /// Vector + full-text, fused by reciprocal rank (M-4).
    async fn recall(&self, q: &str, k: usize, filter: Option<Filter>) -> Result<Vec<Hit>>;
    /// Exact / trigram.
    async fn search(&self, q: &str, k: usize) -> Result<Vec<Hit>>;
    async fn contact(&self, person: &str) -> Result<Option<Contact>>;
    async fn touch_contact(&self, person: &str, channel: &str, note: &str) -> Result<()>;
    async fn due_reviews(&self, now: Ts, k: usize) -> Result<Vec<Review>>;
    /// SM-2 update.
    async fn grade_review(&self, id: ReviewId, grade: u8) -> Result<()>;
    async fn baseline_get(&self, watch: &str) -> Result<Option<Baseline>>;
    async fn baseline_set(&self, watch: &str, b: Baseline) -> Result<()>;
    async fn append(&self, e: LedgerEvent) -> Result<LedgerId>;
    async fn tool_health(&self, domain: &str) -> Result<Vec<ToolHealth>>;
    async fn tool_health_set(&self, h: ToolHealth) -> Result<()>;
    /// `class` is a `gate.rules.class` key: a `Class` spelling, or `Class:hint` for
    /// classifier-scoped rules such as `Submit:job_application` (D-39).
    async fn gate_rule(&self, domain: &str, class: &str) -> Result<Option<GateRule>>;

    // ---- M-3 tables without an M-2 method (D-48) ----

    /// Insert a `runs` row (H-4).
    async fn run_start(&self, run: Run) -> Result<()>;
    async fn run_end(&self, id: &RunId, status: RunStatus, ended: Ts) -> Result<()>;
    async fn run_get(&self, id: &RunId) -> Result<Option<Run>>;
    /// Returns the new `schedules.id`.
    async fn schedule_add(&self, s: Schedule) -> Result<i32>;
    async fn schedule_list(&self) -> Result<Vec<Schedule>>;
    async fn schedule_rm(&self, id: i32) -> Result<()>;
    /// Sets `last_run`.
    async fn schedule_fired(&self, id: i32, at: Ts) -> Result<()>;
    /// `harness gate set` (H-7). Rejects a `Challenge` rule whose mode is not `stop` (D-5).
    async fn gate_rule_set(
        &self,
        domain: &str,
        class: &str,
        rule: GateRule,
        note: &str,
    ) -> Result<()>;
    /// New SM-2 item, due now.
    async fn review_add(&self, item: &str, answer: &str) -> Result<ReviewId>;
    /// A run's ledger in insertion order, for `report.md` and `harness ledger <run>`.
    async fn ledger_events(&self, run: &RunId) -> Result<Vec<(LedgerId, LedgerEvent)>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // The point of I-1 is `Arc<dyn MemoryStore>`; this fails to compile if the trait stops being dyn-compatible.
    #[allow(dead_code)]
    fn holds_dyn(store: Arc<dyn MemoryStore>) -> Arc<dyn MemoryStore> {
        store
    }

    #[test]
    fn source_uses_ref_on_the_wire() {
        let s = Source {
            kind: SourceKind::Email,
            reference: "msg-1".into(),
            run_id: None,
        };
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            serde_json::json!({"kind": "email", "ref": "msg-1"})
        );
    }

    #[test]
    fn ts_now_fits_timestamptz() {
        assert_eq!(ts_now().timestamp_subsec_nanos() % 1_000, 0);
        assert_eq!(contact_key("  Vasanth@Example.COM "), "vasanth@example.com");
    }

    #[test]
    fn ledger_mode_spellings() {
        assert_eq!(
            serde_json::to_string(&LedgerMode::TimeoutProceed).unwrap(),
            "\"timeout_proceed\""
        );
        assert_eq!(LedgerMode::from(Mode::Ask), LedgerMode::Ask);
        assert_eq!(
            serde_json::to_string(&ToolStatus::NeedsRepair).unwrap(),
            "\"needs_repair\""
        );
    }
}
