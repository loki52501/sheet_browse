# Review: T-19 JS half (injected.js, media.js, tests, fixtures)

Commits 0ffb37a, 7581bf9, c779d39 on swarm/page. crates/sb-act is out of scope.
`node --test injected/test/`: 32/32 pass (run 2026-10-23, 39.8 s; cost line
`{"count":1601,"walk":142,"index":28,"dirtyIndex":26,"text":39}`).
I reproduced each finding below with inline jsdom probes. No files were written for them.

What is right: install in world sb, frozen and non-configurable, with a test that `window.__sb` is
undefined in the main world (browser.test.js:40). The password value getter is never called for
`type=password` (jsdom.test.js:262 instruments it). No fixed sleeps: every setTimeout is a capped
condition deadline with a comment (injected.js:1476, 1631, 1637; media.js:137). act awaits two rAF
for the stability check (injected.js:1513-1519). No DOM writes, no FormData, no innerHTML. The
IndexRow keys and serde defaults match index.rs `index_row_wire_shape`; every omitted State field
equals `State::default()` (visible/enabled true, other booleans false, options empty,
checked/value None).

## Blocking

- injected.js:687 (fieldValue) with :651 (sensitive): a field that was a password is read and
  emitted once its type changes. Repro: `<input id=p type=password>`, value set, walk, then
  `p.type="text"` (a "show password" toggle, or a site that swaps the type on focus). index() then
  returns `{"kind":"text","text":"SECRET-CANARY"}`. This breaks rule 5, and rule 2 if the value
  came from `@vault`. The cause: sensitivity is recomputed live from the current type, and the
  dirty re-walk also resets `inputType`. Fix: keep a WeakSet of elements ever seen as sensitive,
  and count computed `-webkit-text-security` other than `none` as sensitive (masked text inputs
  used to defeat autofill). Add both cases to the jsdom getter test.
- injected.js:227-236, 1010-1016, 1029-1033: the D-55 / I-11 rule "ids are never reused in a tab
  frame" depends on call order. `base` is adopted only when `walk({base})` is the first walk. Any
  earlier `index()`, `text()`, `frames()`, `fingerprint()`, `state()` or `name()` (I-11 says they
  refresh by themselves, B-411) runs `ensure()` with no base, so ids restart at 1 in every document
  generation. Any earlier `id(el)` hands out ids below the later base. Example: media.js:31
  `__sbMedia.list()` calls `__sb.id`. Repro: `id(btn)` returns 1, then `walk({base:100})`, then
  `resolve(1)` returns `{why:"stale"}` for a live, connected button, while `index()` still emits
  id 1. Fix: refuse an un-based first call (`{why:"no base"}`), or let a later base move the
  already-issued ids, and test the case.

## Should fix

- injected.js:288-297, 995, 1260: `selfChallenge()` checks the URL against
  `/...|captcha/i`. Any page whose URL contains "captcha" (a blog slug, the reCAPTCHA docs) then
  returns `index() == []` and `text() == []` with no CHALLENGE row. Only frames() says
  `challenge:true`. That is an empty read, not a reported challenge (repro: `/posts/how-captchas-work`).
  Fix: emit one CHALLENGE row for the frame when self-challenged, and match known challenge
  hosts/paths instead of the bare substring.
- injected.js:909-915, 98-102: CHALLENGE detection gaps. (a) "Verify you are human" text is only
  checked on heading and dialog names. A Cloudflare interstitial (`<p>Verify you are human by
  completing the action below.</p>`, Turnstile inside a closed shadow root) produces no CHALLENGE
  row at all (reproduced). (b) Arkose (`client-api.arkoselabs.com`, title "Verification
  challenge") and GeeTest (`geetest_*` classes) are not matched (reproduced). (c) Unused
  signals: `[name=cf-turnstile-response]`, `[name=g-recaptcha-response]`,
  `[name=h-captcha-response]`, title "Just a moment...". Rule 3 needs the stop to be reported.
- injected.js:269-279: the reCAPTCHA v3 / invisible badge (`.grecaptcha-badge`, `size=invisible`)
  is flagged CHALLENGE (reproduced). Over-flagging is the safe direction for D-5, but with D-404(4)
  and a skeleton "⚠ CHALLENGE", every v3 login page looks like a stop. Raise a blocker for
  supreme: either a passive marker or flag only when a challenge bframe becomes visible.
- injected.js:856 and 1305: heading level comes from site input, `Number(aria-level) || 2`, without
  clamping. `aria-level="300"` or `"2.5"` produces `level: 300` / `2.5` (reproduced), which
  `TextBlock.level: Option<u8>` rejects. One bad attribute fails the whole `Vec<TextBlock>`
  deserialization. Clamp it to an integer in 1..=6.
- jsdom.test.js:82: the wire-shape test checks key names and types in JS only. TextBlock has no
  shape test, and nothing deserializes real index()/text() output with serde. Add a Rust test
  that deserializes a captured fixture into `Vec<IndexRow>` / `Vec<TextBlock>`, so a drift like
  the level bug fails CI. (An untracked `injected/test/check-wire-shape.js` from another agent
  exists in the worktree. It is not part of this review.)
- injected.js:1590-1607, 1655 (D-416 (d)): settle on a page with no "content" always runs to the
  cap. Reproduced: about:blank and `<embed type=application/pdf>` give `{settled:false}` at the cap
  (3 s by default). Rule 6 holds (it is a capped condition), but under D-7 this costs 3 s on every
  step in a fresh or popup tab before navigation, every PDF, and closed-shadow apps. Fix: add
  `embed,object` to the content selector, skip (d) for `about:` URLs, and apply (d) only for a
  bounded window after `load`.
- injected.js:1565-1566, 1577-1587: attribute mutations remember their target, and any running
  animation inside a remembered node counts. One class toggle on `<html>` or `<body>` (scroll
  classes, theme) therefore lets any always-on infinite animation (shimmer, marquee) hold settle
  to the cap. Limit attribute-changed nodes to animations on the node itself, or exclude html/body.
- media.js:235-260, 199: `frames(id, times)` has no total budget. Each time can wait up to
  `seekCapMs` (5 s) for `loadedmetadata`, `seeked` or `loadeddata`, so a `preload=none` video or a
  long `times` list runs well past the 10 s command timeout (B-4 #11). The video then stays paused
  until the loop ends. Add a per-call deadline, stop after the first timeout, and call `load()`
  when `readyState == 0`.
- media.js:62-69: `challenged()` is a weaker copy of injected.js detection. It ignores
  `data-hcaptcha-widget-id`, challenge iframes and `frameElement`. Use `__sb` when it is present.
- injected.js:72-76, 764 (D-414): a plain `<form>` never gets `landmark:"form"`, but the IndexRow
  and TextBlock vocabulary lists `form`, so `sheet(region=form)` returns nothing on most form pages.
  Per ARIA, a `<form>` with an accessible name (aria-label, aria-labelledby, title) is a form
  landmark. Treat it like `section`, which this file already maps to region when named. Same for
  the `<search>` element.
- decisions.yaml:177 (D-412): this departs from B-5's explicit order (own text before
  placeholder/title for name-from-content roles, the `name` attribute moved after nearby). It
  matches accname and the reasoning is sound. But B-5 is not ambiguous, and srv-web must build the
  same names (D-51: one shape for live and fetched rows). Record it as a proposal or blocker for
  supreme to amend B-5, so the srv-web ladder follows it.
- injected.js:112-143: the in-page copy of the D-49 alias table goes against D-49's "one alias
  table" (data/roles.txt does not exist yet). Pass the table in from sb-driver, or run
  fingerprint() in Rust over index(), before the two copies drift.

## Notes

- injected.js:1107-1214: index() ships every row on each call (the dirty set only saves
  recompute). 28 ms for 1601 rows is acceptable, but D-51 says the index is shipped
  "incrementally, via the dirty set". Track a delta API as a follow-up. Walk is about 90 us per row
  on first walk, so 10k-element apps will cost about 1 s on first open.
- injected.js:280-287: act's `inChallenge` reads `rec.challenge`, but challenge rows found by
  heading/dialog text set only `row.challenge` (injected.js:910-914). act on that heading or
  dialog is not refused. Harmless for headings, wrong for a challenge dialog's buttons.
- injected.js:1532-1549: a text change in an `aria-labelledby` target does not re-dirty the
  control, so its cached name goes stale until a full walk. `<label>` is handled (:1546).
- injected.js:1551-1557: `S.recent` holds strong references to up to 300 elements, which keeps
  detached subtrees alive. Use WeakRef or prune on read.
- injected.js:648: `closest('[aria-disabled="true"]')` stops at shadow boundaries. Use
  `closestComposed`.
- injected.js:151, 233; media.js:36: `WeakRef` is ES2021, not ES2020. B-4 #4 asks for it and
  Chrome has it, so this is fine; noted against the CLAUDE.md "ES2020" wording.
- Covered detection: pointer-events:none overlays are correctly not covered (elementFromPoint
  skips them). An overlay that partly covers the element but not its centre is not covered, as
  B-4 #6 specifies (centre hit test). Fixed or sticky interactive rows are never covered-checked
  (`!row.cand`, :1130).
- `__sbMedia` (B-410) returns frame pixels as data URLs. That is allowed: this is the B-9 `video.*`
  path, and rule 5 permits pixels there.
- Fixtures: no external network URLs and no user-facing drafts (rule 7 not applicable).

## Verdict

Two blocking items: a password value can still reach index(), and id/base handling depends on
call order. The rest is solid and well tested.

Verdict: changes

# Round 2 (reviewer-5): 4d1c76d "injected.js review fixes" (D-405, D-422..D-428, B-412 / D-73)

Scope: `git diff b973fb4..4d1c76d` on injected/, bench/pages/ and
crates/sb-act/tests/wire_from_injected.rs. No later commit touches these paths. I checked against
I-11 as it stands on this branch (main is merged, so D-73 is present).

Runs (TMP/TEMP/TMPDIR on F:, 2026-10-23):
- `node --test injected/test/`: 56/56 pass in 135.6 s (jsdom, browser, media, accept-t19). The
  A3b busy cap held on the first run, so no rerun was needed.
- `cargo test -p sb-act --test wire_from_injected`: 3/3 pass.
- `cargo clippy -p sb-act --tests -- -D warnings` is clean, and so is `rustfmt --check` on
  wire_from_injected.rs.
- The fixtures register.index.json and register.text.json contain no canary or secret strings. The
  password row's value is `{"kind":"empty"}`.

I reproduced every finding below with jsdom probes in my session scratchpad. No repo files were
written for them.

## Round-1 items

| Round-1 item | Status | Where / test |
|---|---|---|
| B1: password value after a type swap | Fixed (D-405 (1), D-426). The WeakSet `S.sens`, `masked()` for -webkit-text-security and sensitive autocomplete all mark a field. The MutationObserver records `attributeOldValue`, so an old type of password is caught even before any walk. | injected.js:773, 1760-1767; jsdom.test.js:268 (instrumented getter, toggle, full re-walk), :310 (swap before the walk, masked field, OTP, masking removed); browser.test.js:343; accept A5b |
| B2: ids depend on call order | Fixed (D-405 (2), D-425, D-73). No id is issued before `walk({base})`, and every read returns `{why:"no base"}`. A later base is ignored. resolve answers stale below base and gone at or above next. | injected.js:254-283, 1192-1206; jsdom.test.js:356 replays the round-1 repro exactly; media.test.js:181 |
| S: URL substring "captcha" | Fixed (D-405 (3), D-424). Pages are matched by host, path or title. A challenge page yields one `document` CHALLENGE row, one text block and the self frame. | jsdom.test.js:398, :435; browser.test.js:303 |
| S: detection gaps (interstitial text, Arkose, GeeTest, response fields, title) | Fixed (D-405 (4), D-422). | jsdom.test.js:448; browser.test.js:303 (closed shadow root) |
| S: v3 badge over-flag | Fixed, and closed by B-402 / D-405 (5). A hidden bframe becomes CHALLENGE once it is visible. | browser.test.js:303 |
| S: aria-level not clamped | Fixed (D-405 (6)). The list_item level is also clamped to u8. | injected.js:966; jsdom.test.js:486 |
| S: no serde round-trip | Fixed (D-405 (10), D-428). The fixtures are committed, and a browser test diffs them against live output. The Rust test deserializes them, classifies Register as Account and classifies the CHALLENGE row as Challenge. | wire_from_injected.rs; browser.test.js:359 |
| S: settle runs to the cap on blank and PDF pages | Fixed (D-405 (8), D-427). | injected.js:1842; browser.test.js:268 (about:blank, script-less page, PDF embed, content_ms 1000 and 300) |
| S: html/body class toggle plus an infinite animation | Fixed (D-427). Attribute and text changes count only the node's own animations, and infinite animations are ignored. | browser.test.js:268 (anim.html) |
| S: media frames() has no budget | Fixed (D-405 (9), D-423). budgetMs defaults to 8000, truncated entries stay aligned to times, the loop stops after the first seek timeout, and load() runs for preload=none. | media.js:253; media.test.js:202 |
| S: media challenged() copy | Fixed. CHALLENGE and ids come only from `__sb`. | media.js:44-77; media.test.js:181 |
| S: form landmark | Fixed (D-405 (7)). | jsdom.test.js:494 |
| S: D-412 order vs B-5 | Closed by blocker B-403 (a proposal for supreme). | blockers.yaml |
| S: in-page copy of the D-49 aliases | Closed by blocker B-404 (a drift test once data/roles.txt exists). | blockers.yaml |
| N: act ignored row.challenge | Fixed. row.challenge now sets rec.challenge. | injected.js finishRow |
| N: S.recent held strong refs | Fixed with WeakRef and a 5 s prune. | injected.js remember / runningAnims |
| N: closest() for aria-disabled | Fixed with closestComposed. | injected.js isDisabled |
| N: delta index API, aria-labelledby re-dirty | Not taken. Both are listed in js-coder-2 not_done. | handoff |

I-11 fidelity: every shape in the I-11 notes and D-73 matches what the code returns:
- `{why:"no base"}` from id, index, text, fingerprint, state, name, frames and an un-based walk.
- resolve answers stale.
- The challenge-page shapes of index, text and frames.
- `__sbMedia`: list and cues may return `{why}`, frames entries may carry `truncated: true`, and
  budgetMs defaults to 8000.

Rules:
- No fixed sleeps. The new content-deadline timer and the media timers are capped condition
  deadlines, and each has a comment.
- No new value reads. The only `.value` reads are in fieldValue, behind `sensitive()`. The new
  `input` listener reads only caret and validity.
- No DOM writes and no main-world globals (A6 is green).
- A challenge page is never read inside: rowFor, nameApi and act all check selfCh.

## Blocking

- none

## Should fix

- injected.js:773, 1760 (D-405 (1) scope): a show-password that replaces the element still leaks,
  because sensitivity follows the element.
  - Repro: walk a page with `<input type=password name=pw>` holding a value. Then call
    `p.replaceWith(t)`, where `t` is a new `type=text name=pw_plain` input with the same value.
    The dirty walk's index() contains the value.
  - Older plugins and some React toggles do this.
  - Cheap partial fix: in onMut, a childList record that removes a sensitive input (or a subtree
    holding one) marks the inputs it adds as sensitive. replaceWith produces a single record.
  - Also extend SENSITIVE_NAME_RE with `\bpw\b|pw[_-]`.
  - Not Blocking: D-405 (1) is implemented and tested as written, and the outgoing vault scrub
    (rule 2) is the backstop for `@vault` values.
- injected.js:111 (D-422): "press and hold" and "are you human" over-flag ordinary UI text.
  - Repro: `<p>Press and hold Shift to select several files.</p>`,
    `<p>Press and hold the mic button to record a voice message.</p>` and
    `<h2>Are you human?</h2>` (a quiz) each produce a CHALLENGE row.
  - Cost: a normal page gets a challenge marker in the skeleton (D-404 (4)), which wastes a stop
    or a turn (D-7).
  - Fix: require a human, robot or bot word in the prompt for "press and hold". The PerimeterX
    prompt "Press & Hold to confirm you are a human" still matches, and `#px-captcha` is already
    caught as a box. Treat a lone "are you human" heading the same way, or require a widget nearby.

## Notes

- Two cases that nothing observes still leak the value (both reproduced):
  - a password input in an open shadow root whose type changes before the first walk (a shadow
    root is observed only once a walk reaches it);
  - an input created detached as `type=password`, given a value, switched to text, then inserted.

  Both need the value to be present before sb-driver's first walk, or the field to be built off
  the document, so they are rare. A one-line handoff note for sb-driver is enough.
- injected.js:1019 / D-422: when a challenge text block wraps more than two controls, it only sets
  rec.challenge. The controls stay ordinary rows in index() (flags.challenge false). act refuses
  them in the page, but sb-act classifies them by their own class. This is as ruled. The planner
  learns of the stop one turn later.
- injected.js:106: the title regex also matches "Just a moment" without the ellipsis. That
  over-flags, which is the safe direction.
- injected.js:1198: `walk({base: "5"})` is accepted because Number("5") is an integer. Harmless,
  though I-11 says the base is an integer.
- injected.js:901: `<search>` maps to landmark `form`. The IndexRow vocabulary has no `search`, so
  this is reasonable, but the D-405 (7) text does not mention it. Add it when D-414 is next amended.
- media.js:44: challengeRoots() runs a full `__sb.index()` on every list, cues and frames call
  (about 28 ms at 1 600 rows). That is fine at video.* call rates.
- media.js:210, 253: `load()` on a preload=none video, and a caller budgetMs above 10 s, are left
  to sb-media (B-4 #11). The default budget stays under the command timeout.
- Out of scope: reviewer-4 noted that injected.js SUBMIT_WORDS_RE does not carry the D-407 words.
  That still needs the sb-act/injected ruling.

## Verdict

Both round-1 Blocking items are fixed, and each has a test that replays the original repro. Every
round-1 Should item is fixed and tested, or closed by B-402..B-404. The code matches I-11 / D-73.
The two new Should items are a leak path outside the D-405 (1) ruling and over-flagging of
challenge text. Neither breaks a hard rule as ruled.

Verdict: approve

# Round 3 (reviewer-8): 9ed9218 "injected.js D-429 rebuilt password fields, challenge text"

Scope: injected/injected.js, injected/test/jsdom.test.js, injected/test/browser.test.js against
D-429, B-5, I-11 / D-73, hard rules 3, 5 and 6. Runs (TMP=F:/sb-swarm/tmp-page):
`node --test injected/test/` 61/61 pass, `cargo test -p sb-act --test wire_from_injected` 3/3 pass.
Probes were kept in F:/sb-swarm/tmp-page/r8 (not committed).

## Round-2 items

| Round-2 item | Status | Where |
|---|---|---|
| S1: replaceWith rebuild leaks the password | Fixed for same-batch rebuilds; tested in jsdom (getter never called) and Chromium (rebuild.html, getter counted in world sb) | injected.js:1807 inheritSensitive, 1828 flushMut |
| S2: press and hold / are you human over-flag | Fixed as ruled; jsdom and Chromium (hold.html) negatives and positives | injected.js:116-117, 377, 384 |
| N: walk({base: "5"}) | Fixed: typeof number and safe positive integer | injected.js:1233 |
| N: "Just a moment" title | Left as is (safe direction) | handoff not_done 1 |

Same-batch paths that are now closed (each probed, no canary in index/text/state/name/fingerprint):
replaceWith, remove + append into the same parent, insertBefore then remove, wrapper replaceWith,
`outerHTML =`, wrapper `innerHTML =`, cloneNode + detached type change + replaceWith, moving the old
node and then changing its type (sticky), replaceWith inside a walked open shadow root, and a
same-task read through state(el) or name(el) with no walk (rowFor -> ensure -> flushMut).

takeRecords: flushMut runs the taken records through onMut, so markDirty, remember, mutCount and
lastMut all still see them; nothing is dropped. It runs before the S.full check in doWalk, so a
flushed record that sets S.full still forces a full walk. The waiters it wakes only re-arm timers,
so there is no re-entry. The observer belongs to world sb, and A6 stays green.

## Blocking

- none

## Should fix

- injected.js:1807-1826 (D-429 (1) is too wide): the net breaks state on a common SPA flow.
  - Repro (Chromium): `#app` holds a login form (email plus password, both typed). Sign in
    calls `app.replaceChildren(main)`, where `main` has a search box, `<input value="1 Main St">`
    and `<textarea>gift</textarea>`.
  - After the dirty walk, Shipping address and Note read `{kind:"empty"}`, and a field the page
    filled reads `filled` instead of its text. They stay that way for the element's lifetime
    (S.sens is sticky).
  - So the planner sees a filled field as empty. It retypes the field or misreports it, which
    costs a turn and can corrupt data if type appends. The handoff gotcha says the cost is "only
    text values". It is also wrong `empty` evidence, because filledNoRead has no evidence on an
    untouched pre-filled field.
  - Proposed D-429 amendment for supreme: an added field inherits only if it looks like the
    removed sensitive field. Either it shares its id, name, aria-label, placeholder, autocomplete
    or label text, or it is a 1:1 input-for-input swap in one record. A show toggle copies those
    attributes; a route swap does not.
- injected.js:1807 (D-429 (1) scope): a same-task rebuild split by a microtask still leaks.
  - Repro (Chromium, reproduced): `o.remove(); await null; n.value = v; p.appendChild(n)`. The
    observer delivers the removal in its own batch before the append, so `n` is not inherited and
    the dirty walk's index() carries the value. Same in jsdom with queueMicrotask.
  - A later task fails the same way (setTimeout, or a Vue `<Transition mode="out-in">`). The
    handoff lists that case as out of scope.
  - Fix, once the identity match above is in: remember the parent and identity of each removed
    sensitive field for a short window (about 2 s, or N batches), and match later adds against
    it. That covers both cases cheaply.
  - Not Blocking: this is outside "same mutation batch" as ruled, async toggles are uncommon,
    and the vault scrub covers `@vault` values. A value the user typed has no backstop.

## Notes

- Other leak paths, by realism:
  - Two inputs toggled by display (moderate). A hidden text twin kept in sync by site JS
    and shown in place of the password: no node is removed, so nothing is inherited. Only the
    name pattern guards it. Old jQuery show-password plugins did this.
  - A sibling parent (low). The field is removed from one wrapper and added to a sibling
    wrapper.
  - A swapped custom element host (low). fieldsIn uses the light-DOM querySelectorAll, so an
    `<x-pw>` host whose shadow root held the password, replaced by a new host with a text
    input in its shadow, is not matched. Real components such as sl-input toggle the type
    internally, which is covered.
  - All three were reproduced in jsdom.
- injected.js:133: the new `\bpw\b|pw[_-]` only hits rare names beyond the intended ones
  (kpw-color, cpw_x, npw_2). Probed common names (power, upwork, pwa-install, spw, pw2) are not
  sensitive.
  - The older alternatives over-match far more: `otp` in footprint and hotpink, `ssn` in
    businessName, addressNumber and classname, `pass` in passenger, passport and compass.
  - Given the empty-evidence problem above, a pre-filled businessName reads `empty`. This is not
    new in this commit; word boundaries on otp and ssn would fix most of it.
- injected.js:116-117, 377 (D-429 (2)):
  - Lost: a bare "Press and hold the button" next to an unrecognised widget is no longer
    CHALLENGE. Before this commit it was. An Akamai-style "Let us know you're human" + "Press and
    hold" page with `#sec-cpt-if` now has no CHALLENGE row.
  - Over-flagged: "Press and hold the mic to talk to our chat bot" and "...to a human agent"
    still flag.
  - Cheap middle ground: accept hold text when `HUMAN_WORD_RE || S.chCtx`.
  - The real prompts still flag: Cloudflare "Verify you are human...", Arkose "Verify you're
    a human", PerimeterX "Press & Hold to confirm you are a human (and not a bot)", and the
    challenge-interstitial.html and challenge-widgets.html fixtures.
- injected.js:384: chCtx is true on every site that has a reCAPTCHA v3 badge or a
  `[data-sitekey]` submit (challenge-widgets.html #login). There, a quiz "Are you human?" heading
  is still CHALLENGE. Rare, and the safe direction.
  - challengeContext() runs a substring-attribute querySelector plus a pass over the frames on
    every walk, incremental ones included. That is cheap next to the walk.
  - It looks only at the light DOM. A widget inside a shadow root is still caught as a box.
- Pre-existing, not a regression: Amazon's "make sure you're not a robot" text is not in
  CHALLENGE_TEXT_RE. The `captchacharacters` id rule still flags its field.
- Rules 3, 5 and 6, and isolation:
  - No new timers or sleeps.
  - No new `.value` reads; fieldValue is still the only reader, behind sensitive().
  - No DOM writes and no new globals.
  - The handoff says each new test fails with its fix removed. I did not re-run those mutation
    checks, but reading the tests confirms each one asserts zero getter calls and no canary, not
    only that nothing panics.

## Verdict

Both round-2 Should items are fixed as D-429 rules them. Each has a jsdom test and a Chromium test
that replay the repro and count value-getter calls. No hard rule is broken as ruled, and I-11 /
D-73 shapes are unchanged.

Two Should items need one D-429 amendment from supreme:
- The inheritance net is wide enough to misreport every pre-filled field after an SPA login as
  `empty`.
- A rebuild split by an `await` still leaks a typed password.

Verdict: approve

# Round 4 (reviewer-9): 343a533 "injected.js D-435 match-based inheritance, D-436 v3 not challenge context"

Scope: 343a533 (injected/injected.js, jsdom/browser/accept-t19 tests, B-414 in blockers.yaml),
checked against D-429, D-435, D-436, D-437 (B-414 accepted) and I-11. Line numbers are for
343a533. The worktree also holds a tester's uncommitted `accept-t19.test.js` (A12, A13) and
three new `bench/pages/accept-*.html` files. I did not review them.

Gates: `node --test injected/test/` 75/75 (run before the tester's edits appeared),
`cargo test -p sb-act --test wire_from_injected` 3/3. Probes are in runs/reviewer-9/ (not
committed): probe-jsdom.js, probe-jsdom2.js, probe-chromium.js, probe-v3.js and mutants.js.
There is also a mutated mirror tree, runs/reviewer-9/mut, for running suites against a
mutant without touching injected.js.

## Round-3 items

| Round-3 item | Status |
|---|---|
| S1: D-429 net too wide (SPA login) | Fixed by D-435 (1) and D-437. jsdom and Chromium SPA tests show the pre-filled fields read their text. |
| S2: microtask or later-task split leaks | Fixed for remove-first rebuilds that share identity: jsdom and Chromium tests, with the 2000 ms boundary pinned on a frozen clock. Not fixed for add-first rebuilds (Blocking 1) or neutral identity (known, handoff not_done 3). |
| N: press and hold with challenge context | Done as D-435 (3); A11 and hold.html updated |
| N: v3 badge counted as context | Done as D-436 |
| N: otp / ssn over-match | Still open |

## Blocking

1. injected.js:1903-1949 (D-435 (2), hard rule 5): an add-first rebuild split across batches
   leaks, even when the new field shares name and aria-label with the password. inheritSensitive
   only checks fields added after a removal has been remembered. A field added before its
   sensitive twin is removed is never checked again.
   - Repro, Chromium (probe-chromium.js, fade.html): "Show" inserts
     `<input type=text name=acct aria-label="Account key">` after the password and copies the
     value. The old field is removed on transitionend (a 200 ms fade). framer-motion
     AnimatePresence (sync mode), Vue `<Transition>` (default mode) and Svelte outros swap
     elements in this order. After the old node is gone, index() and state() carry the canary.
   - jsdom (probe-jsdom2.js A, B): `o.after(n); await null; o.remove()` and
     `o.after(n); await tick(); o.remove()` both leak, with identical id and aria-label. The
     same order inside one batch is caught (A2).
   - Why Blocking: D-435 (2) says a rebuild split by a microtask or a later task within 2 s
     inherits by the same match. It does not limit the order, and what leaks is a typed
     password.
   - Fix: keep recently added non-sensitive input and textarea fields (WeakRef plus time, same
     2000 ms window and cap). When a sensitive field is removed, match its keys against them,
     then mark the matches sensitive and dirty. Add a jsdom test (microtask and later task) and
     a Chromium test (transitionend), each counting getter calls.

## Should fix

1. injected.js:1828, 1932 (D-437 swap count): fieldsIn counts every input type. A wrapper that
   also holds a show-password checkbox or a hidden input is therefore not a one-for-one swap
   (out = 2), and a replacement that shares no key reads.
   - Repro, Chromium (wrapcb.html):
     `<div><span>Password</span><input type=password><label><input type=checkbox> Show</label></div>`
     is replaced by the same markup with `<input type=text>`, and the canary is in index().
   - jsdom: D (checkbox) and D2 (a hidden sibling) leak too.
   - Fix: count only value-bearing fields in the swap. Exclude hidden, checkbox, radio,
     submit, button, reset, image, file, range and color.
   - Not Blocking: D-437 says "field", the code counts fields, and the case needs a neutral
     replacement.
   - Related, fieldLabels (1861-1876): label[for] is searched only in the document and in the
     removed node that holds the field. Sibling removed nodes of the same record are not
     searched. A wrapper innerHTML rebuild (label, input, checkbox) with new ids, on a field
     that was never walked, leaks (jsdom D5). Walked fields are saved by the row-name key (D4
     ok).
2. injected.js:1917-1921 fails open, not closed as the commit message says. If fieldKeys
   throws on the removed sensitive field, nothing is remembered, so a split rebuild in the
   window reads. The add side (1941) does fail closed.
   - Fix: push an entry that matches every field added within the window (for example,
     `keys: null` meaning match-all).
   - There is no test, and none is possible today (handoff not_done 4), but the fix is cheap.
3. Test gap, D-437 "lost exactly one field": no committed test catches relaxing
   `e.out === 1 && e.sens === 1` (1932) to `>= 1`.
   - Mutant M3 in the mirror: jsdom.test.js and the committed accept-t19 tests all pass.
   - Under M3, a two-field login replaced by a one-field page reads `empty` (mutants.js
     twoOutOneIn).
   - Add that case: user and password out, one pre-filled field in, and the new field reads
     its text.
4. injected.js:339 passiveFrame is pre-existing, but challengeContext (409-411) now uses it
   too. Its `[?&]size=invisible` test matches anywhere in src, including a `#hash`.
   - A visible hCaptcha challenge frame whose hash carries `&size=invisible` gets no
     CHALLENGE row and gives no context. probe-v3.js: 0 CHALLENGE rows next to
     "Are you human?".
   - I could not confirm offline that real hCaptcha challenge frames carry size in the hash.
   - Capture one, and limit the test to the query string of reCAPTCHA anchor frames
     (google.com/recaptcha, recaptcha.net). This is the hard rule 3 direction.
5. The overlap window of add-first swaps: during the fade both fields exist, and a walk reads
   the new one (Chromium fade.html: leak during the transition).
   - settle() normally waits out the transition before a read, so this is narrower than
     Blocking 1.
   - Fix it together with Blocking 1: at add time, match an added field's keys against
     connected sensitive fields in the same parent.

## Notes

- D-436 (challengeContext, 394-414): passive markers are dropped only as context for weak
  text. Vendor widgets keep their own CHALLENGE rows through challengeBox and the `captcha`
  name/id rule.
  - Checked: a v2-invisible widget whose bframe is shown at body level gives CHALLENGE rows
    (the bframe iframe, the heading and the hold text).
  - A badge alone gives none.
- 403: `tag === "script"` cannot trigger, because a script is never displayed. So the "bound
  script" cases (jsdom, Chromium) cannot fail because of it (mutant M1 survives). Harmless.
  Mutant M2 (drop the response-field rule) is caught by jsdom.test.js:454.
- Over-marking, which costs efficiency only:
  - An OTP field `name=code`, followed by an app page with a "Coupon" field `name=code`,
    reads `empty` (probe G).
  - A wrapping label that names two fields gives both the same `t:` key.
  - D-437's accepted cost: a one-field password step swapped for a one-field page reads
    `empty` (G2).
- The S.gone cap of 32 (1934) is untested and can drop the password. One batch removes a
  password plus 40 hidden `row_token_N` inputs (sensitive by name), then a later rebuild by
  id leaks (probe F, contrived). Should 1's filter, applied to S.gone too, closes it.
- A site's own hidden `g-recaptcha-response` field for a v3 token counts as context, because
  D-436 says response fields count. An "Are you human?" quiz on that page is then CHALLENGE.
  This is rare and the safe direction.
- S.gone holds `{t, keys}` only. Keys come from attributes, label text and row names, and
  textOf skips input, textarea and select, so no value enters a key. S.gone is never returned,
  logged or exposed, so rule 5 holds for the memory itself.
- Spec ids: D-435 (1)-(3), D-436 and D-437 are implemented.
  - Beyond the spec, all raising protection: aria-labelledby text and the walked row name as
    keys, AC_GENERIC, the cap of 32, and fail-closed on the add side.
  - I-11 shapes are unchanged. There are no new globals, and `__sb` is still frozen.
- Rules 1, 2, 6 and 7: no keys, no timers or sleeps added (entries are pruned on use), no
  drafts.
- pr-ready section 5:
  - No untrusted text reaches the model through the new code.
  - Fail-open: Should 2 and Should 4.
  - Key folding: whitespace is collapsed, case is folded, and a trailing `:` or `*` is
    dropped.
  - No vault paths.
- Still open from earlier rounds:
  - a neutral-identity split rebuild (probe C, handoff not_done 3);
  - a display-toggled twin input;
  - a sibling-parent move;
  - a custom-element host swap;
  - otp and ssn over-match.

## Round 4 verdict

D-435 and D-436 are in. Their tests count getter calls and pin the 2000 ms boundary, and
round-3 Should 1 is fixed. One hard rule 5 leak is still inside D-435 (2)'s wording: an
add-first rebuild split across batches.

Verdict: request changes
