# Review: T-19 combined (B-5, B-7, I-10, I-11, D-435..D-442)

reviewer-12, 2026-10-26, round 7. Pinned to 7cb7020 on swarm/page. The D-442 code is 295253e,
and D-442 supersedes D-440 and D-441. This is the file the merge gate reads. Earlier rounds:
round 6 (reviewer-11) `git show 0cc160e:bench/results/review-T-19.md`, round 5
`git show 1b0b5f9:bench/results/review-T-19.md`, round 4 `git show 71aa8c0:bench/results/review-T-19.md`.

Question this round: can a password value reach index(), state() or text() at 7cb7020
(hard rule 5)?

Method:
- All runs used a `git archive 7cb7020 injected bench/pages` snapshot in runs/reviewer-12/snap,
  with NODE_PATH pointing at the worktree's injected/test/node_modules.
- Re-run on the snapshot (copies in runs/reviewer-12): probe-r11-jsdom.js (real and frozen
  clock), probe-r11-chromium.js, probe-r10-jsdom.js (loading and post-load, each real and
  frozen) and probe-r10-chromium.js (4 modes).
- New:
  - runs/reviewer-12/probe-r12-jsdom.js (X1..X6) and probe-r12b-jsdom.js (X7), run in all 4 modes.
  - runs/reviewer-12/probe-r12-chromium.js (O1..O4): click a show toggle, `await __sb.settle()`
    with its defaults, then walk and read. "same" reads in the evaluate that awaited settle;
    "next" reads in a second evaluate.
- reads/getter is the number of value getter calls on canary fields. LEAK means the canary
  appears in index(), text() or state().

Gates at 7cb7020:
- `node --test injected/test/` (snapshot): 98 tests, 98 pass, 0 fail, 146 s.
- ACCEPT A18 diagnostic: the swap ran at 234 ms and 219 ms after navigation and read `filled`.
  The late runs (2210 ms and 2204 ms) read `ada`, so A18's over-mark branch really runs.
- Mutants on jsdom.test.js:
  - The 583eae0 (D-441) injected.js fails :1249 and :1411.
  - `all = true` fails :1249 (the "a login parent that lost two fields" control).
  - `all = false` fails :1249 and :1411.
- The 583eae0 injected.js also fails both new D-442 Chromium tests (browser.test.js:648, :664)
  with "a value getter was called".
- `git diff 583eae0 7cb7020 -- crates/ agent-memory/interfaces.yaml` is empty, so the sb-act
  approval stands.

## Probe results (7cb7020)

| # | Case | loading | after load | frozen clock |
|---|---|---|---|---|
| **M1** | round 6 Blocking. Chromium, d441 shape, page held "loading" by a sync script | ok, getter 0; after load ok, getter 0 | | |
| M2 / M3 | same, page interactive / complete | | ok, getter 0 / ok, getter 0 | |
| **M4** | round 6 Blocking. Chromium, copy added while loading, swap after load (433 ms) | ok, getter 0 | | |
| **N1** | jsdom, copy added while loading, swap after load | ok, reads 0 | | ok |
| N1r, N2c, N3, N3r, N4, N4s | | ok, reads 0 | | ok |
| N2 | after load, copy, 70 fields elsewhere, swap | | LEAK, reads 74 | LEAK, reads 74 |
| N2b | same with 60 fields | | LEAK, reads 64 (real clock crosses 2 s) | ok |
| N5 | one batch: copy and another field added, password removed | | LEAK, reads 4 | LEAK |
| V1..V4b, V6, V7, V7k, V7d, V8, V8k, V11..V13 | round 5 variants | ok, reads 0 | ok, reads 0 | ok |
| V3m | neutral copy, read while both fields exist | after the removal ok; mid LEAK (reads 1) | same | same |
| V5 | 3 batches, cross-parent, neutral | LEAK, reads 3 | LEAK, reads 3 | LEAK |
| V9 | S.born cap, key-matched copy in another parent, 70 fields | LEAK, reads 73 | LEAK, reads 73 | LEAK |
| C shared | Chromium fade, shared keys | mid ok, after ok, getter 0 | | |
| C neutral / wrap / d440 | Chromium, read while both fields exist | mid LEAK getter 3 each; after ok, getter 0 | | |
| X1 | new. Copy added to a sibling div (other parent), neutral, password removed | LEAK, reads 3 | LEAK | LEAK |
| X1b | new. Copy in the parent plus a mirror in another parent | the mirror leaks, reads 4 | LEAK | LEAK |
| X2w | new. Wrapper [display:none text input, password] rebuilt add-first, plus a re-added #nick | LEAK, reads 4 | LEAK | LEAK |
| X2w0 | new. Same, without #nick | LEAK, reads 4 | LEAK | LEAK |
| X2h | new. Same as X2w, but the extra input is `type=hidden` (not a value field, e.out = 1) | ok, reads 0 (LEAK, reads 4 at 583eae0) | ok | ok |
| X5 | new. Fragment [trick, password]: add [trick2, copy], then remove both | LEAK, reads 3 | LEAK | LEAK |
| X5s / X6 | new. Single remembered field / copy moved in from another parent | ok / ok | ok / ok | ok |
| X7 / X7f | new. Password plus confirm, one toggle, neutral copies (wrapper / same parent) | LEAK, reads 10 / LEAK, reads 10 | LEAK | LEAK |
| **O1** | new. Chromium, CSS fade on the old field, removed on transitionend, read after settle() | same: settled true, LEAK getter 3; next: ok | | |
| **O2** | new. Chromium, JS-timed exit (setTimeout 400 ms, no animation), read after settle() | same and next: settled true, LEAK getter 3 | | |
| **O3** | new. Chromium, exit class on the wrapper, transition on the input inside it | same and next: settled true, LEAK getter 3 | | |
| O4 | new. Chromium, 3.5 s fade (longer than the settle cap) | settled false, LEAK getter 3 | | |

D-442 closes round 6's Blocking item. M1, M4 and N1 read 0, and browser.test.js:648 and :664
pin it with a held parser-blocking script (no fixed delay). It also closes V8, N3 and N3r,
which had leaked since before D-440.

What is still open:
- The pre-existing shapes that D-442 sends to T-19b (N2, N5, V5, V9, the overlap read).
- New shapes: X1, X1b, X2w, X2w0, X5, X7.
- None of these is a regression: each also leaked at 583eae0 (X and O probes re-run on the
  583eae0 injected.js in runs/reviewer-12/muta). X2h leaked at 583eae0 and D-442 (2) fixes it.

## Blocking

- injected.js:894 and :899 with :2156 (overlap read, hard rule 5).
  - What: a neutral add-first copy is read while the password is still in the page, and this
    happens after `settle()` resolves with `settled: true`.
  - Why Blocking now: rounds 4 to 6 ruled this Should fix on the premise that "settle() usually
    waits the transition out", and D-442 deferred it to T-19b on that record. O1..O3 show the
    premise is false:
    - O2: a JS-timed exit (react-transition-group `timeout`, Vue `duration`) has no running
      animation. Settle is quiet after 150 ms, and the old field is removed at 400 ms.
    - O3: when the exit class is on a wrapper, `runningAnims` counts only animations on that
      element (`c.self`, :2156), not the transition on the input inside it.
    - O1: Animation.finished resolves before transitionend is dispatched, so a caller that reads
      in the same evaluate (settle then walk) reads during the overlap.
    - docs/05-BROWSER.md #9 has the consequence capture diff the page "≤ 500 ms or until
      settle" after every act step. So the normal path (click Show, settle, diff) walks during
      the transition.
    - Every add-first rebuild has an overlap. That makes this the most reachable leak left,
      more reachable than round 5's V7, which was Blocking and needed the same neutral copy
      plus a second field.
  - Proposed fix, prototyped in runs/reviewer-12/fix (about 15 lines):
    - The S.born entry records `ns`: the add-time `nearOf(p)` found a sensitive field in that
      parent that this batch did not add.
    - `fieldValue` returns `filledNoRead` for a field that has a live `ns` entry while its
      parent still holds a field in S.sens.
    - Result: O1, O2 and O3, C neutral/wrap/d440 mid and V3m mid all read 0.
  - Cost of the fix: `node --test` goes to 93/98. Five controls see an unrelated field that was
    added next to a live password within 2 s read `empty` instead of its text for that window.
    The five are jsdom.test.js:951, :1082 and :1249 (the login-parent control), A14 and A16.
    This is the same trade D-442 made for B-415, but it changes the D-435 (1) and D-438 controls.
  - Supreme has two options:
    - Take the fix in an amendment (D-443), and turn O1..O3 into browser tests.
    - Re-accept this leak in writing, citing O1..O3 (as D-442 did for the others). This item
      then becomes Should fix.
  - O4 (an exit longer than the 2 s window or the 3 s cap) stays open under either option.
    Settle returns `settled: false` there, which the driver can act on.

## Should fix

All of these are pre-existing (they also leaked at 583eae0). Each needs a neutral copy plus a
batch or tree shape that framework toggles rarely produce. D-442 accepts N2, N5, V5 and V9 in
writing. Each one-line ruling says how reachable the shape is.

- N5 (one batch adds the copy and a second value field and removes the password).
  - Should fix: a keyed re-render that turns one input into two in one commit is unusual for a
    show toggle.
  - Fix: when one sensitive field out and 2..k in with no key match, mark all k (fail closed,
    the same trade as D-442).
- S.born cap (N2, V9: 70 value fields; N2b is ok at 60 on a frozen clock).
  - Should fix: it needs more than 64 value fields added inside the copy's 2 s window. A
    spreadsheet or grid mount could do that by accident, but a login page will not.
  - Fix: when the cap is hit, evict entries whose parent has no sensitive field first. Add a
    fakeClock test. S.gone (32) and NEAR_MAX (64) are still untested.
- V5, X1, X1b (cross-parent neutral copy: a new wrapper filled in a later batch, a copy in a
  sibling container, a mirror field).
  - Should fix: there is no structural or key link left to follow. Closing these needs either
    the overlap fix above (it covers the window while the password exists only for the same
    parent) or a "page shows the value elsewhere" rule.
- X7 / X7f (password plus confirm rebuilt by one toggle, neutral copies; 10 reads).
  - Should fix: a sign-up "show passwords" toggle is common, but a rebuild that drops every key
    is not.
  - Cheap fix at :2011: `all = g.e.out === g.e.sens`. Every value field the parent lost was
    sensitive, so this cannot be an SPA login swap, where the username is not sensitive.
- X2w, X2w0, X5 (the lost set holds the password plus a CSS-hidden text input, such as an
  autofill-trick username).
  - Should fix: e.out = 2 looks like an SPA login swap, so with two remembered fields neither
    is marked.
  - The trick input is not sensitive, so `e.out === e.sens` does not help. This needs an
    "invisible value field does not count" refinement, or the overlap fix.
- Tests:
  - pastBornWindow (accept-t19.test.js:64) is a page-clock condition with a 15 s cap. It is
    honest: S.born has no event at the end of its window. Note that it adds about 2 s to A10,
    A14 and A18 each.
  - The O1..O3 shapes have no tests (they would come with the Blocking fix).

## Notes

- Test honesty (583eae0..7cb7020):
  - A18 now asserts the B-415 over-mark. Inside 2 s Nickname must be `filled`, and the run took
    the `at <= 2000` branch on this machine. After the window it must read `ada`.
  - A slow machine (at > 2000) accepts either reading and prints the diagnostic. That is a
    weak branch, but a correct one.
  - A10 and A14 only add `pastBornWindow` before their swaps. Their password assertions are
    unchanged, and a later swap means fewer fields in S.born, so the leak checks are not weaker.
  - jsdom: "same parent, two fields" moved from the aged-out block into the D-442 test, and its
    expectation flipped from text to masked, as D-442 (2) says. `sameMasked` maps only
    empty|filled to masked, so a text reading still fails. Canary-in-blob and reads == 0 stay
    on every case. No test was weakened to hide a leak.
- helpers.serve `hold()` waits on a promise the test releases, not on a timer. The tests
  release it in `finally`.
- Conformance:
  - 295253e implements D-442 (1) and (2) as written: the ld flag and the swap skip are gone,
    and `all = g.e.out === 1`.
  - No timers or sleeps were added (rule 6), and no value enters S.born (rule 2/5).
  - d442-loading.html is a fixture, not a user draft (rule 7).
  - Rules 1 and 3 do not apply. I-11 shapes are unchanged.
- The handoff file is named T-19-js-coder-8.yaml, but it says `from: js-coder-9`.
- Over-marking (efficiency only, open): OTP and SSN name matches, D-437 and D-439 one-field
  swaps, r9 I, and now D-442 (2). S.sens is sticky, so an over-marked parser field stays
  `filled` for the life of the page.
- sb-act, round 5 (reviewer-7, 361b28d, D-409): approve, unchanged since.

## Resolved in earlier rounds

- Round 6 Blocking (D-441 `ld` skip; M1, M4, N1): fixed by D-442 (295253e). Pinned by
  browser.test.js:648 and :664 and by jsdom.test.js:1411, and both tests fail on the D-441 code.
  Full text: `git show 0cc160e:bench/results/review-T-19.md`.
- Round 5 Blocking (D-440; V7, C d440): closed by D-441 and kept by D-442 (V7 and d440 read 0).
  See `git show 1b0b5f9:bench/results/review-T-19.md`.
- V8, N3, N3r, X2h (two remembered fields in a one-field parent): fixed by D-442 (2).
- Round 4 Blocking (D-435 (2) split add-first, shared keys): fixed by D-438 (1). See
  `git show 71aa8c0:bench/results/review-T-19.md`.
- D-438 (2)..(4), R1 B1/B2, and sb-act R2 B1..B3 and R4 B1: fixed in earlier rounds.

## Verdict

D-442 is implemented as written and closes round 6's Blocking item (M1, M4, N1 read 0 and are
pinned by tests that fail on the old code). The tests are honest.

One Blocking item remains, on new evidence. The overlap read, previously excused because settle
would wait it out, happens after `settle()` returns `settled: true` (O1..O3). On the normal act,
settle and diff path, a neutral add-first show toggle puts the password in index(). A ~15-line
fail-closed fix exists (runs/reviewer-12/fix). It needs a supreme amendment because it
over-marks five controls for 2 s. Supreme can instead re-accept the leak in writing citing
O1..O3, which would make this Should fix.

Verdict: request changes
