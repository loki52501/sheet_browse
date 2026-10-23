# 02 · Memory (crates `core` trait, `core-pg` backend)

One database, behind a trait. Items are `M-n`.

## M-1 The five jobs memory does

1. **Facts with provenance and time.** "Vasanth is at Rapidflare, from an email on 2026-10-03."
2. **Relations.** person ↔ company ↔ event ↔ thread ↔ tool. Multi-hop questions.
3. **Recall by meaning.** "that Rust CDP crate I looked at" with no keyword. Vectors.
4. **Exact search.** names, domains, subjects. Full-text and trigram.
5. **Append-only history.** ledger, watch baselines, tool health over time.

## M-2 The trait (in `core`)

```rust
pub trait MemoryStore: Send + Sync {
    async fn remember(&self, f: Fact) -> Result<FactId>;
    async fn forget(&self, id: FactId) -> Result<()>;              // user-requested only
    async fn link(&self, src: NodeId, rel: &str, dst: NodeId, source: Source) -> Result<()>;
    async fn node(&self, kind: &str, name: &str) -> Result<NodeId>; // get-or-create
    async fn neighbors(&self, n: NodeId, rel: Option<&str>, depth: u8) -> Result<Vec<Node>>;
    async fn recall(&self, q: &str, k: usize, filter: Option<Filter>) -> Result<Vec<Hit>>; // vector + fts fused
    async fn search(&self, q: &str, k: usize) -> Result<Vec<Hit>>;  // exact / trigram
    async fn contact(&self, person: &str) -> Result<Option<Contact>>;
    async fn touch_contact(&self, person: &str, channel: &str, note: &str) -> Result<()>;
    async fn due_reviews(&self, now: Ts, k: usize) -> Result<Vec<Review>>;
    async fn grade_review(&self, id: ReviewId, grade: u8) -> Result<()>; // SM-2 update
    async fn baseline_get(&self, watch: &str) -> Result<Option<Baseline>>;
    async fn baseline_set(&self, watch: &str, b: Baseline) -> Result<()>;
    async fn append(&self, e: LedgerEvent) -> Result<()>;
    async fn tool_health(&self, domain: &str) -> Result<Vec<ToolHealth>>;
    async fn tool_health_set(&self, h: ToolHealth) -> Result<()>;
    async fn gate_rule(&self, domain: &str, class: &str) -> Result<Option<Mode>>;
}
```

Every harness component holds `Arc<dyn MemoryStore>`. Nothing else talks to the DB.

## M-3 Postgres schema (`core-pg`, `migrations/`)

```sql
create schema memory; create schema ledger; create schema tools; create schema gate;

create table memory.facts (
  id bigserial primary key, subject text not null, key text not null, value text not null,
  source jsonb not null,           -- {kind: email|page|user|tool, ref, run_id}
  seen_at timestamptz not null default now(), confidence real default 1.0,
  embedding vector(1536), tsv tsvector generated always as (to_tsvector('english', subject||' '||key||' '||value)) stored
);
create index on memory.facts using hnsw (embedding vector_cosine_ops);
create index on memory.facts using gin (tsv);
create index on memory.facts using gin (subject gin_trgm_ops);

create table memory.nodes (id bigserial primary key, kind text not null, name text not null, unique(kind, name));
create table memory.edges (src bigint references memory.nodes, rel text not null, dst bigint references memory.nodes,
  since timestamptz default now(), source jsonb, primary key (src, rel, dst));

create table memory.contacts (person text primary key, channel text, last timestamptz, count int default 0, note text);
create table memory.reviews (id bigserial primary key, item text, answer text, due timestamptz, interval_days real default 1, ease real default 2.5, reps int default 0);
create table memory.baselines (watch text primary key, hash text, snapshot jsonb, updated timestamptz);

create table ledger.events (id bigserial primary key, run_id text, ts timestamptz default now(), server text, tool text,
  class text, mode text, args jsonb, result_summary text, consequences jsonb, duration_ms int, ok bool, why text);
create or replace function ledger.no_change() returns trigger language plpgsql as $$ begin raise exception 'ledger is append-only'; end $$;
create trigger ledger_ro before update or delete on ledger.events for each row execute function ledger.no_change();

create table runs (id text primary key, started timestamptz, ended timestamptz, prompt text, status text, folder text, parent_run text);
create table schedules (id serial primary key, cron text, prompt text, kind text, watch_target text, enabled bool default true, last_run timestamptz);

create table tools.health (domain text, name text, version int, status text, runs int default 0, fails int default 0,
  last_ok timestamptz, last_fail timestamptz, why text, pinned bool default false, primary key (domain, name));
create table gate.rules (domain text, class text, mode text, note text, primary key (domain, class));
insert into gate.rules values ('*','Pay','ask',''),('*','Account','ask',''),('*','Delete','ask',''),('*','Challenge','stop','');
```

Extensions: `vector`, `pg_trgm`. Embeddings come from a local model
(`fastembed` crate, bge-small, 384 dims is acceptable; if so change the column) because
there are no API keys. Decide once in `decisions.yaml`.

## M-4 Recall fusion

`recall(q)` = top 20 by cosine ∪ top 20 by `ts_rank` → reciprocal rank fusion → top k.
Filters: subject prefix, since, source kind. Returns `Hit{fact, score, why: "vector"|"fts"|"both"}`.

## M-5 Relations

`neighbors(n, rel, depth)` is a recursive CTE capped at depth 4 and 500 rows. Kinds used
by Phase 1: `person company event thread role site tool`. Edges: `works_at met_at
attended applied_to emailed about_tool`.

## M-6 What is not in the database

| Lives in | Why |
|---|---|
| `tools/` (git) | tool code and `_site.yaml`: human-readable, diffable, revertable |
| vault file | secrets and personal data: never in a row a query can return |
| `runs/` | reports, files, frames: what the user opens |
| `agent-memory/*.yaml` | coding-agent memory: lives with the code |

## M-7 Swap targets

For a shipped desktop build, implement `MemoryStore` on SQLite + sqlite-vec + FTS5
first, SurrealDB embedded second. Each must pass the Phase 1 test ladder before it is
offered. Never run two backends at once. The schema above ports to both: nodes/edges
tables and recursive CTEs work in SQLite; SurrealDB uses record links.

## Acceptance (step 1 and step 4)

- `remember` + `recall("Rust CDP crate")` returns the fact with `why=both` when the fact
  text contains "chromiumoxide" and the query does not.
- 3-hop `neighbors` returns roles reachable from a person via `met_at → attended → works_at`.
- `touch_contact` makes a Send to that person `allow` on the next gate check.
- `grade_review` follows SM-2: grade 5 roughly multiplies interval by ease; grade < 3 resets to 1 day.
- A `watch` schedule with an unchanged target produces no notification; a changed one produces exactly one.
