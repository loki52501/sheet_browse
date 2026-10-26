# Review T-08: core::sched loop, harnessd, harness CLI (round 2)

Scope: T-08's commits 71acaef (D-124, B-195), 4620fb3 (code and tests) and 6a98d8e (handoff), from `git diff 3868c7e..6a98d8e -- crates/harness crates/core/src/sched.rs agent-memory/decisions.yaml agent-memory/blockers.yaml`. 1a2dd01 (review T-09) is not in scope.
I verified in a detached worktree at 6a98d8e with its own target dir, Postgres on 127.0.0.1:5433, and a unique SB_VAULT_CANARY. `cargo fmt --all -- --check` is clean. `cargo clippy -p harness -p harness-core --all-targets -- -D warnings` is clean. `cargo test -p harness --no-fail-fast` is green: lib 6, accept_t08 18, step1_accept 7, t08 10, t08_vault 6, vault_cli_repair_t05_r4 1. `cargo test -p harness-core --lib sched::` is green (8). The canary appears 0 times in any build or test log.

## Round 1 items

- Blocking (CLI prints unscrubbed text): **resolved.** cli.rs:247 installs the vault for every command except `vault set`, which returns at :240-245 before the install. t08_vault.rs:233-299 installs an empty vault first, so the `schedule list` and `tools health` assertions fail if the install at :247 is removed. The same test covers fail-closed on an undecryptable vault, with nothing printed.
- SF1 (`vault set` cannot repair): **resolved.** cli.rs:642 uses `open_for_set`. Covered by t08_vault.rs:306-344 and vault_cli_repair_t05_r4.rs. The test fails if the call goes back to `open`.
- SF2 (D-79 untested): **resolved.** t08.rs asserts the exact AlreadyExists text at library level, and also the binary's exact stderr and exit code 1.
- SF3 (scrubbing untested): **resolved.** t08_vault.rs:352-410 runs the real ClaudeLauncher against a compiled fake. It checks prompt.md, runs.prompt, the slug, stdin, answer.md and report.md.
- SF4 (no session or stdout cap): **resolved, with test gaps** (Should fix 1 and 2). The session cap is at launcher.rs:267. The stdout cap and scrub-then-cut are at launcher.rs:196-213 and :245-261.
- SF5 (`schedule add` leaves its run `running`): **resolved.** cli.rs:530-562. t08.rs `schedule_add_failing_after_the_row_ends_its_run_failed` asserts Failed, `ended`, and the marker.
- SF6 (`harness run` untested): **resolved.** t08.rs `harness_run_uses_the_launcher_seam` asserts strict, parent_run, status and the exact printed line, for both exit 0 and exit 3.
- SF7 (harnessd never reopens the vault): **resolved.** daemon.rs:43-55 wraps the launcher, and sched.rs:370 calls `before_run` before the scrub. t08_vault.rs:521-606 covers a value added after startup, and a broken vault giving BeforeRun with no folder and no launch.

B-192 argv. `-p` (prompt on stdin) and the interactive `<prompt>` both come before `--disallowedTools` and its 3 rules (launcher.rs:113-129). So the variadic list cannot swallow a prompt in either mode. Asserted by the launcher.rs unit test, by t08_vault.rs:402-405 on a real process, and by accept_t08.

accept_t08.rs. Exactly three hunks changed, at :785, :1521 and :1562. Each replaces one exact argv string with the same string plus the four B-192 lines. Every assertion is still `assert_eq!` on the full argv, so nothing is weakened. In vault_cli_repair_t05_r4.rs the fixture now builds itself and `#[ignore]` is gone. Its assertions are unchanged.

## Blocking

- none

## Should fix

1. crates/harness/tests/t08_vault.rs:461-466. The answer-cap test does not test "scrub before the 1 MiB cut" (D-124 (4)). The "cut" case puts the value across `cap + ANSWER_SCRUB_SLACK`, which `end.min(cap)` cuts off anyway. The "kept" case puts it well inside `cap`. If someone moved `answer_text` (launcher.rs:196) to cut first and scrub afterwards, both cases would still pass. Add a case with the value across `max_answer_bytes` itself, for example `"a".repeat(cap - 4) + v` with a small pad. Assert that no prefix of the value is written. The launcher.rs unit test `answer_is_cut_at_the_cap` covers the slack drop but has no vault value in it.
2. crates/harness/tests/t08_vault.rs:417-450. The hung-session test would still pass if `kill_session` (launcher.rs:175-190) did nothing. The `child` is dropped on return, and `kill_on_drop` kills it. The test also never checks that the process is gone, and on Windows it never exercises the `taskkill /T` path for a `.cmd` shim, which is the case D-124 (3) exists for. Have the fake write its pid, and assert the process has exited after the launch returns. Preferably start it through a `claude.cmd` wrapper on Windows.

## Notes

- Rule 1 holds. Only `claude` (or `SB_CLAUDE`) and `taskkill` are spawned. No key is read or set, and the diff has no key-shaped strings. The fakes are compiled Rust programs, not models.
- Rule 2 holds. `ReopenVault` maps `VaultError` (which names entries only) to `LaunchError`, and the launch problem is scrubbed again in execute. Tests redact the canary in every assertion message.
- Rule 6 holds. Every new wait is a condition with a cap: `timeout(max_session, session)`, `KILL_WAIT` on taskkill and on reaping, and test waits on `fired` / `next_deadline` under `CAP`. There is no sleep.
- Rule 4, scope of B-192. `Bash(harness ask:*)` is a command-prefix rule. It does not stop `harness.exe ask`, a full path, `cmd /c harness ask`, or `cargo run -p harness -- ask`. It also does not stop the session from writing the ask answer file itself with Write or Bash, since `ask::answer_path` is a plain file under `runs/`. So the deny list is a speed bump, not the rule-4 guarantee. Supreme should decide whether answer files need protection, for example a per-run nonce or a check that the answer was written by the user process, before B-192 is closed.
- launcher.rs:113-121 (pre-existing, not B-192). Interactive `harness run "update"`, `"doctor"`, `"mcp"` and similar make `claude` read the prompt as a subcommand. The leading-space trick used for `-` would cover this case too.
- launcher.rs:188. On non-Windows, `start_kill` kills only the direct child, so MCP server grandchildren of a capped session are orphaned. A process group (`process_group(0)` plus killpg) would match the Windows tree kill.
- sched.rs:370 and daemon.rs:44. `before_run` is synchronous and runs on a tokio worker. With the default keychain key (D-101), every firing makes a blocking keychain call and a decryption. That is cheap on Windows Credential Manager. On macOS the keychain may prompt the user. This is acceptable for Phase 1 and worth knowing.
- sched.rs:353. `fire` marks the schedule fired before `execute`, so a BeforeRun refusal drops that firing. There is no retry, and only the harnessd stderr line records it. That is correct for fail-closed, but `schedule list` shows `last_run` as if the run happened.
- cli.rs:247. `harness db migrate|status` now fails when the vault cannot be opened, although neither command prints vault-bearing text. This follows from D-124 (1) and is a fine default.
- launcher.rs:205. The note `kept {end} of {total} bytes` mixes scrubbed-text bytes with raw bytes. This is cosmetic.
- Interface fidelity. The new `Launcher::before_run` and `SchedError::BeforeRun` match B-195 exactly (`fn before_run(&self) -> Result<(), LaunchError>`, default `Ok`), and they are pending supreme for I-13. `cli::schedule_add` went from `pub` to private. It was never in I-13 or B-185, so no consumer breaks.
- Spec ids. 4620fb3 claims H-4, H-5, H-7, D-79 and D-116, and its body names D-124, B-192 and B-195. All of them match the code. D-124 (1) to (5) are each implemented as written. I found no behaviour outside D-124 and B-195.

## Verdict

Approve. Every round-1 item is resolved, and each fix has a test that fails if the fix is reverted. The two should-fix items are test gaps on the cap's edge cases. The code itself does the right thing in both.

Verdict: approve
