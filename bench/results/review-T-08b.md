# Review T-08b (swarm/core-T08b)

Round 2 (D-89, final) at 5e98d49. Scope: `git diff 0bb4698..HEAD` (crates/harness/src/cli.rs, crates/harness/tests/accept_t08_r2.rs, crates/core/tests/mcp_t07.rs, crates/core/tests/accept_t08b_d85.rs, blockers.yaml B-186, tasks.yaml, handoffs, bench/results/accept-T-08b.md).

Runs here (private target dir, -j 1, fresh `SB_VAULT_CANARY=sbcanary-x-<16 hex>` per run, 0 canary hits in every log):
- `cargo test -p harness --test accept_t08_r2 -- --test-threads=1`: 15 passed, 0 failed, 0 ignored.
- `cargo test -p harness-core --test mcp_t07 --test accept_t08b_d85 -- file_asker_stderr d85`: 3 passed (1 ignored child).
- `cargo clippy -p harness -p harness-core --all-targets -- -D warnings`: clean. `cargo fmt --all -- --check`: clean.
- Red proof: with cli.rs:908 reverted to `format!("{d}/{n}")` and cli.rs:517 reverted to the raw `watch_target`, both tests failed on their own assertion. `d124_1_tools_commands_echo_the_tool_id_as_literal` failed at accept_t08_r2.rs:683 (`did not echo luma.com/@vault(test): pinned: luma.com/<CANARY>: pinned`). `d124_1_writing_commands_store_and_print_no_value` failed at :910 (`schedules.watch_target is stored scrubbed`). Restored with `git checkout -- crates/harness/src/cli.rs` and `touch`. The tree is clean.

## Blocking

- none

Round 1 Blocking items, both closed:
1. `--watch-target` stored raw (rule 2). It is now scrubbed before the Schedule row and the ledger args are built (cli.rs:517). The writing test adds a `--kind watch --watch-target https://example.com/<canary>/inbox` case and asserts that the stored `watch_target` is the literal (accept_t08_r2.rs:908-913). The `db.dump()` grep also covers it. Proven red above.
2. The tools echo test never reached the echo. It now seeds a `tools.health` row and `luma.com/<canary>.js` (accept_t08_r2.rs:658-664), requires Ok, and asserts `luma.com/@vault(test): pinned|unpinned|retired` for all three commands, then checks the final row (Retired, not pinned). Proven red above.

No new Blocking defect found in round 2:
- No rule 2 leak. `refuse_vault_value` puts only `clean(s)` and `@vault(name)` into its message (cli.rs:356-385). The test checks the unscrubbed `{err:#}` (accept_t08_r2.rs:770-771).
- Nothing fails open. The refusal runs before `gate_rule_set`, `known_tool` still propagates errors, and `install_vault` ordering is unchanged.
- No test that cannot fail. The B-186 test needs `expect_err` plus an empty `gate.rules`. The unknown-id test needs `CliError::NotFound` and an empty `tools.health` dump. The parent-run test needs the literal in stdout. The mcp_t07 child asserts that its temp path is over 200 chars (mcp_t07.rs:1563-1566), so an environment that drops the padded TEMP goes red instead of passing vacuously.

## Should-fix

- B-186 refusal checks the typed text, not the stored key. `refuse_vault_value` compares `scrub(s)` with `s` (cli.rs:356-359). `gate_rule_set` then canonicalises through `gate::rule_domain` (UTS-46 `to_ascii`, crates/core/src/gate.rs:146-176). That maps full-width forms to ASCII and deletes soft hyphens (U+00AD). The scrub spellings (crates/core/src/vault.rs:1108) cover ASCII case but not UTS-46 mapping. So a domain like `sbcanary\u{AD}-x-....com` or a full-width spelling passes the check, and the ASCII value lands in `gate.rules`. Fix: call `harness_core::gate::rule_key(&domain, &class, &rule)` first and run `refuse_vault_value` on the canonical domain and class too. Add one full-width or soft-hyphen case to `b186_gate_set_refuses_a_vault_value_in_the_domain_or_class`. Not Blocking: it needs a deliberately mapped spelling of the user's own value, and the B-186 default as written is implemented.
- B-186 is still open in blockers.yaml. Supreme should accept or replace the default (refuse) before the area PR merges.

## Notes

- Round 1 should-fix items, all done:
  - `tools list` prints through `tool_id(&d, &n)` (cli.rs:857). The reading test adds a `<canary>.com` folder, with and without the domain argument.
  - `parent run` goes through `clean(&parent.id.0)` (cli.rs:614), with a new test that seeds an `SB_RUN_ID` holding the value.
  - The `gate set` refusal is in (B-186).
  - The unknown pin/unpin test asserts `NotFound`, empty stdout and an empty `tools.health` (accept_t08_r2.rs:694-735).
  - accept_t08b_d85.rs is committed (af12f27).
  - mcp_t07 pads TEMP/TMP/TMPDIR past 210 chars and stays under 250, so Windows GetTempPath still honours it (mcp_t07.rs:1587-1600).
- Both nits are done: the own run's task text is scrubbed (cli.rs:536), and the `tool_id` helper is used by pin, unpin, known_tool, retire and list.
- `retire` without a row still creates a row named with the raw `n`. That name matches an existing file, so this is pre-existing data and not new in this round.
- Rules 1, 3-7: not touched. No keys, sleeps, or `unwrap` on external input in non-test code. No interfaces.yaml change.

## Round 1 (9ff6c08, summary)

Verdict was changes, with 2 Blocking items (above, now closed) and the should-fix items listed in Notes (all done). D-85, the D-124 (1) ledger header, the `gate set --note` scrub, the `known_tool` refusal and the B-526 wording were already correct and red-capable.

## Verdict

Both round 1 Blocking items are closed and proven red-capable. Round 2 adds no rule 2 leak, no fail-open path and no vacuous test. One should-fix is left: the B-186 refusal should check the canonical key.

Verdict: approve
