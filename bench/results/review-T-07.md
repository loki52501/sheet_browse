# Review T-07 round 2: H-6 MCP wrapper and FileAsker (commit 8cdb1ed, checked at 15e09d3)

Scope: crates/core/src/mcp.rs, crates/core/src/ask.rs, crates/core/tests/mcp_t07.rs, crates/core/tests/accept_t07.rs (`git diff main...15e09d3`). Round 1 was 7cb8691.
Checked in a detached worktree at 15e09d3 with a random `SB_VAULT_CANARY`:
- `cargo test -p harness-core --test mcp_t07 --test accept_t07`: 33 + 10 pass, 1 ignored child.
- `cargo test -p harness-core --lib -- mcp:: ask::`: 11 pass.
- `cargo clippy -p harness-core --lib --tests -- -D warnings` and `cargo fmt -p harness-core --check` are clean.
- The canary never appears in the test output.
- A stress loop (40 runs of `-- file_asker` over both targets) failed in 15 of the 40 runs. See Should fix 1.

## Round 1 items

Blocking:
- FileAsker failed open: fixed. The card is published by `publish` (ask.rs:108-129); when that returns None, `ask` returns TimeoutSkip (ask.rs:265-267). Tested by mcp_t07.rs:1464, both directly and through dispatch, with `asks` made a file.
- Mid-call asks ignored gate rules: fixed. `ToolCtx::ask` runs the card through `Gate::check` with the call's hints and the run's strict, and stops on Stop (mcp.rs:324-352). Tested by mcp_t07.rs:961 (stop rule) and :992 (strict rule, hint rule and a control case). One gap is left: Should fix 2.
- Card cleaned before it was scrubbed: fixed. `clean_line` scrubs, cleans, then scrubs again (mcp.rs:509-519); `sanitise_card` does the same (mcp.rs:472-505); every `diag` and Transport message goes through them. Tested by mcp.rs:1219 (multi-line value, and truncation at the cap) and mcp.rs:1235.

Should fix:
- Each missing test from round 1 now exists:
  - mid-call Challenge: mcp_t07.rs:930
  - ledger failure on the ask path, and a gate or run lookup error: :1227, :1250
  - mid-call ledger failure: :1275
  - strict run: :1058
  - panic: :1290
  - out-of-order concurrent calls: :1334
  - FileAsker stderr canary, run in a child process: :1579
- A panic now writes an ok=false tool event (mcp.rs:989-991, :892-905).
- The Allow-path ledger gap is documented (mcp.rs:33-36).
- Only payload strings are scrubbed (mcp.rs:680-703; tests at mcp.rs:1325 and mcp_t07.rs:1421).
- A non-UTF-8 line gets -32700 and the server keeps reading (mcp.rs:623-626; test at mcp_t07.rs:1391).
- Every asker call is capped (mcp.rs:447-467; unit tests at :1298 and :1313; B-176 explains why there is no end-to-end test).
- `answer()` refuses an ask that is already closed (ask.rs:331-358; test at mcp_t07.rs:1506).
- A missing runs row reads as strict (mcp.rs:426-431 and :911-916; test at mcp_t07.rs:1081).
- File I/O runs through spawn_blocking.
- `core_pg::bootstrap` still accepts an `SB_RUN_ID` with no runs row. It is deferred as B-175 because it is outside this brief; the wrapper side now fails closed.

## Blocking

- none

## Should fix

1. ask.rs:166-189 (`write_card`). The card is claimed with `create_new` (:171-174) and its bytes are written only afterwards (:180-182). A reader can therefore see an empty or partial `<n>.json`. Confirmed: in 15 of 40 runs, at least one FileAsker test failed with "EOF while parsing a value". The failures hit accept_t07.rs:692 and :758 (a7_file_asker_yes, a7_file_asker_timeout, a7_file_asker_through_dispatch) and mcp_t07.rs:844 (file_asker_answered_yes/no, file_asker_times_out_per_card). The step-1 acceptance suite is red about a third of the time, and `harness ask` (T-08) can show an empty card. There is also a fail-open corner: if `write_all` fails, the empty `<n>.json` stays on disk, so `answer()` (ask.rs:334, which only checks `is_file`) accepts an answer that nobody reads. Fix: claim the card the same way as the answer. Write a temp file, `hard_link` it to `<n>.json`, and on AlreadyExists try the next n (reuse `claim`). Remove the temp file on failure. The tests then need no change.
2. mcp.rs:329 together with :221-228 and :884-889. The mid-call `Call.domain` is `card.site` and nothing else. If a handler builds the card with `site: None`, `Gate::check` sees no domain. Domain-scoped rules for the call's own site, such as `('x.com','Account','stop')` or a strict ask, are then never consulted; only `'*'` rules and the defaults apply. dispatch already has the domain as `classified.domain` (:923). Fix: store it in `CallInfo` and use it when `card.site` is None. Add a test with a mid-call card that has no site on a call classified to x.example with a stop rule.

## Notes

- If the ask cap fires (mcp.rs:459) or `close` hits a non-AlreadyExists error (ask.rs:143-148), FileAsker is dropped, or returns, without claiming `<n>.answer`. A later `harness ask` then gets Ok for an answer nobody reads. This only happens when a blocking-pool or filesystem step takes more than 5 s, so it is acceptable for now. Worth a line in the ask.rs module doc.
- The handoff gotcha "the gate treats empty recipients as all known" (T-07-rust-coder-2.yaml) states the opposite of what the code does. Gate::check treats an empty Send as Send(new) with `may_proceed = false` (gate.rs:430-457), which is why a mid-call Send card always skips. The behaviour is correct; only the wording is wrong.
- Carried over from round 1, still open and listed in the handoff:
  - A mid-call Challenge followed by a handler `stopped` writes two core stops.
  - `ToolRegistry::add` (mcp.rs:166) can lower a re-registered tool's class.
  - `read_until` (mcp.rs:617) has no line-length cap. stdin is Claude Code, so this is low risk.
- I-2 fidelity: no `pub` signature changed between 7cb8691 and 15e09d3 (mcp.rs, ask.rs). The new items are `pub(crate)` (`ASK_CAP_MARGIN`, `clean_line`). The behaviour changes match the I-2 note in interfaces.yaml (D-79 / B-177). I-3 is used as specified, and `clean_card` is the one sanitiser (D-70).
- Spec ids in the commit message (H-6, H-1.3, H-1.4, H-2, H-3, I-2, I-3, D-70, D-71, D-119) all match the code. Two behaviours are not in docs/01 but are recorded in D-119 (1) and (3): a mid-call card is still shown when the gate allows, and a TimeoutProceed on a Skip card is downgraded.
- Hard rules:
  - Rule 1: no keys and no model calls.
  - Rule 2: every outgoing string is scrubbed before it is cleaned, and scrubbed again after. Panic payloads are never shown (mcp.rs:1144).
  - Rule 3: a Challenge is never shown as a card.
  - Rule 4: gate checks are done before the call and before every mid-call ask; every error path fails closed.
  - Rule 5: text only.
  - Rule 6: the only sleep is the `sleep_until(deadline)` fallback when no file watcher is available, and it has a comment (ask.rs:299-301).
  - Rule 7: not applicable.
- docs/01 acceptance (dispatch half): accept_t07 a1 to a5 cover allow, timeout_proceed and skip on forced Pay, Challenge, and the vault grep. a6 covers serve_io and a7 covers FileAsker. The Postgres and real-process half belongs to T-09.

## Verdict

Approve. All three round-1 blocking items are fixed, each with a test that asserts the behaviour. Two items should still be fixed: the card publish race in Should fix 1, which makes the a7 acceptance tests fail about a third of the time and should be fixed before T-08's `harness ask` relies on the card file, and the domain fallback for mid-call cards with no site in Should fix 2.

Verdict: approve
