# Review: T-19 combined (B-5, B-7, I-10, I-11, D-435, D-436, D-437)

reviewer-9, 2026-10-25. Branch swarm/page at b0ecf58. This file is the one the merge gate
reads. It covers both halves:

- injected.js: round 4 of 343a533. Details, probes and repros are in
  `bench/results/review-T-19-js.md`, "Round 4".
- sb-act: the round 5 verdict in `bench/results/review-T-19-sb-act.md`, plus 086d248
  (`crates/sb-act/tests/contract_paths.rs`).

The tester's uncommitted `injected/test/accept-t19.test.js` (A12, A13) and three new
`bench/pages/accept-*.html` files are not part of this review.

Gates:
- `node --test injected/test/`: 75/75.
- `cargo test -p sb-act`: all pass (lib 60, classify 16, forms 11, quote_fuzz 6, round2 18,
  round3 15, round4 8, round5 7, contract_paths 1, wire 3).
- `cargo clippy -p sb-act --all-targets -- -D warnings` is clean, and
  `cargo fmt -p sb-act --check` is clean.

## Blocking

- injected.js:1903-1949 (D-435 (2), hard rule 5): an add-first rebuild split across observer
  batches leaks the password, even when the new field shares name and aria-label with it.
  - Cause: inheritSensitive matches only fields added after a removal has been remembered.
  - Repro, Chromium: a show toggle inserts `<input type=text name=acct aria-label="Account key">`
    after the password, copies the value, and removes the old field on transitionend. This is
    the order of AnimatePresence sync mode, the Vue `<Transition>` default and Svelte outros.
    After the removal, index() and state() carry the canary
    (runs/reviewer-9/probe-chromium.js, fade.html).
  - jsdom: `o.after(n); await null; o.remove()` leaks (probe-jsdom2.js A), and so does the
    same with a later task (B).
  - Fix: remember recently added non-sensitive fields for the same 2000 ms window. On a
    sensitive removal, match them by key, and mark the matches sensitive and dirty. Test it in
    jsdom (microtask and later task) and in Chromium (transitionend), counting getter calls.

## Should fix

- injected.js:1828, 1932: the D-437 swap counts every input type. A wrapper that also holds a
  show-password checkbox or a hidden input is not a one-for-one swap, so a replacement with no
  shared key reads.
  - Chromium repro: wrapcb.html leaks.
  - Fix: count only value-bearing fields.
  - Also, fieldLabels (1861) misses a label[for] removed as a sibling node of the same record
    (jsdom D5).
- injected.js:1917-1921: a fieldKeys error on the removed sensitive field remembers nothing.
  That fails open, while the commit message says "fail closed". Push a match-all entry
  instead.
- Test gap: no committed test pins D-437's "lost exactly one field". Mutant
  `e.out >= 1 && e.sens >= 1` passes the jsdom and committed accept-t19 suites. Add a test
  where a two-field login is replaced by a one-field pre-filled page, and the new field reads
  its text.
- injected.js:339 (pre-existing, now also used by challengeContext): passiveFrame's
  `[?&]size=invisible` also matches inside a `#hash`. A visible hCaptcha challenge frame with
  `&size=invisible` in its hash gets no CHALLENGE row (probe-v3.js), which is the hard rule 3
  direction. Confirm the shape against a real capture, then limit the test to the query
  string of reCAPTCHA anchor frames.
- Add-first overlap window: while both fields exist, a walk reads the new one. settle()
  usually waits the transition out. Close it together with the Blocking fix by matching each
  added field against connected sensitive fields in the same parent.
- sb-act: none.

## Notes

- injected.js, D-436:
  - Passive v3 markers are dropped only as context for weak prompt text. Vendor widgets still
    get CHALLENGE rows through challengeBox and the captcha name/id rule.
  - A shown v2-invisible bframe is CHALLENGE, and a badge alone is not.
  - The `script` exclusion (403) cannot trigger, so its test cases cannot fail because of it.
    Harmless.
- injected.js, over-marking (efficiency only): an OTP `name=code` key catches a later "Coupon"
  `name=code` field, and D-437's accepted one-field swap cost shows up as expected.
- injected.js, the S.gone cap of 32 is untested, and a contrived batch of 40 sensitive-named
  hidden inputs can push the password out. Should item 1's filter fixes it.
- injected.js, S.gone holds keys only, never a value: textOf skips fields, and S.gone is never
  emitted.
- injected.js, conformance:
  - I-11 shapes are unchanged.
  - No timers or sleeps were added (hard rule 6).
  - Rules 1, 2 and 7 do not apply.
  - Spec ids: D-435 (1)-(3), D-436 and D-437 are implemented. The only additions beyond the
    spec are keys (labelledby, row name), AC_GENERIC and the cap, and they only raise
    protection.
- sb-act, round 5 (reviewer-7, 361b28d, D-409): approve, with no Blocking or Should items.
  - D-409 closes R4 Blocking 1 (Enter chords lowering Delete) and R4 Should 1 and 2, all with
    raise-only changes.
  - A diff of 361b28d^ against 361b28d over 12,864 steps found 1,206 raised and 0 lowered.
  - Open nits from earlier rounds: the "New password" field without autocomplete, and the
    250 ms wall-clock bound in the tester-owned `accept_t19_round3.rs`.
- sb-act, 086d248 `crates/sb-act/tests/contract_paths.rs`: it names every I-10 path at the
  `sb_act::` root, and a wrong path fails compilation (pr-ready section 4, D-74). The paths:
  - `parse`, `quote`, `classify`, `classify_step` and `check_planner`, each with a typed fn
    pointer;
  - `ActClass`, `Step`, `Scroll`, `ParseError` and `PlannerError`;
  - `PlannerError::ChallengeTarget { step }`, constructed and matched;
  - `MAX_WAIT`, asserted to be 30 s (D-431).

  The test passes, and clippy and fmt are clean.

## Resolved in earlier rounds

- injected.js R1 B1: a field that had been a password was read after a type swap. Fixed by the
  sticky S.sens and masked-text detection (D-405 (1)).
- injected.js R1 B2: id reuse before the first `walk({base})`. Fixed by B-412 / D-73 (`{why:
  "no base"}`).
- sb-act R2 B1 to B3: control-character keys, Enter dropping the form max, and the
  one-time-code exemption hiding CVC. Fixed by D-406.
- sb-act R4 B1: Enter chords lowering Delete. Fixed by D-409 (1).

## Verdict

The sb-act half is approved (round 5, plus a contract_paths test that is complete). The
injected.js half has one hard rule 5 leak inside D-435 (2): an add-first rebuild split across
observer batches.

Verdict: request changes
