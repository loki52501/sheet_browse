# Review T-21, round 2 (area driver)

Scope: `git diff 6ca7d99..HEAD`. That is commit d895155 (code) plus cdaa3b2 (handoff), with D-615 to D-619, B-613, B-614 and the new `crates/sb-driver/src/scrub.rs`. I did not run cargo, per the brief.

## Blocking

- none

## Round 1 Blocking, closed

- Blocking 1 (rule 2, scrub before cap) is closed in sb-driver.
  - `finish` (act.rs:1001) scrubs every raw page string before any escape or cap: Navigated URL, OpenedTab title, Dialog text, Download name and guid, and the diff rows' `name` and `form`. It then renders, then scrubs every shown string again, including whys and the stop why.
  - The caps that run before the raw pass all leave a cut the prefix rule (scrub.rs:98-140) sees:
    - the event log's 2000-character `raw()` (consequence.rs:81) and `tab_title` (act.rs:818) cut at the end of the string;
    - the index's 150 cut ends at `…` (injected.js:234);
    - the actionability whys' `neutral` and WHY_CAP cuts end at `...`, and the second pass matches them through the `neutral` and `why` spellings (scrub.rs:46-65, which mirror actionable.rs neutral and tidy_why).
  - No `return Ok` in `act()` skips `finish` (the only early exits are `Err`). The diff's role alias (diff.rs:304) is not in the raw list, but it is capped at 20 with `...`, so the second pass covers it.
  - `tabs()` and `frames()` (lib.rs:669, 725) and `download()` (consequence.rs:531) use core's whole-value scrubber only. Their strings are not capped by the driver first, so the round-1 leak cannot happen there. Prefix cuts made by the page itself on those paths are B-613 (core).
- Blocking 2 (rule 4, exact focus) is closed for light DOM and open shadow roots.
  - `focused` is now `a === el || delegated(el, a)` (actionable.rs:86). The `delegated` exception needs all of: `op.text === true` (t# only), `el` itself being an open delegatesFocus host, and the deep active element being an input of type text, search, email, url, tel or number, or a textarea.
  - Password is excluded by the focused element's own `type`. The isolated world does not see page overrides of that property.
  - Nested shadow roots: `ownShadow` (actionable.rs:67-77) walks up through hosts, so a text field in a component nested inside `el`'s own shadow tree passes, by design ("own shadow tree", D-615). It is still never a password field. A delegatesFocus host that is a descendant of `el`, and slotted light-DOM children, cannot pass, because `el.shadowRoot` must be the root that is reached.
  - A failed focus returns `NO_FOCUS` before `type_text` or `dispatch` (act.rs, start_step). The live test (accept_t21.rs:936) covers the autofocused Pay button with `k#dlg=Enter`, and a focused password field in a container with `t#box=x`. It asserts zero keydown events and an empty password field.

## Should-fix

1. Rule 4 residual: closed shadow roots. `deepActive` (actionable.rs:61) cannot descend into a closed root, and `document.activeElement` is retargeted to the host. So for a host with a closed shadow root and an autofocused inner button, `el.focus()` is a no-op, `a === el` holds, and `k#<host>=Enter` presses the inner button. That is the round-1 Pay-button vector again, and with delegatesFocus the same applies to a closed inner password field for t#. I did not make this Blocking because clicks on such a host have the same opacity: the gate classifies the host row and the click lands inside it. Cheap fail-closed fix: for k# and t#, when `a === el`, ask CDP `DOM.describeNode({objectId, depth: 1, pierce: true})` and fail the step if `el` has a `closed` shadow root. Restrict the call to possible hosts (custom elements, plus the tags `attachShadow` allows) so most steps pay no round trip. Add a live test.
2. No test covers the delegatesFocus exception. Add one live case for each side: `t#<open delegatesFocus host>` with an inner text input types into it, and the same host with an inner password input fails with `NO_FOCUS` and leaves the field empty. Today a mutation that drops the type check would stay green.
3. The status check is untested. `a_refetch_with_an_error_status_is_not_saved` (accept_t21.rs:1080) passes on `ERR_HTTP_RESPONSE_CODE_FAILURE` alone, so removing the new `httpStatusCode` check (consequence.rs:628) stays green. Add a mock-CDP unit test that answers `success: true, httpStatusCode: 405`, and one for a `file:` URL refused (consequence.rs:585).
4. Round-1 item 7 is carried over (B-614). The frame part is done (D-617, act.rs:405). For frame 0, the cheap interim is still open: do not emit `NoVisibleChange` when the step's live class is Submit or higher, or tag it unsettled. The class is already computed by `classify_step` before the step. A false "no visible change" after a slow submit invites a second submit, and that costs more than one condition.
5. Dialog mark ordering. `open_dialog` is checked (act.rs:304) before `events_mark` is taken (act.rs:320). A dialog logged between the two is not seen by `or_dialog`'s `dialog_since(mark)`, so the step stalls to the read or actionability cap before it reports the dialog. The window is tiny. To close it, take the mark first, then check `open_dialog`, or have `or_dialog` also test `open_dialog(tab)`.

## Notes

- Frame scroll (row 10b), actionable.rs:118-161.
  - Each ancestor level re-checks `inside(p, vp(pw))` after the scroll, and the hit test must return `fe` through open shadow roots. The final point is checked against the top viewport for every frame, frame 0 included, so no out-of-viewport point is dispatched.
  - Both new whys are `Retry` in `classify_why` (actionable.rs:225, 228), so retries stay inside the existing T-20 budget loop and its deadline. No new timer.
  - A frame element inside a closed shadow root in its parent now fails as `covered by <host>` until the cap. That is a false failure, but it fails closed, and it is rare.
- The stale check (D-618, act.rs:452-475) is a read-only resolve. It is skipped on a CHALLENGE and only turns `stale` or `gone` into a failed step. `live` and any doubt keep the Submit stop. It fails closed.
- Cf stripping (diff.rs:27-52) matches Unicode 15.1 Cf exactly, and format characters do not count toward the cap. It also drops U+200C/U+200D (ZWNJ/ZWJ), which slightly alters Persian and Indic text and splits emoji ZWJ sequences in shown text and saved names. This is acceptable for display, and the comparisons run on raw rows.
- `clean_url` now keeps any `@vault(name)` literal unencoded (diff.rs:105, 134), including one the page wrote itself. Page text could already contain such a literal, so this adds no new channel. It can at most mislead the model.
- Scrub cost and termination: `cut_prefix` anchors on 8-character heads, and `MAX_REWRITES` withholds the whole string when it trips (fail closed). Matching is ASCII case-insensitive at char boundaries. The unit tests cover `...`, `…`, the end-of-string cut, non-ASCII text and the withheld case.
- The T-20 probes now count `index()` hit tests by stack frame (accept_t20.rs, chromium_t20.rs). Act's own hit tests count again, and the coder reports the extra-hit mutation red.
- Rule 6: no new sleeps. The only timer is still the documented `sleep_until` slot (actionable.rs:670). Rules 1, 3, 5 and 7: nothing new. No keys, no solver, no HTML or pixels, no user drafts.
- Interfaces: no public signature changed (`tabs`, `frames` and `download` keep their I-9 shapes). Spec ids in d895155 (B-4 #7 #9, H-2, D-615..D-619) match the code.
- Tests: these can fail:
  - the long echo (index `…` cut, URL `ends_with("&v=@vault(long)")`, title, alert);
  - the focused descendant;
  - Enter in a form under Fill, u# under Read and on a CHALLENGE row, and the stale id;
  - the dialog between steps (under 2.5 s against the 5 s cap);
  - `tabs()` and `frames()` with a GET echo.

  The round-1 Should-fix items 1, 2, 3, 5 and 6 are done, item 4 is done, and item 7 is partly done (Should-fix 4 above). `unwrap` appears only in tests. Clippy and the workspace run are owed to the tester.
- B-613 (prefix rule and `ToolCtx::scrub` in core) is the right home for values typed in earlier programs, and for page-side cuts on `tabs()` and `download()`.

## Verdict

Both round-1 Blocking items are closed within sb-driver's reach. The remaining items are a closed-shadow-root residual that has the same opacity clicks already have, two untested branches, and one deferred interim guard. Each is a small follow-up.

Verdict: approve
