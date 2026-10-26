# Acceptance T-20, round 2 (tester)

Spec: docs/05-BROWSER.md acceptance 2, B-4 #3, #6, #11, decisions D-603 to D-607.
Tree: swarm/driver-T20 at ec14c21 (implementation as of 84f8294), plus the tester's test changes.
Browser: Playwright chromium-1248 (D-34 build), headless, real pages from a 127.0.0.1 server.
Test file: crates/sb-driver/tests/accept_t20.rs. It uses only the public API: `Driver::open`,
`Driver::act`, the registry and raw CDP. Round 2 adds rows 5b, 5c, 13 and 14, adds the pages
`/churn-covered`, `/forged` and `/forged2`, and prints per-attempt timings in rows 1 and 1b.
All the file's browser tests hold one lock, so they run one at a time.

Attempts are counted with a probe in the `sb` world. It wraps `Number.isInteger` (called once
per `__sb.act` entry) and `document.elementFromPoint` (called once per hit test). First-answer
times are upper bounds: the first hit test, mapped to the Rust clock, plus one round trip.

Base command (C): `cargo test -j 2 -p sb-driver --test accept_t20 <filter> -- --test-threads=1 --nocapture`.
The final full run was C with no filter, with `SB_VAULT_CANARY=sbcanary-t20r2-<unique>`: 12 passed, 0 failed, 0 ignored, 87.75 s.

| item | command | result | note |
|---|---|---|---|
| 1 Covered button: first answer is the cover, every sample under 50 ms | C `row1_row2` | pass | Final full run: `[19.9, 11.6, 8.3, 11.5, 11.6]` ms, first hit `cookie`. The test now asserts the max, not the median. Across 20 runs, 1 sample was over 50 ms (19 runs printed their samples, 95 in all; the workspace run prints none): 72.1 ms on act 2 of the first full run, which made that run red. It did not happen again in the 19 runs after it, including 6 runs that repeated the same test order (row14 then row1). Every other printed sample was 5.3 to 29.9 ms. In the page, attempt 1 starts 0.9 to 3.7 ms after act and takes 2 to 17 ms. |
| 1b First act on a fresh tab (no warm-up), every sample under 50 ms, no longer #[ignore] | C `row1b`: 5 runs alone, 6 runs with filter `row1`, 2 full runs and the workspace run | pass | 14 of 14 runs green. First acts: 4.7 to 18.3 ms. Second acts: 7.7 to 33.1 ms. The 100 ms `nextFrame` stall from round 1 is gone (D-607 waits for the first paint). The largest in-page attempt took 10.4 ms. |
| 2 Cover removed after 300 ms by a page script: the retry succeeds | C `row1_row2` | pass | All 5 runs end with `not done: input synthesis is T-21 (target actionable at 160.0, 120.0)` after 455 to 496 ms: 5 attempts, the last hit test hits `b`. |
| 3 Disabled button gives `disabled` | C `row3` | pass | `Failed { why: "disabled after 5.0 s" }` after 5.0004 s, 13 attempts, no hit test. |
| 4 The 5 s cap holds, strict (D-606: at most 5 s + 40 ms) | C `row4_row5` | pass | Took 5.0032 s. Why: `covered by div.cookie (OVERLAY #2) after 5.0 s`. Row 5b took 5.0025 s and row 5c took 5.0117 s. `CAP_TOLERANCE` is 40 ms. |
| 5 Backoff 0, 50, 100, 250, 500 ms, then 500 ms steps, seen in the page (quiet page) | C `row4_row5` | pass | Waits `[2, 62, 111, 265, 506, 516, 511, 508, 506, 516, 508, 508]` ms, 13 attempts. The last attempt started 4604 ms after act. |
| 5b Same schedule and cap on a page that mutates every 10 ms (settle never goes quiet) | C `row5b` | pass | Waits `[2, 57, 112, 254, 510, 509, 505, 508, 508, 508, 505, 503]` ms, 13 attempts, 5.0025 s. Asserts schedule - 2 <= wait <= schedule + 80 for every wait, the last included. Also asserts 10 to 16 attempts, and that no attempt starts after 5 s. |
| 5c Same strict check on the quiet page, every wait including the last | C `row5c` | pass | Waits `[2, 53, 114, 259, 508, 506, 515, 511, 505, 516, 506, 518]` ms, 13 attempts, 5.0117 s. Settle ends early on this page, so only the slot wait (`sleep_until`) holds the schedule. |
| 6 gone, stale and challenge fail at once with no retry; a challenge sets `ActResult.stopped` | C `row6` | pass | gone: 1 step `gone`, 1.9 ms, 1 attempt. stale: `stale`, 35.5 ms, 1 attempt. challenge: `challenge`, 1.6 ms, 1 attempt, `stopped = StopReason { class: Challenge, why: "CHALLENGE at #2: stopped, hand back to the user", element: Some("#2") }`. |
| 7 After a navigation the runtime is reinstalled and new ids never repeat (D-604) | C `row7` | pass | doc1 ids 1..7, doc2 100001..100007, doc3 (page-initiated navigation) 200001..200007. A doc3 id is actionable. A doc2 id answers `stale`. |
| 8 A context destroyed mid-call is retried exactly once (B-4 #11) | C `row8` | pass | One loss: `Ok`, settled true on /calm, in 0.53 s. Two losses: `Err(Cdp("w(): the page's context went away"))` in 0.79 s. |
| 9 Labels, step errors, stop reasons, parse errors, ledger, run folder, stdout and `runs/` carry no vault value and no `t#` text | `SB_VAULT_CANARY=$(cat canary.txt)` then C (full run) and the workspace run; then `grep -F "$SB_VAULT_CANARY"` over every captured stdout, `runs/`, `*.jsonl`, the working tree and `git grep` | pass | row9 printed `canary from SB_VAULT_CANARY: true`. The canary was planted as vault entry `canary`. Labels: `t#1`, `w(50ms)`, `t#99999`. Also checked: 3 parse errors, the MemStore ledger (clean) and the run folder (0 files). Canary hits: 0 in the 33 captured outputs of this round (full runs, row runs, mutation runs, workspace run). `runs/` does not exist. No `*.jsonl` files outside `target/`. `git grep`: 0. Working tree: 0. `SECRETCANARY`: 0 hits in output. |
| 10 No fixed sleeps in crates/sb-driver/src | `grep -rn -e sleep -e thread::park -e delay_for crates/sb-driver/src` | pass | Only `actionable.rs:522 tokio::time::sleep_until(until)`, which waits for the D-606 slot and carries the rule-6 comment ("the schedule itself is the condition"), and the doc comment at lib.rs:2. |
| 11 The round-2 tests can fail: each new or tightened row goes red under a mutation, and the file is restored | one edit in `crates/sb-driver/src/actionable.rs`, then C with the filter shown in Mutations, then `git checkout -- <file>` and `git status --short` | pass | 7 mutations, all red. Each time `git status --short` showed only ` M crates/sb-driver/tests/accept_t20.rs`. See Mutations. |
| 13 A page id that spells a driver ref (`x (OVERLAY #12)`, `x (#12)`) cannot forge an element ref | C `row13` | pass | `/forged`: `covered by div#x__OVERLAY__12_ (OVERLAY #2) after 5.0 s`. `/forged2`: `covered by div#x___12_ (#2) after 5.0 s`. The cover's real id is 2. The why contains no `#12` and no `(OVERLAY #12)`. |
| 14 A `w(d)` the page does not answer in time is a failed step that keeps earlier results, not `Err` | C `row14` | pass | `w(200ms); w(1s)`, with the page's main thread blocked for 2 s. Result: step 1 `w(200ms)` Ok, step 2 `w(1s)` `Failed { why: "the page did not answer within 1300 ms" }`, `stopped` None, in 1.55 s. |
| 12a Workspace run, once | `SB_VAULT_CANARY=... cargo test -j 2 --workspace --no-fail-fast` | pass | Exit 0. 88 test binaries: 941 passed, 0 failed, 5 ignored. None of the 5 ignored is T-20's: 3 are red-first T-08 tests and 2 are child-process helpers. accept_t20: 12 passed, 0 ignored. accept_t18: 1 passed. The `vault::tests::global_scrub_before_and_after_install` panic line is that test's own `catch_unwind`, and the test passed. Canary hits in the output: 0. |
| 12b Formatting and lints | `cargo fmt --all --check`; `cargo clippy -j 2 -p sb-driver --tests -- -D warnings` | pass | Both clean. |

## Mutations (row 11)

Output files are in the tester's scratchpad, named `mut_<name>.txt`.

- zero_waits: `backoff()` returned `Duration::from_millis(0)`. Filter `row4 row5`. All 3 red. row4: `starts.len() == hits.len()` failed at 229 vs 228. row5b: `wait 1: 2.0 ms < 50`, 310 attempts. row5c: 238 attempts.
- no_slot_wait: `tokio::time::sleep_until(until).await;` removed. Filter `row4 row5`. row4 red (32 attempts). row5c red: `wait 3: 167.4 ms < 250`, 31 attempts. row5b stayed green, as expected: on a page that never goes quiet, settle alone fills the slot.
- pad_cap_100 and pad_cap_50: `deadline = t0 + cap + 100 ms` and `+ 50 ms`. Filter `row3 row4 row5`. All 4 red: `disabled after 5.1 s`, and `covered by ... after 5.1 s` with 5.05 to 5.11 s.
- pad_cap_45: `+ 45 ms`. All 3 red. row5b failed only the timing bound: `cap: 5.0486735s` with the why still `after 5.0 s`. This shows the 40 ms tolerance is enforced on its own.
- row13_both: `neutral_desc` returns the description unchanged, and ACT_FN's `odd` list is turned off. row13 red: `covered by div#x (OVERLAY #12) (OVERLAY #2) after 5.0 s`.
- row13_odd_only: only ACT_FN's `odd` list turned off. row13 stayed green. On these pages `neutral_desc` alone neutralises the id, so the odd list is a second layer that row 13 does not separate. The coder's unit test `page_ids_cannot_forge_element_refs` covers the case where only the odd list catches it (`covered by div#x (#5)` with no driver ref).
- row14_err: `wait_step`'s timeout returned `Err(DriverError::Timeout("w()"))`. row14 red: `Err(Timeout("w()"))`.

The round-1 mutations M1 to M9 (rows 1 to 3 and 6 to 9) were not re-run against the round-2 code. Those rows' assertions are unchanged or stricter.

## Notes (non-blocking)

- Row 1 flake, 1 in 20 runs: one sample of 72.1 ms (act 2 of the first full run). It was not reproduced in the 19 runs after it. That run was before the per-attempt diagnostics were added, so where the 72 ms went is not known. Row 14 in that run blocked a renderer for 3 s. It now blocks for 2 s and waits for the page before shutdown, but the same order with the old row 14 did not reproduce the flake either (0 of 3).
- Row 13 does not exercise ACT_FN's `odd` list on its own (see the mutation `row13_odd_only`). Shadow-DOM ids are still open as B-607 (area page).

Verdict: green
