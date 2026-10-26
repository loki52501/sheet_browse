# Review: T-19 combined (B-5, B-7, I-10, I-11, D-435..D-440)

reviewer-10, 2026-10-25, round 5 (D-89 final round). Pinned to 5fed11f on swarm/page (the
D-440 code is 1fe3caf; D-438 and D-439 are 343a533..edd30e4). This file is the one the merge
gate reads. Round 4 (reviewer-9) is at `git show 71aa8c0:bench/results/review-T-19.md`, and its
probes are in runs/reviewer-9/.

Only question this round: is round 4's Blocking item closed? That item was D-435 (2), hard
rule 5: an add-first password rebuild split across observer batches leaks the value through
index(), state() or text().

Method:
- Every run used a `git archive 5fed11f injected bench/pages` snapshot in
  runs/reviewer-10/snap, never the live worktree.
- For comparison, the same probes ran on a 1fe3caf^ (pre-D-440) snapshot in
  runs/reviewer-10/pre440.
- Probes (not committed):
  - runs/reviewer-10/r9-probe-jsdom2.js and r9-probe-chromium.js are reviewer-9's probes,
    re-pointed at the snapshot.
  - runs/reviewer-10/probe-r10-jsdom.js holds new variants.
  - runs/reviewer-10/probe-r10-chromium.js holds Chromium variants. They count value getter
    calls inside world sb by redefining HTMLInputElement.prototype.value there.

Gates at 5fed11f:
- `node --test injected/test/` (snapshot): 93/93 pass, 0 fail.
- sb-act is unchanged since round 4 (`git diff 71aa8c0 5fed11f -- crates/sb-act` is
  empty), so the round-5 sb-act approval stands.
- A copy of the snapshot with D-437's count relaxed to `e.out < 1 || e.sens < 1` fails
  jsdom.test.js:1020, so that mutant is killed.

Probe results. reads is the number of value getter calls on canary fields; leak means the
canary appears in index(), text() or state().

| # | Case | 5fed11f | 1fe3caf^ |
|---|---|---|---|
| r9 A | `o.after(n); await null; o.remove()`, same id and aria-label | ok, reads 0 | |
| r9 A2 | same, one batch | ok, reads 0 | |
| r9 B | same, split by a later task | ok, reads 0 | |
| r9 B3 | Vue out-in (remove, later task, add) | ok, reads 0 | |
| r9 F | S.gone cap with 40 hidden inputs | ok, reads 0 | |
| r9 Chromium fade.html | transitionend, read during the transition and after it | ok, ok | |
| r9 Chromium wrapcb.html / modal.html | | ok / ok | |
| C shared | Chromium fade, shared name and aria-label | mid ok, after ok, getter 0 | same |
| V1 | add-first wrapper, shared keys, later task | ok, reads 0 | |
| V2 | add-first wrapper, neutral, later task | ok, reads 0 | |
| V3 | add-first sibling, neutral, later task | ok, reads 0 | |
| V4 / V4b | 3 batches (empty wrapper, input into it, remove old), shared keys | ok / ok | |
| V6 | aria-label set in a later batch, before the removal | ok | |
| V7d | D-440: the removal batch moves n itself into the swap | ok | |
| V7k | D-440 skip path, n shares aria-label | ok | |
| V11 | other parent, shared label text only | ok | |
| V12 | neutral sibling, removal deeper in the tree | ok | |
| V13 | 3 microtask batches, shared keys | ok | |
| **V7** | **D-440 skip: neutral n added next to the password, later batch replaceWith's the password with another value field** | **LEAK, reads 4** | ok, reads 0 |
| **C d440** | **V7 in Chromium (n inserted, old replaced by `#hint` on transitionend)** | **after: LEAK, getter 3** | after ok |
| V3m / C neutral / C wrap | neutral, read while both fields exist | mid LEAK, getter 3, after ok | same |
| V5 | 3 batches, neutral, n in a new wrapper (cross-parent) | LEAK, reads 3 | LEAK |
| V8 | same parent: n, then another field, then the removal, neutral | LEAK, reads 4 | LEAK |
| V9 | S.born cap: n in another parent (shared keys), 70 fields added before the removal | LEAK | LEAK |

Round 4's repro is closed: every shape it named, and every variant sharing a key, reads 0
times and never emits the value. Committed tests pin it:
- jsdom.test.js:886, microtask and later task, with and without shared keys;
- jsdom.test.js:960, the overlap with shared keys;
- browser.test.js:560, fade-show.html, transitionend, with counted getters;
- accept-t19 A16.

The general class of round-4 Blocking, "an add-first rebuild split across observer batches
leaks", is still open in one shape, and 1fe3caf (D-440) reintroduced it. That shape is
below.

## Blocking

- injected.js:2010 (D-440, hard rule 5; a regression from 1fe3caf): an add-first rebuild
  whose new field shares no key leaks once the batch that removes the password also adds one
  other value field to the same parent.
  - Why: that batch's D-437 swap sets `e.swap`, which empties `inParent`. The field that
    really carries the value (added earlier, the single born field in that parent) is then
    never marked, and the swap marks the wrong field.
  - Repro, Chromium (runs/reviewer-10/probe-r10-chromium.js, mode d440):
    1. "Show" inserts `<input type=text id=q9>` after `#acct` with its value.
    2. On transitionend, `old.replaceWith(<input id=hint placeholder=Hint>)`.
    3. After the removal, index() and state() carry the canary, with 3 getter calls.
  - Repro, jsdom: V7 in probe-r10-jsdom.js (4 reads).
  - At 1fe3caf^ both repros are ok (reads 0).
  - D-440's why says "without opening a leak". These repros show that it does.
  - D-440's text allows this leak, and the committed test jsdom.test.js:1275 ("remove + append
    next to the neighbour") pins the same structure as readable. So the fix needs supreme to
    amend D-440. Under the fix below, 1275 stays green only because jsdom's document is still
    loading at that point.
  - Proposed fix: under a swap, skip the same-parent rule only for born entries recorded
    while `document.readyState === "loading"` (B-415's #nick is parser-inserted). Mark every
    other single born neighbour, as D-438 (1) did.
  - Tried as a 3-line change in a scratch copy (runs/reviewer-10/fix):
    1. Add `ld: doc.readyState === "loading"` to the S.born entry.
    2. Carry `ld` into `prior`.
    3. Use `inParent = prior.filter((x) => x.p === g.parent && !(g.e.swap && x.ld))`.
  - Result: `node --test injected/test/` passes 93/93, and A10, A14, A18 and
    jsdom.test.js:1253 stay green. Chromium d440 then reads 0 times after the removal. V7
    reads 0 times once the document has loaded (in jsdom readyState stays "loading" until the
    first tick, so a test must await one first).
  - Add V7 as a jsdom test and as a Chromium test that counts getter calls.

## Should fix

- Add-first overlap window, neutral identity (carried from round 4, partly resolved).
  - Resolved: D-438 (1) closed the overlap for fields that share a key, in the same parent or
    its subtree (C shared mid: getter 0).
  - Open: a neutral copy is still read while both fields exist (C neutral, C wrap: mid getter
    3, canary in index()). settle() usually waits the transition out (reviewer-9 gotcha), so
    the driver path rarely reads it.
  - Fix: while an un-walked field added within 2 s shares a parent with a connected sensitive
    field, report it as `filled` without reading it (an over-mark that lasts at most 2 s).
- Neutral-identity residue outside D-438's letter (pre-existing, not a regression; supreme
  should rule or file a follow-up):
  - V5: a 3-batch build (empty wrapper, then the input into it, then the removal) is
    cross-parent, so the same-parent rule never sees it.
  - V8: a second field added to the parent before the removal means there is no "single
    field".

  Both read the value. The vault scrub is not a backstop for a password the user typed.
- Caps are untested (carried). V9 shows the S.born cap of 64 evicts a key-matched copy when
  more than 64 value fields are added inside the window. It is contrived, but a grid
  rendered during the toggle hits it. The S.gone (32) and NEAR_MAX (64) caps are also
  untested.
- Resolved since round 4, each with a test:
  - The D-437 swap counted every input type. Fixed by D-438 (2) and NONVALUE_TYPES
    (injected.js:1843). Chromium wrapcb.html is ok, and jsdom.test.js:979 covers it.
  - fieldLabels missed a label[for] removed as a sibling node. Fixed by D-438 (3)
    (injected.js:1907). Chromium modal.html is ok.
  - The remove-side fieldKeys catch failed open. It now remembers a match-all entry,
    `keys: null` (injected.js:1969). jsdom.test.js:1031 covers it.
  - No test pinned D-437 "lost exactly one". Now pinned by jsdom.test.js:1020, and the
    relaxed-count mutant is killed (above).
  - passiveFrame matched `size=invisible` in a #hash. Fixed by D-438 (4)
    (injected.js:338-350): an hCaptcha frame=challenge frame is never passive by URL.
    jsdom.test.js:1072 covers it. No real capture was taken (js-coder-5 not_done 5).
- sb-act: none.

## Notes

- injected.js, D-436: unchanged since round 4. Passive v3 markers are dropped only as context
  for weak prompt text. Vendor widgets still get CHALLENGE rows. The `script` exclusion
  cannot trigger (harmless).
- Over-marking (efficiency only, still open):
  - OTP and SSN name matches (`name=code`, footprint, businessName).
  - D-437 and D-439 one-field swaps mark an unrelated single field.
  - The fix proposed under Blocking adds one more case of the same kind.
- S.gone and S.born hold keys, WeakRefs and times only, never a value. textOf skips field
  contents, and neither list is emitted.
- Conformance at 5fed11f:
  - I-11 shapes are unchanged.
  - No timers or sleeps were added (hard rule 6).
  - Rules 1, 2 and 7 do not apply.
  - Commit 1fe3caf claims D-440 and implements it as written. The issue is D-440's premise,
    not a drift from the spec.
- sb-act, round 5 (reviewer-7, 361b28d, D-409): approve, and nothing has changed since.
  086d248 `contract_paths.rs` names all 11 I-10 paths (details in 71aa8c0). Open nits: the
  "New password" field without autocomplete, and the 250 ms wall-clock bound in the
  tester-owned `accept_t19_round3.rs`.

## Resolved in earlier rounds

- Round 4 Blocking (343a533, D-435 (2)): add-first rebuild split across batches, shared keys.
  Fixed by D-438 (1) in edd30e4 and later commits. r9 A, A2, B and fade.html all read 0 times,
  and jsdom.test.js:886 and browser.test.js:560 pin it. Full text:
  `git show 71aa8c0:bench/results/review-T-19.md`.
- injected.js R1 B1: a field that had been a password was read after a type swap. Fixed by the
  sticky S.sens and masked-text detection (D-405 (1)).
- injected.js R1 B2: id reuse before the first `walk({base})`. Fixed by B-412 / D-73
  (`{why: "no base"}`).
- sb-act R2 B1 to B3: control-character keys, Enter dropping the form max, and the
  one-time-code exemption hiding CVC. Fixed by D-406.
- sb-act R4 B1: Enter chords lowering Delete. Fixed by D-409 (1).

## Verdict

The round-4 leak is closed for every shape it named, with tests. D-440 (1fe3caf) reopened a
narrow add-first split leak: a neutral copy plus a one-for-one swap in the removing batch.
Chromium shows the value in index() and state() after the rebuild, so hard rule 5 is
broken. The fix needs a supreme amendment to D-440 (proposal above). A 3-line version passes
93/93 in a scratch copy. If supreme instead accepts the trade-off explicitly in D-440, this item
becomes a Should fix.

Verdict: request changes
