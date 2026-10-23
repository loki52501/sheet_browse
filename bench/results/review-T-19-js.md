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
