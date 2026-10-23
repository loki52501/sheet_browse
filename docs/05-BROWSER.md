# 05 · Browser: sheetbrowse v2 (crates `sb-cdp sb-driver sb-sheet sb-act sb-media sb-mcp`, `injected/`)

A small driver built for a model, on CDP, borrowing five ideas from Playwright
(isolated-world script, actionability checks, auto-wait, event-driven target tracking,
real input) and skipping the rest. Items are `B-n`.

## B-1 Shape

```
sb-mcp     tools: open find sheet read act tabs screenshot video.* eval cdp network
sb-sheet   skeleton · find ranking · sheet lines · read renderer (pure)
sb-media   video frames, contact sheets, screenshots
sb-act     act DSL parser + gate classification (pure)
sb-driver  registry · act+retry · input · consequences · settle · scheduler · recovery
sb-cdp     Transport trait: Pipe (default) | WebSocket | ElectronNapi | ExtensionRelay
injected/  injected.js (world "sb" in every frame) · media.js
```

## B-2 Chromium and profile

- Launch our own Chromium (download pinned revision on first run) with
  `--remote-debugging-pipe --user-data-dir=<profile>`.
- **Profiles:** `bench` (fresh, headless allowed) and `me` (persistent, headed, where the
  user has logged into Gmail, Calendar, Luma once by hand). Mail and calendar use `me`.
  Headless is never used with `me`.
- No stealth flags, no UA override, no fingerprint changes (D-5).

## B-3 Transport (`sb-cdp`)

```rust
trait Transport { async fn send(&self, msg: Bytes); fn recv(&self) -> Receiver<Bytes>; }
```
One writer task, one reader task, `HashMap<u64, oneshot::Sender<Value>>` for replies,
flattened sessions (`Target.setAutoAttach{flatten:true}`), pipelined sends. Events are
fanned out on a broadcast channel keyed by session. Backends: Pipe first; WebSocket for
attaching to a running Chrome; ElectronNapi and ExtensionRelay are stubs with the trait
implemented and `todo!()` bodies until needed.

## B-4 The 11 driver components

| # | Component | Critical | Algorithm |
|---|---|---|---|
| 1 | Transport | | above |
| 2 | Target & frame registry | ★ | state machine fed only by `Target.attachedToTarget/detachedFromTarget`, `Page.frameAttached/Navigated/Detached`, `Runtime.executionContextCreated/Destroyed`; `openerId` → "opened by tab N"; per-frame generation counter so stale ids are rejected; zero round trips to look up |
| 3 | Runtime install | | `Page.addScriptToEvaluateOnNewDocument(source, worldName:"sb")` once per target; later calls are `Runtime.callFunctionOn` on `globalThis.__sb` in that world |
| 4 | Element identity | ★ | in page: `WeakMap<Element,id>` + `Map<id,WeakRef>`; same element same id; ids scoped per frame `f2.15`; gone → `{why:"gone"}` |
| 5 | Query engine | | skeleton / find / sheet / text from one in-page index, refreshed by a MutationObserver dirty set |
| 6 | Actionability | ★ | in page `act(id)`: resolve → connected → scrollIntoViewIfNeeded → visible (box > 0, not hidden) → stable (same box across 2 rAF, awaited in-call) → enabled → hit test (`elementFromPoint` at centre) → `{x,y}` or `{why}`; Rust: on why, `await settle()`, retry at 0·50·100·250·500 ms then 500 ms steps, 5 s cap; final why becomes the step error |
| 7 | Input synthesis | | click: `Input.dispatchMouseEvent` move/press/release pipelined; type: focus + Ctrl/Cmd+A + `Input.insertText`; select: set `value` + dispatch `input`,`change`; check: click; upload: `DOM.setFileInputFiles`; keys: `dispatchKeyEvent`; vault resolved only into `insertText` |
| 8 | settle() | ★ | in page promise: no DOM mutation for 150 ms AND no in-flight requests (Rust tracks `Network.requestWillBeSent/loadingFinished/Failed`) AND no running CSS animation on changed nodes; cap 3 s; returns `{settled: bool, changed: n}` |
| 9 | Consequence capture | ★ | after each act step, listen ≤ 500 ms or until settle for: `Page.frameNavigated` (main) → "navigated to"; `Target.attachedToTarget` with opener = this tab → "opened tab N: title"; `Page.javascriptDialogOpening` → "dialog: text" (paused); `Page.downloadWillBegin` → "download: name" (stopped, asks); none and empty diff → "no visible change" |
| 10 | Scheduling | | one task per tab, per-tab mutex, tabs in parallel, max 6 live tabs, extra `open()` queues; independent reads batched |
| 11 | Errors & recovery | | `Inspector.targetCrashed` → report; `Runtime.executionContextDestroyed` mid-call → retry once in new context; command timeout 10 s → cancel caller with reason; browser exit → relaunch, registry rebuilt, tabs reported lost |

Build order inside the browser: 4 → 6 → 9 → 2 → 8 → 1 → 5 → read → media → forge.

## B-5 injected.js

Functions on `globalThis.__sb` (world "sb"): `walk(dirty?)`, `id(el)`, `resolve(id)`,
`name(el)`, `role(el)`, `state(el)`, `index()`, `skeleton()`, `find(q, k)`, `sheet(region)`,
`text(region)`, `act(id)`, `settle(opts)`, `fingerprint(spec)`, `frames()`.

- **walk:** `TreeWalker` in document order; invisible containers skipped with their
  subtree (`nextSkippingChildren`); enters open shadow roots; same-origin iframes are
  separate frames with their own copy; cross-origin iframes become one opaque row.
- **role:** ARIA role → tag → behaviour (`div` with click handler + `cursor:pointer` → button).
- **name ladder:** aria-labelledby → aria-label → `label[for]` → wrapping label →
  placeholder/title/alt/name → own text (skipping aria-hidden) → words to the right
  (checkbox/radio) → nearby text ≤ 200 px above/left. Consumed label text is not
  emitted again as a text row.
- **state:** visible, enabled, checked, value (password → `empty|filled` only), submitish,
  opens new tab, options for select (capped).
- **flags:** SUBMIT, CHALLENGE (recaptcha/hcaptcha/turnstile frames or containers),
  OVERLAY (fixed/sticky and covering), OPAQUE (cross-origin frame).
- **collapse:** interactive element owns its subtree; container emits only its own text
  nodes; a plain table row folds to `a | b | c`.
- **incremental:** MutationObserver marks dirty subtrees; `walk(dirty)` re-walks only
  those; untouched ids unchanged.

## B-6 Read modes (`sb-sheet`)

**B-6.1 skeleton** (on `open`, ~80–150 tokens): title · URL · landmarks · each form
(field count, submit name, section heading) · h1/h2 · counts (elements, links, buttons,
fields) · ⚠ CHALLENGE / OVERLAY / dialog · `tools for this site:` line (T-3) ·
content hint (`main: article 2 400 words, 3 tables` or `main: form`).

**B-6.2 find(q, k=3 per part).** Query split on commas. Per part, per element:

```
score = 3·BM25(name) + 1.5·BM25(nearby) + 0.5·BM25(heading)
      + 2 if a query verb/noun maps to the element's role
      + 1 visible / −2 covered or disabled
      + 1 if in the form that won the most top-1 hits across parts (group bonus)
```
BM25 with k1 = 1.2, b = 0.75, IDF computed over the page's index. Synonyms
(`data/synonyms.txt`, ~200 pairs) expand the query before scoring. Role prior table in
`data/roles.txt`. Best score < 3 → line marked `?`. Always appended: CHALLENGE rows,
OVERLAY rows, the winning form's submit. Output in document order. `find("name:input")`
forces a role. Scoring runs in the page; only winners cross the wire.

**B-6.3 sheet(region)**: every element in a region, numbered, document order, no cap
(the caller asked for all of it). `region` is a landmark, a form id, or a heading.

**B-6.4 read(tab, region?, cursor?, max_tokens?)**: W-5 renderer on the live index with
`text(region)` blocks. Infinite-scroll pages: scroll + settle until `end` or budget.

## B-7 act DSL (`sb-act`, pure)

```
t#7=hello world        type (replaces)        Fill
t#7=@vault(pw)         type a vault value     Fill
s#12=Plus              select by label/value  Fill
c#14                   click / check          Navigate|Submit|Pay|Account|Delete by target
k#7=Enter              key                    Fill (Enter on a form → Submit)
u#9=files/cv.pdf       upload                 Fill
sc(down|up|#N)         scroll                 Navigate
w(3s)                  wait with settle       Read
?s                     stop here, return state (required before a Submit in planners)
```
Steps are classified individually; the program's class is the max. A Submit target is
detected from SUBMIT flag + form context; Pay from card/IBAN fields in the same form;
Account from password/email-change fields or "create account" text; Delete from
delete/cancel/unsubscribe text.

## B-8 act(program) result

```
step 1 t#12  ok
step 2 t#14  ok
step 3 c#19  ok · navigated to /welcome · settled
diff: + h1 "Welcome, Lokesh" · − form f2
```
or on failure: `step 3 c#19  failed: covered by div.cookie (OVERLAY #31) after 5.0 s`.
Never an empty success.

## B-9 sb-media

- `video.list(tab)` → id, duration, source kind (file | MSE | DRM), captions, visible.
- `video.transcript(tab, v)` → text tracks or site transcript if present.
- `video.frames(tab, v, from, to, every=1s, size=512, dedupe=true)` → frames in
  `runs/frames/`, 9-up contact sheets with timestamps, `manifest.json`. Source order:
  direct file via ffmpeg → in-page seek + canvas → element screenshot while playing.
  DRM → reported as protected, no frames. Dedupe by perceptual hash.
- `video.frame(tab, v, t, size=1280)` one sharp frame.
- `video.watch(tab, seconds, fps=2)` screencast of the viewport.
- `screenshot(tab, id|region|full)`.
Captcha frames are never captured (D-5).

## B-10 Escape hatches

`eval(tab, js)` in world "sb", JSON result, classified by a static check of the source
(`fetch(`, `.submit(`, `.click(` raise the class). `cdp(tab, method, params)` any
command, classified by domain (`Input.*`, `DOM.setFileInputFiles` → Fill; `Page.navigate`
→ Navigate; `Network.setCookie`, `Storage.*` → Account). `network(tab)` recent
requests/responses, bodies capped. All three go through the gate.

## B-11 Bench parity

`bench/` keeps the 30 web tasks and the hard set from the old repo. A run records
success, steps, seconds, tokens, no-op acts, challenge stops. The target to beat is the
old sheetbrowse (10/30 web, 40/80 no-op acts) and then browser-use (13/30).

## Acceptance (step 6, in order)

1. Identity: same element → same id across 3 walks with unrelated DOM changes; removed element → `gone`.
2. Actionability: covered button → `{why:"covered by …"}` in < 50 ms; after removing cover, retry succeeds; disabled → `disabled`; 5 s cap respected.
3. Consequences: `target=_blank` link → "opened tab 2" in the same result; `alert()` → "dialog" paused; download → stopped and asked; no-op click → "no visible change".
4. Registry: popup, same-origin iframe, cross-origin iframe all present with correct opener/parent after a scripted test page.
5. settle(): SPA that renders 400 ms after load → skeleton not empty; fixed 3 s cap never exceeded.
6. find(): on `bench/pages/register.html`, `find("email, password, agree, register")` returns #ids of the right four plus the captcha and overlay rows, in document order.
7. read(): article page → markdown without nav; chunk cursors work.
8. media: a local mp4 → 1 fps frames deduped to scene changes; contact sheet rendered.
9. Bench: ≥ 13/30 web, no-op acts < 10 %, zero vault leaks, zero captcha attempts.
