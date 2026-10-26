# Review T-07 rounds 3 and 4: H-6 MCP wrapper and FileAsker (2330457, bfa2df9; decisions 9fc45d7, handoff 5267f81)

Scope: `git diff 9824f94..5267f81 -- crates/core/src/mcp.rs crates/core/src/ask.rs crates/core/tests/mcp_t07_r3.rs crates/core/tests/mcp_t07_r4.rs crates/core/tests/accept_t07_r2.rs`, plus the T-07 entries in agent-memory/decisions.yaml (D-119 (2) amended, D-123, D-127). Round 2 was reviewed at 15e09d3.

Checked in a detached worktree at 5267f81 with a unique `SB_VAULT_CANARY`, Postgres on 127.0.0.1:5433:
- First `cargo test -p harness-core` run: mcp_t07_r3 `a_polling_reader_only_ever_sees_a_complete_card` failed once. The 20 s cap elapsed on a 1 ms ask (mcp_t07_r3.rs:139). See Should fix 2.
- Second run with `--no-fail-fast`: all 22 targets pass. lib 125, accept_t07 10, accept_t07_r2 15 (+1 ignored child), mcp_t07 33 (+1), mcp_t07_r3 7, mcp_t07_r4 13, plus the T-04, T-05 and T-06 targets.
- FileAsker stress, 12 loops of accept_t07 `a7_file_asker`, accept_t07_r2 `d119_4 file_asker`, mcp_t07 `file_asker` and the whole of mcp_t07_r3: 12 of 12 green. Three more standalone mcp_t07_r3 runs were also green. The round-2 "EOF while parsing a value" flake did not come back.
- `cargo test -p harness --test step1_accept`: 7 pass. b2 ran with the canary set, not skipped.
- `cargo clippy -p harness-core --all-targets -- -D warnings` and `cargo clippy -p harness --all-targets -- -D warnings` are clean, and rustfmt `--check` is clean on the touched files.
- The canary appears 0 times in all test output.

## Round 2 items

- Should fix 1, card published atomically: fixed. `write_card` goes through `claim` (temp file + `hard_link`), and AlreadyExists moves to n+1 (ask.rs:219-235, :266-291). Tests that would fail if this broke:
  - mcp_t07_r3.rs:125: a reader polls the card path through 215 asks and sees no partial card. It also checks that no temp file is left.
  - mcp_t07_r3.rs:160: 12 concurrent askers with separate counters get 12 distinct n.
  - mcp_t07_r3.rs:191: `answer` refuses an empty or partial card with InvalidData (ask.rs:426-435).
  - accept_t07_r2.rs:849: the tester's red-first test, now un-ignored.
  The no-hard-link fallback `claim_without_links` (ask.rs:296-305) is correct on reading (placeholder via `create_new`, one rename, placeholder removed on failure) but has no test. See Should fix 1.
- Should fix 2, site-less mid-call card: fixed. `CallInfo.domain` is set from the scrubbed classified domain (mcp.rs:1099) and used when `card.site` is None (mcp.rs:397). Tested with a stop rule plus a control on another site (mcp_t07_r3.rs:386) and a strict rule (:416).
- Note 1, answer claimed on every exit path: fixed. The paths are:
  - The `OpenAsk` guard closes the ask in Drop (ask.rs:45-64).
  - The guard rides the oneshot from the card-writing thread, so a card that lands after the drop is closed too (ask.rs:146-176).
  - `close_answer` falls back to a bare `create_new` (ask.rs:240-257).
  - A failed close is TimeoutSkip, never proceed (ask.rs:193-199).
  The cap drop and the drop mid-publish are tested at mcp_t07_r3.rs:211 and :244 (120 drop points swept). The `create_new` fallback and the failed-close TimeoutSkip path have no test (Should fix 1).
- Stop lines cleaned (scrub, clean_card, scrub), four places, all through `clean_line`/`clean_stop` (mcp.rs:1299):
  - Gate stop before the call: mcp.rs:1132. Tested at accept_t07_r2.rs:1087, second half.
  - Mid-call Challenge: mcp.rs:376. Tested at accept_t07_r2.rs:1087, first half.
  - Handler `stopped`: mcp.rs:1317. Tested at accept_t07_r2.rs:1147, covering the ledger, the returned value and the model text. Also mcp_t07_r4.rs:477 (trailing stop).
  - Mid-call gate Stop: mcp.rs:407. Correct, but no test checks that its why comes out as one clean line (Should fix 3).
- Carried note, registry lowering a class: fixed. Each name keeps a floor that clones share and that survives `remove`; `new` and `add` register `max(floor, class)` (mcp.rs:180-230). Tests:
  - mcp_t07_r4.rs:203: re-adding a tool keeps the higher class and takes the new handler.
  - mcp_t07_r4.rs:229: the floor survives remove, and clones share it.
  - mcp_t07_r4.rs:251: a name listed twice in `new` gets the higher class.
  - mcp_t07_r4.rs:265: dispatch asks as Pay for a Pay tool re-registered as Read.
  Rule 4 holds.
- Carried note, two core stops for one stopped call: fixed. While the handler runs, a mid-call stop is held, and only the first is kept (mcp.rs:495-521, :1152-1161). When the handler returns stopped, the order is: tool stop event, then one trailing core stop carrying the held stop (mcp.rs:1188-1203). Otherwise the held stop is written before the tool event (:1206, :1219). This is consistent with D-121 (4) and `ledger::report`:
  - A stopped call is a tool stop plus its trailing core stop, grouped by the forward rule.
  - A mid-call stop on a call that went on is a lone core stop, listed once.
  - A gate Stop is one core stop.
  Tests assert both the event shapes and the `Not done` count (mcp_t07_r4.rs:289, :332, :366, :390 for ok/fail/panic, :434 for the H-1.4 relisting, :477).
- Carried note, no line cap: fixed. `read_line_capped` (mcp.rs:785-823) caps a line at `MAX_LINE_BYTES` = 8 MiB, newline not counted. It frees the buffer past the cap, answers -32600 with id null, and keeps reading (mcp.rs:718-745). Tests:
  - mcp_t07_r4.rs:550: a long line, then the next line is served.
  - mcp_t07_r4.rs:569: exactly the cap is served, one byte more is refused.
  - mcp_t07_r4.rs:590: a long last line with no newline still gets an answer.
  - mcp.rs:1574: the buffer stays bounded and the read is cancel safe when dropped part-way.
- Carried note, handoff wording about empty recipients: noted again in T-07-rust-coder-3.yaml. The behaviour was always correct.

## Blocking

- none

## Should fix

1. ask.rs:240-257 and :296-305. `close_answer`'s bare `create_new` fallback, `claim_without_links`, and the failed-close TimeoutSkip in `close` (:193-199) have no test. A unit test in `ask::tests` can call `claim_without_links` directly. It should assert that:
   - the target holds the whole text;
   - the temp file is gone;
   - a second claim gets AlreadyExists;
   - a failed rename (target dir made read-only, or a tmp path that does not exist) leaves no placeholder.
   These are the fail-closed paths of D-123 (1) and (3), and today a regression in them would pass CI.
2. mcp_t07_r3.rs:139 (`a_polling_reader_only_ever_sees_a_complete_card`). The 20 s cap on a 1 ms `FileAsker::ask` elapsed in 1 of 16 runs of this target. It happened in the first full-suite run, and that run's other targets were green. Run times of the target vary from 20 s to 64 s. The cause is not pinned down. Two candidates:
   - The reader thread spins on the card path with no pause.
   - `notify::recommended_watcher` + `watch` run synchronously inside the async `ask` (ask.rs:352-355). This code predates this round. On Windows, `watch` blocks on an ack from the watcher thread.
   This is not the round-2 partial-card flake (that check never failed), but step-1 CI can still go red on it. Find where the time goes, or give the reader a yield, or run the watcher setup off the executor.
3. mcp.rs:407. A mid-call gate Stop goes through `clean_stop`, but no test checks that its why is one clean line. accept_t07_r2.rs:1087 covers the Challenge and the gate stop before the call, :1147 covers the handler stop, and this is the fourth place. If it regressed to `scrub` alone, no test would fail. Add a case with a dirty site or element to `d119_7_stop_lines_are_one_clean_scrubbed_line`.
4. ledger.rs:40-42 (T-06 owner). The module header is now wrong on the writer side. It says "a mid-call challenge adds a core stop from `ToolCtx::ask` before them, written while the call ran". Since D-127, the wrapper writes no core stop before a stopped call's tool event: the mid-call stop becomes the trailing stop, and on a call that did not stop it is written as a lone core stop just before the non-stop tool event. The reader rules below that sentence are still right, and `report()` handles both shapes, so the code is fine; only that sentence needs rewording.

## Notes

- With D-127 the backward grouping rule in D-121 (4) (ledger.rs:864-875, "core stops right before it whose ts lies inside the call") is only needed for ledgers written before D-127. The one case it can now get wrong: a gate Stop on the same (tool, args), followed within duration_ms + 100 ms by a handler-stopped call, merges into one listing. That is unlikely under H-1.4 (the run is handed back). The T-06 owner could limit the rule to old runs.
- No-link fallback edge: on a filesystem without hard links, `close` can see the user's empty placeholder and return TimeoutSkip (ask.rs:186-190), while `harness ask` then reports Ok. This fails safe (nothing proceeds), but the user is told their answer was taken. Hard links exist on NTFS and ext4, so this is low priority.
- `OpenAsk::drop` (ask.rs:57-64) does small blocking file calls inline on the executor. The code says why, and it is acceptable.
- A `ToolCtx::ask` made from `classify` sees no call domain (mcp.rs:1052; the domain is set only at :1099), and its stops are written at once (documented). Asks from classify are unusual; worth one doc line.
- The class floor lives as long as the registry, so it resets when the process restarts. Keeping the class from going down when a tool is repaired or forged across processes is the tool store's job (rule 4, docs/03). The `floors` map only grows; it is bounded by the names seen.
- I-2 fidelity: no `pub` signature changed. `MAX_LINE_BYTES` is `pub(crate)`. Behaviour that the I-2 note in interfaces.yaml could list (supreme): `ask::answer` returning InvalidData for an incomplete card, the class floor, and the 8 MiB / -32600 line cap. harness `cli.rs:312` passes the new error through with its message.
- Spec ids in the commits (H-6, H-1.3, H-1.4, I-2, D-119, D-123, D-127) match the code. The D-119 (2) amendment (a failed run lookup refuses the call in dispatch) matches mcp.rs:1074-1080 and is tested by `d119_2_dispatch_failed_lookup_never_proceeds`. No behaviour was found outside D-123 or D-127.
- Hard rules:
  - Rule 1: no keys and no model calls.
  - Rule 2: every stop line and the trailing stop are scrubbed (`stop_event` scrubs again); the canary had 0 hits, including step1 b2.
  - Rule 3: a Challenge is never shown as a card (mcp_t07_r4.rs:289 asserts that no card was shown).
  - Rule 4: the class floor; mid-call cards with no site are checked against the call's domain; every failed close is TimeoutSkip.
  - Rule 5: text only. An over-long line is dropped, never echoed back.
  - Rule 6: no new sleeps.
  - Rule 7: not applicable.
- Quality: no `unwrap` on external input in the library diff. Lock poisoning is handled with `into_inner`. Clippy and fmt are clean.

## Verdict

Approve. Every round-2 Should fix and Note, and the three notes carried since round 1, are fixed in code. Each has a test that would fail if it broke, except the fallback paths in Should fix 1 and the mid-call gate-stop clean line in Should fix 3. Step-1 acceptance is green. The polling-test timeout (Should fix 2) is a new, rarer flake that should be run down before CI depends on that test.

Verdict: approve
