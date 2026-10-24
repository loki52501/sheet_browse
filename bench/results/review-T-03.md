# Review T-03: core-pg MemoryStore on Postgres (commit ea1befb)

Scope: crates/core-pg/src/lib.rs, crates/core-pg/src/embed.rs, crates/core-pg/Cargo.toml,
crates/core-pg/tests/pg.rs. Contract: crates/core/src/memory.rs trait doc comments (I-1),
docs/02 M-2..M-5 + step-1 acceptance, H-3, D-32/39/45/48/100/103/110/112.

Runs (this review):
- `cargo clippy -p core-pg --all-targets -- -D warnings`: clean.
- `cargo fmt -p core-pg --check`: clean.
- `cargo test -p core-pg`: 4 unit + 7 pg.rs + 7 accept_t03.rs passed (about 12 to 15 s each binary).
  Postgres on 127.0.0.1:5433 was healthy. No B-102 recovery-mode event during the run.
- After the run, 1 `sb_t_*` database is left on the server. I can't tell whose it is because other areas share the server. The `sb` database was not touched.

## Blocking

- crates/core-pg/src/lib.rs:275-319 (`rule_domain`, `rule_key`, `check_rule`), used at :803 and :931. These private copies accept keys that D-110 (1) says must be rejected. The gate's canonicalisers are in the working tree (crates/core/src/gate.rs:131-207, `rule_domain`/`rule_hint`/`rule_class_key`/`rule_key`), and they reject the same keys. So `gate_rule_set` stores rules that `Gate::lookup` can never match. That gives a silently dead rule, which is the bypass D-110 was written to close. Concrete cases the core-pg copy accepts and the gate rejects:
  - `*.bank.com`: embedded `*`. This is a plausible user input for "all subdomains".
  - `bank.com:443`, `a..b`, `.bank.com`, and domains with `@ ? # \` or control characters.
  - Hints with whitespace, `:` or control characters, such as `Submit:job application` or `Submit:a:b`.

  Fix: delete the copies and call `harness_core::gate::rule_key(domain, class, &rule)` in `gate_rule_set`. In `gate_rule`, call `rule_domain`/`rule_class_key`, with `Err` meaning `Ok(None)`. Keep the unit test `rule_keys_are_canonical` and add the rejection cases above. This must land in the same core PR as the T-04 gate.rs change.

## Should fix

- crates/core-pg/src/lib.rs:455-458 `link` does not bind `memory.edges.since`, so the column takes the server's `now()`. D-112 says core-pg writes every timestamp from the process (`ts_now()`). Bind it.
- crates/core-pg/src/lib.rs:548-562: a filtered `recall` costs 4 round trips (BEGIN, SET LOCAL, query, COMMIT) just to set `hnsw.iterative_scan`. Latency is ranked above token cost. Set it once per connection instead, with `PgPoolOptions::after_connect` or the URL `options=-c hnsw.iterative_scan=relaxed_order`. Then every recall is one round trip, and the transaction branch goes away.
- crates/core-pg/src/lib.rs:64: `pub fn pool()` exposes the raw pool to every consumer of core-pg. That conflicts with M-2's "Nothing else talks to the DB", and it lets anything bypass the store. Gate it behind a `testing` feature (as D-111 does for core) or `#[doc(hidden)]`, and have pg.rs use the feature.
- crates/core-pg/tests/pg.rs:196-197: the stop-word recall assertion (`all(why == Vector)`) is vacuous when `hits` is empty. Assert `!hits.is_empty()` first.
- crates/core-pg/tests/pg.rs:254-331: the 500-row cap on `neighbors` (M-5, trait doc) is never exercised. Add a star of more than 500 nodes and assert the length is 500, nearest first.
- crates/core-pg/tests/pg.rs: no test pins rule 2 for backend errors. Add one that triggers a database error with a canary string bound in (for example a NUL byte in `remember`, or a type error), then assert the canary is not in `err.to_string()` or in the `source()` chain.
- crates/core-pg/tests/pg.rs: no test asserts a `Why::Fts`-only recall hit, meaning a fact matched by words but outside the vector top 20. As it stands, `why` for FTS is only checked through `Both`.
- The docs/02 acceptance "touch_contact makes a Send to that person allow on the next gate check" is covered only against MemStore (crates/core/tests/gate_accept_t04.rs:256). One `Gate::check` over `PgStore` after `touch_contact` would show the real backend's `contact_key` path agrees with the gate.

## Notes

- I-1 fidelity is good on every trait doc comment:
  - `neighbors` walks both directions (:333-335), applies `rel` on every hop (:336), caps depth at `min(depth, 4)` (:506), limits to 500 rows (:514), excludes `n` (:340), and returns nearest first.
  - `recall` uses cosine top 20 plus OR-of-lexemes `ts_rank` top 20, fused with RRF k=60 (D-103). The three filters apply on both sides, and `why` comes from path membership.
  - `search` returns exact matches (equality 1.0, substring 0.9) and then trigram matches on subject that exact missed.
  - `gate_rule_set` rejects any non-stop Challenge rule, hinted or not (D-5).
  - `append` returns `LedgerId`. `ledger_events` is ordered by id. `review_add` is due at `ts_now()`. `schedule_add` ignores `s.id`.
  - All signatures match crates/core/src/memory.rs (the compiler enforces this).
- Rule 2: `describe` (:93-113) emits only the operation, SQLSTATE and constraint name. It drops the server message and detail, except P0001 from our own `RAISE` (fixed text). `Configuration` errors hide the URL. Two places could carry text:
  - `other.to_string()` (:111) is reachable only by sqlx-generated variants (Io, Protocol, PoolTimedOut...), which carry no bound values.
  - embed.rs:68,74 pass fastembed/tokenizer error text through. That is low risk, but it is the one path where library text about the embedded input could appear. Consider a fixed message.
- Rule 2 / vault: the ledger fixture uses only `@vault(site_pw)` references, never a value. I found no key-shaped strings or `.env` files. No `unwrap` on external input outside tests. No sleeps: `node()` retries 3 times without a sleep, and the pool's acquire timeout is a cap.
- D-100: each test creates `sb_t_<hash>` on SB_TEST_DATABASE_URL (default: the `postgres` maintenance database), migrates it through `PgStore::connect`, and drops it `with (force)` in `Drop` on a separate runtime. An unreachable server panics with the compose hint, with no silent skip. Databases leak only if the process is killed. The handoff ties leftover `sb_t_*` fsyncs to B-102 recovery time, so a sweep of `sb_t_*` older than an hour would help (for example in `harness db`).
- NEIGHBORS_SQL joins on `e.src = w.id or e.dst = w.id`. `memory.edges` has no index on `dst` (the primary key leads with `src`), so the reverse direction scans. That's fine at personal scale. A migration with `create index on memory.edges (dst)` would cover it later.
- embed.rs keeps one process-wide mutex across model load and every inference, which serialises all embedding. That's acceptable for Phase 1.
- `bootstrap()` (:974-976) is still `todo!()`. It is T-07's stub and not in T-03's scope.
- Commit ids: M-2, M-3, M-4, M-5, I-1, D-100 and D-103 are all implemented. H-3 append-only is covered by the migration trigger, and the test asserts it (UPDATE, UPDATE matching no rows, DELETE and TRUNCATE all fail with "append-only"). Implemented but not claimed: SM-2 (named in the title), D-110 (in the body), and client-side timestamps (later recorded as D-112). Extra public surface not in I-1: `PgStore::embed`, `model_dir`, `sm2`, `pool`.
- The SM-2 arithmetic matches `harness_core::testing::MemStore` (HEAD testing.rs:404-429). The only difference is a `.max(1.0)` floor on the multiplied interval.

## Verdict

One blocking item. The private D-110 canonicalisers diverge from `harness_core::gate`'s, so core-pg stores rules the gate can never match. Everything else meets I-1 and the step-1 acceptance, and clippy, fmt and the tests are green.

Verdict: request changes
