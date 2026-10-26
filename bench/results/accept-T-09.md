# Acceptance: T-09 (echo server + step-1 acceptance)

Spec: docs/01-HARNESS-CORE.md "Acceptance for this doc (step 1)" (all four bullets) and the step-1 items of
docs/02-MEMORY.md "Acceptance (step 1 and step 4)". Decision D-122, handoff agent-memory/handoffs/T-09-rust-coder-1.yaml.
Code under test: commit 9fd42d7 on swarm/core, run in a detached worktree (F:/sb-swarm/tmp-core/wt-accept-t09,
`CARGO_TARGET_DIR=F:/sb-swarm/tmp-core/target-accept-t09`), never in the shared checkout.
Coder's tests: crates/harness/tests/step1_accept.rs with crates/harness/examples/echo.rs (spawned over stdio).
Tester's test (added): crates/harness/tests/accept_t09.rs, an independent vault-leak test (9 adversarial calls,
8 encoded forms, plus `harness ledger <run>` stdout) that can keep its run folder and database (`SB_T09_KEEP_DIR`)
for an external grep.
Database: compose Postgres on 127.0.0.1:5433, one throwaway `sb_t_*` / `sb_t_a09_*` database per test (D-100);
the kept ones were grepped and then dropped; no `sb_t_a09_*` left. Rule 1: no model is called (scheduler items use a
fake Launcher in process, or bench/fixtures/fake_claude.rs for acc4).
Every run used a fresh SB_VAULT_CANARY (44 to 46 chars; the vault-grep runs used one containing a backslash, a double
quote, a space, `+`, `/`, `&` and `%`, so the JSON-escaped and URL-encoded spellings differ from the raw one).

Final evidence run (clean tree, after all mutations reverted):
`SB_VAULT_CANARY=<fresh> SB_T09_KEEP_DIR=<dir> cargo test --no-fail-fast -p harness --test step1_accept --test accept_t09 -- --nocapture --test-threads=3`
-> `step1_accept: test result: ok. 7 passed; 0 failed; 0 ignored` and `accept_t09: test result: ok. 1 passed; 0 failed; 0 ignored`.

External grep (independent of both tests): `python -I grep_canary.py <keep_dir> <test log>` (script kept at
F:/sb-swarm/tmp-core/accept-t09-tools/grep_canary.py, never prints the value). It searches case-insensitively for the
raw, upper, lower, JSON-escaped (serde style and ASCII), JSON-escaped twice and URL-encoded (`quote` safe none,
`quote` default, `quote_plus`, lower-hex) spellings in: the whole cargo test stdout+stderr log, echo's stdout and stderr
(saved by accept_t09), the stdout and stderr of `harness ledger <run>`, the sink file, every file and every file or
folder name under runs/ (ledger.jsonl, report.md, prompt.md, asks/1..3.json and .answer, files/, frames/, handoff/),
and `pg_dump --data-only` plus full `pg_dump` of the kept database (12 COPY blocks, every table of every schema,
11 ledger rows). Result on two separate canaries (run 2 and the final run): `items_scanned=40 ... HITS=0`.
Positive control: the same script on files holding the JSON-escaped, upper-case `quote_plus` and swapped-case canary
-> `HITS=3`. The test logs of every other run in this report (core suites, acc4, mutation runs) also grep `HITS=0`.

Also run (step-1 "done when"): `cargo test --no-fail-fast -p harness-core -p core-pg -- --test-threads=2` -> every
binary `ok`, 0 failed (core-pg: 4 + 8 + 1 + 11; harness-core: 124 unit + 15 integration binaries). Ignored there:
2 `child` helpers and 2 `red-first T-07 round 3` tests in accept_t07_r2.rs (T-07's, fixed after this commit by 2330457;
not step-1 acceptance bullets). First attempts of that run hit B-102 (Postgres backend `exited with exit code 2`
at 01:04:55 UTC, recovery mode, then a pool timeout under load); re-run green.
Out of scope: docs/02 "watch schedule" (step 4, T-16). The docs/02 store items are evidenced by
crates/core-pg/tests/accept_t03.rs (tester, T-03) at this commit.

Mutation checks (each applied to the detached worktree only, rebuilt, run, then reverted with `git checkout`):
M1 echo `eprintln!` of the resolved value; M2 `amount` no longer raises Send to Pay; M3 `recipients` empty;
M4 trigger reduced to `before truncate`; M5 ledger.jsonl append removed; M6 `parent_run: None` in sched.rs;
M8 scrubber returns its input unchanged (env switch). Probe M7 (not a break): echo returns the resolved value in its
result text and consequences; the H-6 wrapper's scrub removed it and both vault tests stayed green, external grep 0.

| item | command | result | note |
|---|---|---|---|
| docs/01 b1: echo.send with `to` in contacts runs, ledger class=Send mode=allow | `cargo test -p harness --test step1_accept b1_send_to_contact_runs_with_mode_allow` | pass | one sink line, no asks/ dir, 1 ledger row server=echo class=Send mode=allow ok, args.to = ann@example.com (D-121), mirror equal; M3 makes it fail (`skipped: no answer before the ask timed out`) |
| docs/01 b1: unknown `to` asks, timeout proceeds, ledger shows timeout_proceed | `cargo test -p harness --test step1_accept b1_unknown_recipient_asks_and_timeout_proceeds` | pass | asks/1.json class Send on_timeout proceed names new@example.org, 1.answer = timeout, stderr notice `ask 1 (Send)`, core event + echo event both timeout_proceed, report `Asked: 1 (0 answered / 1 timed out)`; M3 makes it fail |
| docs/01 b1: class forced to Pay, timeout skips | `cargo test -p harness --test step1_accept b1_forced_pay_times_out_to_skip` | pass | `amount` raises Send to Pay even for a contact: card class Pay on_timeout skip, sink untouched, single core event timeout_skip, report `Did: 0 actions`; M2 makes it fail (card class `Send`) |
| docs/01 b2: @vault(test) resolves in the sink, nowhere else (coder's grep) | `SB_VAULT_CANARY=<fresh> cargo test -p harness --test step1_accept b2_vault_value_resolves_in_the_sink_and_nowhere_else` | pass | sink bytes + FNV-1a match the resolved body; raw and JSON forms absent from echo stdout/stderr, tool results, every DB row, runs/ files and names, sink; M1 fails it (`echo stderr`), M8 fails it (`ledger.events`, `ledger.jsonl`) |
| docs/01 b2: vault canary, tester's adversarial test (literal, raw flipped case, raw as recipient, pct and JSON pasted, bad-argument and tool-error paths) | `SB_VAULT_CANARY=<fresh> cargo test -p harness --test accept_t09` | pass | `scanned 7 forms, 21 db rows, 14 paths under runs/, 9 replies`; recipient canary shown as @vault(test) in card, why, ledger args, result, sink; M1 fails it (`echo stderr [raw]`), M8 fails it (echo stdout/stderr, reply, harness ledger stdout, sink, ledger.events rows...) |
| docs/01 b2: independent external grep of test stdout/stderr, echo stdout/stderr, pg_dump of the kept DB, ledger.jsonl, runs/ files and names (raw, JSON-escaped, URL-encoded, upper/lower) | `SB_VAULT_CANARY=<fresh> SB_T09_KEEP_DIR=<dir> cargo test -p harness --test accept_t09` then `python -I grep_canary.py <dir> <log>` | pass | `forms=6 items_scanned=40 copy_blocks=12 dump_bytes=18989 ledger_rows=11` and `HITS=0`, on two canaries; positive control `HITS=3`; coder run log also `HITS=0` |
| docs/01 b3: UPDATE ledger.events fails | `cargo test -p harness --test step1_accept b3_ledger_is_append_only_and_mirrored` | pass | after 4 echo events: `update ... set why`, `update ... where id = min(id)`, `delete from ledger.events` all error at the DB, count unchanged; corroborated by `cargo test -p core-pg --test accept_t03 a03_ledger_update_delete_truncate_fail` (`ledger is append-only` for update, delete, truncate); M4 fails it (`update ledger.events set why = 'rewritten' must fail at the database`) |
| docs/01 b3: runs/<id>/ledger.jsonl matches the table row-for-row | `cargo test -p harness --test step1_accept b3_ledger_is_append_only_and_mirrored` | pass | read_jsonl == ledger_events (all fields, keyed by id), one line per row, before and after the refused writes; same check in every b1/b2 test and in accept_t09 (11 rows); M5 fails b1 and b3 |
| docs/01 b4: `harness schedule add "*/1 * * * *" "say hi"` fires, run folder with report.md, runs row with parent_run set | `cargo test -p harness --test step1_accept b4_schedule_add_through_the_cli_fires_a_child_run` | pass | real `harness` binary adds it, in-process harnessd (ManualClock, fake Launcher) fires once at the next minute, report.md starts `# say hi` / `Result: done`, row parent_run = the CLI's schedule-add run, status Done; corroborated by `cargo test -p harness --test accept_t08 acc4` (real harness + harnessd binaries, fake claude, ok); M6 fails it (`left: None`) |
| docs/02: remember + recall("Rust CDP crate") returns the chromiumoxide fact with why=both | `cargo test -p core-pg --test accept_t03 a03_recall_chromiumoxide_why_both` | pass | `a03 recall: top="chromiumoxide" why=Both score=0.032786883` |
| docs/02: 3-hop neighbors reaches roles via met_at, attended, works_at | `cargo test -p core-pg --test accept_t03 a03_neighbors_three_hops_reach_role` | pass | depth 3: event "Rust Bangalore meetup", person "Ravi", role "Staff Engineer, Acme" |
| docs/02: touch_contact makes a Send to that person allow on the next gate check | `cargo test -p harness --test step1_accept docs02_touch_contact_makes_the_next_send_allow` | pass | through echo: first send timeout_proceed with asks/1.json, after touch_contact the second send mode=allow and no asks/2.json; also `cargo test -p core-pg --test accept_t03 a03_touch_contact_makes_send_allow_through_the_gate` -> `a03 gate after touch_contact: Allow`; M3 fails it |
| docs/02: grade_review follows SM-2 (grade 5 multiplies by ease, grade < 3 resets to 1 day) | `cargo test -p core-pg --test accept_t03 a03_grade_review_sm2` | pass | `a03 sm2: 17 * 2.7999997 = 47.599995, got 48`; grades 0, 1, 2 reset to 1 day |

Verdict: green
