# Acceptance T-08b: D-85 SchedError non_exhaustive, T-08 red rows D-124 (1), B-526 (docs/01-HARNESS-CORE.md H-4, H-7; D-85, D-124, D-119)

Code under test: swarm/core-T08b at 9ff6c08 (fixes in da34238 and 4154cc4). I ran it in worktree `F:/sb-swarm/wt/core/wt/T08b` with a private `CARGO_TARGET_DIR=F:/sb-swarm/wt/core/wt/T08b/target`, `-j 1`, and `TMP`/`TEMP` set to a scratch folder. The first `-j 2` attempt ran out of memory inside rustc (`memory allocation of 729104 bytes failed`, STATUS_STACK_BUFFER_OVERRUN) while 25 rustc/cargo/link processes from other sessions were running. That was the environment, not this change, and the `-j 1` run built cleanly. Postgres was the compose instance on 127.0.0.1:5433, with one throwaway `sb_t_<random>` database per test (D-100). No model was called (rule 1).

Canary: `SB_VAULT_CANARY=sbcanary-x-<16 hex>`, fresh for this run, planted by the tests as vault value `test` (an on-disk age vault for accept_t08_r2, `Vault::in_memory` for mcp_t07).

Added test: `crates/core/tests/accept_t08b_d85.rs` (2 tests). Its match outside the crate names all 6 SchedError variants plus a `_` arm, under `#![deny(unreachable_patterns)]`, so it compiles only while the enum is `#[non_exhaustive]`. A second test checks the source attribute.

Commands:
- F = `SB_VAULT_CANARY=$C cargo test -j 1 -p harness-core -p harness --no-fail-fast`. exit 0. Every binary `ok`: harness lib 6, accept_t08 18, accept_t08_r2 12 (0 ignored), accept_t09 1, step1_accept 7, t08 10, t08_vault 6, vault_cli_repair_t05_r4 1, vault_editor_cli_t05c 1. harness_core lib 133, accept_t08b_d85 2, mcp_t07 33 (+1 ignored: the child helper, which the parent runs), mcp_t07_r3 7, mcp_t07_r4 13, the rest of the core suites ok. Doctests 5. The pre-existing `#[ignore]`s (ladder_vault_plant 1, accept_t07_r2 1) are not T-08b items.
- N = `SB_VAULT_CANARY=$C cargo test -j 1 -p harness --test accept_t08_r2 -- --test-threads=1` (included in F): `test result: ok. 12 passed; 0 failed; 0 ignored`.
- RC = the same as N with da34238's cli.rs hunk reverted (`git apply -R`, then `git checkout` to restore). exit 101, `test result: FAILED. 9 passed; 3 failed; 0 ignored`. The failures are `d124_1_gate_set_note_is_scrubbed_before_the_database`, `d124_1_ledger_header_prints_run_ids_as_literals` and `d124_1_tools_commands_echo_the_tool_id_as_literal`.
- B = `TEMP=TMP=<208-char folder> SB_VAULT_CANARY=$C cargo test -j 1 -p harness-core --test mcp_t07 -- file_asker_stderr_has_no_canary --exact`. Fixed: exit 0, `1 passed`. With 4154cc4's ask.rs hunk reverted: exit 101, `panicked at crates\core\tests\mcp_t07.rs:1602:5` (the "fail-closed line is printed" assert).
- RD = remove `#[non_exhaustive]` from sched.rs:47, then `cargo test -j 1 -p harness-core --test accept_t08b_d85`. exit 101, `error: unreachable pattern --> crates\core\tests\accept_t08b_d85.rs:21:9`. sched.rs restored after.

| item | command | result | note |
|---|---|---|---|
| D-85: `harness_core::sched::SchedError` is `#[non_exhaustive]` | F (`accept_t08b_d85`: `d85_sched_error_needs_a_wildcard_arm_outside_the_crate`, `d85_sched_error_source_carries_non_exhaustive`); red proof RD | pass | 2 passed. With the attribute removed, the test crate fails to compile (`unreachable pattern` on the `_` arm), so the test proves the attribute is there. |
| D-124 (1): `harness ledger <run>` header prints the run id and parent_run as `@vault(test)` (T-08 row 38) | N, `d124_1_ledger_header_prints_run_ids_as_literals`; red proof RC | pass | Un-ignored. ok with the fix, FAILED with cli.rs reverted. 0 canary hits in stdout. |
| D-124 (1): `harness tools pin/unpin/retire <domain>/<name>` echo the id as the literal; pin of an unknown id creates no tools.health row holding the value (T-08 row 39) | N, `d124_1_tools_commands_echo_the_tool_id_as_literal`; red proof RC | pass | Un-ignored. pin/unpin of an unknown id is now NotFound (`no tool luma.com/@vault(test)`), and `tools.health` holds no value (the new assert from 4154cc4). FAILED with cli.rs reverted. |
| Rule 2 / D-124 (1): `harness gate set --note` stores no vault value in gate.rules (T-08 row 40) | N, `d124_1_gate_set_note_is_scrubbed_before_the_database`; red proof RC | pass | Un-ignored. gate.rules dump and stdout clean. FAILED with cli.rs reverted. |
| T-08 rows that were already green still pass (regression) | F (accept_t08 18, the other 9 tests in accept_t08_r2, t08, t08_vault, step1_accept, accept_t09) | pass | All ok, 0 failed. |
| B-526: FileAsker's fail-closed stderr line keeps "nobody saw the card" under the line cap with a long TEMP, and holds no canary | B (fixed and reverted) | pass | With the fix: 1 passed at TEMP length 208. With the old ask.rs: fails at mcp_t07.rs:1602 (the fail-closed words were cut). 0 canary hits in both logs. |
| Vault canary never leaves: grep stdout/stderr logs, the temp tree (the tests' run folders, `runs/` and `ledger.jsonl` mirrors), the repo's `runs/`, and the working tree | `grep -a -c "$C"` on full.log, full2.log, build1.log, red-cli.log, b526-fixed.log, b526-red.log; `grep -rl -a "$C" <TMP>`; `grep -rl -a "$C" --exclude-dir=target --exclude-dir=.git .` | pass | 0 hits in every log, 0 files in the temp tree, no repo `runs/` folder, 0 files in the working tree. The tests grep the DB tables themselves (ledger.events, runs, schedules, gate.rules, tools.health) and passed. The lib test `global_scrub_before_and_after_install` prints a panic on purpose (catch_unwind), and its message held no canary. |
| Added test is fmt and clippy clean | `rustfmt --edition 2021 --check crates/core/tests/accept_t08b_d85.rs`; `cargo clippy -j 1 -p harness-core -p harness --all-targets -- -D warnings` | pass | fmt exit 0, clippy exit 0. |

Not covered: a live `claude` session (rule 1).

Verdict: green
