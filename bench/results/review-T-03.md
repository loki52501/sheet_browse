# Review T-03 (round 3): core-pg MemoryStore on Postgres (10cb4b5, a4f3224; at 15e09d3)

Scope: `git diff main...15e09d3 -- crates/core-pg migrations` (round-3 delta 2628379~1..15e09d3). Contract: I-1 (crates/core/src/memory.rs), docs/02 M-2..M-5 and step-1 acceptance, H-3. Decisions checked: D-62, D-100, D-103, D-110, D-112, D-113, D-114, D-115, D-120, B-175.

Runs, in a detached worktree at 15e09d3 with its own target dir, against compose Postgres on 127.0.0.1:5433:
- `cargo clippy -p core-pg --all-targets -- -D warnings`: clean. `cargo fmt -p core-pg --check`: clean.
- `cargo test -p core-pg`: lib 4, accept_t03 8, bootstrap_b175 1, pg 11. All pass (pg.rs about 78 s).

## Round-2 items

- Blocking (private D-110 canonicalisers): resolved. `gate_rule_set` calls `harness_core::gate::rule_key` (lib.rs:908) and `gate_rule` calls `gate::rule_domain`/`rule_class_key`, with `Err` meaning `Ok(None)` (lib.rs:778-781). The unit test pins every rejection case from the round-2 review (lib.rs:1111-1139). It also pins the D-120 A-label (`BÜCHER.de` -> `xn--bcher-kva.de`).
- Should fix 1 (`edges.since` from the process): resolved. lib.rs:439-448 binds `ts_now()` (D-112).
- Should fix 2 (one round trip per recall): resolved. `hnsw.iterative_scan=relaxed_order` is now a startup option on every pooled session (lib.rs:56-59), and recall is one `fetch_all` (lib.rs:536). Relaxed order cannot misrank, because the vector CTE re-sorts with `row_number() over (order by d, id)` (lib.rs:327).
- Should fix 3 (`pool()` public): resolved. It is behind `cfg(any(test, feature = "testing"))` and `#[doc(hidden)]` (lib.rs:77-79), and the self dev-dependency follows the D-111 pattern (Cargo.toml:20-28).
- Should fix 4 (vacuous stop-word assertion): resolved. pg.rs:197 asserts non-empty first.
- Should fix 5 (500-row cap): resolved. `neighbors_caps_at_500_nearest_first` uses a 510-leaf star plus a far node with the lowest id, and asserts exactly the first 500 depth-1 leaves.
- Should fix 6 (rule 2 for backend errors): resolved. `backend_errors_never_carry_bound_values` binds a canary into four failing writes (NUL in text, `\u0000` in jsonb). It asserts the canary is absent from Display, Debug and the full `source()` chain, and that `sqlstate` is present.
- Should fix 7 (FTS-only hit): resolved. `recall_reports_a_words_only_hit_as_fts` asserts `Why::Fts` for the target and exactly 20 vector hits.
- Should fix 8 (Send allow over PgStore): resolved. `a03_touch_contact_makes_send_allow_through_the_gate` (accept_t03.rs:313-363) shows ask/Proceed before `touch_contact` and `Allow` after it. It also shows an IDN stop rule written as `BÜCHER.DE` stopping a call on `xn--bcher-kva.de`.
- Note (edges(dst) index): done. migrations/0002_edges_dst.sql (D-114).
- Note (embed error text): the inference error now has a fixed message (embed.rs:74-75).

## Blocking

- none

## Should fix

- crates/core-pg/src/lib.rs:67-68: no test covers D-114 `ignore_missing`, the reason the change exists (a shared DB that already has a newer migration). Add a pg.rs test: insert a `_sqlx_migrations` row with a version this binary lacks (for example 9999), then assert a second `PgStore::connect` on the same DB succeeds.
- Leftover throwaway databases: after this run, six `sb_t_*` databases remained on the shared server. I can't attribute them, because other areas run tests at the same time. The `Drop` cleanup only runs when the process exits normally. A `sb_t_*` sweep of databases older than an hour (in `harness db` or the test harness setup) would keep B-102 recovery time down.

## Notes

- Hard rules: no keys, `.env` or key-shaped strings. No sleeps. No `unwrap`/`expect` outside `#[cfg(test)]` and tests. Rule 2 at the edges:
  - `describe` (lib.rs:108-128) emits only the operation, SQLSTATE and constraint, plus fixed P0001 text.
  - `migrating: {e}` (lib.rs:71) passes sqlx's MigrateError text. That text comes from our own migration SQL and holds no bound values.
  - The model-load error (embed.rs:68) passes fastembed text. It carries no user input.
- D-115:
  - SEARCH_SQL (lib.rs:366-395) matches the decision: normalised subject equality scores 1.0, a substring of subject/key/value scores 0.9, and the trigram tier is `subject %> $1` (`$1 <% subject`, so the index applies) scored by `word_similarity`. Results are ordered by score, then tier.
  - The per-tier `limit $2` followed by a global order and limit gives the same top k as MemStore's single sort, because tier is constant within a tier.
  - Parity is pinned by `search_follows_d115_like_memstore`, including the 5/7 score.
  - SM-2 `ceil_days` keeps 1e-4 slack against f32 noise and matches MemStore. The unit test covers 16.2 -> 17.
- Exact-tier cost: `regexp_replace` and `strpos` run over every fact on each search. No index serves them. That is fine at personal scale. An expression index on the normalised subject would cover the equality branch later.
- B-175 (lib.rs:987-995, check at :989): `bootstrap` now refuses an SB_RUN_ID that has no runs row. The error is scrubbed and names the id and the fix. `bootstrap_b175` tests both halves. This is T-07 behaviour carried in the T-03 commit, as B-175's default says.
- Feature unification: under `cargo test --workspace`, the self dev-dependency turns on `testing` for core-pg in that build, so `pool()` is visible to other crates' tests. Production builds do not see it. Acceptable, and the same trade-off as D-111.
- Commit ids:
  - 10cb4b5 claims M-2..M-5, H-3, I-1, D-100, D-110, D-112, D-113, D-114, D-115 and D-120. All are implemented. D-114's index landed separately in a4f3224, and `ignore_missing` is in 10cb4b5.
  - B-175 is in the title.
  - Nothing implemented is outside a claimed id.
- I-1: signatures are unchanged, and the compiler enforces the trait.

## Verdict

The round-2 blocking item and all eight should-fix items are resolved, and each has a test that checks the real outcome. Clippy, fmt and every core-pg test are green. What is left is a missing D-114 regression test and test-DB hygiene.

Verdict: approve
