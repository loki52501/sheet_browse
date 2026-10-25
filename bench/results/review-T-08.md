# Review T-08: core::sched loop, harnessd, harness CLI (round 1)

Scope: `git diff main...15e09d3 -- crates/core/src/sched.rs crates/harness` (commit b7aa4f4, which 374290c restores). The five files b7aa4f4 reverted are out of scope.
Verified in a detached worktree at 15e09d3 with its own target dir. `cargo fmt --check` is clean. `cargo clippy -p harness -p harness-core --all-targets -D warnings` is clean. `cargo test -p harness` passes (3 unit + 7 Postgres integration), and so does `cargo test -p harness-core --lib sched::` (8).

## Blocking

- crates/harness/src/cli.rs:555, :747 (and :19-20). Rule 2. `schedule list`, `tools list` and `tools health` print database text (schedule prompts, run ids, `tools.health.why`) through `cell()` -> `scrub()`, but these commands never call `install_vault()`. With no vault installed, `harness_core::vault::scrub` returns its input unchanged (crates/core/src/vault.rs:466-472), so nothing is scrubbed. The same holds for the binary's error path (crates/harness/src/main.rs:20) for `schedule list|rm`, `gate set`, `tools *`, `ask` and `db`. The module doc says "everything printed from the database ... goes through the vault scrub", and that is not true. Rows written outside the CLI (srv-cli is named as a consumer in I-13) reach the terminal raw. Fix: install the vault once at the top of `execute` for every command except `vault set`, failing closed as `run`, `schedule add` and `ledger` already do. Add a test that a `schedules.prompt` holding a vault value prints as `@vault(name)` in `schedule list`.

## Should fix

1. crates/harness/src/cli.rs:612. `vault set` opens the vault with `Vault::open`. Since fc23dd2 (T-05 r4, after T-08), `Vault::open_for_set` is the handle meant for this command (vault.rs:290-300). Today, a vault file that has an entry under the D-118 floor cannot be repaired. `open` refuses with `ValueTooShort`, whose message says to run `harness vault set <name>`, and that command refuses again. This is a dead end. Use `open_for_set`, and test the repair through the CLI.
2. crates/harness/tests/t08.rs:459-463. D-79 has no test. The code is correct by reading: io::Error -> `CliError::Io` (transparent) -> `harness: <message>`. But no test answers the same ask twice and asserts the exact "ask 1 in run <id> is already answered (yes); nothing changed" text, at the library level and on the binary's stderr. The only check is `is_err()` for a missing card.
3. crates/core/src/sched.rs:357 and crates/harness/src/launcher.rs:159. Rule 2 scrubbing has no tests. Nothing shows that a vault value in a schedule prompt becomes `@vault(name)` in prompt.md, `runs.prompt`, the run slug and the prompt the launcher receives. Nothing shows that `files/answer.md` is scrubbed either. Add a sched test with an installed in-memory vault. Add a launcher test that points `SB_CLAUDE` (or `ClaudeLauncher::print(path)`) at a fake program that echoes the value.
4. crates/harness/src/launcher.rs:142-146. A `claude -p` session is awaited with no cap. A hung session keeps its schedule "running" forever (the loop skips it) and blocks harnessd's shutdown drain. `read_to_end` is also unbounded. Add a per-run wall cap (configurable; `kill_on_drop` is already set) and a stdout byte cap.
5. crates/harness/src/cli.rs:516-534. When `schedule add` creates its own run and the ledger write, report or `run_end` fails, the run row stays `running`. The schedule row also stays without `notify_changed`. The `schedule_add` failure branch (:510-512) ends the run as Failed. Do the same on every later error path.
6. crates/harness/src/cli.rs:383-416. `harness run` (H-7) has no test, although the `Ctx.launcher` seam exists for it. Assert `strict` on the row, `parent_run` from `SB_RUN_ID`, and the printed status/report line, using a fake launcher.
7. crates/harness/src/bin/harnessd.rs:22. harnessd installs the vault once at startup. A value added later with `harness vault set` is not scrubbed from firings' prompts or `answer.md` until a restart. Re-open the vault per firing (or on the `.sched` marker), failing closed.

## Notes

- Rule 1 holds. `ClaudeLauncher` only spawns `claude` (or `SB_CLAUDE`), with the prompt on stdin for `-p`. It sets only `SB_RUN_ID` and an absolute `SB_RUNS_ROOT`. No API call, no key read or set, and no key-shaped strings in the diff. The child inherits the parent environment, which is Claude Code's own business.
- Rule 6 holds. The only waits are `SystemClock::sleep_until` on a computed deadline (sched.rs:129) and the capped DB-retry backoff (sched.rs:525). Both have a comment saying why. Change detection uses the `.sched/changed` notify watcher (daemon.rs:16-25), with no poll interval. The tests wait on `ManualClock` deadlines and the `fired` channel, with caps.
- Rules 3, 5 and 7 do not apply: no challenge path, no HTML or pixels, no user-sent drafts.
- Rule 4, design level (D-105, not T-08). The CLI is user-side, not a tool server. But a session allowed to use Bash could run `harness ask <run> <n> yes` or `harness gate set '*' Send allow` and approve its own asks. Consider having `ClaudeLauncher` pass `--disallowedTools` for `Bash(harness ask:*)`, `Bash(harness gate:*)` and `Bash(harness vault:*)`, or recording a decision.
- launcher.rs:65. The session's stderr is inherited, so it reaches the terminal without harnessd's scrub. The risk is low because tool results are already scrubbed, but this should be stated.
- harnessd.rs:35-40. The second Ctrl-C calls `process::exit(130)`, which skips `kill_on_drop`. On Windows the first Ctrl-C also reaches the `claude` child (same console group), so the "drain" mostly records `failed (session exited ...)`.
- sched.rs:119-133. Long sleeps run on the monotonic clock. After a host suspend the firing can be late until the monotonic deadline passes. This is acceptable for Phase 1.
- sched.rs:629. A panicked or cancelled firing is reported as `SchedError::Io`, which displays as "run folder: firing panicked". The message is misleading.
- Interface fidelity. Every I-13 name in D-80 is exported with the shape it describes (`Scheduler{runner, wake, fired}::run_loop(shutdown)`, `Runner::{fire, execute}`, ...). I-2 is used through `ask::answer`. I-7 paths go through `try_tool_path` and `check_tool_id` (D-74). The code also uses `toolstore::ARCHIVE`, `ToolStore::new` and `ToolStore.root`, which the I-7 `rust:` line does not list. Supreme may want to add them.
- Spec ids. The commit's H-4, H-5, H-7, D-105, D-106, D-107 and D-116 match the code: the H-4 layout is asserted in t08.rs:289-302, and acceptance item 4 in t08.rs:195-332 is a real assertion test (row, folder, report.md, parent_run, a single firing, marker wake-up). D-74 and D-79 are implemented but not named in the commit. watch and reflect are deferred to T-16 explicitly (`NotInPhase1`, a single log line). The extras beyond H-7 (`ask`, `--strict`, `--note`, `--kind`, `--why`, `--no-commit`, `answer.md`, the `schedule.add` ledger event) are all covered by D-105, D-107 or D-116.

## Verdict

Request changes. There is one blocking item: CLI output that the code says is vault-scrubbed is printed unscrubbed, because the vault is never installed for those commands.

Verdict: request changes
