-- 0001 init: docs/02 M-3, with the wave-0 deviations recorded in D-45 and D-48:
--   * extensions are created here (M-3 lists them; the image only ships them)
--   * memory.facts.embedding is vector(384): fastembed bge-small-en-v1.5 (D-32)
--   * the append-only guard on ledger.events is statement-level, so UPDATE/DELETE fail even
--     when they match no row, and TRUNCATE is blocked too (H-3)
--   * NOT NULL wherever the Rust contract field is not an Option (harness_core::memory)
--   * gate.rules.strict and runs.strict: H-1.3 "strict on a run or a rule"
--   * schedules.run_id: the run that created the schedule, so a firing can set parent_run (H-4)
--   * schedules.watch_target (keyed to memory.baselines.watch) replaces H-5's baseline_id
--   * seeded user rules: ('*','Submit:job_application','ask') and ('*','Submit:signature','ask') (H-1.2, D-39)
-- ledger.events.args holds scrubbed arguments (H-3 calls it args_scrubbed).

create extension if not exists vector;
create extension if not exists pg_trgm;

create schema memory; create schema ledger; create schema tools; create schema gate;

create table memory.facts (
  id bigserial primary key, subject text not null, key text not null, value text not null,
  source jsonb not null,           -- {kind: email|page|user|tool, ref, run_id}
  seen_at timestamptz not null default now(), confidence real not null default 1.0,
  embedding vector(384), tsv tsvector generated always as (to_tsvector('english', subject||' '||key||' '||value)) stored
);
create index on memory.facts using hnsw (embedding vector_cosine_ops);
create index on memory.facts using gin (tsv);
create index on memory.facts using gin (subject gin_trgm_ops);

create table memory.nodes (id bigserial primary key, kind text not null, name text not null, unique(kind, name));
create table memory.edges (src bigint references memory.nodes, rel text not null, dst bigint references memory.nodes,
  since timestamptz default now(), source jsonb, primary key (src, rel, dst));

create table memory.contacts (person text primary key, channel text, last timestamptz, count int not null default 0, note text);
create table memory.reviews (id bigserial primary key, item text not null, answer text not null, due timestamptz not null,
  interval_days real not null default 1, ease real not null default 2.5, reps int not null default 0);
create table memory.baselines (watch text primary key, hash text not null, snapshot jsonb not null, updated timestamptz not null);

create table ledger.events (id bigserial primary key, run_id text, ts timestamptz not null default now(),
  server text not null, tool text not null, class text, mode text, args jsonb not null default '{}',
  result_summary text not null default '', consequences jsonb not null default '[]', duration_ms int not null default 0,
  ok bool not null, why text);
comment on column ledger.events.args is 'scrubbed: vault values only as @vault(name) (H-2, H-3)';
create or replace function ledger.no_change() returns trigger language plpgsql as $$ begin raise exception 'ledger is append-only'; end $$;
create trigger ledger_ro before update or delete or truncate on ledger.events for each statement execute function ledger.no_change();

create table runs (id text primary key, started timestamptz not null, ended timestamptz, prompt text not null,
  status text not null, folder text not null, parent_run text, strict bool not null default false);
create table schedules (id serial primary key, cron text not null, prompt text not null, kind text not null,
  watch_target text, enabled bool not null default true, last_run timestamptz, run_id text);

create table tools.health (domain text, name text, version int not null default 1, status text not null,
  runs int not null default 0, fails int not null default 0, last_ok timestamptz, last_fail timestamptz, why text,
  pinned bool not null default false, primary key (domain, name));
create table gate.rules (domain text, class text, mode text not null, note text not null default '',
  strict bool not null default false, primary key (domain, class));
insert into gate.rules (domain, class, mode, note) values ('*','Pay','ask',''),('*','Account','ask',''),('*','Delete','ask',''),('*','Challenge','stop','');
insert into gate.rules (domain, class, mode, note) values
  ('*','Submit:job_application','ask','job submissions are mine (H-1.2)'),
  ('*','Submit:signature','ask','signatures are mine (H-1.2)');
