# Review: T-19 combined (B-5, B-7, I-10, I-11, D-435..D-445)

reviewer-15, 2026-10-26, round 10. Pinned to 7dfc6f1 on swarm/page (code fbce10e, D-445).
Earlier rounds: round 9 (reviewer-14) `git show 0cd5f8d:bench/results/review-T-19.md`,
round 8 `git show 7c73a1c:bench/results/review-T-19.md`, round 7 `git show dc14a2c:...`,
round 6 `git show 0cc160e:...`.

Question this round: does fbce10e implement D-445 as written, does it regress any earlier probe,
and can a mainstream show-password toggle still put a password into index(), state() or text()
(hard rule 5)? Scope per D-89: Blocking is (a) a regression, (b) a shape D-445 or earlier
decisions claim closed that still leaks, (c) a new leak from a mainstream toggle, (d) a vault
canary in output, a value getter outside fieldValue, or a fixed sleep without a rule 6 comment.

Method:
- All runs used `git archive 7dfc6f1 injected bench/pages` in runs/reviewer-15/snap (the worktree
  holds a tester's uncommitted accept-t19.test.js and accept-d445.html). Runs were serial.
- runs/reviewer-15/run-probes.sh re-ran every reviewer-10..14 probe (jsdom in 4 modes, dbg3-timed,
  Chromium r9..r14, the cost probe). cmp-probes.py compares the leak rows against
  runs/reviewer-14/out (86cb048). Output: runs/reviewer-15/out, summary out/cmp.txt.
- New: runs/reviewer-15/probes/probe-r15-chromium.js (M1..M10, mainstream toggles; r14
  same/next/late protocol, reads include open shadow roots) and probe-r15-settle.js (settle on
  pages that mutate every task).

Gates at 7dfc6f1:
- `node --check injected/injected.js` ok. `node --test injected/test/`: 131/131 (221 s),
  runs/reviewer-15/out/node-test.txt.
- `git diff 86cb048 7dfc6f1 -- crates agent-memory/interfaces.yaml` is empty, so the sb-act
  approval stands. No em dashes in d445-twins.html. No `\x00-\x08` bytes in injected.js.

## Probe results (7dfc6f1)

| # | Case | result |
|---|---|---|
| (a) regression sweep | every reviewer-10..14 row (jsdom 4 modes x 12 files, dbg3-timed, Chromium r9..r14) | no row that read 0 at 86cb048 leaks now, except r11-dbg3 `postload k=50` (below) |
| r11-dbg3 k=50 | born window at the 2 s SENS_KEEP_MS edge | timing, not a regression. dbg3-timed shows the edge: k=50 is ok at 1937 ms and leaks at 2006 ms and 2060 ms on both trees. A re-run of r11-dbg3 on 86cb048 and 7dfc6f1 back to back gives ok on both (out/r11-dbg3-on-86cb048.real.txt). Known T-19b shape |
| (b) Z3, Z4, Z4v, Z5m, Z6, Z7, Z7v, Z7i | round 9 leaks, D-445 claims closed | ok, getter 0 at same/next/late, settled true (out/chromium-r14.txt) |
| (b) Z1, Z2, Z5, Z5w, Z6a, Z8; Y3, Y5, Y6, Y7 | round 9 closed list | ok, getter 0 |
| still open, unchanged | V5, V9 (jsdom), r12 long.html, r13 y1g late, o5, o6 late, r11-dbg k >= 55 / dbg3 k >= 61 (real clock) | same leak rows as the 86cb048 baseline. All named open in D-444/D-445 (T-19b) |
| M1 MUI OutlinedInput | React controlled, `value` attribute synced, IconButton flips type | ok, getter 0 |
| M2 Ant Design Input.Password | affix wrapper, suffix icon (mousedown preventDefault), type flip | ok, getter 0 |
| M3 Vue 3 + Vuetify v-if/v-else | whole v-text-field subtree unmounted, new one mounted, value written after insert | ok, getter 0 |
| M4 Shoelace-style `<sl-input password-toggle>` | open shadow root, toggle inside it, type flipped in a microtask | ok, getter 0 |
| M5 Angular Material two `*ngIf` inputs | remove-first, new mat-input id, value written in a resolved promise | ok, getter 0 |
| M6 Svelte 5 `{#if}{:else}` + bind:value | add-first branch swap with wrappers | ok, getter 0 |
| M7 Bootstrap "Show password" checkbox | form-check checkbox flips type | ok, getter 0 |
| M8 React key remount of an MUI TextField | delete then place a FormControl subtree, value set before insert | ok, getter 0 |
| M9 Vuetify 2 twins, depth 6 | v-show on two v-col, input 6 levels below the v-row | ok, getter 0 |
| **M10** twins at depth 7 | M9 plus a v-card / v-card__text wrapper per column | settled true; LEAK same/next/late, getter 3/6/9 |
| settle S1..S5 | setTimeout 0 chain, MessageChannel ping-pong, rAF, 50 ms interval, microtask per timer | settled false at the cap: 1501..1505 ms (cap 1500), 3000..3011 ms (default). No hang, no loop |
| settle S6, S7 | idle page; one transitionend that sets display none | settled true at 151..164 ms; S7 353 ms with the element hidden at resolve |
| cost (probe-r14-cost) | accepted cost | c1, c3, c4 as at 86cb048. c2 (off-screen trick password) now masks: pre-filled `pre` and `ta` read `empty`, the rest `filled`. As D-445 states |

## Blocking

- none

## Should fix

- injected.js:922, :965 (TWIN_DEPTH 6): a twin pair whose inputs sit 7 or more levels below
  their common ancestor still leaks persistently with `settled: true` (M10: Vuetify 2 v-col >
  v-card > v-card__text > v-text-field, getter 3/6/9). This is not Blocking: D-445 (1) sets the
  6-level cap in writing, M9 (the plain Vuetify 2 grid, depth 6) is closed, and Vuetify's own
  toggle flips `:type`, which M3-style flips already cover. The climb is memoized and stops at
  the first ancestor that holds another value field, so the cap buys little. Raise it (12, or
  the NOT_SHOWN_DEPTH 16) and add an M10-shaped case. T-19b.
- Accepted cost (probe-r14-cost, D-444/D-445): masked fields that hold a value but give no
  evidence report `empty`. c1, c2 and c4 read a pre-filled email and textarea as `empty`. c3
  (identifier-first: email plus a hidden password in one form, the Google and Microsoft
  pattern) reads the email the agent typed as `filled`, never as text, so the agent cannot
  verify it. With D-445 (2), c2's off-screen autofill trick now masks the whole form too.
  This is a false statement, not a coarse one, and a planner may retype. Report `filled` when
  unsure, or `masked`. T-19b.
- Open T-19b leaks, unchanged from round 9: V5, V9, the born or time window edge (r11-dbg and
  dbg3 k >= 50 on a slow clock), r12 long.html, r13 y1g late, o5, o6 late.
- Merge needs the tester's run at 7dfc6f1. accept-d445.html and the accept-t19.test.js changes
  are uncommitted in the worktree, and accept-T-19.md is still round 5 at 86cb048.

## Notes

- Item 1 (rules): no keys, vault, captcha, gate or model-path changes. `fieldValue` (:1134) is
  still the only value getter call site (:1141, :1146), and it runs only from index()
  (stateApi and text go through it). notShown, clipRect, clipPathBox and onlyField never touch
  `.value`. S.hid holds WeakMaps of elements and booleans, and it is reset at index() (:1627) and
  walk() (:1477). No vault canary was set or seen in my runs. The only new timer is the settle
  yield at :2540, and it has the rule 6 comment (:2533..:2539).
- settle (:2498..:2545): every wake re-arms the quiet wait and clears `yielded`. capTimer is
  independent of both, so a page that mutates on every task ends at the cap (S1..S5). The yield
  adds one task to each settled resolve (S6 151 ms for quiet 150).
- Item 2 (conformance): fbce10e implements D-445 (1), (2) and (3) as written. (1) climbs while
  only(ancestor) === el, scans only the stop ancestor's children, and treats a shadow root as
  its host. (2) is used only by hidSens, so readers still use displayed(). An unparsed clip-path
  fails closed on the field. (3) is the one-task recheck. No behaviour outside the spec.
- Item 3: no interface changes.
- Item 4: browser Z3..Z7v, Z5m, Z6, Z6s and the D-445 control use the Y6/Y7 twinCase protocol
  (canary, same/next/late, world-sb getter counter). The jsdom cases cover depth 2 and 3,
  mixed depth, opacity, the memo reset, both shadow-root paths and textarea twins. That closes
  round 9's test gap. The coder's mutants (depth1, noshown, noyield, noshadow, nomemoreset,
  noclip, noclipfail, nooffscreen, noscroller, noopacity, notiny) are each killed
  (runs/js-coder-12/out/matrix-*.tap). I did not re-run them.
- Item 5: hidSens fails closed on a throw. checkVisibility is guarded. jsdom has no layout, so
  only the style checks apply there, as the comment says.
- Not a leak, but worth knowing: React's controlled inputs mirror the value into the `value`
  attribute (M1). Readers take a name from `value` only for submit, reset, button and image
  inputs (:646..:649), so nothing reads it.
- sb-act: round 5 approval (reviewer-7, 361b28d) stands.

## Resolved in earlier rounds

- Round 9 Blocking (Z7, Z7v, Z7i, Z5m, a sibling-wrapper rule that stopped at depth 1): fixed
  by D-445 (1) in fbce10e. Browser tests pin it, and the depth1 mutant fails them.
- Round 9 Should fix: Z6 (settle before transitionend) fixed by D-445 (3), with the rule 6
  comment. Z3, Z4 and Z4v fixed by D-445 (2). The jsdom shadow-root and textarea tests are added.
- Rounds 4..8 Blocking items: still closed (the r9..r14 probes read 0, as in the table).

## Verdict

D-445 closes round 9's Blocking item and the Z3, Z4, Z4v and Z6 gaps, and its tests kill
their mutants. The full re-run of the reviewer-10..14 probes shows no regression. The one
difference (r11-dbg3 k=50) is the known 2 s window edge, and it behaves the same on both trees.
Eight mainstream toggles not seen in earlier probes read 0: MUI, Ant Design, Vue/Vuetify v-if,
a Shoelace-style shadow input, Angular *ngIf, Svelte 5 {#if}, a Bootstrap checkbox and a React
key remount. settle() cannot hang. The depth-7 twin pair and the accepted-cost reporting are
Should fix for T-19b.

Verdict: approve
