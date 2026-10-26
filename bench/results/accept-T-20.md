# Acceptance T-20, round 1 (tester)

Spec: docs/05-BROWSER.md acceptance 2, B-4 #3, #6, #11, decisions D-603, D-604, D-605.
Tree: swarm/driver-T20 at e846bd3 (implementation as of 8a32525; e846bd3 adds only the review).
Browser: Playwright chromium-1248 (D-34 build), headless, real pages from a 127.0.0.1 server.
New test file: crates/sb-driver/tests/accept_t20.rs (public API only: `Driver::open`, `Driver::act`,
registry and raw CDP). Its browser tests hold one lock, so they run one at a time even under
the default parallel runner.

How attempts are counted: a probe installed in the `sb` world (never the page's world) wraps
`Number.isInteger`, which `__sb.act` calls once on entry (`resolve`), and
`document.elementFromPoint`, which it calls once per hit test. `__sb` itself is frozen. First
answer times are upper bounds: the first hit test in page time, mapped to the Rust clock
through a `performance.now()` mark, plus one round trip.

Base command (C): `cargo test -j 2 -p sb-driver --test accept_t20 <filter> -- --test-threads=1 --nocapture`

| row | what | result | evidence |
|---|---|---|---|
| 1 | Covered button: the first answer is the cover, in under 50 ms | pass | C with `row1_row2`. Asserts the first hit test hits div.cookie and that the median of 5 first answers is under 50 ms. Baseline run: 11.7, 11.7, 11.4, 11.8, 11.8 ms. Row 4 checks the why text: `covered by div.cookie (OVERLAY #2) after 5.0 s`. The coder's in-crate `chromium_t20::acceptance_2_actionability_with_retry_and_runtime_install` also passed in the workspace run. Note: 6 of the 15 first acts on the browser's first tab were 104 to 154 ms. See row 1b. |
| 1b | Every first answer is under 50 ms, including the first act after launch (max, not median) | fail | C with `row1b -- --ignored`. 2 of 6 runs failed, on the first act of the first tab after launch: 108.9 ms and 120.9 ms. Every other sample was 5.6 to 18.2 ms. The page-side attempt durations (row 4 output: 104, 106, 119 ms for attempt 1) show `__sb.act` itself waiting out `nextFrame`'s 100 ms fallback, so rAF did not fire in the first window. Missing: the first act after launch should not wait on an animation frame the compositor has not produced yet. For example, `open` could await one rendered frame in its settle-and-walk round trip. The test is `#[ignore]`d so the workspace stays stable. |
| 2 | Cover removed after 300 ms by a page script: the retry succeeds | pass | C with `row1_row2`. In all 5 runs the step ends with `not done: input synthesis is T-21 (target actionable at 160.0, 120.0)` (T-20 success) after 309 to 336 ms. There were 4 or 5 attempts, and the last hit test hits the button. Asserts 250 ms <= took < 1000 ms. |
| 3 | Disabled button gives `disabled` | pass | C with `row3`: `Failed { why: "disabled after 5.0 s" }` after 5.0275 s with 14 attempts. Answered before the hit test, so `disabled` is retried as D-603 says. |
| 4 | The 5 s cap holds | pass | C with `row4`: act took 5.0123 s, 5.0229 s, 5.0164 s and 5.0133 s on 4 serial runs. Asserts 5 s <= took <= 5.35 s. The why is `covered by div.cookie (OVERLAY #2) after 5.0 s`. The last attempt started 5007 to 5015 ms after act, because the last wait is capped by what is left of the budget. |
| 5 | Backoff 0, 50, 100, 250, 500, then 500 ms steps, observed in the page | pass | C with `row4`. Measured waits between attempts (hit test to next entry, two CDP round trips included) were `[2, 55, 118, 256, 514, 505, 510, 512, 516, 510, 511, 511, 415]` ms, and on another run `[2, 53, 104, 258, 533, 512, 509, 517, 512, 508, 510, 505, 408]`. Each run had 14 attempts and the last wait was capped by the budget. Asserts schedule - 2 <= wait <= schedule + 80. Unit tests `backoff_follows_the_schedule_then_500_ms_steps` and `next_wait_is_capped_by_the_budget_and_ends_with_it` also pass. |
| 6 | gone, stale and challenge fail at once with no retry; a challenge sets `ActResult.stopped` | pass | C with `row6`. gone: `c#2; c#2` gives 1 step `gone` in 1.7 ms, 1 attempt. stale (after `Page.navigate`): `stale` in 6.0 ms, 1 attempt. challenge (title `Just a moment...`): 1 step `challenge` in 12.3 ms, 1 attempt, `stopped = StopReason { class: Challenge, why: "CHALLENGE at #2: stopped, hand back to the user", element: Some("#2") }`. |
| 7 | After a navigation the runtime is reinstalled and new ids never repeat old ones (D-604) | pass | C with `row7`. doc1 ids are 1..7. After `Page.navigate`, `__sb.act` exists in the new sb world and doc2 ids are 100001..100007. After a page-initiated `location.href` back to the same URL, doc3 ids are 200001..200007. Each set is disjoint and strictly above the one before. A doc3 id is actionable and a doc2 id answers `stale`. |
| 8 | A context destroyed mid-call is retried exactly once (B-4 #11) | pass | C with `row8`. `w(3s)` on a page that never settles and navigates once: the retry answers `Ok`, settled true on /calm, in 0.53 s. The same with two navigations in a row: `Err(Cdp("w(): the page's context went away"))` in 0.79 s, so the second loss is not retried even though a third document would have settled. |
| 9 | Step labels, step errors, stop reasons and parse errors carry no `@vault` value and no text after `=` in a `t#` step | pass | `SB_VAULT_CANARY=sbcanary-t20-<unique>` planted as vault entry `canary`, then C with `row9`. Programs: `t#1=@vault(canary); t#2=SECRETCANARY`, the same on the live input, behind `w(50ms)`, on a gone id, a stale id and a challenge page, plus 4 bad programs that fail to parse. Checked `{:?}` of ActResult, serde_json of steps and stopped, every label and why, and the DriverError Display and Debug. Labels are exactly `t#1`, `t#99999` and `w(50ms)`. MemStore ledger: clean. Run folder: 0 files. grep of the canary in every captured stdout (runs, mutation runs, workspace run), in repo `runs/` (absent) and `git grep`: 0 hits. `SECRETCANARY`: 0 hits in output. |
| 10 | No fixed sleeps in crates/sb-driver/src | pass | `grep -rn -e sleep -e thread::park -e delay_for crates/sb-driver/src` finds only two doc comments (actionable.rs:6, lib.rs:2). The only in-page timer is `CHANGE_FN`'s `setTimeout`, which caps a MutationObserver condition. |
| 11 | Tests can fail: each covered row goes red under a mutation, and the file is restored | pass | 12 mutations, each applied to one implementation file, run with C, restored with `git checkout -- <file>`, then `git status --short` showed only `?? crates/sb-driver/tests/accept_t20.rs`. All 12 were red. See the list below. |
| 12a | Workspace run, once | pass | `cargo test -j 2 --workspace --no-fail-fast`: 928 passed, 3 failed, 6 ignored. All 3 failures came from the tester and were re-run green. (1) core-pg `accept_t03_r3::r3_rule2_constraint_errors_carry_no_bound_canary` requires a canary containing `sbcanary`, and my first canary did not. Re-run with `SB_VAULT_CANARY=sbcanary-...`: 11 passed. (2, 3) accept_t20 `row4` (wait 1 was 133 ms) and `row9` (`w(50ms)` gave `Timeout("w()")`) failed with 7 browsers running in parallel in one binary. The file now serializes its browsers. `cargo test -j 2 -p sb-driver --test accept_t20` at default parallelism: 7 passed, 1 ignored. sb-driver lib passed 47 of 47, including chromium_t20. `cargo clippy -j 2 -p sb-driver --tests -- -D warnings` and `cargo fmt --check` are clean. |
| 12b | Flake check for T-18 `accept_t18.rs:142` (bench profile left after drop) | pass | `cargo test -j 2 -p sb-driver --test accept_t18` alone 5 times: 5 of 5 passed (2.3 to 12.9 s). It also passed in the workspace run. Not reproduced (0 of 6). The coder's single failure looks like a T-18 profile-cleanup race under load, not a T-20 defect. T-20 does not touch launch.rs or ProfileDir. |

## Mutations (row 11)

Each is one text replacement in an implementation file, then the named test, then
`git checkout -- <file>` and `git status --short`, which showed only the untracked test file.
Output is in the tester's scratchpad, mut_<name>.txt.

- M1 (row 1), actionable.rs `ACT_FN`: `return sb.act(id);` became `return new Promise((r) => setTimeout(() => r(sb.act(id)), 60));`. `row1_row2` red: median first answer `[67.6, 74.2, 76.4, 81.4, 172.2]` ms.
- M2 (row 2), actionable.rs `classify_why`: `starts_with("covered by ")` became `starts_with("covered by XX")`. `row1_row2` red: the step failed at once with `unknown answer from the page: covered by div.cookie (OVERLAY #2)` and no retry.
- M3 (row 3), actionable.rs: `| "disabled"` removed from the retryable list. `row3` red: `unknown answer from the page: disabled`.
- M4 (row 4), actionable.rs: `ACT_CAP` changed from 5 s to 6 s. `row4` red: took 6.019 s, `... after 6.0 s`.
- M5 (row 5), actionable.rs: `SCHEDULE_MS` changed to `[0, 50, 250, 100, 500]`. `row4` red: `wait 2: 254.2 ms > 100 + 80`.
- M6a (row 6 gone), actionable.rs: `"gone" => WhyKind::Retry`. `row6` red: `gone after 5.0 s`, 14 attempts.
- M6b (row 6 challenge), actionable.rs: `if w.kind == WhyKind::Challenge {` became `... && w.attempts == 0 {`. `row6` red: `stopped None`, panic `a challenge stops the program`.
- M6c (row 6 stale), actionable.rs: `"stale" => WhyKind::Retry`. `row6` red: `stale after 5.0 s`, 14 attempts.
- M7 (row 7), runtime.rs `Bases::next_base`: `f.docs += 1;` became `f.docs += 0;`. `row7` red: doc2 ids were 1..7 again, `doc2 ids start after doc1's`.
- M8a (row 8, no retry), runtime.rs `call_sb`: the guard became `if !retried && tab.0 == u32::MAX`. `row8` red: one loss gave `Err(Cdp("w(): the page's context went away"))`.
- M8b (row 8, unbounded retry), runtime.rs `call_sb`: `retried = true;` became `retried = tab.0 == u32::MAX;`. `row8` red: two losses gave `Ok`, panic `a second loss is not retried`.
- M9 (row 9), actionable.rs `step_label`: `Step::Type { target, .. } => format!("t{target}")` became `Step::Type { .. } => format!("{step:?}")`. `row9` red: `program 0: @vault text leaked`. The canary got 0 hits in that run's output.

## Notes (non-blocking)

- Row 1b is the only failing row. Steady-state first answers are about 11 ms. The miss is the first act after launch, where `nextFrame` falls back to its 100 ms timer.
- Under heavy load (7 browsers in one process), `w(50ms)` can miss its 50 ms + 300 ms call deadline. `Driver::act` then returns `Err(Timeout("w()"))` for the whole program instead of a failed step with the earlier steps' results. This is outside acceptance 2. It is filed in the handoff for T-21/T-22.

Verdict: red
