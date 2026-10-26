# Review: T-20 (area driver), round 2

Reviewer-2, 2026-10-26, under D-89. Scope: `git diff 73b35fe..HEAD`. That is commit 84f8294
(code) and ec14c21 (handoff, D-606, D-607, B-607). I read `describe()` and `coverWhy()` in
injected.js to check the tidy_why neutralisation. I ran no cargo command (per the brief). The
coder's gate results are in agent-memory/handoffs/T-20-rust-coder-2.yaml.

## Blocking

- none

## Round 1 Blocking, closed

Round 1 Blocking 1 (strict 5 s cap) is closed:
- `ANSWER_GRACE` is gone.
- Every act call, wait and `Page.bringToFront` is capped at `t0 + cap`:
  - `crates/sb-driver/src/actionable.rs:412`, `:461` (`slot.min(deadline)`) and `:485-493`;
  - `call_sb` and `raw_call` send nothing once the deadline has passed
    (`crates/sb-driver/src/runtime.rs:202-205`, `:336-338`).
- A budget that runs out mid-attempt reports the last real answer with the measured time
  (`actionable.rs:341-345`). The text reads `not answering` only when the page never answered.
- The tolerance is now 40 ms (`chromium_t20.rs:23`, `accept_t20.rs:29`). The coder measured
  5.002 to 5.013 s.

Round 1 Blocking 2 (B-4 #6 retry wait) is closed:
- `CHANGE_FN` is gone. `wait_slot` (`actionable.rs:501-524`) awaits `__sb.settle` capped at
  `min(slot, deadline)`, then `sleep_until(slot)`.
- A CDP error during the wait now fails the step (`WaitEnd::Failed`). It is no longer an
  instant retry.
- D-606 records the change as a sub-orchestrator decision.
- The test asserts the executed schedule against literal spec numbers. The page mutates every
  10 ms, the bound per wait is -2/+80 ms, and 10 to 16 attempts must fit
  (`chromium_t20.rs:369-400`). The coder reports seeing both mutations go red.

## Should-fix

Filed as follow-ups. None of these blocks.

- **B-606's flag contradicts its text.** The round-2 note says me scheduling is deferred to
  "T-22 and the user", but `needs_user` is still `false` (agent-memory/blockers.yaml, B-606).
  Under `Profile::Me`:
  - `open` still makes a new OS window (`lib.rs:480`, `newWindow: true`), which can take the
    user's focus;
  - an occluded `me` window now fails every step at once with `TAB_HIDDEN_ME`.

  Set `needs_user: true`, or get a supreme decision before T-22.
- **The settle inside `wait_slot` has no effect on timing.** It is capped at the slot, and
  `sleep_until(slot)` follows it whatever it returns (`actionable.rs:506-522`). So the wait is
  exactly the slot on every page. The settle costs one CDP round trip and an in-page observer
  per retry, about 13 per capped step, and changes nothing. The rule-6 comment is accurate.
  Supreme should choose one of these in a D-606 amendment:
  - drop the settle and keep the slot timer;
  - let settle extend past the slot (to the budget) on a noisy page, so the spec's
    "`await settle()`" means something.
- **Waiting out a slot that no attempt follows.** When the next slot is at or after the
  deadline, `wait_slot` still waits until the deadline (`actionable.rs:459-461`). Then the
  step fails with the same why, so up to 500 ms is spent for nothing. Latency outranks the
  literal "after 5.0 s". Return `out_of_time` at once when `slot >= deadline`, or record why
  not.
- **D-607 can cost 3 s on a page that never paints.** The paint timing spec does not emit
  `first-paint` for a document that renders only the default background, for example an empty
  body before an SPA mounts. Such a page waits the full `PAINT_CAP_MS` in `open`
  (`runtime.rs:51-81`). D-607 admits this ("only a page that never paints pays the cap").
  Consider capping at about 1.5 s, or resolving on the first rAF after settle as well.
- **B-607 residual (area page).** An id that contains whitespace inside a shadow root is not
  reported in `odd`, so `covered by tag#x (#99)` still reads as ref 99. It is correctly routed
  to page. `describe()` should neutralise the id and class itself.
- **The round-1 Notes that were not addressed carry over:**
  - the dead `NoBase` path;
  - the unused `force` parameter in `based_ctx` (`runtime.rs:333`);
  - `open` accepts any URL scheme;
  - a challenge overlay is retried as a cover for 5 s instead of stopping;
  - the total wait in `open` can reach 3 x `OPEN_CAP`.

## Notes

- **Checks the brief asked for:**
  - **`sleep_until`:** there is one in the crate (`actionable.rs:522`), and it has a rule-6
    comment at `:519-521`. The crate doc (`lib.rs:2-3`) names it as the one timer.
  - **D-607 first-paint wait:** it is a condition with a cap. A `PerformanceObserver('paint')`
    or an existing paint entry ends it, and `setTimeout(end, min(paintCap, settle.cap))` caps
    it (`runtime.rs:56-73`). It is skipped for hidden and `about:` documents. The entry check
    and `observe` run in the same task, so no entry can be missed between them. If the
    observer throws, the wait ends at once.
  - **`open` drop guard:**
    - `CloseOnDrop` (`lib.rs:312-326`) is armed after `createTarget` (`:489`) and disarmed
      only on success (`:494`). An error from `?` or a dropped future closes the tab.
    - A mock test and a live test (`http://127.0.0.1:1/`) cover this.
    - Gap: a drop while `createTarget` is still in flight leaks the tab, because its id is not
      known yet. This is minor.
  - **tidy_why:** I found no bypass in the light DOM.
    - `split_ref` needs `" ("` and digits only. `describe()` gives the page whitespace only
      through `id`: parser tag names and the first class token cannot hold whitespace.
    - `neutral_desc` maps everything in the description except `[alnum-_]` and the one
      separator. So a forged ref inside D is always destroyed, and the trailing ref is the
      driver's own.
    - The only way to forge the trailing ref is an id that ends in `" (#n)"` with no real ref
      after it. `odd` covers that in the light DOM, from the isolated world, so the page
      cannot tamper with it. The shadow-DOM case is B-607.
    - Exception text now goes through the fallback filter (no `#`, `(` or `)`).
    - Unit and live tests: `/forged`, `/forged2`.
  - **`me` and `bringToFront`:** the only `Page.bringToFront` is in `bring_to_front`
    (`actionable.rs:481`). It is reached only after the `Profile::Me` check at `:434-436`.
    The mock test asserts nothing is sent, and the live test asserts the opener stays
    `hidden`.
- **Round-1 Should-fix status:**
  1. Bases: fixed. `give_back` happens only on `no runtime` (`runtime.rs:367`).
  2. Forged refs: fixed, with the B-607 residual above.
  3. Untested behaviour: fixed.
     - `a_context_lost_twice_is_not_retried_a_third_time` and
       `a_failed_open_is_retried_once_and_closes_its_half_open_tab` both assert no third call.
     - The live half-open close test.
  4. `me`/D-605: the code is fixed. B-606's flag remains (above).
  5. `< 50 ms`: fixed. Every sample is asserted (chromium_t20 (a), accept_t20 rows 1 and 1b),
     and row 1b is no longer `#[ignore]`. The coder saw 18 of 19 runs green. The one red run
     was before `PAINT_CAP` 3000, and its values were not captured. The tester should confirm
     stability under load.
- **Tolerance:** 40 ms is more than "a few ms". It is acceptable on Windows timer granularity,
  given the measured 2 to 13 ms.
- **Hard rules hold in this diff:**
  - no keys and no em dashes;
  - no new non-test `unwrap` or `expect`;
  - rule 2: whys and labels carry no values;
  - rule 3: `challenge` is still terminal;
  - rule 5: `ACT_FN` adds only id strings from the why.
- **Interfaces:** I-9 public signatures are unchanged. `pid()` returns 0 only for the in-crate
  mock (`pipe: None`).
- **Still owed:** a full workspace test run (the handoff's `not_done` 1).

## Verdict

Both round-1 Blocking items are closed, with tests that fail on the relevant mutations. Round
2 adds no new rule break. The remaining items are follow-ups.

Verdict: approve
