//! core-pg: `MemoryStore` (I-1) on Postgres (docs/02 M-3..M-5). Owned by area core (T-03).
//! Schema: `migrations/` at the repo root. Embeddings: fastembed bge-small-en-v1.5, 384 dims (D-32).

use async_trait::async_trait;
use harness_core::memory::*;
use harness_core::run::RunId;
use harness_core::Harness;

/// docker-compose.yml's database (D-33): loopback only, trust auth, no password in the repo.
pub const DEFAULT_DATABASE_URL: &str = "postgres://sb@127.0.0.1:5433/sb";

/// Embedding width; must match `vector(384)` in migrations/0001_init.sql (D-32).
pub const EMBEDDING_DIMS: usize = 384;

pub struct PgStore {
    _private: (),
}

impl PgStore {
    /// Connects and runs pending migrations.
    pub async fn connect(url: &str) -> Result<PgStore> {
        let _ = url;
        todo!("T-03: sqlx pool + migrate")
    }
}

#[async_trait]
impl MemoryStore for PgStore {
    async fn remember(&self, f: Fact) -> Result<FactId> {
        let _ = f;
        todo!("T-03")
    }
    async fn forget(&self, id: FactId) -> Result<()> {
        let _ = id;
        todo!("T-03")
    }
    async fn link(&self, src: NodeId, rel: &str, dst: NodeId, source: Source) -> Result<()> {
        let _ = (src, rel, dst, source);
        todo!("T-03")
    }
    async fn node(&self, kind: &str, name: &str) -> Result<NodeId> {
        let _ = (kind, name);
        todo!("T-03")
    }
    async fn neighbors(&self, n: NodeId, rel: Option<&str>, depth: u8) -> Result<Vec<Node>> {
        let _ = (n, rel, depth);
        todo!("T-03: M-5 recursive CTE")
    }
    async fn recall(&self, q: &str, k: usize, filter: Option<Filter>) -> Result<Vec<Hit>> {
        let _ = (q, k, filter);
        todo!("T-03: M-4 fusion")
    }
    async fn search(&self, q: &str, k: usize) -> Result<Vec<Hit>> {
        let _ = (q, k);
        todo!("T-03")
    }
    async fn contact(&self, person: &str) -> Result<Option<Contact>> {
        let _ = person;
        todo!("T-03")
    }
    async fn touch_contact(&self, person: &str, channel: &str, note: &str) -> Result<()> {
        let _ = (person, channel, note);
        todo!("T-03")
    }
    async fn due_reviews(&self, now: Ts, k: usize) -> Result<Vec<Review>> {
        let _ = (now, k);
        todo!("T-03")
    }
    async fn grade_review(&self, id: ReviewId, grade: u8) -> Result<()> {
        let _ = (id, grade);
        todo!("T-03: SM-2")
    }
    async fn baseline_get(&self, watch: &str) -> Result<Option<Baseline>> {
        let _ = watch;
        todo!("T-03")
    }
    async fn baseline_set(&self, watch: &str, b: Baseline) -> Result<()> {
        let _ = (watch, b);
        todo!("T-03")
    }
    async fn append(&self, e: LedgerEvent) -> Result<LedgerId> {
        let _ = e;
        todo!("T-03")
    }
    async fn tool_health(&self, domain: &str) -> Result<Vec<ToolHealth>> {
        let _ = domain;
        todo!("T-03")
    }
    async fn tool_health_set(&self, h: ToolHealth) -> Result<()> {
        let _ = h;
        todo!("T-03")
    }
    async fn gate_rule(&self, domain: &str, class: &str) -> Result<Option<GateRule>> {
        let _ = (domain, class);
        todo!("T-03")
    }
    async fn run_start(&self, run: Run) -> Result<()> {
        let _ = run;
        todo!("T-03")
    }
    async fn run_end(&self, id: &RunId, status: RunStatus, ended: Ts) -> Result<()> {
        let _ = (id, status, ended);
        todo!("T-03")
    }
    async fn run_get(&self, id: &RunId) -> Result<Option<Run>> {
        let _ = id;
        todo!("T-03")
    }
    async fn schedule_add(&self, s: Schedule) -> Result<i32> {
        let _ = s;
        todo!("T-03")
    }
    async fn schedule_list(&self) -> Result<Vec<Schedule>> {
        todo!("T-03")
    }
    async fn schedule_rm(&self, id: i32) -> Result<()> {
        let _ = id;
        todo!("T-03")
    }
    async fn schedule_fired(&self, id: i32, at: Ts) -> Result<()> {
        let _ = (id, at);
        todo!("T-03")
    }
    async fn gate_rule_set(
        &self,
        domain: &str,
        class: &str,
        rule: GateRule,
        note: &str,
    ) -> Result<()> {
        let _ = (domain, class, rule, note);
        todo!("T-03: reject Challenge rules that are not stop (D-5)")
    }
    async fn review_add(&self, item: &str, answer: &str) -> Result<ReviewId> {
        let _ = (item, answer);
        todo!("T-03")
    }
    async fn ledger_events(&self, run: &RunId) -> Result<Vec<(LedgerId, LedgerEvent)>> {
        let _ = run;
        todo!("T-03")
    }
}

/// Builds the [`Harness`] a tool server's `main` hands to `harness_core::serve`: the store at
/// `DATABASE_URL` (default [`DEFAULT_DATABASE_URL`]), the vault, the asker, the run from the
/// launching session. Which variables and files it reads is T-07's call (never a key, D-1).
pub async fn bootstrap() -> Result<Harness> {
    todo!("T-07: server bootstrap")
}
