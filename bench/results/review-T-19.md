# Review: T-19 combined (B-5, B-7, I-10, I-11, D-435..D-441)

reviewer-11, 2026-10-25, round 6. Pinned to 583eae0 on swarm/page (the D-441 code is 7d2170f;
D-441 amends D-440). This file is the one the merge gate reads. Round 5 (reviewer-10) is at
`git show 1b0b5f9:bench/results/review-T-19.md`, round 4 at `git show 71aa8c0:bench/results/review-T-19.md`.

Only question this round: can a password value still reach index(), state() or text() at
583eae0 (hard rule 5)? Round 5's single Blocking item was the D-440 regression (V7, Chromium d440).

Method:
- Every run used a `git archive 583eae0 injected bench/pages` snapshot in runs/reviewer-11/snap.
  The pre-D-440 snapshot runs/reviewer-10/pre440 (1fe3caf^) is the baseline column.
- Re-run: runs/reviewer-11/probe-r10-jsdom.js (all cases), r9-probe-jsdom2.js, probe-r10-chromium.js
  (all 4 modes), r9-probe-chromium.js, re-pointed at the snapshot. The jsdom ones ran with
  readyState "loading" (no tick) and after a tick ("complete"; POSTLOAD=1), each on the real
  clock and on a frozen clock (FROZEN=1).
- New: runs/reviewer-11/probe-r11-jsdom.js (N1..N5) and probe-r11-chromium.js (M1..M4). M1..M4 use
  their own server, where a parser-blocking `<script src=/slow.js>` keeps the page "loading"
  and a slow `<img>` keeps it "interactive".
- reads/getter is the number of value getter calls on canary fields (Chromium: counted inside
  world sb). LEAK means the canary is in index(), text() or state().

Gates at 583eae0:
- `node --test injected/test/` (snapshot): 95 tests, 95 pass, 0 fail.
- Mutant: reverting the inParent filter to D-440 (`!g.e.swap`) fails jsdom.test.js:1253 and
  :1342, so the D-441 tests catch the round-5 regression.
- `git diff 5fed11f 583eae0 -- crates/` is empty, so the sb-act approval stands.

## Probe results

| # | Case | 583eae0 loading | 583eae0 after load | 1fe3caf^ |
|---|---|---|---|---|
| r9 A, A2, B, B3, F | add-then-remove (microtask, batch, task), Vue out-in, S.gone cap | ok, reads 0 | ok, reads 0 | ok |
| r9 Chromium fade / wrapcb / modal | | ok / ok / ok | | ok |
| C shared | Chromium fade, shared keys | mid ok, after ok, getter 0 | | ok |
| C neutral / C wrap | Chromium, neutral copy, read while both exist | mid LEAK getter 3, after ok | | same |
| C d440 | round 5 Blocking, Chromium, page complete | mid LEAK getter 3, **after ok getter 0** | | after ok |
| V1..V4b, V6, V7k, V7d, V11..V13 | round 5 variants | ok, reads 0 | ok, reads 0 | ok |
| V3m | neutral, read during the overlap | mid LEAK, after ok | same | same |
| **V7** | **round 5 Blocking, jsdom** | **LEAK, reads 4** | ok, reads 0 | ok |
| V5 | 3 batches, neutral, cross-parent | LEAK, reads 3 | LEAK, reads 3 | LEAK |
| V8 | same parent, two adds, then removal, neutral | LEAK, reads 4 | LEAK, reads 4 | LEAK |
| V9 | S.born cap, key-matched copy, 70 fields | LEAK (frozen clock too) | LEAK | LEAK |
| **M1** | **Chromium d441 shape, page held "loading" by a sync script** | **after LEAK getter 3; still LEAK after load, getter 3** | | ok, getter 0 |
| M2 | same, page "interactive" (slow img) | ok, getter 0 (interactive) | ok | ok |
| M3 | same, page complete | | ok, getter 0 | ok |
| **M4** | **Chromium: copy added while loading, swap 0.45 s later after load** | **LEAK, getter 3** | | ok, getter 0 |
| **N1** | **jsdom: copy added while loading, swap after load** | **LEAK, reads 4** | | ok |
| N1r | same, plain removal (no swap) | ok, reads 0 | | ok |
| N2 | after load: copy, 70 fields elsewhere, swap (frozen clock) | | LEAK, reads 74 | LEAK (real clock) |
| N2b | same with 60 fields (frozen clock) | | ok | not run frozen |
| N3 | parser neighbour (ld), then post-load copy, swap | ok, reads 0 | | LEAK (fixed by D-440, kept by D-441) |
| N3r | same, plain removal | LEAK, reads 4 | | LEAK |
| N4 / N4s | SPA mount (user + password one parent), copy, remove / swap | | ok / ok | ok |
| N5 | after load, one batch: copy and another field added, password removed | | LEAK, reads 4 | LEAK |
| jsdom interactive | copy added at readyState "interactive", swap | | ok | |

Round 5's repro is closed once the page is no longer "loading" (V7 after load, C d440, M2, M3).
jsdom.test.js:1342 and browser.test.js:590 pin that. 7d2170f's message says it closes V7 and
d440, which is true only for copies added after loading. The same shape still leaks when
the copy is added while the document is loading (M1, M4, N1), and 1fe3caf^ read 0 in all three.

## Blocking

- injected.js:2012 with :2090 (D-441, hard rule 5; what is left of the D-440 regression).
  - What: `ld` records `doc.readyState === "loading"` when the observer batch runs. D-441 treats
    that as "a parser insert". It is not: classic scripts, event handlers and user input all run
    while the parser waits on a sync `<script src>` (common with third-party tags at the end of
    body) or on a streamed body. A show toggle clicked then is recorded as `ld: true`. If the
    batch that removes the password also swaps in one other value field, the copy that holds
    the value is skipped and reads.
  - Repro, Chromium (runs/reviewer-11/probe-r11-chromium.js):
    - M1: the d441.html shape on a page that is still "loading". After the removal, the canary
      is in index() and state() with 3 getter calls, and stays there after load.
    - M4: the copy is added while loading and the swap comes 0.45 s later, after load. Same leak.
    - Both read 0 at 1fe3caf^.
  - Repro, jsdom: N1 and V7 run without a tick, 4 reads each.
  - D-441's text allows this, and its why ("B-415's over-mark came only from parser inserts")
    rests on the same wrong premise. Like round 5, this needs supreme to amend D-441, or to
    accept the leak in writing (then it becomes a Should fix).
  - Proposed fix, tried in a scratch copy (runs/reviewer-11/fix2), 5 lines:
    1. The capture `input` listener records `EDIT_AT.set(field, now())` (a time, never a value).
    2. The lost entry carries `edit: EDIT_AT.get(f)`.
    3. `prior` carries the born time `t`.
    4. The skip becomes `!(g.e.swap && x.ld && (g.edit === undefined || x.t < g.edit))`. A
       neighbour added before the user last edited the password is still skipped (B-415's
       #nick). A neighbour added after an edit (a copy) is not skipped.
  - Result: `node --test injected/test/` passes 95/95 on fix2, so A10, A14, A18 and
    jsdom.test.js:1253 stay green. M1 and M4 then read 0.
  - Left open after the fix: N1 in jsdom still reads, because there the value is set by script
    with no input event.
  - A first try that keys on fill evidence at add time (runs/reviewer-11/fix) also closes M1
    and M4. It fails jsdom.test.js:1253, because that fixture fills the password before #nick
    is added, and it reopens N3. Not recommended.
  - Add M1 and M4 as browser tests (a parser-blocking script route in the test server) that
    count getter calls.

## Should fix

My judgment on the leaks that already existed before D-440: none is Blocking. Each one needs a
neutral copy (no shared id, name, autocomplete token, aria-label, placeholder or label text)
plus an unusual batch shape inside the 2 s window. None is a regression, and each is outside
D-438's wording. They are gaps in the matching heuristic, not a bypassed rule, and none was
reproduced with a real framework's toggle. They stay open as residual risk under hard rule 5
and should get a follow-up task.

- V8 / N3r (same parent, two remembered fields, neutral).
  - What: a second field added to the parent within 2 s means there is no single field.
  - Why it matters: N3r makes this plausible. The password sits next to a username the parser
    inserted, and the toggle comes within 2 s of load.
  - Fix: the EDIT_AT time above. When more than one entry is in the parent, keep only those born
    after `g.edit`.
- Overlap window, neutral identity (carried from round 4).
  - V3m, C neutral, C wrap and C d440 all read the copy while both fields exist (mid getter 3,
    canary in index()). settle() usually waits the transition out.
  - Fix: while an unwalked field added within 2 s shares a parent with a connected sensitive
    field, report `filled` without reading it.
- N5 (one batch adds the copy and another value field, removes the password, neutral).
  - D-437 needs exactly one add, and no key matches, so both fields read.
  - Supreme should rule whether "one sensitive out, 2..k value fields in, no key match" marks
    them all.
- V5 (3 batches: empty wrapper, then input into it, then removal; neutral). This is
  cross-parent, so the same-parent rule never sees it.
- Caps are untested (carried).
  - S.born holds 64. V9 (shared keys) and N2 (neutral, same parent) leak with 70 value fields
    added inside the window. N2b with 60 is ok on a frozen clock.
  - Without a frozen clock, jsdom is slow enough that 35 to 60 fields cross the 2 s window, so
    a cap test must use fakeClock.
  - Fix: evict entries whose parent holds no sensitive field first. S.gone (32) and NEAR_MAX
    (64) are also untested.
- Tests: the D-441 tests (jsdom.test.js:1342, browser.test.js:590) cover only a page past
  "loading". Add M1 and M4 with the Blocking fix.
- Resolved this round: round 5's V7 and C d440 on a loaded page (D-441, with tests and the
  killed mutant above). N3 (a parser neighbour plus a post-load copy under a swap) also leaked
  at 1fe3caf^ and is now ok.

## Notes

- Conformance at 583eae0:
  - 7d2170f implements D-441 as written (T-19, B-5, D-440, D-441).
  - No sleeps or timers were added (hard rule 6). `ld` is a boolean, never a value.
  - d441.html is a test fixture, not a user draft.
  - Rules 1, 2, 3 and 7 do not apply. I-11 shapes are unchanged.
- jsdom's readyState moves to "interactive" and then "complete" within the microtasks after
  construction (k=1 field already sees "interactive"). A probe that needs "loading" must mutate
  synchronously after the base walk.
- Over-marking (efficiency only, open):
  - OTP and SSN name matches.
  - D-437 and D-439 one-field swaps.
  - r9 I (a label wrapping two fields hands its text key to an unrelated new field).
  - The Blocking fix adds no new over-mark beyond D-441's.
- sb-act, round 5 (reviewer-7, 361b28d, D-409): approve, unchanged since. Open nits: the "New
  password" field without autocomplete, and the 250 ms wall-clock bound in `accept_t19_round3.rs`.

## Resolved in earlier rounds

- Round 5 Blocking (1fe3caf, D-440): V7 and C d440 on a loaded page. Fixed by D-441 (7d2170f),
  pinned by jsdom.test.js:1253 and :1342 and by browser.test.js:590. Full text:
  `git show 1b0b5f9:bench/results/review-T-19.md`.
- Round 4 Blocking (343a533, D-435 (2)): add-first rebuild split across batches, shared keys.
  Fixed by D-438 (1); r9 A, A2, B and fade.html all read 0 times. See `git show 71aa8c0:bench/results/review-T-19.md`.
- D-437 swap counting every input type (D-438 (2)), label[for] removed as a sibling (D-438 (3)),
  remove-side fieldKeys failing open, D-437 "lost exactly one" unpinned, passiveFrame
  `size=invisible` in a hash (D-438 (4)): all fixed with tests (details in 1b0b5f9).
- injected.js R1 B1 (sticky S.sens, D-405 (1)), R1 B2 (B-412 / D-73). sb-act R2 B1..B3 (D-406),
  R4 B1 (D-409 (1)).

## Verdict

D-441 closes round 5's leak only for copies added after the page leaves "loading". With a
parser-blocking script at the end of the page, the committed d441.html shape still puts the
password in index() and state() in Chromium (M1, M4: 3 getter calls). It did not at 1fe3caf^,
so the D-440 regression is narrowed but not closed. The fix needs a supreme amendment to D-441
(EDIT_AT proposal above; 95/95 in a scratch copy), or an explicit written acceptance of the
trade-off, which would move this item to Should fix.

Verdict: request changes
