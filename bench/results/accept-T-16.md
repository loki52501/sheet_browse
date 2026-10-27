# Acceptance T-16 (step 4: H-5, docs/02 step-4 items, T-6..T-8, G-3)

Branch `swarm/core-T16` at 2322a9b (main merged in), worktree F:/sb-swarm/wt/core/wt/T16.
Tester: tester-1 (a different agent from the coders and the reviewer). Date 2026-10-26 (local) /
2026-10-27 UTC. No test or implementation file was added or changed.

Scope: docs/09 step 4 "Done when: ladder rungs 6, 7, 8 pass; reflect retires a planted stale
ephemeral tool", docs/01 H-5, docs/02 acceptance (step 4 bullets), D-140..D-143, B-187.

## Environment

- `CARGO_BUILD_JOBS=1`, `CARGO_TARGET_DIR=F:/sb-swarm/wt/core/wt/T16/target`, debug profile.
- `SB_VAULT_CANARY=sbcanary-x-<16 random hex>` set for every cargo command and every ladder
  process. It was planted as vault entry `gh_token` in a scratch vault
  (`SB_VAULT_PATH=F:/sb-swarm/tmp-core/t16-tester-ladder/vault/vault.toml.age`) with
  `target/debug/deps/ladder_vault_plant-*.exe --ignored` (the ignored ladder fixture; it reopens
  the vault and checks the scrubber replaces the value: 1 passed).
- Postgres: the compose container on 127.0.0.1:5433. Tests use their own throwaway databases
  (D-100). The ladder uses the throwaway database `sb_t16_ladder`, migrated with `harness db
  migrate`. It is not the `sb` database and was left in place for inspection.
- Ladder method: the same as bench/results/ladder-phase1-servers.md. Runs are created by the real
  `harness run "<prompt>"` and by the real `harnessd`. `SB_CLAUDE` is a .cmd shim for
  `bench/fixtures/ladder_session.py`, which starts a headless `claude -p` with these settings:
  - cwd is an empty scratch folder;
  - `--strict-mcp-config --mcp-config` points at harness-web, harness-files and harness-mem
    (`target/debug/srv-web.exe`, `srv-files.exe`, `harness.exe mcp`, built from this branch);
  - `--setting-sources local --permission-mode default`;
  - `SB_LADDER_ALLOWED="mcp__harness-web mcp__harness-files mcp__harness-mem"`;
  - the deny list is the harness three plus the built-ins;
  - `SB_LADDER_CAP=600` s; `SB_SESSION_CAP_SECS=700`.

  Every transcript says Claude Code 2.1.311, model claude-sonnet-5-5, `apiKeySource: none`
  (rule 1). There are 9 shim transcripts. The reflect replay session used the real
  `claude.exe` (see row 2b) so that harnessd's own readers-only flags were exercised.
- Scratch tools root: a copy of the repo's `tools/`, at F:/sb-swarm/tmp-core/t16-tester-ladder/tools.
- Logs: F:/sb-swarm/tmp-core/t16-tester-*.log and *.out. Ladder files are under
  F:/sb-swarm/tmp-core/t16-tester-ladder/ (runs/, logs/ transcripts).
- Scorer: F:/sb-swarm/tmp-core/t16-tester-score.py. For each run it compares the transcript's MCP
  tool_use blocks with the run's ledger.jsonl rows and the `ledger.events` table rows.

## Results

| item | command | result | note |
|---|---|---|---|
| 1a H-5 schedules table, kinds cron/watch/reflect, `harness schedule add` | `cargo test -p harness-core -p harness --no-fail-fast` (accept_t16_mem `schedule_add_creates_a_row_with_this_run_as_parent_and_wakes_harnessd`, accept_t16_watch `reflect_row_fires_a_fresh_run`, accept_t09 `schedule_add_*`) | pass | All ok. The live rungs 7 and 8 created cron and watch rows through `schedule.add`, and row 2b created a reflect row with the CLI. |
| 1b H-5 a firing is a fresh session with the stored prompt and a run folder; the preamble names the run files; print sessions are allowed the 4 harness servers | same run (accept_t16_watch `rung7_weekday_cron_fires_with_deliverable`, `run_preamble_names_run_files_and_watch_file`; launcher unit tests `print_sessions_allow_the_harness_servers`, `session_prompt_is_preamble_then_task`, `every_session_denies_the_user_only_commands`, `session_cap_parses`, `answer_is_cut_at_the_cap`, `missing_program_is_a_launch_error`) | pass | All ok. The live rung 7 firing had `prompt.md` equal to the stored prompt and `runs.parent_run` set to session A's run. |
| 1c H-5 watch / docs/02 bullet 5: an unchanged target sends nothing, a changed one exactly one notification, and the new baseline is stored | same run (accept_t16_watch `rung8_watch_notifies_once_per_change`, `watch_without_state_sends_nothing_and_keeps_baseline`) | pass | Red proof 1 (watch.rs: `Some(b) if b.hash == hash && false => Unchanged`): `cargo test -p harness-core --test accept_t16_watch` gave `rung8_watch_notifies_once_per_change ... FAILED` with `left: "Watch: jobs: changed, notified (...)" right: "Watch: jobs: no change, nothing sent"` and `5 passed; 1 failed`. Restored with `git checkout -- crates/core/src/watch.rs` and touch, then re-run: `6 passed`. |
| 1d docs/02 bullet 3: `touch_contact` makes a Send to that person allowed on the next gate check (H-1.1, D-141, B-198) | same run (accept_t16_contacts, 10 tests) | pass | 10/10 ok, including the answered no, timeout_proceed, failed, stopped and mid-call stop cases, which touch nothing. Red proof 2 (mcp.rs: `if false && class == Class::Send && ...`): `cargo test -p harness-core --test accept_t16_contacts` gave `6 passed; 4 failed`, among them `send_new_answered_yes_then_the_same_recipient_is_allowed_without_a_card` and `every_distinct_recipient_is_touched_once`. Restored with checkout and touch, then re-run: `10 passed`. |
| 1e docs/02 bullet 4: `grade_review` follows SM-2 (M-2, M-3 reviews) | same run (accept_t16_mem `rung6_review_add_grade_then_a_new_connection_shows_the_sm2_date`) plus `cargo test -p core-pg --test accept_t03 a03_grade_review_sm2` | pass | rung6 test: grade 5 gives 1 d and reps 1, grade 4 gives 6 d and reps 2, grade 1 resets to 1 d, all over a new connection. a03 (grade 5 multiplies the interval by the ease, within 1 d; grade < 3 resets to 1 day): `1 passed`, no SKIPPED. |
| 1f harness-mem server (`harness mcp`): review/schedule/contact/watch tools over stdio through the gate | same run (accept_t16_mem `harness_mcp_serves_the_mem_tools_over_stdio`, `watch_observe_writes_the_run_watch_file_and_contact_get_reads_contacts`, `a_vault_value_in_a_review_never_reaches_a_reply_the_row_or_the_ledger`) | pass | 5/5 ok on Postgres, with no SKIPPED line. schedule.add asks as Account (B-187); in the live rungs 7 and 8 it showed an ask card and was answered with `harness ask <run> 1 yes`. |
| 1g H-5 reflect / G-3: readers only, tools.health updated, proposals under `proposals:`, pinned tools only proposed, never a planner, replay session limited to readers, commit limited to the sweep's paths | same run (accept_t16_reflect `reflect_retires_stale_tools_on_memstore`, `reflect_retires_stale_tools_on_postgres`, `reflect_commits_the_sweep`; unit `the_replay_session_is_readers_only`, 4 reflect and 5 proposals unit tests; contract_paths `reflect_paths`) | pass | All ok. Red proof 3 (reflect.rs: sweep given `now - Duration::days(30)`): `cargo test -p harness --test accept_t16_reflect` gave `0 passed; 3 failed`, `left: Ephemeral right: Retired` at accept_t16_reflect.rs:503. Restored with checkout and touch, then re-run: `3 passed`. |
| 2a Reflect retires a planted stale ephemeral tool (test) | `cargo test -p harness --test accept_t16_reflect` (and the full run) | pass | `example.com/stale_list` is ephemeral with last_ok 15 days ago. It ends retired and archived, with a `tools.retire` event in the reflect run's ledger and jsonl, on MemStore and on Postgres. |
| 2b Reflect retires a planted stale ephemeral tool (live harnessd) | `harness schedule add --kind reflect "40 4 * * *" "Nightly reflect: ..."`, then `harnessd` with `SB_CLAUDE=claude.exe` (Claude Code session variables unset), run `2026-10-27T04-40-00-nightly-reflect-check-saved-tools-replay` | pass | Planted `example.test/stale_list.js` and a `tools.health` row (ephemeral, last_ok now - 15 d) in a git-initialised scratch tools root. `reflect.md` says `sweep: retired 1 ... replay: 1 readers; commit: dc4d4d6`. tools.health is now `retired`, why `ephemeral, unused 14 days`. The file moved to `_archive/example.test/stale_list.1.js` in a commit that touches only that path. The ledger has sched.fire, tools.retire and reflect.sweep. The replay session used `replay-mcp.json`, which lists harness-web only; it made 2 Read calls (fetch and ashbyhq.list_jobs, both ledgered), and its answer was `ashbyhq.list_jobs: ok`. 0 proposals, so no proposals file was written. |
| 3 Ladder rung 6 (quiz and reviews, restart) | `harness run "<rung-6 Session A>"` then a new `harness run "<rung-6 Session B>"`, runs `2026-10-27T04-31-39-quiz-me-on-the-paper-attention-is-all-yo`, `2026-10-27T04-32-43-which-of-my-review-cards-are-due-now-gra` | pass | Scored yes on finished, evidence, no unrequested action, ledger complete and vault clean. A: 5 `review.add` (Fill allow); there are 5 `memory.reviews` rows, all due now. B, with a new session, mem process and connection: 1 `review.due` and 3 `review.grade`. Rows 1 and 2 are reps 1, 1 d, ease 2.6. Row 3 is reps 0, 1 d, ease 1.96. Rows 4 and 5 are untouched. The dates in the answer match the rows. Transcript MCP calls equal ledger rows (5=5=5, 4=4=4). Caveat: A wrote the cards from memory and never fetched the arXiv page, and its answer says so. The rung's scoring does not require the fetch. Interactive `harness run` passes no grounding preamble (servers ladder failure 1). |
| 4 Ladder rung 7 (weekday schedule with a file) | `harness run "<rung-7 Session A>"`, `harness ask <run> 1 yes`, `update schedules set cron='36 4 * * 1-5' where id=2` (was `0 15 * * 1-5`), then `harnessd`, runs `2026-10-27T04-33-35-every-weekday-at-8-am-check-https-jobs-a`, firing `2026-10-27T04-36-01-fetch-https-jobs-ashbyhq-com-linear-list` | pass | A: one `schedule.add` (Account, answered_proceed), kind cron, `0 15 * * 1-5` UTC, parent run = A. The answer states that 8 am Pacific is 15:00 UTC. Only minute and hour were moved; the weekday field was kept, and 2026-10-27 is a Tuesday. Firing: new run folder, `prompt.md` = the stored prompt, `runs.parent_run` = A, status done. It made 5 MCP calls (fetch, fs.glob, fs.read of A's file, ashbyhq.list_jobs, fs.write), with 6 ledger rows including sched.fire, table = jsonl. `files/linear-job-matches-2026-10-26.md` is listed in report.md, and all 5 of its job URLs appear in the list_jobs result. Nothing was submitted. |
| 5 Ladder rung 8 (watch an event page) | `python -m http.server 8765` serving bench/pages/t16, `harness run "<rung-8 Session A>"`, `harness ask <run> 1 yes`, `harnessd` (4 firings, page swapped to the open page after firing 2), `harness schedule rm 1` | pass | A: fetch plus `schedule.add` kind watch (Account, answered), `* * * * *`, watch_target `http://127.0.0.1:8765/event.html registration status`. The firings at 04:27, 04:28, 04:29 and 04:30 reported, in order: `first observation, baseline set, nothing sent`; `no change, nothing sent`; `changed, notified (.../files/notification.md)`; `no change, nothing sent`. There is exactly one notification.md, one line in runs/notifications.md and one `watch.notify` (firing 3 only). `watch.check` appears in every firing. The `memory.baselines` row was updated at firing 3. Each firing made fetch (Read) and watch.observe (Fill) only. Calls equal ledger rows in every run. |
| 6 Vault canary nowhere | `grep -rl -a -F "$SB_VAULT_CANARY"` over F:/sb-swarm/tmp-core/t16-tester-ladder (runs/, notifications.md, logs/ transcripts and stderr, cwd, tools, reflect files; the encrypted vault and the canary note excluded), F:/sb-swarm/tmp-core/t16-tester-*.log and *.out (16 files: test, clippy, red-proof, SM-2, harness run and harnessd output), `pg_dump sb_t16_ladder` (815 lines: ledger.events 46 rows, runs, schedules, memory.*, tools.health), and the worktree outside target/ | pass | 0 hits in every place. No proposals file was written in the live pass, because there were 0 proposals. The proposals files in the tests are covered by accept_t16_reflect's own canary assertions. |
| 7a Gate: fmt | `cargo fmt --all -- --check` | pass | exit 0 |
| 7b Gate: clippy | `cargo clippy -p harness-core -p harness --all-targets -- -D warnings` | pass | `Finished`, exit 0, no warnings |
| 7c Gate: tests | `cargo test -p harness-core -p harness --no-fail-fast` | pass | Exit 0: 479 passed, 0 failed, 3 ignored, 0 SKIPPED lines. The 2 grep hits for "skipped" are test names (`torn_last_line_is_skipped...`). The ignored tests are `plant_ladder_canary` (the ladder fixture, run separately above, 1 passed) and two child-process helpers (`d119_7_diag_child`, `file_asker_stderr_child`) that their parent tests spawn. |

## Notes for T-16b (not failures of the step-4 acceptance)

- Deferred by D-143 and not tested here: H-5/G-3 "compare output shape"; T-6 growth and forge
  proposals; G-3 expectation learning and after-each-task reflect; H-5 "nightly" (no reflect row is
  created automatically).
- `harness schedule list` prints `T-16` in the `next (UTC)` column for watch and reflect rows
  (cli.rs `schedule_list`). `harness schedule add --kind reflect|watch` still prints "note: kind
  reflect is stored; harnessd runs it from T-16 (not in Phase 1)". Both are stale placeholders from
  T-08.
- The readers-only replay session printed `Permission deny rule "MultiEdit" matches no known tool`
  to harnessd's stderr. MultiEdit is in the launcher's built-in deny list, and Claude Code 2.1.311
  has no such tool. This is harmless.
- The shim replaces harnessd's `--allowedTools` with `SB_LADDER_ALLOWED`. The launcher's own
  allow list for print sessions is therefore proven by the unit test, not by rungs 7 and 8.
  harnessd's readers-only flags were proven live, in row 2b.
- In rung 6 session A the paper was not fetched. A grounding preamble for interactive `harness run`
  is the same gap as servers ladder failure 1.

Verdict: green
