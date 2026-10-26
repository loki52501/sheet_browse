# Review T-21, round 1 (area driver)

Scope: `git diff 02ce736..HEAD`, which is commit 8deaa56 plus handoff 4064360. That covers act.rs,
input.rs, consequence.rs, diff.rs, and the T-18/T-20 edits (lib.rs D-610/D-612, runtime.rs,
actionable.rs ACT_FN, the probes in accept_t20/chromium_t20, sb-cdp start_raw and the pipe
writer). I did not run cargo, per the brief.

## Blocking

- Rule 2 and pr-ready 5 ("split or truncated"): page strings are capped before they are scrubbed, so a typed vault value that crosses a cap leaks its prefix. `page_text` cuts mid-word at 120 characters (act.rs:740, 752, 758) and at 60 for diff names (diff.rs:18, 242). `clean_url` cuts at 200 (diff.rs:101). `scrub` (act.rs:895-929) runs after that, and `spellings` (act.rs:844) only matches whole values. Example: a heading `Signed in as <long @vault(email) value>` past 60 characters shows its first characters in `diff:`. The page index drops a partial last word when it truncates (injected.js:234), but the driver's cap does not. Fix: scrub the raw string before `page_text`/`clean_url` cap it, or replace a value prefix (8 or more characters) that ends at `...`. Add a test with an echo that crosses the cap.
- Rule 4: a key or type step can act on an element the gate never classified. In ACT_FN `runOp` (actionable.rs:57-61), `focused` is `a === el || el.contains(a)`. `el.focus()` does nothing on a non-focusable row, so if focus already sits on a descendant, the check passes and the input goes to that descendant. Example: a dialog that autofocuses its "Pay now" or "Delete" button, then `k#<dialog>=Enter` or `=Space`. The gate classifies the dialog row (sb-act `enter` on a row outside a form is a click on that row, classify.rs:355-359, so Navigate), and Enter then presses the Pay or Delete button under a Navigate approval. The same gap lets `t#<container>=` type into a focused password field inside the container, which is above Fill. Fix: require the deep active element to be `el` itself (or inside `el`'s own shadow root), or re-gate against the focused element's row. Add a live test.

## Should-fix

1. Dialogs between steps. `open_dialog` is checked only before step 1 (act.rs:262). The per-step index read (act.rs:403, 411-421) and actionability (act.rs:435-490) are not raced with `or_dialog`. A dialog from a timer after the previous settle causes a stall of up to 3 s (index) or 5 s (actionability). Then the gate either stops with a misleading "step N is Submit ... above the approved" (no index means Submit), or a failed step. Fix: check `open_dialog(tab)` before each step (zero round trips) and race the reads with `or_dialog`.
2. Download re-fetch. When `Network.loadNetworkResource` returns `success: true` with a non-2xx status, the error body is saved under the download's name (consequence.rs:566). Example: a 405 from a POST-only export (B-608). Refuse when `httpStatusCode` is not 2xx. Also drop `file` from the allowed schemes (consequence.rs:537): a page never needs it, and it would copy a local file into `runs/<id>/files`.
3. Download names and page text keep Unicode format characters. `safe_name` (consequence.rs:265-274) and `page_text` (diff.rs:32) drop only `is_control()`. So U+202E (RLO), zero-width characters and other bidi controls survive: a saved `report<RLO>fdp.exe` shows in Explorer as `reportexe.pdf`. Strip general category Cf. Minor: `COM¹`-`COM³`, `CONIN$` and `CONOUT$` are also reserved on Windows.
4. The T-20 probe change makes row 5 unable to fail on extra hit tests. `if (P.hits.length >= P.starts.length) return hit;` (accept_t20.rs:267, chromium_t20.rs:195) caps hits at the number of starts, so "one hit test per attempt" can no longer catch 2 per attempt. Count only hits between an attempt's start and its answer instead: ignore hits before the first start and after the last answer.
5. A step whose target is not in a fresh live index (a stale id after a navigation) is reported as a gate stop with class Submit (`StopReason`, act.rs:294). It should be a failed step such as "not on the live page". Both fail closed, but the stop invites sb-mcp to ask the user to approve Submit for a step that cannot run, which costs a turn.
6. Missing tests through `Driver::act`:
   - `k#N=Enter` on a field in a form under Fill stops before any key event;
   - `u#` is gated (for example under Read, or on a CHALLENGE row);
   - the focus check rejects a focused descendant (Blocking 2);
   - a long echo crosses the cap (Blocking 1).
7. `NoVisibleChange` can be wrong until T-22. The window's settle tracks the DOM only, so a submit click whose XHR takes more than 500 ms reads as "no visible change", which invites a retried submit. Until network tracking lands, do not emit it for steps whose live class is Submit or higher, or report it as "unsettled".

## Notes

- Vault wire path, traced:
  - The value is resolved once in `ctx.resolve_into` (input.rs:366). It is written into a `Zeroizing` buffer pre-sized at 6x+16, so it never reallocates.
  - `start_raw` builds an exact-capacity `Wiped` message (client.rs:295-309), which is zeroed when the last `Bytes` handle drops.
  - The pipe writer zeroes its framing `Vec`, whole capacity included, after each write (pipe.rs:106-120). There is no `BufWriter`.
  - Errors name only the method (`Input.insertText`) or the vault entry.
  - The mock test (lib.rs `a_vault_value_goes_only_into_the_insert_text_message`) asserts that exactly one wire message carries the canary.
- Remaining memory-hygiene gaps (B-152, accepted): temporaries in `spellings()`, the page strings `scrub` replaces, and `IndexRow` `FieldValue::Text` of non-password typed fields are held unzeroed.
- B-611 is acceptable: sb-driver never builds a WebSocket transport.
- Gate (rule 4, H-1.4): every step except `w()` and `?s` reads or reuses its frame's live index and is classified before any focus, scroll or input (act.rs:291-304). That includes `u#` and `sc()`. With no index, every target is Submit, and a CHALLENGE row or page always stops; unit and live tests cover both. Enter in a form is Submit through sb-act (classify.rs:349-369).
- B-612 (stop rather than ask) is acceptable. It fails closed, and sb-mcp can turn `stopped` into an ask. Supreme should align the `ToolCtx::ask` doc comment (core mcp.rs:355-356) with it.
- Rule 3: the gate runs before actionability, so a challenge row or page is never focused, scrolled or clicked; the live test checks the title is unchanged. No solver or evasion code is added.
- Upload containment does not fail open. Paths are refused on `..`, a root, a drive (including `C:x`), UNC or verbatim components. Both the root and the target are canonicalized, which covers symlinks, junctions, 8.3 names, case and `..::$INDEX_ALLOCATION`, and the target must be a file. Remaining gaps need local write access to `files/`: hard links, and a swap between the check and Chrome's read at submit time.
- Downloads: deny is set at launch, so nothing lands before `download(allow=true)`. The live test checks `canceled`, the home folder and `~/Downloads`, and that a refused download is never fetched. `create_new` claims the name and a `.part` file is renamed onto it. B-608 (blob:, POST, one-time links; the re-fetch is a second credentialed request) is acceptable as a default. Under Profile::Me, deny also blocks the user's own manual downloads in that browser while the driver is attached.
- Consequence window (B-4 #9): the 3 s wait for a pending main-frame navigation is a condition wait, capped at B-4 #8's 3 s settle cap. It avoids a false "no visible change" and a wasted turn, so it is acceptable. D-613 records it. The in-page settle is DOM-only until T-22.
- Dialogs: the `or_dialog` race is correct. The observer logs the event before `bump`, it is biased toward the future, and no in-page call is made on the tab after a dialog in the window; the diff is skipped. The gap between steps is Should-fix 1.
- Untrusted text: titles, dialog text, download names, guids, URLs, diff names and form ids are escaped (`"`, backticks, `[ ]`, ref-like `#`, newlines) and capped. The diff cannot close its quotes (unit test). Errors from page calls go through `tidy_why`.
- Rule 5: the new in-page code (ACT_FN ops, SCROLL, UPLOAD, TITLE) reads no field values. The diff prints no values, and password rows count only as empty or filled.
- Rule 6: no new sleeps. All waits are conditions with caps.
- Rule 1 and rule 7: no keys, no `.env`, no em dashes. U+2212 in the diff is B-8's minus sign.
- D-612 applies to every tracked session (OOPIFs, prerenders), not only popups. T-22 acceptance 4 should confirm that a cross-origin iframe's first document still gets the sb world.
- D-610 under Profile::Me navigates the user's only blank tab. This is acceptable, but worth a line in B-606.
- Input limits for a follow-up:
  - cross-origin frames answer `cross-origin frame`, so steps cannot target controls inside cross-origin iframes (payment and login iframes);
  - the frame offset ignores CSS transforms and zoom;
  - `NoVisibleChange` compares only the step's frame;
  - `o.selected = true` adds to a `<select multiple>` instead of replacing.
- H-1.4 "marks the tab" is not in sb-driver. sb-mcp or T-25 should own it.
- Interfaces: the I-9 signatures are unchanged (`dialog`, `download` filled in). `start_raw` on I-8 is additive and pending supreme (B-609); contract_paths compiles both paths.
- Spec ids in 8deaa56 match the code (B-4 #7, #9, B-8, H-1.4, D-608..D-614). B-4 #8's network settle is T-22, and the handoff says so.
- Tests: acceptance 3 rows a to e assert the behaviour, not just that it runs (opened tab 2 and its title, dialog paused with later steps not run, a download canceled then saved byte-exact, `NoVisibleChange`, navigated plus diff). The coder reports seeing the mutation tests go red. The workspace test run is still owed (handoff `not_done` 1).

## Verdict

There are two rule breaks: a typed vault prefix can survive the caps (rule 2), and a key or type step can reach an unclassified focused descendant (rule 4). Both are small, local fixes, each needing a test.

Verdict: changes
