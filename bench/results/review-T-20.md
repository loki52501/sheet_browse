# Review: T-20 (area driver), round 1

Reviewer-1, 2026-10-26. Scope: `git diff 0d7bae6..HEAD`, which is commit 8a32525 (code) and
64405ae (handoff). Merged T-18 and page code is out of scope. injected.js `act()`, `settle()`,
`walk()`, `resolve()` and `describe()` were read to check how the Rust side uses them. No cargo
command was run (per the brief); the coder's gate results are in
agent-memory/handoffs/T-20-rust-coder-1.yaml.

## Blocking

- **The 5 s cap is not strict (B-4 #6, acceptance 2 "5 s cap respected").** Three places run
  past the deadline:
  - `crates/sb-driver/src/actionable.rs:227`: every attempt gets `deadline + ANSWER_GRACE`
    (300 ms).
  - `actionable.rs:325`: `wait_change` also gets `+ ANSWER_GRACE`, and its `wait` is capped only
    at the deadline.
  - `actionable.rs:311`: `bring_to_front` gets the same grace.

  The worst case is a busy page. The last wait ends at 5.3 s and the attempt after it at 5.6 s.
  The text still reads `after 5.0 s` (`actionable.rs:274-278` prints `cap`, not the elapsed
  time). The test makes this acceptable: `crates/sb-driver/src/chromium_t20.rs:23`
  (`CAP_TOLERANCE` 350 ms) and `:289` and `:304` assert `<= 5.35 s`. Required:
  - no in-page call or wait may end after `t0 + cap`;
  - when the budget runs out mid-attempt, the step reports the last real answer, not `not
    answering`;
  - the test asserts `<= ACT_CAP` plus a scheduling epsilon of a few ms, not 350 ms.
- **The retry wait is neither the spec's `await settle()` nor the schedule (B-4 #6 "on why,
  `await settle()`, retry at 0·50·100·250·500 ms then 500 ms steps").** `CHANGE_FN`
  (`actionable.rs:47-59`) resolves at the first DOM mutation. On any page that mutates
  continuously (a carousel, a clock, an ad slot, style-attribute animations) each wait ends at
  once. Retries then run back to back, about 20 per second. That is roughly 100 `act()` calls in
  5 s, each one running `scrollIntoViewIfNeeded` again and doing a CDP round trip. The schedule
  holds only on a page that does not move.

  D-603 records the change as a coder decision, but the spec text is not ambiguous, so it needed
  a blocker for supreme (CLAUDE.md). `wait_change` also turns every `DriverError::Cdp`
  (`actionable.rs:331`) into an immediate retry, which is another way to bypass the schedule.

  The fix is either of these, and the choice is supreme's:
  - wait with `__sb.settle({cap: step})`, so the wait never ends before a mutation-free quiet
    period or the step;
  - wait the full step as the condition's deadline.

  Either way, add a test that fails when the executed schedule is wrong. Today the mutation
  "`retry` never incremented" (every wait 0 ms) passes all of chromium_t20. A
  disabled-for-5-s case could assert attempts in roughly 10 to 20 (the schedule gives about
  14).

## Should-fix

- **Bases can repeat (D-604, D-55).** `crates/sb-driver/src/runtime.rs:317-321` hands back the
  base when the walk call comes back `Lost`, but the walk may already have run in the page
  (`S.based = true`, injected.js:1278-1281) before the context died. If that document is
  restored from bfcache, it keeps the base. The next document was given the same base, so ids
  from the two overlap in one tab frame, and an id the model holds from document Y resolves to
  a different element in restored document X. Only the `no runtime` give-back
  (`runtime.rs:329-331`) is provably safe. Do not give back on `Lost`. It costs one stride out
  of 42949.
- **Page text can forge element refs in a why.** `tidy_why` (`actionable.rs:156-173`) allows
  space, `(`, `)`, `#` and digits. `describe()` appends the raw `id` attribute
  (injected.js:1351-1354), so `<div id="x (OVERLAY #12)">` yields `covered by div#x (OVERLAY #12)
  (OVERLAY #40)`, which points the model at element 12. This is pr-ready section 5's first
  defect. In the Rust part of the why, replace `(`, `)` and `#` that the driver did not write,
  or have area page sanitise `describe()`. `exception_text` (`runtime.rs:155-163`) puts page
  exception text into `DriverError` with no character filter. That one is lower risk because of
  the isolated world.
- **Untested behaviour that the brief asks for:**
  - "Context-destroyed retry at most once." There is no test where the context is lost twice.
    Mutating `if !retried` to always retry stays green in `call_sb` (`runtime.rs:364`) and
    `open` (`lib.rs:301`). Add a mock test: `Lost` twice returns `Lost` and sends no third call.
  - "`open` closes the half-open tab." There is no test. `open` of an unreachable URL (for
    example `http://127.0.0.1:1/`) should return `Err` and leave `tabs()` unchanged.
- **D-605 does not fit the `me` profile.** E-2 (docs/07:19) says scheduled mail runs use `me`
  headed in a hidden window. A minimized window, or on Windows one that other windows fully
  cover (native occlusion), reports `visibilityState hidden`. Two things follow:
  - `Page.bringToFront` (`actionable.rs:244-249`) activates the OS window, which steals focus
    from the user and un-hides the "hidden window";
  - if the window stays hidden, every step fails with `the tab is not visible after 5.0 s`.

  B-606 has `needs_user: false`, and its default covers only "tabs in one window". It should
  cover the case above:
  - mark B-606 needs_user, or get a supreme decision before T-22;
  - do not call `bringToFront` under `Profile::Me`;
  - consider `--disable-backgrounding-occluded-windows`, `--disable-renderer-backgrounding` and
    `--disable-background-timer-throttling`. These are scheduling flags, not UA or fingerprint
    changes, but check them against D-5.

  For `bench` (headless) a window per tab is fine.
- **The "< 50 ms" check is weakened.** `chromium_t20.rs:236-239` asserts the median of three
  first answers, while acceptance 2 says the covered answer comes back in under 50 ms. The
  reason given (a stall in a fresh window's compositor) is plausible. Record it as a decision or
  blocker so the weakening is visible, or warm the window and then assert every run.

## Notes

- **Hard rules hold in this diff:**
  - rule 6: no `sleep`. Every wait is a condition with a cap: `wait_registry`, the in-page
    `CHANGE_FN` and `settle` timers as condition deadlines, and `timeout_at`.
  - rule 2: `act_steps` never touches `ctx`, and `step_label` drops values (test
    `step_labels_never_carry_values`). `Why` and `StepResult` hold only labels and page whys.
  - rule 3: `challenge` is terminal. The program stops with `StopReason{class: Challenge}`,
    tested at `chromium_t20.rs:392-415`.
  - rule 5: the WALK, ACT and SETTLE functions return counts, points or whys only.
  - No keys and no em dashes.
- **Fail-closed:** an unknown why, or an answer that is neither `{x,y}` nor `{why}`, is
  terminal (`classify_why`, `parse_answer` and tests). Good.
- **Install ordering:** resume happens only in the reply hook of
  `Page.addScriptToEvaluateOnNewDocument` (`lib.rs:174-186`). The mock test
  `paused_target_runs_only_after_the_runtime_install_replied` asserts nothing is sent before
  the reply. Good. Resuming after an error reply fails open for the runtime but recovers
  through the `createIsolatedWorld` fallback. That is acceptable.
- **Base overflow** fails closed with a unit test (`runtime.rs:410-427`).
- **I-9 and I-11:** signatures are unchanged. `open` and `act` match I-9. The I-11 use is
  correct: `walk({base})` before the first call into each sb context, then `act(id)` as a
  promise. D-73's "walk with a base after every navigation" holds.
- **Dead code:**
  - The `NoBase` re-walk path in `actionable` cannot fire through `act()`, because `resolve()`
    answers `stale` when the document has no base (injected.js:286). D-603's text about `no
    base` is misleading.
  - The `force` parameter of `based_ctx` is always `false`.
- **`open`:**
  - Dropping the `open` future after `Target.createTarget` leaks the tab (no drop guard).
  - The total wait can reach 3 x `OPEN_CAP` (90 s).
  - Any scheme is accepted (`javascript:`, `file:`, `chrome:`). This is upstream gate
    territory, but worth a T-22 or T-23 check.
- **Challenge overlay:** a challenge overlay that covers the target answers `covered by ...`
  and is retried for 5 s. Nothing is solved, but stopping at once would be better (page area:
  `coverWhy` could check `inChallenge(hit)`). `w()` does not check for a challenge either.
- **Test placement:** putting `src/chromium_t20.rs` in `src/` is justified for now. Acceptance
  2 needs the first in-page answer time and `actionable_within`, `call_sb` and `Why`, which are
  all `pub(crate)`. The cost is that `cargo test --lib` launches Chromium. A `testing` feature
  that exposes them (as harness-core does) would let the test move to `tests/` later.
- **Owed:**
  - The handoff says the full workspace test run is still owed after 8a32525.
  - `accept_t18` row 7 flaked once under load (T-18c).

## Verdict

Two Blocking items:
- the 5 s cap overruns by design, and the test allows it;
- the B-4 #6 retry wait follows neither the spec nor the schedule, and nothing tests the
  executed schedule.

Verdict: changes
