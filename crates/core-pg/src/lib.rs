//! core-pg: `MemoryStore` (I-1) on Postgres (docs/02 M-3..M-5). Owned by area core (T-03).
//! Schema: `migrations/` at the repo root. Embeddings: fastembed bge-small-en-v1.5, 384 dims (D-32).
//!
//! Queries are runtime `sqlx::query` with binds (no compile-time macros, so no DATABASE_URL
//! at build time). Errors map to `MemoryError::Backend` with the operation and SQLSTATE, never
//! the server's message text (which can quote a bound value), except our own `RAISE` text.

mod embed;

pub use embed::model_dir;

use async_trait::async_trait;
use harness_core::class::{Class, Mode};
use harness_core::memory::*;
use harness_core::run::RunId;
use harness_core::Harness;
use pgvector::Vector;
use serde::{de::DeserializeOwned, Serialize};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::types::Json;
use sqlx::Row;
use std::fmt;
use std::time::Duration;

/// docker-compose.yml's database (D-33): loopback only, trust auth, no password in the repo.
pub const DEFAULT_DATABASE_URL: &str = "postgres://sb@127.0.0.1:5433/sb";

/// Embedding width; must match `vector(384)` in migrations/0001_init.sql (D-32).
pub const EMBEDDING_DIMS: usize = 384;

/// M-4: each retrieval path contributes its top 20 before fusion.
const RECALL_POOL: i64 = 20;
/// D-103: reciprocal rank fusion constant.
const RRF_K: i64 = 60;
/// M-5: neighbors never walks deeper than 4 hops or returns more than 500 nodes.
const MAX_DEPTH: u8 = 4;
const MAX_NEIGHBORS: i64 = 500;

const SETUP_HINT: &str =
    "is Postgres up? run `docker compose up -d --wait postgres` at the repo root";

pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    /// Connects and runs pending migrations.
    pub async fn connect(url: &str) -> Result<PgStore> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(10))
            .connect(url)
            .await
            .map_err(|e| backend(format!("connecting: {} ({SETUP_HINT})", describe(&e))))?;
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .map_err(|e| backend(format!("migrating: {e}")))?;
        Ok(PgStore { pool })
    }

    /// The underlying pool, for tests and `harness db` maintenance. Nothing outside the store
    /// writes memory through it.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Embeds `text` with the store's model (bge-small-en-v1.5). Loads the model on first use.
    pub async fn embed(text: &str) -> Result<Vec<f32>> {
        embed::embed_one(text.to_string()).await
    }
}

// ---------------------------------------------------------------- errors

/// A backend failure whose message is already safe to show: no bound values.
#[derive(Debug)]
struct PgError(String);

impl fmt::Display for PgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PgError {}

pub(crate) fn backend(msg: impl Into<String>) -> MemoryError {
    MemoryError::Backend(Box::new(PgError(msg.into())))
}

/// A description of a sqlx error that carries no row or parameter values.
fn describe(e: &sqlx::Error) -> String {
    match e {
        sqlx::Error::Database(d) => {
            let mut s = format!("sqlstate {}", d.code().unwrap_or_default());
            if let Some(c) = d.constraint() {
                s.push_str(" on ");
                s.push_str(c);
            }
            // P0001 is RAISE EXCEPTION from our own functions (the ledger guard): fixed text.
            if d.code().as_deref() == Some("P0001") {
                s.push_str(&format!(": {}", d.message()));
            }
            s
        }
        sqlx::Error::ColumnDecode { index, .. } => format!("decoding column {index}"),
        sqlx::Error::Decode(_) => "decoding a value".into(),
        sqlx::Error::Encode(_) => "encoding a value".into(),
        sqlx::Error::Configuration(_) => "bad database URL or options".into(),
        other => other.to_string(),
    }
}

fn db(op: &'static str) -> impl FnOnce(sqlx::Error) -> MemoryError {
    move |e| backend(format!("{op}: {}", describe(&e)))
}

fn sqlstate(e: &sqlx::Error) -> Option<String> {
    match e {
        sqlx::Error::Database(d) => d.code().map(|c| c.into_owned()),
        _ => None,
    }
}

// ---------------------------------------------------------------- enum spellings

/// The serde spelling of a unit enum variant (`needs_repair`, `timeout_proceed`, ...).
fn spell<T: Serialize>(v: &T) -> Result<String> {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        _ => Err(backend("enum has no string spelling")),
    }
}

fn parse<T: DeserializeOwned>(column: &str, s: String) -> Result<T> {
    serde_json::from_value(serde_json::Value::String(s))
        .map_err(|_| backend(format!("unknown value in column {column}")))
}

fn get<'r, T>(row: &'r PgRow, col: &'static str) -> Result<T>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get(col)
        .map_err(|e| backend(format!("reading {col}: {}", describe(&e))))
}

// ---------------------------------------------------------------- row mapping

fn fact_row(row: &PgRow) -> Result<(FactId, Fact)> {
    let Json(source): Json<Source> = get(row, "source")?;
    Ok((
        FactId(get(row, "id")?),
        Fact {
            subject: get(row, "subject")?,
            key: get(row, "key")?,
            value: get(row, "value")?,
            source,
            seen_at: get(row, "seen_at")?,
            confidence: get(row, "confidence")?,
        },
    ))
}

fn review_row(row: &PgRow) -> Result<Review> {
    Ok(Review {
        id: ReviewId(get(row, "id")?),
        item: get(row, "item")?,
        answer: get(row, "answer")?,
        due: get(row, "due")?,
        interval_days: get(row, "interval_days")?,
        ease: get(row, "ease")?,
        reps: get(row, "reps")?,
    })
}

fn run_row(row: &PgRow) -> Result<Run> {
    Ok(Run {
        id: RunId(get(row, "id")?),
        started: get(row, "started")?,
        ended: get(row, "ended")?,
        prompt: get(row, "prompt")?,
        status: parse("runs.status", get(row, "status")?)?,
        folder: get(row, "folder")?,
        parent_run: get::<Option<String>>(row, "parent_run")?.map(RunId),
        strict: get(row, "strict")?,
    })
}

fn schedule_row(row: &PgRow) -> Result<Schedule> {
    Ok(Schedule {
        id: get(row, "id")?,
        cron: get(row, "cron")?,
        prompt: get(row, "prompt")?,
        kind: parse("schedules.kind", get(row, "kind")?)?,
        watch_target: get(row, "watch_target")?,
        enabled: get(row, "enabled")?,
        last_run: get(row, "last_run")?,
        run_id: get::<Option<String>>(row, "run_id")?.map(RunId),
    })
}

fn health_row(row: &PgRow) -> Result<ToolHealth> {
    Ok(ToolHealth {
        domain: get(row, "domain")?,
        name: get(row, "name")?,
        version: get(row, "version")?,
        status: parse("tools.health.status", get(row, "status")?)?,
        runs: get(row, "runs")?,
        fails: get(row, "fails")?,
        last_ok: get(row, "last_ok")?,
        last_fail: get(row, "last_fail")?,
        why: get(row, "why")?,
        pinned: get(row, "pinned")?,
    })
}

fn ledger_row(row: &PgRow) -> Result<(LedgerId, LedgerEvent)> {
    let class = match get::<Option<String>>(row, "class")? {
        Some(c) => Some(
            c.parse::<Class>()
                .map_err(|_| backend("unknown value in column ledger.events.class"))?,
        ),
        None => None,
    };
    let mode = match get::<Option<String>>(row, "mode")? {
        Some(m) => Some(parse::<LedgerMode>("ledger.events.mode", m)?),
        None => None,
    };
    let Json(args): Json<serde_json::Value> = get(row, "args")?;
    let Json(consequences): Json<serde_json::Value> = get(row, "consequences")?;
    Ok((
        LedgerId(get(row, "id")?),
        LedgerEvent {
            run_id: get::<Option<String>>(row, "run_id")?.map(RunId),
            ts: get(row, "ts")?,
            server: get(row, "server")?,
            tool: get(row, "tool")?,
            class,
            mode,
            args,
            result_summary: get(row, "result_summary")?,
            consequences,
            duration_ms: get(row, "duration_ms")?,
            ok: get(row, "ok")?,
            why: get(row, "why")?,
        },
    ))
}

// ---------------------------------------------------------------- pure helpers

/// SM-2 after one graded repetition: `(reps, interval_days, ease)` -> the new triple.
/// A pass (grade >= 3) steps the interval 1, 6, then `interval * ease` (rounded to whole
/// days, using the ease before this grade); a fail resets to 1 day and reps 0. The ease
/// moves by the SM-2 formula on every grade and never drops below 1.3. Same arithmetic as
/// `harness_core::testing::MemStore`, so the fake and the real store agree.
pub fn sm2(reps: i32, interval_days: f32, ease: f32, grade: u8) -> (i32, f32, f32) {
    let q = f32::from(grade.min(5));
    let (reps, interval) = if grade >= 3 {
        let interval = match reps {
            r if r <= 0 => 1.0,
            1 => 6.0,
            _ => (interval_days * ease).round().max(1.0),
        };
        (reps.max(0).saturating_add(1), interval)
    } else {
        (0, 1.0)
    };
    let ease = (ease + (0.1 - (5.0 - q) * (0.08 + (5.0 - q) * 0.02))).max(1.3);
    (reps, interval, ease)
}

/// D-110: a `gate.rules.domain` key is trimmed, lowercased, trailing dot removed, or `*`.
/// `None` when nothing canonical is left (empty, or whitespace or `/` inside).
// TODO(T-03/D-110): call harness_core::gate's canonicalisers once they land, so every
// backend and the gate share one definition.
fn rule_domain(domain: &str) -> Option<String> {
    let d = domain.trim().to_lowercase();
    let d = d.strip_suffix('.').unwrap_or(&d);
    if d.is_empty() || d.chars().any(|c| c.is_whitespace() || c == '/') {
        return None;
    }
    Some(d.to_string())
}

/// D-110: a `gate.rules.class` key is `Class::as_str()`, optionally `:` and the trimmed,
/// lowercased hint (D-39). `None` for an unknown class or an empty hint.
fn rule_key(class: &str) -> Option<(Class, String)> {
    let (base, hint) = match class.split_once(':') {
        Some((b, h)) => (b, Some(h.trim().to_lowercase())),
        None => (class, None),
    };
    let base: Class = base.trim().parse().ok()?;
    match hint {
        Some(h) if h.is_empty() => None,
        Some(h) => Some((base, format!("{}:{h}", base.as_str()))),
        None => Some((base, base.as_str().to_string())),
    }
}

/// Canonicalises a rule's `(domain, class)` (D-110) and enforces D-5.
fn check_rule(domain: &str, class: &str, rule: GateRule) -> Result<(String, String)> {
    let d = rule_domain(domain).ok_or_else(|| {
        MemoryError::Invalid(format!("gate rule domain {domain:?} is not a domain"))
    })?;
    let (base, key) = rule_key(class).ok_or_else(|| {
        MemoryError::Invalid(format!(
            "gate rule class {class:?} is not `Class` or `Class:hint`"
        ))
    })?;
    if base == Class::Challenge && rule.mode != Mode::Stop {
        return Err(MemoryError::Invalid(
            "a Challenge rule must be stop (D-5)".into(),
        ));
    }
    Ok((d, key))
}

fn limit(k: usize) -> i64 {
    i64::try_from(k).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------- SQL

/// M-5. Walks edges both ways; `$3` (rel) applies to every hop. `union` dedups (node, depth)
/// pairs, so the walk is bounded by nodes x depth even on cyclic graphs. Nearest first.
const NEIGHBORS_SQL: &str = "
with recursive walk(id, depth) as (
    select $1::bigint, 0
  union
    select case when e.src = w.id then e.dst else e.src end, w.depth + 1
    from walk w
    join memory.edges e on e.src = w.id or e.dst = w.id
    where w.depth < $2 and ($3::text is null or e.rel = $3)
)
select n.id, n.kind, n.name, min(w.depth) as d
from walk w join memory.nodes n on n.id = w.id
where n.id <> $1
group by n.id, n.kind, n.name
order by d, n.id
limit $4";

/// M-4 + D-103: cosine top 20 and OR-of-lexemes `ts_rank` top 20, fused by RRF (k = 60).
/// Filters: $3 subject prefix, $4 since, $5 source kind. $6 = k.
const RECALL_SQL: &str = "
with tq as (
  select replace(plainto_tsquery('english', $1)::text, ' & ', ' | ')::tsquery as q
),
v as (
  select id, row_number() over (order by d, id) as r from (
    select id, embedding <=> $2 as d from memory.facts
    where embedding is not null
      and ($3::text is null or starts_with(subject, $3))
      and ($4::timestamptz is null or seen_at >= $4)
      and ($5::text is null or source->>'kind' = $5)
    order by embedding <=> $2
    limit $7
  ) s
),
t as (
  select id, row_number() over (order by rk desc, id) as r from (
    select f.id, ts_rank(f.tsv, tq.q) as rk from memory.facts f, tq
    where f.tsv @@ tq.q
      and ($3::text is null or starts_with(f.subject, $3))
      and ($4::timestamptz is null or f.seen_at >= $4)
      and ($5::text is null or f.source->>'kind' = $5)
    order by rk desc, f.id
    limit $7
  ) s
),
fused as (
  select coalesce(v.id, t.id) as id,
         (coalesce(1.0 / ($8 + v.r), 0) + coalesce(1.0 / ($8 + t.r), 0))::float8 as score,
         v.id is not null as hv, t.id is not null as ht
  from v full join t on v.id = t.id
)
select f.id, f.subject, f.key, f.value, f.source, f.seen_at, f.confidence,
       fused.score, fused.hv, fused.ht
from fused join memory.facts f on f.id = fused.id
order by fused.score desc, f.id
limit $6";

/// Exact first (case-insensitive equality scores 1.0, substring 0.9, on subject/key/value),
/// then trigram similarity on subject for what exact missed.
const SEARCH_SQL: &str = "
with ex as (
  select id,
         case when lower(subject) = lower($1) or lower(key) = lower($1) or lower(value) = lower($1)
              then 1.0 else 0.9 end::float8 as score,
         0 as tier
  from memory.facts
  where strpos(lower(subject), lower($1)) > 0
     or strpos(lower(key), lower($1)) > 0
     or strpos(lower(value), lower($1)) > 0
  order by score desc, seen_at desc, id
  limit $2
),
tg as (
  select id, similarity(subject, $1)::float8 as score, 1 as tier
  from memory.facts
  where subject % $1 and id not in (select id from ex)
  order by score desc, seen_at desc, id
  limit $2
),
hits as (select * from ex union all select * from tg)
select f.id, f.subject, f.key, f.value, f.source, f.seen_at, f.confidence, hits.score, hits.tier
from hits join memory.facts f on f.id = hits.id
order by hits.tier, hits.score desc, f.seen_at desc, f.id
limit $2";

// ---------------------------------------------------------------- the store

#[async_trait]
impl MemoryStore for PgStore {
    async fn remember(&self, f: Fact) -> Result<FactId> {
        if !(0.0..=1.0).contains(&f.confidence) {
            return Err(MemoryError::Invalid("confidence must be in 0..=1".into()));
        }
        let embedding = embed::embed_one(format!("{} {} {}", f.subject, f.key, f.value)).await?;
        let id: i64 = sqlx::query_scalar(
            "insert into memory.facts (subject, key, value, source, seen_at, confidence, embedding)
             values ($1, $2, $3, $4, $5, $6, $7) returning id",
        )
        .bind(&f.subject)
        .bind(&f.key)
        .bind(&f.value)
        .bind(Json(&f.source))
        .bind(f.seen_at)
        .bind(f.confidence)
        .bind(Vector::from(embedding))
        .fetch_one(&self.pool)
        .await
        .map_err(db("remember"))?;
        Ok(FactId(id))
    }

    async fn forget(&self, id: FactId) -> Result<()> {
        let done = sqlx::query("delete from memory.facts where id = $1")
            .bind(id.0)
            .execute(&self.pool)
            .await
            .map_err(db("forget"))?;
        if done.rows_affected() == 0 {
            return Err(MemoryError::NotFound(format!("fact {}", id.0)));
        }
        Ok(())
    }

    async fn link(&self, src: NodeId, rel: &str, dst: NodeId, source: Source) -> Result<()> {
        if rel.trim().is_empty() {
            return Err(MemoryError::Invalid("edge relation is empty".into()));
        }
        sqlx::query(
            "insert into memory.edges (src, rel, dst, source) values ($1, $2, $3, $4)
             on conflict (src, rel, dst) do nothing",
        )
        .bind(src.0)
        .bind(rel)
        .bind(dst.0)
        .bind(Json(&source))
        .execute(&self.pool)
        .await
        .map_err(|e| {
            if sqlstate(&e).as_deref() == Some("23503") {
                MemoryError::NotFound(format!("node {} or {}", src.0, dst.0))
            } else {
                db("link")(e)
            }
        })?;
        Ok(())
    }

    async fn node(&self, kind: &str, name: &str) -> Result<NodeId> {
        if kind.trim().is_empty() || name.trim().is_empty() {
            return Err(MemoryError::Invalid(
                "node kind and name are required".into(),
            ));
        }
        // Insert-or-select; a concurrent insert of the same node can make both miss for one
        // round (the insert waits for the other commit, the select's snapshot predates it).
        for _ in 0..3 {
            let id: Option<i64> = sqlx::query_scalar(
                "with ins as (
                   insert into memory.nodes (kind, name) values ($1, $2)
                   on conflict (kind, name) do nothing returning id)
                 select id from ins
                 union all
                 select id from memory.nodes where kind = $1 and name = $2
                 limit 1",
            )
            .bind(kind)
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(db("node"))?;
            if let Some(id) = id {
                return Ok(NodeId(id));
            }
        }
        Err(backend("node: get-or-create did not settle"))
    }

    async fn neighbors(&self, n: NodeId, rel: Option<&str>, depth: u8) -> Result<Vec<Node>> {
        let depth = depth.min(MAX_DEPTH);
        if depth == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(NEIGHBORS_SQL)
            .bind(n.0)
            .bind(i32::from(depth))
            .bind(rel)
            .bind(MAX_NEIGHBORS)
            .fetch_all(&self.pool)
            .await
            .map_err(db("neighbors"))?;
        rows.iter()
            .map(|r| {
                Ok(Node {
                    id: NodeId(get(r, "id")?),
                    kind: get(r, "kind")?,
                    name: get(r, "name")?,
                })
            })
            .collect()
    }

    async fn recall(&self, q: &str, k: usize, filter: Option<Filter>) -> Result<Vec<Hit>> {
        if k == 0 || q.trim().is_empty() {
            return Ok(Vec::new());
        }
        let filter = filter.unwrap_or_default();
        let kind = match filter.source_kind {
            Some(sk) => Some(spell(&sk)?),
            None => None,
        };
        let qv = Vector::from(embed::embed_one(q.to_string()).await?);
        let query = sqlx::query(RECALL_SQL)
            .bind(q)
            .bind(&qv)
            .bind(filter.subject_prefix.as_deref())
            .bind(filter.since)
            .bind(kind.as_deref())
            .bind(limit(k))
            .bind(RECALL_POOL)
            .bind(RRF_K);
        let filtered = filter != Filter::default();
        let rows = if filtered {
            // With a filter the HNSW scan must keep going until 20 rows pass it (pgvector
            // 0.8 iterative scan); without one the plain index scan already returns 20.
            let mut tx = self.pool.begin().await.map_err(db("recall"))?;
            sqlx::query("set local hnsw.iterative_scan = relaxed_order")
                .execute(&mut *tx)
                .await
                .map_err(db("recall"))?;
            let rows = query.fetch_all(&mut *tx).await.map_err(db("recall"))?;
            tx.commit().await.map_err(db("recall"))?;
            rows
        } else {
            query.fetch_all(&self.pool).await.map_err(db("recall"))?
        };
        rows.iter()
            .map(|r| {
                let (id, fact) = fact_row(r)?;
                let score: f64 = get(r, "score")?;
                let why = match (get::<bool>(r, "hv")?, get::<bool>(r, "ht")?) {
                    (true, true) => Why::Both,
                    (true, false) => Why::Vector,
                    _ => Why::Fts,
                };
                Ok(Hit {
                    id,
                    fact,
                    score: score as f32,
                    why,
                })
            })
            .collect()
    }

    async fn search(&self, q: &str, k: usize) -> Result<Vec<Hit>> {
        let q = q.trim();
        if k == 0 || q.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(SEARCH_SQL)
            .bind(q)
            .bind(limit(k))
            .fetch_all(&self.pool)
            .await
            .map_err(db("search"))?;
        rows.iter()
            .map(|r| {
                let (id, fact) = fact_row(r)?;
                let score: f64 = get(r, "score")?;
                let tier: i32 = get(r, "tier")?;
                Ok(Hit {
                    id,
                    fact,
                    score: score as f32,
                    why: if tier == 0 { Why::Exact } else { Why::Trigram },
                })
            })
            .collect()
    }

    async fn contact(&self, person: &str) -> Result<Option<Contact>> {
        let row = sqlx::query(
            "select person, channel, last, count, note from memory.contacts where person = $1",
        )
        .bind(contact_key(person))
        .fetch_optional(&self.pool)
        .await
        .map_err(db("contact"))?;
        row.map(|r| {
            Ok(Contact {
                person: get(&r, "person")?,
                channel: get(&r, "channel")?,
                last: get(&r, "last")?,
                count: get(&r, "count")?,
                note: get(&r, "note")?,
            })
        })
        .transpose()
    }

    async fn touch_contact(&self, person: &str, channel: &str, note: &str) -> Result<()> {
        let key = contact_key(person);
        if key.is_empty() {
            return Err(MemoryError::Invalid("contact is empty".into()));
        }
        // An empty channel or note keeps what is stored (same as MemStore). Timestamps come
        // from this process, not the server, so they compare with callers' `now` (the
        // container clock can run ahead of the host).
        sqlx::query(
            "insert into memory.contacts (person, channel, last, count, note)
             values ($1, nullif($2, ''), $4, 1, nullif($3, ''))
             on conflict (person) do update set
               count = memory.contacts.count + 1,
               last = excluded.last,
               channel = coalesce(excluded.channel, memory.contacts.channel),
               note = coalesce(excluded.note, memory.contacts.note)",
        )
        .bind(key)
        .bind(channel)
        .bind(note)
        .bind(ts_now())
        .execute(&self.pool)
        .await
        .map_err(db("touch_contact"))?;
        Ok(())
    }

    async fn due_reviews(&self, now: Ts, k: usize) -> Result<Vec<Review>> {
        let rows = sqlx::query("select id, item, answer, due, interval_days, ease, reps from memory.reviews where due <= $1 order by due, id limit $2")
        .bind(now)
        .bind(limit(k))
        .fetch_all(&self.pool)
        .await
        .map_err(db("due_reviews"))?;
        rows.iter().map(review_row).collect()
    }

    async fn grade_review(&self, id: ReviewId, grade: u8) -> Result<()> {
        if grade > 5 {
            return Err(MemoryError::Invalid(format!("grade {grade} outside 0..=5")));
        }
        let mut tx = self.pool.begin().await.map_err(db("grade_review"))?;
        let row = sqlx::query("select id, item, answer, due, interval_days, ease, reps from memory.reviews where id = $1 for update")
        .bind(id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db("grade_review"))?
        .ok_or_else(|| MemoryError::NotFound(format!("review {}", id.0)))?;
        let r = review_row(&row)?;
        let (reps, interval, ease) = sm2(r.reps, r.interval_days, r.ease, grade);
        let secs = (f64::from(interval) * 86_400.0) as i64;
        let due = ts_now() + chrono::Duration::seconds(secs);
        sqlx::query(
            "update memory.reviews set reps = $2, interval_days = $3, ease = $4, due = $5
             where id = $1",
        )
        .bind(id.0)
        .bind(reps)
        .bind(interval)
        .bind(ease)
        .bind(due)
        .execute(&mut *tx)
        .await
        .map_err(db("grade_review"))?;
        tx.commit().await.map_err(db("grade_review"))?;
        Ok(())
    }

    async fn baseline_get(&self, watch: &str) -> Result<Option<Baseline>> {
        let row =
            sqlx::query("select hash, snapshot, updated from memory.baselines where watch = $1")
                .bind(watch)
                .fetch_optional(&self.pool)
                .await
                .map_err(db("baseline_get"))?;
        row.map(|r| {
            let Json(snapshot): Json<serde_json::Value> = get(&r, "snapshot")?;
            Ok(Baseline {
                hash: get(&r, "hash")?,
                snapshot,
                updated: get(&r, "updated")?,
            })
        })
        .transpose()
    }

    async fn baseline_set(&self, watch: &str, b: Baseline) -> Result<()> {
        sqlx::query(
            "insert into memory.baselines (watch, hash, snapshot, updated) values ($1, $2, $3, $4)
             on conflict (watch) do update set
               hash = excluded.hash, snapshot = excluded.snapshot, updated = excluded.updated",
        )
        .bind(watch)
        .bind(&b.hash)
        .bind(Json(&b.snapshot))
        .bind(b.updated)
        .execute(&self.pool)
        .await
        .map_err(db("baseline_set"))?;
        Ok(())
    }

    async fn append(&self, e: LedgerEvent) -> Result<LedgerId> {
        if e.duration_ms < 0 {
            return Err(MemoryError::Invalid("duration_ms is negative".into()));
        }
        let mode = match e.mode {
            Some(m) => Some(spell(&m)?),
            None => None,
        };
        let id: i64 = sqlx::query_scalar(
            "insert into ledger.events (run_id, ts, server, tool, class, mode, args,
               result_summary, consequences, duration_ms, ok, why)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) returning id",
        )
        .bind(e.run_id.as_ref().map(|r| r.0.as_str()))
        .bind(e.ts)
        .bind(&e.server)
        .bind(&e.tool)
        .bind(e.class.map(Class::as_str))
        .bind(mode)
        .bind(Json(&e.args))
        .bind(&e.result_summary)
        .bind(Json(&e.consequences))
        .bind(e.duration_ms)
        .bind(e.ok)
        .bind(e.why.as_deref())
        .fetch_one(&self.pool)
        .await
        .map_err(db("append"))?;
        Ok(LedgerId(id))
    }

    async fn tool_health(&self, domain: &str) -> Result<Vec<ToolHealth>> {
        let rows = sqlx::query("select domain, name, version, status, runs, fails, last_ok, last_fail, why, pinned from tools.health where domain = $1 order by name")
        .bind(domain)
        .fetch_all(&self.pool)
        .await
        .map_err(db("tool_health"))?;
        rows.iter().map(health_row).collect()
    }

    async fn tool_health_set(&self, h: ToolHealth) -> Result<()> {
        if h.domain.trim().is_empty() || h.name.trim().is_empty() {
            return Err(MemoryError::Invalid(
                "tool domain and name are required".into(),
            ));
        }
        sqlx::query(
            "insert into tools.health (domain, name, version, status, runs, fails, last_ok,
               last_fail, why, pinned)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             on conflict (domain, name) do update set
               version = excluded.version, status = excluded.status, runs = excluded.runs,
               fails = excluded.fails, last_ok = excluded.last_ok,
               last_fail = excluded.last_fail, why = excluded.why, pinned = excluded.pinned",
        )
        .bind(&h.domain)
        .bind(&h.name)
        .bind(h.version)
        .bind(spell(&h.status)?)
        .bind(h.runs)
        .bind(h.fails)
        .bind(h.last_ok)
        .bind(h.last_fail)
        .bind(h.why.as_deref())
        .bind(h.pinned)
        .execute(&self.pool)
        .await
        .map_err(db("tool_health_set"))?;
        Ok(())
    }

    async fn gate_rule(&self, domain: &str, class: &str) -> Result<Option<GateRule>> {
        // Keys are stored canonical (D-110); a lookup that does not canonicalise matches nothing.
        let (Some(domain), Some((_, class))) = (rule_domain(domain), rule_key(class)) else {
            return Ok(None);
        };
        let row =
            sqlx::query("select mode, strict from gate.rules where domain = $1 and class = $2")
                .bind(&domain)
                .bind(&class)
                .fetch_optional(&self.pool)
                .await
                .map_err(db("gate_rule"))?;
        row.map(|r| {
            let mode: String = get(&r, "mode")?;
            Ok(GateRule {
                mode: mode
                    .parse::<Mode>()
                    .map_err(|_| backend("unknown value in column gate.rules.mode"))?,
                strict: get(&r, "strict")?,
            })
        })
        .transpose()
    }

    async fn run_start(&self, run: Run) -> Result<()> {
        sqlx::query(
            "insert into runs (id, started, ended, prompt, status, folder, parent_run, strict)
             values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&run.id.0)
        .bind(run.started)
        .bind(run.ended)
        .bind(&run.prompt)
        .bind(spell(&run.status)?)
        .bind(&run.folder)
        .bind(run.parent_run.as_ref().map(|r| r.0.as_str()))
        .bind(run.strict)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            if sqlstate(&e).as_deref() == Some("23505") {
                MemoryError::Invalid(format!("run {} exists", run.id))
            } else {
                db("run_start")(e)
            }
        })?;
        Ok(())
    }

    async fn run_end(&self, id: &RunId, status: RunStatus, ended: Ts) -> Result<()> {
        let done = sqlx::query("update runs set status = $2, ended = $3 where id = $1")
            .bind(&id.0)
            .bind(spell(&status)?)
            .bind(ended)
            .execute(&self.pool)
            .await
            .map_err(db("run_end"))?;
        if done.rows_affected() == 0 {
            return Err(MemoryError::NotFound(format!("run {id}")));
        }
        Ok(())
    }

    async fn run_get(&self, id: &RunId) -> Result<Option<Run>> {
        let row = sqlx::query("select id, started, ended, prompt, status, folder, parent_run, strict from runs where id = $1")
            .bind(&id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(db("run_get"))?;
        row.as_ref().map(run_row).transpose()
    }

    async fn schedule_add(&self, s: Schedule) -> Result<i32> {
        let id: i32 = sqlx::query_scalar(
            "insert into schedules (cron, prompt, kind, watch_target, enabled, last_run, run_id)
             values ($1, $2, $3, $4, $5, $6, $7) returning id",
        )
        .bind(&s.cron)
        .bind(&s.prompt)
        .bind(spell(&s.kind)?)
        .bind(s.watch_target.as_deref())
        .bind(s.enabled)
        .bind(s.last_run)
        .bind(s.run_id.as_ref().map(|r| r.0.as_str()))
        .fetch_one(&self.pool)
        .await
        .map_err(db("schedule_add"))?;
        Ok(id)
    }

    async fn schedule_list(&self) -> Result<Vec<Schedule>> {
        let rows = sqlx::query("select id, cron, prompt, kind, watch_target, enabled, last_run, run_id from schedules order by id")
        .fetch_all(&self.pool)
        .await
        .map_err(db("schedule_list"))?;
        rows.iter().map(schedule_row).collect()
    }

    async fn schedule_rm(&self, id: i32) -> Result<()> {
        let done = sqlx::query("delete from schedules where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db("schedule_rm"))?;
        if done.rows_affected() == 0 {
            return Err(MemoryError::NotFound(format!("schedule {id}")));
        }
        Ok(())
    }

    async fn schedule_fired(&self, id: i32, at: Ts) -> Result<()> {
        let done = sqlx::query("update schedules set last_run = $2 where id = $1")
            .bind(id)
            .bind(at)
            .execute(&self.pool)
            .await
            .map_err(db("schedule_fired"))?;
        if done.rows_affected() == 0 {
            return Err(MemoryError::NotFound(format!("schedule {id}")));
        }
        Ok(())
    }

    async fn gate_rule_set(
        &self,
        domain: &str,
        class: &str,
        rule: GateRule,
        note: &str,
    ) -> Result<()> {
        let (domain, class) = check_rule(domain, class, rule)?;
        sqlx::query(
            "insert into gate.rules (domain, class, mode, note, strict) values ($1, $2, $3, $4, $5)
             on conflict (domain, class) do update set
               mode = excluded.mode, note = excluded.note, strict = excluded.strict",
        )
        .bind(&domain)
        .bind(&class)
        .bind(rule.mode.as_str())
        .bind(note)
        .bind(rule.strict)
        .execute(&self.pool)
        .await
        .map_err(db("gate_rule_set"))?;
        Ok(())
    }

    async fn review_add(&self, item: &str, answer: &str) -> Result<ReviewId> {
        let id: i64 = sqlx::query_scalar(
            "insert into memory.reviews (item, answer, due) values ($1, $2, $3) returning id",
        )
        .bind(item)
        .bind(answer)
        .bind(ts_now())
        .fetch_one(&self.pool)
        .await
        .map_err(db("review_add"))?;
        Ok(ReviewId(id))
    }

    async fn ledger_events(&self, run: &RunId) -> Result<Vec<(LedgerId, LedgerEvent)>> {
        let rows = sqlx::query("select id, run_id, ts, server, tool, class, mode, args, result_summary, consequences, duration_ms, ok, why from ledger.events where run_id = $1 order by id")
        .bind(&run.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db("ledger_events"))?;
        rows.iter().map(ledger_row).collect()
    }
}

/// Builds the [`Harness`] a tool server's `main` hands to `harness_core::serve`: the store at
/// `DATABASE_URL` (default [`DEFAULT_DATABASE_URL`]), the vault, the asker, the run from the
/// launching session. Which variables and files it reads is T-07's call (never a key, D-1).
pub async fn bootstrap() -> Result<Harness> {
    todo!("T-07: server bootstrap")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sm2_steps() {
        // First pass: 1 day; second: 6; third: interval * ease (ease before the grade).
        let (r, i, e) = sm2(0, 1.0, 2.5, 5);
        assert_eq!((r, i), (1, 1.0));
        assert!((e - 2.6).abs() < 1e-6);
        let (r, i, _) = sm2(1, 1.0, 2.6, 4);
        assert_eq!((r, i), (2, 6.0));
        let (r, i, e) = sm2(2, 6.0, 2.5, 5);
        assert_eq!((r, i), (3, 15.0));
        assert!((e - 2.6).abs() < 1e-6);
        // A fail resets, and the ease floor is 1.3.
        let (r, i, e) = sm2(7, 40.0, 1.35, 0);
        assert_eq!((r, i), (0, 1.0));
        assert!((e - 1.3).abs() < 1e-6);
    }

    #[test]
    fn challenge_rules_must_stop() {
        let allow = GateRule {
            mode: Mode::Allow,
            strict: false,
        };
        assert!(matches!(
            check_rule("*", "Challenge", allow),
            Err(MemoryError::Invalid(_))
        ));
        assert!(matches!(
            check_rule("*", "Challenge:turnstile", allow),
            Err(MemoryError::Invalid(_))
        ));
        assert!(check_rule("*", "Submit:job_application", allow).is_ok());
        assert!(check_rule("*", "Submit:", allow).is_err());
        assert!(check_rule("*", "Teleport", allow).is_err());
        assert!(check_rule(" ", "Pay", allow).is_err());
    }

    #[test]
    fn rule_keys_are_canonical() {
        let ask = GateRule {
            mode: Mode::Ask,
            strict: false,
        };
        let ok = |d: &str, c: &str| check_rule(d, c, ask).unwrap();
        assert_eq!(
            ok("Bank.COM.", "submit"),
            ("bank.com".into(), "Submit".into())
        );
        assert_eq!(
            ok(" * ", "Submit: Wire "),
            ("*".into(), "Submit:wire".into())
        );
        assert_eq!(ok("x.org", " pay "), ("x.org".into(), "Pay".into()));
        assert!(check_rule("bank com", "Pay", ask).is_err());
        assert!(check_rule("bank.com/x", "Pay", ask).is_err());
        assert!(check_rule(".", "Pay", ask).is_err());
        assert!(check_rule("a.b", "Submit:  ", ask).is_err());
    }

    #[test]
    fn spellings_match_serde() {
        assert_eq!(spell(&ToolStatus::NeedsRepair).unwrap(), "needs_repair");
        assert_eq!(spell(&LedgerMode::TimeoutSkip).unwrap(), "timeout_skip");
        assert_eq!(spell(&SourceKind::Email).unwrap(), "email");
        let k: ScheduleKind = parse("kind", "reflect".into()).unwrap();
        assert_eq!(k, ScheduleKind::Reflect);
        assert!(parse::<RunStatus>("status", "bogus".into()).is_err());
    }
}
