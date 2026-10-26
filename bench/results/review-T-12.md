# Review T-12 final round (srv-web, docs/04 W-1..W-7)

## History

- **Round 1** (579f466, on dbacdbb): request changes. 1 Blocking (rule 5: HTML under a non-HTML or missing content type reached the model), 6 should-fix.
- **Round 2** (99ed4ea, on b9ccca5): request changes. Rule 5 still open (sniffer missed UTF-16, unclosed comments, prefixes, doctype spacing, fragment tags).
- **Closing round** (0afe399, on b2d08fb, D-508): request changes. Round-2 bypasses closed ("walk everything, sniff nothing"). 2 new Blocking rule-5 items: `<option>` labels carried script/style/template source (`o.text()`), and the iCal `URL` (plus feed links) reached the model verbatim and uncapped.
- **Final round** (this file, on 3b750b2).

## Final round: 3b750b2

Checks run (worktree, `CARGO_TARGET_DIR=F:/sb-swarm/target-servers`):
- `cargo fmt --check -p srv-web`: clean.
- `cargo clippy -p srv-web --all-targets -- -D warnings`: clean.
- `cargo test -p srv-web -j 2`: all green. lib 18 (+1: `option_labels_carry_no_script_source`), accept_t12 5 (+1 ignored), accept_w 6 (+1), contract_paths 1, live 0 (+3), round2_t12 7, round3_t12 10, web_tools 13. Nothing regressed.

Probes ran in a `git archive 3b750b2` export at `runs/review-t12-final/` under the servers root (git-ignored, deleted afterwards), file `crates/srv-web/tests/probe_final.rs`. Cases ran through `fetch` (read and sheet; text/html, text/plain and no content type), `feed`, and `search::parse_answer`.

Closing-round Blocking 1 (option labels): **closed for the reported cases.** `<option>` with nested `script`, `style`, `template`, nested `template`, `noscript`, `svg><script`, `plaintext`, `xmp`, an unclosed `script`, `optgroup`, `datalist`, and a `selected` option now give `A` and no markup. However, the new path uses `collect_text`, which does not skip `iframe`, and that leak reaches every accessible name. See Blocking.

Closing-round Blocking 2 (URLs): **closed.**
- iCal `URL` and RSS/Atom/JSON Feed links all go through `http::clean_url`. Markup in the path, query and fragment comes out percent-encoded (`%3Cscript%3E`).
- Dropped, giving `url: ""`:
  - markup in the host (`https://f<script>.test/`);
  - userinfo, with or without markup (`parse_url` refuses credentials);
  - `javascript:` and `data:` links;
  - URLs over 2000 chars.
- Folded iCal lines (`<scr\r\n ipt>`), `\<` escapes and a backslash host all normalise to percent-encoded paths.
- IDN `bücher.example` comes out as `xn--bcher-kva.example`.
- JSON Feed `url` and `external_url` and RSS `<link>` with CDATA markup are both cleaned.
- Atom `xml:base` relative links resolve. Without a base they are dropped (see Should fix).

Search (closing-round should-fix): **closed.** `<span class="highlight">`, `<b>`, `<script>` and whole-document snippets come out as text only. Hit URLs are cleaned like feed URLs.

## Blocking

- **Rule 5: `<iframe>` raw text reaches the model through every accessible name** (`crates/srv-web/src/html.rs:1029`, `collect_text`).
  - The parser reads iframe content as raw text, and a browser never renders it. `collect_text` skips `SKIP_TAGS` only, and `iframe` is not in that list. `fragment_text` does skip it (`html.rs:259`).
  - The module doc (`html.rs:15`) says "nothing inside an `<iframe>` is read".
  - Probe, with `D` = `<!DOCTYPE html><html><body><script>stealCookies()</script>`. Each line is the output for the input after it:
    - `#1 link "Go <!DOCTYPE html><html><body><script>stealCookies()</script>"` for `<a href=/x>Go<iframe>D</iframe></a>` (sheet);
    - `[button "Do <!DOCTYPE html>...<script>stealCookies()</script>"](#1)` for `<button>Do<iframe>D</iframe></button>`. This one leaks in **read** mode as well as sheet;
    - `#2 input "Name <!DOCTYPE html>..."` for `<label for=i>Name<iframe>D</iframe></label>`, and the same for `aria-labelledby`;
    - `#1 select "A <!DOCTYPE html><html><body><script>..."` for `<option>A<iframe>D</iframe></option>`. This is the path 3b750b2 just moved to `collect_text`.
  - This is the same class as the closing-round item: raw-text content the parser does not read as markup. It is capped only by `NAME_MAX`/`OPTION_MAX`.
  - Fix:
    - In `collect_text`, return on `iframe` and `frame` as well as `SKIP_TAGS`, matching `fragment_text`.
    - Extend `option_labels_carry_no_script_source`, or add a test, to cover an iframe inside a link, a button (read mode), a label and an option.

## Should fix

- **Textarea text bypasses `no_markup` in `collect_text`.** This is carried over from the closing round and now also reaches option labels.
  - The textarea value is filtered (`html.rs:800`). The same text reaches names unfiltered through `aria-labelledby`, a link containing a textarea, and now `<option>A<textarea>...</textarea>` (scraper 0.29 keeps it inside the option). Probe: `#1 select "A <!DOCTYPE html><html><body><script>..."`.
  - A browser displays this text, so I do not count it as rule 5. It does contradict D-508's own textarea rule.
  - Fix: apply `no_markup` to textarea text in `collect_text`. That is the same edit as the Blocking fix.
- **api text/plain checks for tags before it decodes JSON** (`crates/srv-web/src/api.rs:394-401`). Carried over.
  - A tag-free text/plain body that is a JSON string with `<` escapes is shown as markup.
  - Fix: run `has_tag` on what will be shown. Also treat a top-level JSON string that holds markup as a document, not data.
- **text/plain loses content in the walk** (`crates/srv-web/src/html.rs:209`). Carried over.
  - `Vec<String>` becomes `Vec` and `#include <stdio.h>` becomes `#include`.
  - At minimum, say so in the output, so the model uses api or a raw view instead.
- **The page client honours the system proxy** (`crates/srv-web/src/http.rs:61-70`). Carried over. Without `.no_proxy()`, `PublicOnly` never runs behind `HTTP(S)_PROXY`.
- **The Content-Type header is echoed** in fetch, api and feed lines (`escape::name(mime, 80)`). Carried over. Replace a type that has a tag with "unusual content type".
- **The `parse_url` doc comment is now attached to `URL_MAX`** (`crates/srv-web/src/http.rs:531-534`). The new const was inserted between the doc lines and the fn. Move the const and its doc above that comment.
- **The URL and snippet fixes have no wiring test.**
  - `clean_url` is unit-tested (`http.rs:593-600`), but nothing asserts that `feed` (iCal `URL`, feed links) or `search::parse_answer` actually emits cleaned URLs and de-tagged title and snippet.
  - The closing round asked for a test on the iCal path.
  - Add one `feed::parse` case and one `parse_answer` case.
- **Relative Atom/RSS/JSON Feed links without `xml:base` are dropped** (`feed.rs:211`). The behaviour is unchanged, but it loses the item link on many real Atom feeds (`<link href="/posts/1"/>`). Resolve them against the feed URL before `clean_url`, which costs nothing.

## Notes

- **Judged not rule 5,** on the same basis as the closing round:
  - an `<option label="<!DOCTYPE html><script>...">` attribute (attribute text, which a browser shows as the label);
  - entity-encoded tags in option text and in search titles (`&lt;script&gt;` shown as `<script>`);
  - CDATA inside an option, which shows as `A stealCookies()))>`;
  - percent-encoded `stealCookies()` inside cleaned URLs.
- `fragment_text` on search snippets turns plain-text `Vec<String>` into `Vec`. SearXNG `content` is usually plain text, so this is minor. The same trade-off applies to the text/plain item above.
- **Spec ids:** the title cites W-1, W-2 and W-4, which matches search, fetch/html and feed. `interfaces.yaml` is untouched.
- **Rules 1, 2, 3, 6, 7:** nothing new. There are no keys, no vault paths, no solving and no sleeps in the diff.

## Verdict

Both closing-round Blocking items are closed, the search should-fix is closed, and nothing regressed. The probe found one more rule-5 path of the same class: `iframe` raw text reaches link, button, label and option names through `collect_text`, in read mode as well as sheet. The fix is one line plus a test.

Verdict: request changes
