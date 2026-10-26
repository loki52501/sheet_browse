# Review T-12 confirm round 2 (srv-web, docs/04 W-1..W-7)

## History

- **Round 1** (579f466, on dbacdbb): request changes. 1 Blocking (rule 5: HTML under a non-HTML or missing content type reached the model), 6 should-fix.
- **Round 2** (99ed4ea, on b9ccca5): request changes. Rule 5 still open (sniffer missed UTF-16, unclosed comments, prefixes, doctype spacing, fragment tags).
- **Closing round** (0afe399, on b2d08fb, D-508): request changes. 2 Blocking rule-5 items: `<option>` labels carried script/template source, and iCal and feed URLs reached the model verbatim.
- **Final round** (3962045, on 3b750b2): request changes. 1 Blocking: `collect_text` read raw `<iframe>` text into names.
- **Confirm round** (b74fd54, on 4913651): request changes. Iframe item closed. 2 new Blocking items in the textarea branch of `collect_text`: (1) a label's own textarea content, secrets included, joined the label name; (2) a double entity decode put `<script>` into a name.
- **Confirm round 2** (this file, on 06fbbf3).

## Confirm round 2: 06fbbf3

The fix is in `crates/srv-web/src/html.rs:1033-1037`. `collect_text` now returns on `textarea` before doing anything else, so a textarea adds nothing to any accessible name on any path. That includes `aria-labelledby`, where `skip_controls=false`. The new test `html::tests::textarea_content_never_reaches_a_name` (`html.rs:1468`) checks every name and value in the index and every block for each of these: the three secrets, the card number, and `<script`. It also checks that the name is exactly `X` and that all six expected names are present.

Checks run in the worktree with `CARGO_TARGET_DIR=F:/sb-swarm/target-servers`:
- `cargo fmt --check -p srv-web`: clean.
- `cargo clippy -p srv-web --all-targets -- -D warnings`: clean.
- `cargo test -p srv-web -j 2`: all green.
  - lib 20 (+1, the new test);
  - accept_t12 5 (+1 ignored), accept_w 6 (+1 ignored), contract_paths 1, live 0 (+3 ignored);
  - round2_t12 7, round3_t12 10, web_tools 13.

Probes ran through `fetch` in read and sheet mode, 29 pages in total. They ran in a `git archive 06fbbf3` export at `runs/review-t12-c2/`, which was deleted afterwards. The ledger dump was checked for secrets too.

**Confirm Blocking 1 (textarea content in label names): closed.** Results, in read and sheet mode:
- `<label>Pass <textarea autocomplete=current-password>` gives `#1 input "Pass" filled`.
- `label for=` gives `"Pass2" filled`.
- `cc-number` gives `"Card" filled`.
- An ordinary textarea gives `"Comment" "prefilled body text"`. The value now appears once, as the value.
- Upper-case `AUTOCOMPLETE="Current-Password"` gives `"Up" filled`, and `section-x billing CC-NUMBER` gives `"Cc" filled`. `attr_lower` handles case.
- A secret textarea inside `aria-labelledby` gives `"Secret" empty` for the input and `filled` for the textarea.
- A label holding a secret textarea and used as a button's `aria-labelledby` gives the button `"Lab"`.
- `aria-labelledby` pointing straight at a secret textarea gives an empty name.
- A secret textarea inside a link, a button, an option, a summary or an `onclick` div: the names are `Go`, `Do`, `Pick`, `Sum`, `Ck`, and the value shows as `filled`.
- No `hunter*` string and no card number appears in any output or in the ledger.

**Confirm Blocking 2 (double decode): closed.**
- `&amp;lt;script&amp;gt;...` inside an `aria-labelledby` span gives the name `"X"`. The textarea's own value is `"&lt;script&gt;stealCookies()&lt;/scri..."`, decoded once, as a browser shows it.
- The `&amp;lt;!DOCTYPE html&amp;gt;` variant gives the name `"Y"`.
- No `<script` or `<!doctype` appears in any output.

**Textarea value path** (`html.rs:798-807`): the value goes through `no_markup`, and `secret` is checked before `Text`. A secret textarea always shows `filled` or `empty`, never its text: `aria-label=Pw autocomplete=current-password` gives `"Pw" filled`. A textarea holding a whole document gives `empty`, because `no_markup` strips it. That is safe.

**Iframe fix from 4913651: still holds.** The iframe was placed inside a link, a button, a `label for`, an `aria-labelledby` span, an option, a titled link, a figcaption, `<iframe role=button title=Tt>` and `frameset/frame`. The names come out as `Go`, `Do`, `Name`, `Mail`, `A`, `TitleY`, `Cap`, `Tt`, `Fr`. No markup appears.

Nothing new was opened. The fix only removes output.

## Blocking

- none

## Should fix

Carried forward unchanged.

- **api text/plain checks for tags before it decodes JSON** (`crates/srv-web/src/api.rs:394-401`).
  - A tag-free text/plain body that is a JSON string with `<` escapes is shown as markup.
  - Fix: run `has_tag` on what will be shown. Also treat a top-level JSON string that holds markup as a document, not data.
- **text/plain loses content in the walk** (`crates/srv-web/src/html.rs:209`).
  - `Vec<String>` becomes `Vec` and `#include <stdio.h>` becomes `#include`.
  - At minimum, say so in the output, so the model uses api or a raw view instead.
- **The page client honours the system proxy** (`crates/srv-web/src/http.rs:61-70`). Without `.no_proxy()`, `PublicOnly` never runs behind `HTTP(S)_PROXY`.
- **The Content-Type header is echoed** in fetch, api and feed lines (`escape::name(mime, 80)`). Replace a type that has a tag with "unusual content type".
- **The URL and snippet fixes have no wiring test.**
  - Nothing asserts that `feed::parse` (iCal `URL`, feed links) or `search::parse_answer` emits cleaned URLs and de-tagged title and snippet.
  - Add one `feed::parse` case and one `parse_answer` case.
- **Relative Atom/RSS/JSON Feed links without `xml:base` are dropped** (`feed.rs:211`). Resolve them against the feed URL before `clean_url`.

## Notes

- **Spec ids:** the commit cites W-2 and rule 5, which matches the change. `interfaces.yaml` is untouched.
- **Rules 1, 3, 6, 7:** nothing in the diff touches them.
- **Behaviour change, acceptable:**
  - `aria-labelledby` that points straight at a non-secret textarea now gives an empty name. A browser would use the textarea's value.
  - A textarea whose text is a whole HTML document reports `empty` as its value.
  - Both fail closed under rule 5. They are not worth a round.
- **Hidden textareas:** a non-secret hidden textarea in a label shows its own value in its own row, marked `hidden` (sheet mode). This matches how an input's `value` is shown. It is not a name leak.
- **Earlier scratch is gone:** `runs/review-t12-r3/` and `runs/probe_r3.rs.bak`, noted last round, are no longer in the servers root `runs/`. That directory is empty after this round's cleanup.

## Verdict

Both confirm-round Blocking items are closed on every probe, in read and sheet mode through `fetch`. The secret value check holds. The iframe fix still holds. The srv-web suite, fmt and clippy are green. The should-fix items carry forward. None of them breaks a hard rule.

Verdict: approve
