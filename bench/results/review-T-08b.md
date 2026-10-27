# Review T-08b round 1 (swarm/core-T08b at 9ff6c08)

Scope: `git diff main...HEAD` (crates/core/src/sched.rs, crates/core/src/ask.rs, crates/harness/src/cli.rs, crates/harness/tests/accept_t08_r2.rs, tasks.yaml, coder handoff). Checked against CLAUDE.md rule 2, pr-ready section 5, H-7, D-85, D-119, D-124, B-526.

Run here: `CARGO_TARGET_DIR=<wt>/target SB_VAULT_CANARY=sbcanary-x-<hex> cargo test -j 2 -p harness --test accept_t08_r2 -- --test-threads=1 d124_1`. Result: 8 passed, 0 ignored. All three former red-first tests run un-ignored and pass.

## Blocking

- Rule 2: `harness schedule add --watch-target` stores the typed text raw. crates/harness/src/cli.rs:516 puts `watch_target.clone()` into the `Schedule` row. core-pg `schedule_add` binds it unchanged into `schedules.watch_target` (crates/core-pg/src/lib.rs:873). The prompt next to it is scrubbed (:473), and `--note` got the same fix in this task (:297). The ledger copy at :476 is only safe because `Ledger::write` scrubs. Fix: `let watch_target = watch_target.map(|w| scrub(&w));` before :475. Add a `--kind watch --watch-target <canary>` case to `d124_1_writing_commands_store_and_print_no_value` (its `db.dump()` grep then covers it).
- The acceptance test for "tools pin/unpin/retire echo ids as literals" never reaches the echo. In `d124_1_tools_commands_echo_the_tool_id_as_literal` (accept_t08_r2.rs:643-677), `luma.com/<canary>` has no row and no file. So `known_tool` (cli.rs:893) and `retire` (:928) return NotFound before the `writeln!`s at cli.rs:864-879 and :951-957, and the error line is scrubbed by main (`binary_error_line`) anyway. If you revert the `clean(...)` in all three success echoes, the test still passes. Only the `known_tool` refusal is guarded. The real case is a tool whose id became a vault value after its row or file existed, the same scenario as the ledger header test. Seed a `tools.health` row (and a file for retire) named `luma.com/<canary>`, then assert that pin, unpin and retire print `@vault(test)` and not the value.

## Should-fix

- Rule 2, other CLI paths that write typed text unscrubbed:
  - `gate set <domain> <class>` stores `domain` and `class` raw in `gate.rules` (cli.rs:297). `gate::rule_key` accepts a canary-shaped label (`[a-z0-9-]`), so `harness gate set sbcanary-x-....com Send ask` puts the value in the database. stdout is cleaned (:302-303). Scrubbing doesn't fit here: scrubbing would turn the key into one `rule_key` refuses, which fails closed but rejects a legitimate rule whose domain happens to contain a short personal value. Write a blocker with a default (proposed: refuse a domain or class that contains a vault value, naming the entry, not the value).
  - `tools list` prints the folder name raw: `format!("{d}/{}", cell(&n, 0))` at cli.rs:813. The name is cleaned but `d` is not. `d` is typed (`tools list <domain>`) or read from disk. Use `cell(&d, 0)`. This is a D-124 (1) reading command, so add a folder named with the canary to `d124_1_reading_commands_print_the_literal`.
  - `schedule add` prints `parent run {}` with `parent.id` raw (cli.rs:570). Under `SB_RUN_ID` that is the session's run id, which can carry a value exactly like the ledger header (fixed here at :691). The error path at :486 already uses `clean`. Use `clean(&parent.id.0)`.
- Test gaps:
  - D-85 has no committed test on the branch. The tester (or coder) should commit crates/core/tests/accept_t08b_d85.rs.
  - The unknown-id refusal is asserted only as "no value in tools.health". Assert that `tools pin x/unknown` and `tools unpin x/unknown` return `CliError::NotFound` and leave `tools.health` empty, with an id that holds no value. As written, a refusal that silently returned Ok would pass.
  - The B-526 guard depends on the environment. `mcp_t07::file_asker_stderr_has_no_canary` (crates/core/tests/mcp_t07.rs:1578) fails on the old wording only when TEMP is long. Put the child's run folder under a path padded past the line cap, so reverting ask.rs:151/:174 goes red on every machine.

## Nits

- `schedule add` without `SB_RUN_ID` writes the cron unscrubbed into `runs.prompt` and prompt.md (`task` at cli.rs:492). `parse_cron` must accept it first, so a value can only land there if it is a valid 5-field cron. Scrubbing it next to `prompt` costs nothing.
- `known_tool` and `retire` each build `clean(&format!("{d}/{n}"))`. A small `tool_id(d, n)` helper would keep the next echo from forgetting it.

## Notes

- D-85: `SchedError` carries `#[non_exhaustive]` (sched.rs:45-47). No committed test guards it: crates/core/tests/accept_t08b_d85.rs exists in the worktree only as an untracked file (written 16:20 next to an untracked T-08b-tester-1.yaml, so probably the tester's), on neither the branch nor main. Its design is right (a `#![deny(unreachable_patterns)]` match with a `_` arm plus a source check, which goes red without the attribute), but it has to be committed. See Should-fix. No external exhaustive match broke.
- D-124 (1) ledger header: run id and parent go through `clean` (cli.rs:686-697), and so does the "no run" error. The test seeds both the parent and the own id with the canary and goes red on revert (the header is the only place either is printed).
- `gate set --note`: `scrub(&note)` before `gate_rule_set` (cli.rs:297). The test dumps `gate.rules` and requires the literal, so it goes red on revert.
- `known_tool`: errors from `try_tool_path` and `health` propagate (`?`), and an unknown id is refused. It does not fail open. With `known_tool` reverted, `set_pinned` would create a row named with the value, and the new `tools.health` dump assertion catches that.
- B-526: both FileAsker fail-closed lines now start with `ask: skipped, nobody saw the card:` (34 chars, far under the `clean_line` cap). The path still goes through `clean_line` (scrub, clean, scrub per D-119 (7)). The fail-closed behaviour itself (`None` leads to TimeoutSkip) is unchanged.
- No fail-open path found in the diff: `install_vault` still runs before every command except `vault set` (cli.rs:248). main.rs scrubs every error line. Errors from `check_tool_id`, which quote the raw id with `{:?}`, only reach the user through that scrub.
- Rules 1, 3-7: not touched. No keys, no sleeps, no `unwrap` on external input in non-test code.
- clippy `-p harness-core -p harness --all-targets -- -D warnings`: clean (run by the reviewer, private target dir, -j 2).

## Verdict

The scoped D-85 and D-124 (1) fixes are correct, and their tests are red-capable, except the tools echo test, which never exercises the echo it names. `--watch-target` is the same rule-2 hole as `--note`: user-typed text stored raw in the database by a writing command. Both are small fixes.

Verdict: changes
