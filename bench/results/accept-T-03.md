# Acceptance: T-03 (core-pg MemoryStore on Postgres), round 3

Spec: docs/02-MEMORY.md Acceptance (step-1 items), docs/01-HARNESS-CORE.md H-3, rule 2, and the round-3 behaviours of 10cb4b5 (D-112, D-113, D-114, D-115, D-120, B-175).
Code under test: swarm/core at 15e09d3, which holds 10cb4b5 (core-pg) and a4f3224 (migrations/0002_edges_dst.sql).
Run in a detached worktree F:/sb-swarm/tmp-core/wt-accept-t0304 at 15e09d3, with `CARGO_TARGET_DIR=F:/sb-swarm/tmp-core/target-accept-t0304`. Every command below runs from that worktree.
`SB_VAULT_CANARY` was a fresh `sbcanary-...` string for every run. The final run used `sbcanary-final0304-<epoch>-r9z`.

Tests:
- Round-1/2 rows: crates/core-pg/tests/accept_t03.rs, unchanged by the tester. 10cb4b5 added `a03_touch_contact_makes_send_allow_through_the_gate` to it.
- Round-3 rows: crates/core-pg/tests/accept_t03_r3.rs and crates/core-pg/tests/accept_t03_r3_bootstrap.rs, both new.
  - The bootstrap test is its own binary because it changes the process environment.
- The coder's crates/core-pg/tests/pg.rs and bootstrap_b175.rs were run as regression only.

Database: every test creates its own throwaway `sb_t_a03_*`, `sb_t_a03r3_*` or `sb_t_a03r3b_*` database on 127.0.0.1:5433 and drops it (D-100). The shared `sb` database was not touched, and no `sb_t_a03*` database was left behind (checked against `pg_database`).

Full run, `cargo test -p core-pg -- --test-threads=4`:
- unit: 4 passed
- accept_t03: 8 passed
- accept_t03_r3: 10 passed, 1 ignored (the red-first row below)
- accept_t03_r3_bootstrap: 1 passed
- bootstrap_b175: 1 passed
- pg: 11 passed

The ignored test, run with `-- --ignored`: `test result: FAILED. 0 passed; 1 failed`.
`cargo clippy -p core-pg --test accept_t03_r3 --test accept_t03_r3_bootstrap -- -D warnings` is clean, and `rustfmt --edition 2021 --check` on both new files is clean.

Vault canary: `grep -c` of the canary in the captured stdout/stderr of every run is 0. `runs/` does not exist in the worktree.
- The rule-2 row puts the canary into five INSERT/UPSERT parameters that fail a check constraint, so no row holding it is ever stored.
- The jsonl mirror is not written by core-pg (T-06/T-09).

Out of scope, not in the table:
- the watch-schedule item of docs/02 (T-16, step 4);
- the end-to-end echo/vault/schedule items of docs/01 (T-09).

| item | command | result | note |
|---|---|---|---|
| remember + recall("Rust CDP crate") returns the chromiumoxide fact with why=both; query lacks "chromiumoxide" (docs/02) | `cargo test -p core-pg --test accept_t03 a03_recall_chromiumoxide_why_both -- --nocapture` | pass | `a03 recall: top="chromiumoxide" why=Both score=0.032786883` |
| 3-hop neighbors returns roles reachable from a person via met_at -> attended -> works_at (docs/02) | `cargo test -p core-pg --test accept_t03 a03_neighbors_three_hops_reach_role -- --nocapture` | pass | depth 3: `[event "Rust Bangalore meetup", person "Ravi", role "Staff Engineer, Acme"]`; 4-hop role excluded |
| touch_contact makes a Send to that person allow on the next gate check, over PgStore (docs/02) | `cargo test -p core-pg --test accept_t03 a03_touch_contact_makes_send_allow_through_the_gate a03_touch_contact_then_contact -- --nocapture` | pass | `a03 gate after touch_contact: Allow` (Ask with on_timeout Proceed before); contact row `count: 1, note: Some("meetup")` |
| grade_review follows SM-2: grade 5 multiplies interval by ease; grade < 3 resets to 1 day (docs/02) | `cargo test -p core-pg --test accept_t03 a03_grade_review_sm2 -- --nocapture` | pass | `a03 sm2: 17 * 2.7999997 = 47.599995, got 48` (rounded up, D-115); grades 0, 1, 2 reset to 1 day, due now+24h |
| UPDATE ledger.events fails; DELETE and TRUNCATE fail (docs/01 H-3) | `cargo test -p core-pg --test accept_t03 a03_ledger_update_delete_truncate_fail -- --nocapture` | pass | update, delete, truncate (6 statements) each -> `ledger is append-only`; row unchanged |
| seeded gate rules present; gate_rule_set rejects a non-stop Challenge rule in any spelling (docs/02 seeds, H-1.2, D-5) | `cargo test -p core-pg --test accept_t03 a03_seeded_rules_and_challenge_allow_rejected -- --nocapture` | pass | 14 spellings of Challenge allow/ask -> `Invalid("a Challenge rule must be stop (D-5)")` |
| gate_rule_set('Bank.COM.','submit',ask) is found by gate_rule('bank.com','Submit') (D-110) | `cargo test -p core-pg --test accept_t03 a03_d110_canonical_keys -- --nocapture` | pass | `gate_rule(bank.com, Submit) -> Some(GateRule { mode: Ask, strict: false })` |
| D-114: migration 0002 adds the memory.edges(dst) index and the planner uses it | `cargo test -p core-pg --test accept_t03_r3 r3_d114_edges_dst_index_from_migration_0002 -- --nocapture` | pass | `CREATE INDEX edges_dst_idx ON memory.edges USING btree (dst)`; `_sqlx_migrations` `[(1, true), (2, true)]`; plan `Bitmap Index Scan on edges_dst_idx` |
| D-114: connect runs the migrator with ignore_missing (a newer migration applied by another worktree) | `cargo test -p core-pg --test accept_t03_r3 r3_d114_connect_ignores_a_migration_only_another_worktree_has` | pass | a `_sqlx_migrations` row 99990101000000 inserted, then `PgStore::connect` is Ok and the store works |
| D-115: subject equality (case and punctuation insensitive) 1.0, substring 0.9, word_similarity tier, ordered by score | `cargo test -p core-pg --test accept_t03_r3 r3_d115_search_equality_substring_and_score_order -- --nocapture` | pass | `foo bar, INC.` -> `("Foo-Bar Inc", 1.0, Exact)`; `Foo-Bar Inc` -> 1.0 beats the newer mention at 0.9; `Vasanh` -> `0.71428573, Trigram`; `Raghavan Vasanth` -> trigram 1.0 ranks above the 0.9 substring. Same fixture and results as MemStore (accept-T-04) |
| D-115: "a typo inside a longer subject ('Vasnth' in 'Vasanth Raghavan') must still hit" | `cargo test -p core-pg --test accept_t03_r3 r3_d115_vasnth_typo_inside_longer_subject_hits -- --ignored --nocapture` | fail | `r3 pg search Vasnth: [] (word_similarity 0.5)`; `left: None right: Some("Vasanth Raghavan")`. The server's word_similarity is 0.5, under the default `pg_trgm.word_similarity_threshold` 0.6, so the `subject %> $1` tier never sees it. Missing: a lower threshold (0.5 as a session option next to hnsw.iterative_scan, in MemStore too), or supreme amends D-115's example. Committed as `#[ignore = "red-first T-03 round 4: ..."]` |
| D-115: SM-2 rounds a fractional interval up; PgStore rows equal `core_pg::sm2` and MemStore | `cargo test -p core-pg --test accept_t03_r3 r3_d115_sm2_rounds_up_like_memstore -- --nocapture` | pass | `r3 pg sm2: [1.0, 6.0, 17.0, 48.0]` (MemStore gives the same); reps/ease equal the pure function; due = grade time + interval within 60 s; an exact 6 * 2.5 stays 15; grade 2 resets to (0, 1.0) |
| D-112: memory.edges.since is bound from the process clock, not the database's now() | `cargo test -p core-pg --test accept_t03_r3 r3_d112_edge_since_comes_from_the_process_clock -- --nocapture` | pass | since inside the host window `[..10.521773, ..10.705802]`. The DB clock skew was only 55 ms this run, so the window alone cannot tell the clocks apart; lib.rs `link` binds `ts_now()` |
| timestamps truncate toward zero from 2000-01-01 like sqlx (parity with MemStore, bae6c45) | `cargo test -p core-pg --test accept_t03_r3 r3_timestamps_truncate_toward_zero_from_2000_like_memstore -- --nocapture` | pass | `1999-12-31T23:59:59.999999500Z -> 2000-01-01T00:00:00Z`, `...12:00:00.000001900Z -> ...12:00:00.000001Z`; MemStore gives the same values |
| hnsw.iterative_scan is a startup option on every pooled session; a filtered recall finds a row outside the nearest neighbours | `cargo test -p core-pg --test accept_t03_r3 r3_iterative_scan_is_on_every_session_and_filtered_recall_finds_far_rows -- --nocapture` | pass | 3 concurrent pooled connections report `relaxed_order`; with 45 near decoys, `recall("Rust CDP crate", subject_prefix "zz-")` -> `[("zz-target", 0.016393442, Vector)]` |
| rule 2: a backend error carries no bound value (SB_VAULT_CANARY in the failing parameter) | `SB_VAULT_CANARY=sbcanary-... cargo test -p core-pg --test accept_t03_r3 r3_rule2_constraint_errors_carry_no_bound_canary -- --nocapture` | pass | check-constraint violations on remember, touch_contact, gate_rule_set, append and baseline_set each give e.g. `memory backend: remember: sqlstate 23514 on no_canary_f`; no canary and no `Failing row` in Display, Debug or the source chain; canary count in stdout 0 |
| D-113/D-120: PgStore gate_rule_set stores rule_key's output (A-label, full-width, ideographic stop) and refuses what it cannot canonicalise | `cargo test -p core-pg --test accept_t03_r3 r3_d113_d120_pg_gate_rule_set_stores_rule_key -- --nocapture` | pass | stored `[("bank.com","Submit:wire"), ("xn--bcher-kva.de","Send"), ("xn--r8jz45g.xn--zckzah","Delete")]`; non-canonical lookups hit. Combining mark, ZWSP, ligature, math bold, host:port, KELVIN SIGN and full-width hints, a space in a hint, and a Challenge:x ask are all refused with Invalid; no extra row is written |
| D-120 over PgStore: Send row, blank and duplicate recipients, a losing lower-class hint keeps the class timeout | `cargo test -p core-pg --test accept_t03_r3 r3_d120_gate_over_pgstore_row_dedupe_and_hint_timeout -- --nocapture` | pass | `[("new recipients (1)", "New@X.com"), ("body", "hi")]`, on_timeout Proceed; Allow after touch_contact; Account hinted newsletter under ('*','Read:newsletter',allow) -> Proceed; Pay -> Skip |
| B-175: bootstrap refuses an SB_RUN_ID with no runs row; accepts it with the row; without SB_RUN_ID it starts its own run | `cargo test -p core-pg --test accept_t03_r3_bootstrap -- --nocapture` | pass | no SB_RUN_ID -> own folder + `running` row; missing folder -> `Invalid(... does not exist)`; folder without row -> `Invalid("SB_RUN_ID <id>: the run folder exists but there is no runs row for it ...")` and no row written; with the row -> `harness.run == id`, no new run created. The coder's bootstrap_b175 also passes |
| regression: the whole core-pg suite | `cargo test -p core-pg -- --test-threads=4` | pass | 4 + 8 + 10 (1 ignored, the row above) + 1 + 1 + 11 passed, 0 failed |

Verdict: red
