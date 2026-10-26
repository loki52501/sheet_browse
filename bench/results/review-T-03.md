# Review T-03 (round 4): core-pg MemoryStore on Postgres (3b66c28; at 4201aae)

Scope: `git show 3b66c28` (crates/core-pg/src/lib.rs, tests/pg.rs, tests/accept_t03_r3.rs), plus the core-pg lines of b829add (lib.rs check_rule test, accept_t03_r3 refused list). Decisions checked: D-114, D-115, D-125 (binding), D-126, B-175.

Runs, in a detached worktree at 4201aae with its own target dir, compose Postgres on 127.0.0.1:5433, `SB_VAULT_CANARY=sbcanary-<hex>-x`:
- `cargo test -p harness-core -p core-pg --no-fail-fast`: exit 0. core-pg: lib 4, accept_t03 8, accept_t03_r3 11 (0 ignored), accept_t03_r3_bootstrap 1, bootstrap_b175 1, pg 14. Canary: 0 hits in the output.
- `cargo clippy -p harness-core -p core-pg --all-targets -- -D warnings`: clean. `cargo fmt -p harness-core -p core-pg --check`: clean.
- Cargo.lock: 3b66c28 does not touch it.

## Round-3 items

- Should fix 1 (no test for D-114 `ignore_missing`): resolved. `connect_ignores_a_migration_this_binary_lacks` (pg.rs:986) inserts a `_sqlx_migrations` row with version 9999, runs a second `PgStore::connect`, and asserts the row is kept and the store works. If `set_ignore_missing(true)` (lib.rs:77) is removed, sqlx returns `VersionMissing(9999)` and the test fails.
- Should fix 2 (leftover `sb_t_*` databases): resolved.
  - `sweep_stale` (pg.rs:89) runs once per test process. It drops only names that pass `is_throwaway` (pg.rs:64, `sb_t_` then `[a-z0-9_]+`), and it applies that filter on both the `pg_stat_file` path and the name-timestamp fallback.
  - The drop is `drop database if exists "<name>"` with no `force` (pg.rs:120), so the server refuses a database that has a session, and the error is only printed. The `with (force)` at pg.rs:204 is the `TestDb` drop of its own database, which is the pre-existing behaviour.
  - Age is the server's own clock (PG_VERSION mtime against `now()`), with a one-hour cut-off.
  - `sweep_touches_only_throwaway_names` (pg.rs:131) pins the name filter: `sb`, `postgres`, `sb_test`, uppercase, a quote-injection name and `xsb_t_1` are refused.
- D-125 trigram cut-off: done.
  - `after_connect` runs `set pg_trgm.word_similarity_threshold = 0.5` on every new pooled session (lib.rs:67). `SEARCH_SQL` keeps `subject %> $1` (lib.rs:403), so the gin index on `subject gin_trgm_ops` (migrations/0001_init.sql:26) still applies.
  - `every_pooled_session_uses_the_d125_trigram_cutoff` (pg.rs:1013) holds all 8 connections and asserts `show` gives 0.5 on each, so it fails without the hook. It also asserts 'Vasnth' hits 'Vasanth Raghavan' as Trigram at 0.5, and that a `%>` plan uses an index scan.
  - The red-first ignore on `r3_d115_vasnth_typo_inside_longer_subject_hits` is removed, and the test passes.

## Blocking

- none

## Should fix

- pg.rs:1052: the index check explains a hand-written `subject %> 'Vasnth'` query, not `SEARCH_SQL`. A later edit of `SEARCH_SQL` to `word_similarity(...) >= 0.5` would keep every test green but lose the index, which is the reason D-125 uses `after_connect`. Add a lib.rs unit test that asserts `SEARCH_SQL` contains `subject %> $1`, or explain the real statement.
- pg.rs:89-126: two sweep guarantees have no test: the no-`force` drop and the age filter. The name filter does. A regression to `with (force)`, or a dropped age clause, would pass. These are hard to test safely on a shared server, so this is not blocking. At least add a comment-level invariant test: assert the formatted drop SQL has no `force`, for example by moving it into a small `fn drop_sql(name)`.

## Notes

- accept_t03_r3.rs:589 (tester file, edited by the coder in b829add): the decomposed, U+200B, fi-ligature and math-bold entries became U+202E and U+200D. Each removal is correct under D-126 and UTS-46, since a browser maps each of those to the host the stored key now carries (bücher, bank, file, bank). U+202E (disallowed) and U+200D (CheckJoiners) are real UTS-46 refusals, and the list still holds the port, Kelvin-hint and full-width-hint refusals. The tester should confirm the edit, as the coder's handoff asks. The `check_rule` unit test (lib.rs, U+200B to U+202E) is the same change.
- Sweep edge: a test process older than one hour whose pool has closed every idle connection (sqlx idle timeout, `min_connections` 0) has no session on its database, so a sweep from another process could drop it. Test processes run for minutes (pg.rs took about 118 s), so this is theoretical.
- The `like 'sb\_t\_%'` filter assumes `standard_conforming_strings = on`, which is the server default. `is_throwaway` is the real guard either way.
- B-175: only the doc changed (lib.rs bootstrap comment). Refusal is still covered by bootstrap_b175.rs and accept_t03_r3_bootstrap.rs, both green.
- Hard rules: no keys, `.env` or sleeps. No vault path changed. The non-test `unwrap`s are all in tests. `unix_now` uses `unwrap_or_default`.
- Commit ids: M-3, D-114, D-125 and B-175 are claimed and implemented. There is no unclaimed behaviour. I-1 signatures are unchanged.

## Verdict

Both round-3 should-fix items and D-125 are resolved, and each has a test that fails when the fix is removed. The remaining gaps are test-only: the index check does not exercise the real search statement, and the sweep's no-force and age rules are untested.

Verdict: approve
