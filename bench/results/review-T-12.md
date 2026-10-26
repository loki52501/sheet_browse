# Review T-12 confirm round (srv-web, docs/04 W-1..W-7)

## History

- **Round 1** (579f466, on dbacdbb): request changes. 1 Blocking (rule 5: HTML under a non-HTML or missing content type reached the model), 6 should-fix.
- **Round 2** (99ed4ea, on b9ccca5): request changes. Rule 5 still open (sniffer missed UTF-16, unclosed comments, prefixes, doctype spacing, fragment tags).
- **Closing round** (0afe399, on b2d08fb, D-508): request changes. 2 Blocking rule-5 items: `<option>` labels carried script/template source, and iCal and feed URLs reached the model verbatim.
- **Final round** (3962045, on 3b750b2): request changes. Both closing-round items were closed. 1 new Blocking item: `collect_text` read raw `<iframe>` text into link, button, label, aria-labelledby and option names.
- **Confirm round** (this file, on 4913651).

## Confirm round: 4913651

Checks run in the worktree with `CARGO_TARGET_DIR=F:/sb-swarm/target-servers`:
- `cargo fmt --check -p srv-web`: clean.
- `cargo clippy -p srv-web --all-targets -- -D warnings`: clean.
- `cargo test -p srv-web -j 2`: all green.
  - lib 19 (+1: `iframe_and_textarea_text_never_reach_names`);
  - accept_t12 5 (+1 ignored), accept_w 6 (+1 ignored), contract_paths 1, live 0 (+3 ignored);
  - round2_t12 7, round3_t12 10, web_tools 13.

Probes ran in a `git archive 4913651` export at `runs/review-t12-confirm/` under the servers root (git-ignored, deleted afterwards), file `crates/srv-web/tests/probe_confirm.rs`. Each case ran through `html::parse` and through `fetch` in read and sheet mode. `D` = `<!DOCTYPE html><html><body><script>stealCookies()</script>`.

**Final-round Blocking item (iframe text in names): closed.** No `<`, `DOCTYPE` or `stealCookies` reaches the output, in read or sheet mode, for an iframe inside any of these:
- a link, a button, `label for`, `aria-labelledby`, an option, a label around a select;
- a link or button whose name falls back to its `title`;
- an `aria-label` link, a link with an image whose `alt` text is the name;
- a figcaption, a summary, an `onclick` div, a `contenteditable` div, a link in a table cell;
- `<iframe role=button title=..>`;
- `<frame>` and `<frameset>` inside a link.

Names come out as `Go`, `Do`, `Name`, `Mail`, `A`, `Pick`, `TitleY`, `TitleB`, `AriaZ`, `PicW`, `Sum`, `Ed`, `Tt`, `Cell`, `Fr`. Textarea markup inside an `aria-labelledby` span, an option or a link is gone too.

**Other text-collection paths checked** (scope item 2):
- `Walker::walk`/`element` (`html.rs:419-520`): skips `SKIP_TAGS`. `iframe`, `frame` and `textarea` always get a control role (`html.rs:1141-1185`), so their children are never walked. Clean.
- `table_row` (`html.rs:671`): uses `walk`. Clean.
- `fragment_text` (`html.rs:248`): skips `SKIP_TAGS`, `textarea` and `iframe`. `frame` cannot occur in a body fragment. Clean.
- `page_title` (`html.rs:1319`) and the textarea value (`html.rs:800`): go through `no_markup`. Clean.
- `onclick_button` (`html.rs:1192`) and `densest` (`html.rs:1343`) call `.text()`, but only to measure length. Nothing is emitted.
- `serialize` (`html.rs:1374`) is `mode="raw"`, which is out of scope. Its text is escaped.
- `feed.rs` sends every text field through `html_to_text`, which calls `fragment_text`. `search.rs:69` calls `fragment_text`.
- `site.rs` reads the parsed doc only. `page.api` returns only JSON.

The one new path, the textarea branch that 4913651 added to `collect_text`, fails two ways. See Blocking.

## Blocking

- **Regression: `collect_text` now reads a field's own textarea content, including secret fields, into its label name** (`crates/srv-web/src/html.rs:1036-1043`). This breaks rule 5 ("passwords are never read from the DOM") and the rule-2 intent of `secret` at `html.rs:773`.
  - The new `tag == "textarea"` branch runs before the `!top && is_hidden(e)` check and the `skip_controls` check (`html.rs:1044-1050`). At 3b750b2 a label's own textarea was skipped (`skip_controls`), and a hidden one was skipped too. Now neither is.
  - Probe, output identical in read and sheet mode:
    - `<label>Pass <textarea autocomplete=current-password>hunter2secret</textarea></label>` gives `#1 input "Pass hunter2secret" filled`. The value is masked as `filled`, and the name holds the secret.
    - `<label for=p>Pass2 <textarea id=p autocomplete=current-password>hunter3secret</textarea></label>` gives `#1 input "Pass2 hunter3secret" filled`.
    - `<label>Card <textarea autocomplete=cc-number>4111111111111111</textarea></label>` gives `#1 input "Card 4111111111111111" filled`.
    - `<label>Comment <textarea>prefilled body text</textarea></label>` gives `#1 input "Comment prefilled body text" "prefilled body text"`. Every labelled textarea's name now repeats its value.
  - Fix:
    - Put the textarea branch after the hidden check and the `skip_controls` check.
    - Emit nothing for a secret textarea (`password` or `cc-` autocomplete, the same test as `html.rs:773`) on every path, including `aria-labelledby` with `skip_controls=false`.
    - Add these four cases to `iframe_and_textarea_text_never_reach_names`.
- **Rule 5: the textarea branch decodes entities twice, so escaped markup comes out as live markup** (`crates/srv-web/src/html.rs:1042`, `fragment_text(&raw)`).
  - The parser has already decoded the RCDATA once. `fragment_text` parses it again and decodes a second time. A browser shows `&lt;script&gt;`; the model gets `<script>`.
  - Probe: `<span id=l3>X<textarea>&amp;lt;script&amp;gt;stealCookies()&amp;lt;/script&amp;gt;</textarea></span><input aria-labelledby=l3>`.
    - Read mode gives `[input "X <script>stealCookies()</script>"](#2)`.
    - Sheet mode gives `#2 input "X <script>stealCookies()</script>" empty`.
    - The textarea's own value is right: `#1 input "&lt;script&gt;stealCookies()&lt;/scri..."`.
    - A whole `&amp;lt;!DOCTYPE html&amp;gt;...` document comes through the same way.
  - Fix: use `no_markup(&raw)`, as the value path at `html.rs:800` does. `no_markup` only reparses when real tags are present. Add this case to the test.

## Should fix

- **api text/plain checks for tags before it decodes JSON** (`crates/srv-web/src/api.rs:394-401`). Carried over.
  - A tag-free text/plain body that is a JSON string with `<` escapes is shown as markup.
  - Fix: run `has_tag` on what will be shown. Also treat a top-level JSON string that holds markup as a document, not data.
- **text/plain loses content in the walk** (`crates/srv-web/src/html.rs:209`). Carried over.
  - `Vec<String>` becomes `Vec` and `#include <stdio.h>` becomes `#include`.
  - At minimum, say so in the output, so the model uses api or a raw view instead.
- **The page client honours the system proxy** (`crates/srv-web/src/http.rs:61-70`). Carried over. Without `.no_proxy()`, `PublicOnly` never runs behind `HTTP(S)_PROXY`.
- **The Content-Type header is echoed** in fetch, api and feed lines (`escape::name(mime, 80)`). Carried over. Replace a type that has a tag with "unusual content type".
- **The URL and snippet fixes have no wiring test.** Carried over.
  - Nothing asserts that `feed::parse` (iCal `URL`, feed links) or `search::parse_answer` emits cleaned URLs and de-tagged title and snippet.
  - Add one `feed::parse` case and one `parse_answer` case.
- **Relative Atom/RSS/JSON Feed links without `xml:base` are dropped** (`feed.rs:211`). Carried over. Resolve them against the feed URL before `clean_url`.

Closed this round:
- the textarea `no_markup` item. The markup is gone, but the double decode is now Blocking, above.
- the `parse_url` doc comment, now back on `parse_url` (`http.rs:543`).

## Notes

- **Spec ids:** the commit cites W-2 and D-508, which matches the html.rs change. `interfaces.yaml` is untouched.
- **Rules 1, 3, 6, 7:** nothing new in the diff. There are no keys, no solving, no sleeps and no drafts.
- **Pre-existing, not counted against this commit:** before 4913651, an `aria-labelledby` span holding a secret textarea already leaked its text into the name, because `skip_controls=false` on that path. The first Blocking fix should cover it.
- **Judged not rule 5,** as in earlier rounds: attribute text (`title`, `aria-label`, `alt`, `option label`), and entity-encoded tags in ordinary text that a browser shows the same way.
- **Leftover scratch from an earlier round:** `runs/review-t12-r3/` and `runs/probe_r3.rs.bak` are still in the servers root `runs/`. They are not from this round and are git-ignored.

## Verdict

The final-round iframe item is closed on every probe, and no other text-collection function leaks raw-text markup. But the new textarea branch in `collect_text` introduces two problems:
- it reads a secret textarea's content into its label name, and a hidden textarea's too, which 3b750b2 did not;
- it double-decodes entities, so escaped markup reaches the model as `<script>`.

Both are small fixes in the same five lines: reorder the branch, skip secret textareas, and use `no_markup`. Each needs a test case.

Verdict: request changes
