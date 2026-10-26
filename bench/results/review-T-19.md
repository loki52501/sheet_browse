# Review: T-19 combined (B-5, B-7, I-10, I-11, D-435..D-443)

reviewer-13, 2026-10-26, round 8. Pinned to a9e246a on swarm/page. The D-443 code is 8fb00b4,
and D-443 extends D-442. This is the file the merge gate reads. Earlier rounds:
round 7 (reviewer-12) `git show dc14a2c:bench/results/review-T-19.md`, round 6 (reviewer-11)
`git show 0cc160e:bench/results/review-T-19.md`, round 5 `git show 1b0b5f9:bench/results/review-T-19.md`,
round 4 `git show 71aa8c0:bench/results/review-T-19.md`.

Question this round: can a password value reach index(), state() or text() at a9e246a
(hard rule 5) in a shape I rule Blocking?

Method:
- All runs used a `git archive a9e246a injected bench/pages` snapshot in runs/reviewer-13/snap,
  with NODE_PATH pointing at the worktree's injected/test/node_modules.
- Re-run on the snapshot (copies with paths rewritten in runs/reviewer-13/from-reviewer-1{0,1,2}):
  every jsdom probe of rounds 5 to 7 in 4 modes (real, FROZEN, POSTLOAD, POSTLOAD+FROZEN), and
  every Chromium probe (r9, r10, r11, r12). Output in runs/reviewer-13/out. The fix, muta, pre440
  and dbg4 copies test other code and were not re-run.
- New:
  - runs/reviewer-13/probe-r13-jsdom.js (Y1..Y6), 4 modes. "mid" is a full read inside the step
    while the password is still in the page.
  - runs/reviewer-13/probe-r13-chromium.js (Y1g, Y3, Y5, Y2c, Y6, Y7, O5, O6): fill, click Show,
    `await __sb.settle()` (defaults), read in the same evaluate ("same"), in a second evaluate
    ("next"), and once the page clock passes click + 3.2 s ("late").
  - runs/reviewer-13/dbg3-timed.js: reviewer-11's dbg3 with the copy-to-swap time and a frozen clock.
  - runs/reviewer-13/fix (prototype, diff in runs/reviewer-13/fix.diff) for the Blocking item.
- reads/getter is the number of value getter calls on canary fields. LEAK means the canary
  appears in index(), text() or state().

Gates at a9e246a:
- `node --test injected/test/` (snapshot): 103 tests, 103 pass, 0 fail, 177 s. O1..O3 report
  settled true with the password present at the same-evaluate read, copy filled, reads {}.
  ACCEPT A18 took its over-mark branch (at 318 ms and 192 ms read `filled`; 2225 ms and 2133 ms read text).
- Mutants on the snapshot: no `nearLivePw` call fails the D-443 jsdom test (:1544); no D-443 (3)
  rule fails it too; no 2 s limit on the mask fails jsdom.test.js:951, the D-439 test and :1249.
  The coder's six mutants (runs/js-coder-10/mut) agree.
- `git diff dc14a2c a9e246a -- crates/ agent-memory/interfaces.yaml` is empty, so the sb-act
  approval stands.

## Probe results (a9e246a)

jsdom columns are real clock / frozen clock; POSTLOAD rows matched the loading rows unless shown.

| # | Case | result |
|---|---|---|
| r9 A, A2, B, B3, F | reviewer-10/11 r9-probe-jsdom2 | ok, reads 0, all modes ("I wrap label: Sign in empty" is the known over-mark line) |
| V1..V4b, V6, V7, V7k, V7d, V8, V8k, V11..V13, V3m (mid), V3k (mid) | probe-r10-jsdom (both copies) and -snap2 | ok, reads 0, all modes |
| V5 | 3 batches, cross-parent, neutral | LEAK, reads 3, all modes |
| V9 | S.born cap, key copy in another parent, 70 fields | LEAK, reads 73, all modes |
| N1, N1r, N2c, N3, N3r, N4, N4s, N5 | probe-r11-jsdom | ok, reads 0, all modes |
| N2 / N2b | 70 / 60 fields elsewhere, swap | real clock LEAK (reads 74 / 64); frozen ok |
| dbg loadingadd/postload k 0..30 | probe-r11-dbg | ok |
| dbg k 62..70 | probe-r11-dbg (has no FROZEN switch, so every mode is real clock) | LEAK. dbg3-timed: copy to swap took 3.1..5.3 s in jsdom; frozen clock k 40..70 all ok |
| dbg3 k 40..61 | probe-r11-dbg3 (real clock only) | LEAK (k 40 ok once): same cause, jsdom took more than 2 s |
| dbg2 | loading fields-then-copy k 0..70 | ok |
| X1 / X1b | copy in a sibling div / mirror in another parent | LEAK, reads 3 / 4, all modes |
| X2w / X2w0 / X5 | wrapper or fragment [CSS-hidden text, pw] rebuilt add-first | LEAK after the removal, reads 4 / 4 / 3, all modes |
| X2h, X5s, X6, X7, X7f | | ok, reads 0, all modes (X7, X7f closed by D-443 (2)) |
| Chromium r9 fade, wrapper+checkbox, modal | | ok |
| Chromium C shared / neutral / wrap / d440 | mid and after | ok, getter 0 (round 7: mid LEAK) |
| Chromium M1..M4 | | ok, getter 0 |
| Chromium O1 css / O2 timer / O3 child | same and next | settled true, ok, getter 0 (round 7: LEAK) |
| Chromium O4 long (3.5 s fade) | same and next | settled false, LEAK, getter 3 |
| **Y1** | new, jsdom. Copy into an existing sibling slot (X1 class) | mid LEAK (reads 3), after LEAK, all modes |
| Y1g | new. Copy appended to the form (the password's grandparent), pw removed from its wrapper | mid ok; after LEAK, all modes; Chromium: same/next ok, late LEAK |
| Y1w | new. Copy in a new wrapper added to the form, old wrapper removed | ok |
| Y2 / Y2b / Y2d | new. Password parent detached and reattached / moved out of the form / reattached in a later task | ok |
| Y2c | new. Parent detached, pw removed while detached, parent put back (one task) | jsdom LEAK; Chromium ok (jsdom has no transient observers) |
| Y3 / Y3d | new. One batch adds the copy and moves (re-attaches) the password after it | mid LEAK (reads 3), after ok; Chromium same/next LEAK, late ok |
| Y3m / Y3f | new. Copy, then one microtask later the pw moved / a new pw attached | ok |
| Y5 | new. One batch: pw replaced by [text copy, new display:none password, same name] | LEAK mid and after, all modes; Chromium LEAK same/next/late |
| **Y6** | new. Clone and hide: copy added, password hidden (display:none) and kept | jsdom real clock LEAK after 2.1 s; Chromium same/next ok, late LEAK (pw still present), settled true |
| **Y7** | new, Chromium. Twin text input present from load (display:none), synced on input; Show flips display | settled true; same, next and late LEAK, getter 2/4/6 |
| O5 | new, Chromium. 2.5 s CSS fade on the old field (inside the 3 s cap) | settled true; same LEAK (pw present, getter 3), next and late LEAK after the removal |
| O6 | new, Chromium. JS-timed exit of 2.5 s | settled true; same/next ok; late LEAK after the removal |

Every leak in the coder's list reproduces as reported. D-443 closes O1..O3, C mid, V3m mid,
X7 and X7f, and N5 stays closed.

## Blocking

- injected.js:909..:922 `fieldValue` (hard rule 5): a show toggle that swaps visibility between
  the password and a plain twin, instead of adding and removing fields, puts the password in
  index(), text() and state() with `settled: true`, and it stays there on every later read.
  - Y7: a twin `<input type=text>` exists from load, hidden, kept in sync by an input listener or
    a shared model (Vue `v-show` twins, Angular `[hidden]` twins, hand-rolled toggles). Click
    Show, settle, read: getter 2 at the first read, the canary in the blob.
  - Y6: clone and hide (the legacy jQuery show-password plugins): the copy is added next to the
    password, the password gets display:none and stays. D-443 (1) masks the copy for 2 s, then
    the next read leaks for as long as the page lives.
  - Why Blocking: this is the round 7 standard. A normal click Show, settle, read flow on a
    pattern a site author writes in three lines reaches it, settle reports settled, and nothing
    tells the driver to wait. It needs less than round 7's Blocking item (no rebuild, no timing),
    and the leak is persistent, not a window. It is not a D-443 regression: no add or remove
    happens in Y7, so no rule from D-435 to D-443 applies, and it leaked at every earlier round.
  - Proposed fix, prototyped in runs/reviewer-13/fix (12 lines, fix.diff): `fieldValue` returns
    `filledNoRead` for a value field whose parent directly holds a sensitive field that is not
    `displayed()`. Results: Y5, Y6 and Y7 read 0 (Chromium, same/next/late), and
    `node --test injected/test/` stays 103/103.
  - Cost: a visible field that sits in the same parent as a hidden password reads empty|filled
    instead of text. An identifier-first login that keeps the hidden password next to the email
    in one container is the likely case. That is an efficiency cost only, and rule 5 outranks it.
  - Supreme options: take the fix in an amendment (D-444) with Y6 and Y7 as browser tests, or
    re-accept the twin and clone-and-hide shapes in writing citing Y6 and Y7. The second option
    turns this item into Should fix.

## Should fix

Rulings for the leaks the coder reports and D-443 sends to T-19b, then the new shapes.
Every item here also leaked at 7cb7020.

- V5 (3 batches, cross-parent): Should fix. An empty wrapper has to mount in one commit and its
  input in a later one, in a container other than the password's parent. React, Vue and Svelte
  commit a toggle's subtree at once.
- X1 and Y1 (copy into an existing, unrelated container): Should fix. There is no structural or
  key link, so this is the page printing the secret somewhere else. A show toggle renders into the
  password's own slot.
- X1b (mirror field elsewhere): Should fix, the same class as X1. The copy in the parent is
  masked now, and only the mirror reads.
- V9 (S.born cap): Should fix. It needs more than 64 value fields mounted inside the 2 s window
  plus a cross-parent copy with a shared key. No show-toggle flow does that. Fix: evict entries
  whose parent holds no sensitive field first, and add a fakeClock test (BORN_MAX, S.gone 32 and
  NEAR_MAX 64 are still untested).
- N2, N2b, dbg k 62..70, dbg3 (real clock): Should fix, and a jsdom artifact as measured.
  dbg3-timed shows jsdom took 3.1 to 5.3 s to add 40 to 70 fields, and with a frozen clock every
  k reads 0. A browser mounts 70 fields in milliseconds, so in a browser this reduces to O5/O6.
- O4 (3.5 s exit, settled false): Should fix. It is a reported non-settle, so the driver can wait
  or re-settle, and show-password exits run 150 to 400 ms in every UI kit.
  - Correct the record: the boundary is the 2 s window (SENS_KEEP_MS, injected.js:897), not the
    3 s settle cap. O5 (a 2.5 s CSS fade) reports `settled: true` and leaks at the same-evaluate
    read and after the removal. O6 (a 2.5 s JS-timed exit) settles at once and leaks on every
    read after the removal, because the copy's S.born entry has expired.
  - Should fix rather than Blocking because no real toggle animates for more than 2 s.
  - D-443's "exits longer than the settle cap" should read "exits longer than 2 s".
- X2w, X2w0, X5 (a CSS-hidden autofill-trick text input in the lost set): Should fix.
  - The copy is masked during the overlap now. The leak starts after the removal (e.out 2,
    e.sens 1, more than one remembered field).
  - It needs both a trick input inside the password's wrapper and an add-first wrapper rebuild.
  - Fix: do not count a value field that is not `displayed()` in e.out.
- Y1g (copy at the grandparent, the password removed from a wrapper that stays): Should fix.
  - It is masked during the overlap, and leaks after the removal because the removal target
    (the wrapper) is not the copy's parent (injected.js:2045).
  - It needs the copy and the password in different JSX slots.
  - Cheap fix: when `all` holds, also inherit a remembered field with a live S.nearPw entry whose
    parent contains `g.parent`. A14 is safe, because an SPA swap also loses a field that is not
    sensitive.
- Y3, Y3d (the password moved in the same batch the copy is added): Should fix. The overlap read
  leaks because nearOf skips every same-batch add (injected.js:2095), including a password that
  was only moved. D-443 (1) says "holds a connected sensitive field at the time of the add",
  which a moved password does. Keyed reconcilers insert before the existing node without moving
  it, so this is rare. Fix: skip only fresh fields that are not also in this batch's removals.
- Y5 (pw replaced by [text copy, hidden new password with the same name]): Should fix. D-443 (3)
  stands down on the new password's key match (injected.js:2025). The Blocking prototype closes
  Y5. Without it, compute addKeyHit over the non-sensitive added fields only.
- Tests:
  - browser.test.js O1..O3 do not assert `same.pw` (it is a diagnostic only). On a fast machine
    the O1 same-evaluate read could land after the removal. The mid read asserts the overlap, so
    this is minor.
  - Y6 and Y7 have no tests (they would come with the Blocking fix).

## Notes

- Item 2 (diff dc14a2c..a9e246a, injected/):
  - `fieldValue` (injected.js:909) is still the only value getter call site: `el.value` at :916
    and :921, select labels at :925.
  - Its one caller is the row state at :1467, which feeds index(), state(), text() and
    fingerprint(). The text collector skips input, select and textarea (:586). `attr(el, "value")`
    is read only for button-type inputs (:644..:647).
  - `nearLivePw` is applied on both the input and the textarea branch (:915, :920). No reader
    bypasses the S.nearPw mask.
  - S.nearPw holds a WeakMap from element to `{ t, parent: WeakRef }`, with no value (rule 2/5).
    No timers or sleeps were added (rule 6).
- Test honesty (dc14a2c..a9e246a):
  - jsdom :951: the expectation moved from text to masked (`sameMasked` still fails on a text
    reading). A new late run pins the 2 s end. reads == 0 and the canary check stay.
  - The D-439 "second later field" case waits 2001 ms before reading text; its reads == 0 stays.
  - The :1249 case flipped from text to masked, as D-443 (3) says.
  - `leakShape` now spies every password, which is stronger.
  - A14: r2's D-435 (1) text reads now accept empty|filled, and the exact text moved to r3 after
    the window. The canary, fragment and reads {} checks now cover r1..r3. That is wider than
    before, not weaker.
  - A16: `pastBornWindow` comes before reading the cumulative `__pwReads`, and the promo
    assertion is unchanged.
  - No reads == 0 or canary assertion was removed or weakened.
- Conformance:
  - 8fb00b4 implements D-443 (1) to (3) as written, except the Y3 gap (a moved password counts
    as "added by this batch").
  - D-443 (4)'s cost shows up exactly in the tests that changed.
  - Rules 1, 3 and 7 do not apply. d443-overlap.html is a fixture, and its `setTimeout` exits
    are the page under test, not driver waits.
- Quality: `nearLivePw` runs querySelectorAll on the add-time parent for every read of every
  masked field for 2 s. With `<main>` as the parent that is O(fields) per read. This is
  efficiency only.
- nearOf (:2088) computes `sensitive()` for at most NEAR_MAX non-S.sens fields per parent. A
  password past the 64th field of a large parent that was never walked is not seen at add time.
  Not probed.
- Over-marking (efficiency only, open): OTP and SSN name matches, D-437 and D-439 one-field
  swaps, r9 I, D-442 (2), D-443 (3) and (4). The Blocking prototype would add hidden-password
  neighbours.
- sb-act, round 5 (reviewer-7, 361b28d, D-409): approve, unchanged since.

## Resolved in earlier rounds

- Round 7 Blocking (the overlap read after `settled: true`; O1, O2, O3, C neutral/wrap/d440 mid,
  V3m mid): fixed by D-443 (1) in 8fb00b4. All read 0, and they are pinned by browser.test.js
  O1..O3 and jsdom.test.js:1544. The old code fails those tests with 20 getter calls (the coder's
  check), and the no-mask mutant fails :1544 (this round). Full text:
  `git show dc14a2c:bench/results/review-T-19.md`.
- X7, X7f (password plus confirm): fixed by D-443 (2). N5: fixed by D-443 (3).
- Round 6 Blocking (D-441 `ld` skip; M1, M4, N1): fixed by D-442 (295253e), still 0. See
  `git show 0cc160e:bench/results/review-T-19.md`.
- Round 5 Blocking (D-440; V7, C d440): closed by D-441, kept by D-442 and D-443. See
  `git show 1b0b5f9:bench/results/review-T-19.md`.
- V8, N3, N3r, X2h: fixed by D-442 (2). Round 4 Blocking (D-435 (2)): fixed by D-438 (1). See
  `git show 71aa8c0:bench/results/review-T-19.md`.
- D-438 (2)..(4), R1 B1/B2, and sb-act R2 B1..B3 and R4 B1: fixed in earlier rounds.

## Verdict

D-443 is implemented as written and closes round 7's Blocking item (O1..O3 and the C mid reads
are 0 in Chromium, pinned by tests that fail on the old code). The tests are honest.

One Blocking item remains, on new evidence. A visibility-swap show toggle (Y7 twin, Y6 clone and
hide) puts the password in every reader with `settled: true`, and it stays there. No add/remove
rule can see it. A 12-line fail-closed fix exists (runs/reviewer-13/fix, 103/103). Supreme can
take it as D-444 or re-accept the shape in writing citing Y6 and Y7, which would make this
item Should fix.

Verdict: request changes
