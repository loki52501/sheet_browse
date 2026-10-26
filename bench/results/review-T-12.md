# Review T-12 closing round (srv-web, docs/04 W-1..W-7)

## History

- **Round 1** (579f466, on dbacdbb): request changes. There was 1 Blocking item (rule 5: HTML under a non-HTML or missing content type reached the model) and 6 should-fix items.
- **Round 2** (99ed4ea, on b9ccca5): request changes. Rule 5 was still open: the sniffer missed UTF-16 documents, unclosed leading comments, text before the doctype, odd doctype spacing and unlisted fragment tags.
  - The round-2 should-fix items were:
    - the private-address guard covered redirects only and had a check-then-connect race;
    - `*.localhost` counted as loopback;
    - D-506 left stale registrations;
    - iCal HTML was not stripped;
    - `SITE_OUTPUT_MAX_CHARS` had no test;
    - redirects and the body each got their own 15 s.

## Closing round: b2d08fb (D-508)

Checks run:
- `cargo fmt --check -p srv-web`: clean.
- `cargo clippy -p srv-web --all-targets -- -D warnings`: clean.
- `cargo test -p srv-web`: all green. lib 17, accept_t12 5 (+1 ignored), accept_w 6 (+1), contract_paths 1, live 0 (+3), round2_t12 7, round3_t12 10, web_tools 13.

Where the probes ran:
- In a `git archive b2d08fb` export at `runs/review-t12-r3/` under the servers root. That folder is git-ignored and outside this worktree; the write hook refuses paths outside the project root.
- One extra file there: `crates/srv-web/tests/probe_r3.rs`.
- Output: `runs/review-t12-r3/probe_out*.txt`.

The round-2 bypass list is closed end to end. Every case returns no markup through fetch (html, plain, none) and api (plain, none, xml): UTF-16 LE/BE, unclosed and empty comments, text/XSSI/JSON/ZWSP/NUL prefixes, doctype spacing, and li/tr/em/pre/custom fragments. The fix is the right shape: walk everything, sniff nothing. Two new paths still carry raw markup, listed below.

## Blocking

- **Rule 5: `<select>` option labels emit raw-text element content, markup included** (`crates/srv-web/src/html.rs:818`).
  - `o.text()` collects every descendant text node. In select parsing, `<script>` and `<template>` are kept inside `<option>`, and `<style>` is too (probe).
  - So script, style and template source reaches the sheet, together with literal markup. Probe, `fetch(mode=sheet)` on text/html (also text/plain and no content type):
    `<option>A<script><!DOCTYPE html><html><body><iframe src=x></script></option><option><style><head><meta></style>B</option>`
    gives
    `#1 select "Pick" "A<!DOCTYPE html><html><body><iframe s..." [A<!DOCTYPE html><html><body..., <head><meta>B]`
  - With `{S}` = `<script>stealCookies()</script>`, `<option>A{S}</option>` gives `[AstealCookies(), ...]`.
  - This is the same class as the round-2 item: raw-text content the parser does not read as markup, which D-508 says is skipped. It is bounded only by `OPTION_MAX` (80) × `OPTIONS_MAX` (25).
  - Fix:
    - Build the label with `collect_text`, which honours `SKIP_TAGS`; `inner_text(o, false)` does this.
    - Add a `raw_text_elements_never_carry_markup_to_the_model` case for script, style and template inside `<option>`.
- **Rule 5: the iCal `URL` property reaches the model verbatim and uncapped** (`crates/srv-web/src/feed.rs:278`, `feed.rs:314`).
  - `parse_url(&self.url).is_ok()` only validates the value; the original string is returned. Probe: `URL:https://f.test/d/<script>stealCookies()</script><!DOCTYPE html><html>` gives `"url": "https://f.test/d/<script>stealCookies()</script><!DOCTYPE html><html>"` in the feed output.
  - A `.ics` can therefore carry a whole document, minus newlines, in one item's `url`. No parser touches it and there is no cap.
  - Fix:
    - Emit the parsed `Url`'s `to_string()`, which percent-encodes `<` and `>`.
    - Cap it, or drop URLs longer than about 2,048 chars.
    - Apply the same cap to RSS/Atom/JSON Feed links (`feed.rs:211`). feed-rs already percent-encodes those, but one probe item's `url` was over 9,000 chars.
    - Add a test.

## Should fix

- **api text/plain checks for tags before it decodes JSON** (`crates/srv-web/src/api.rs:394-401`).
  - A tag-free text/plain body that is a JSON string with `<` escapes is shown as `"<!DOCTYPE html><script>stealCookies()</script>"` (probe). The has_tag check passes because there is no literal `<` until the JSON is decoded.
  - Fix: run `has_tag` on what will be shown, or show text/plain only as text.
  - Related: a body whose top-level JSON value is a string holding markup (application/json, or no content type) is a document, not structured data. Treat it as not shown.
  - HTML inside object fields (`body_html`) stays accepted as data, pending supreme on B-500. I agree with that carve-out.
- **search passes title and snippet through `escape::name` only, and `url` verbatim** (`crates/srv-web/src/search.rs:71-73`).
  - `<span class="highlight">` or any other markup in SearXNG's `title`/`content` reaches the model (`parse_answer` probe).
  - I could not check whether current SearXNG JSON carries highlight spans, because no instance was running here.
  - Fix:
    - Run `html::fragment_text` on title and content.
    - Emit the normalized, capped `Url`.
- **text/plain loses content in the walk** (`crates/srv-web/src/html.rs:209`).
  - `Vec<String>` becomes `Vec`, `#include <stdio.h>` becomes `#include`, `a<b && c>d` becomes `ad`, and `AT&amp;T` becomes `AT&T` (probe, `fetch` on text/plain).
  - raw.githubusercontent.com serves source as text/plain, so the model silently gets wrong code. Efficiency outranks tokens.
  - At minimum, add a line saying tag-like runs were removed from a text/plain body. Then the model knows to use api or a raw view instead of trusting the text.
- **Textarea text bypasses `no_markup` outside the value path.**
  - The value goes through `no_markup` (`html.rs:800`). The same text reaches names unfiltered:
    - through `aria-labelledby` pointing at a textarea (`html.rs:938-946` via `collect_text`);
    - through a link that contains a textarea (`html.rs:900`).
  - Probes:
    - `#2 input "<!DOCTYPE html><script>stealCookies()</script>"`
    - `#1 link "Go <!DOCTYPE html><script>stealCookies()</script>"`
  - This is text a browser displays, so I do not count it as a rule-5 break. It does contradict D-508's own textarea rule. Fix: apply `no_markup` to textarea text in `collect_text`.
- **The page client honours the system proxy** (`crates/srv-web/src/http.rs:61-70`). There is no `.no_proxy()`, and reqwest reads `HTTP(S)_PROXY`. Behind a proxy, DNS happens at the proxy, so `PublicOnly` never runs and the private-address guard is void. Either set `.no_proxy()` on `page`, or document that the guard does not apply when a proxy is set.
- **The Content-Type header is echoed in fetch, api and feed lines** (`escape::name(mime, 80)`). Probe: `HTTP 200 text/<script>stealcookies()</script>, 8 bytes, not shown`. It is small and header-only. Replace a type with `has_tag` by "unusual content type".

## Notes

- **Judged not rule 5.** These are what a browser renders as text, or exposes as an accessible name (injected.js returns the same). They are capped, and they are not unparsed markup:
  - entity-encoded tags (`&lt;script&gt;` shown as `<script>` text);
  - HTML-body CDATA (a bogus comment, so it shows as `stealCookies()]]>`);
  - `<\0script>` in fetch (the parser drops the NUL; a browser shows `<script>...` as text);
  - double-encoded feed titles;
  - attribute values containing `<` (aria-label, title, alt, placeholder, value).
- **api text/plain probes behave correctly.**
  - These are not markup to any HTML tokenizer and are shown as text, which is correct: `<\0script>`, full-width `＜script＞`, `< script>`, `<\tscript>`, UTF-7 `+ADw-script+AD4-`. `has_tag` matches the tokenizer's tag-open rule exactly: `<` followed by an ASCII letter, `!`, `/` or `?`.
  - `</\0script>` trips `</`, so it is not shown (fails closed).
  - UTF-16 declared only by a charset label (no BOM) is decoded first and then not shown.
- **Hostile documents leak nothing in read or sheet mode** (html, plain, none): `<plaintext>`, `<xmp>`, `<noscript>`, `<template>`, `<svg><script>`, `<foreignObject>`, `<math><mtext><script>`, `<style>` in the body, an unclosed `<script>`, a bare `<script`, `<noframes>`, `iframe srcdoc`.
- **Feeds are clean apart from the URL items above.**
  - Item fields are stripped:
    - RSS CDATA HTML in title and description;
    - Atom `type="html"` and `type="xhtml"` title and content;
    - JSON Feed `content_html`.
  - A non-feed body under `application/rss+xml` is refused and not echoed.
  - Error messages quote only the host, the status and the escaped type, never the body.
- **Question 2: b2d08fb introduces no hard-rule problem.**
  - **Vault:** `PublicOnly` sees only host names, and its error maps to fixed sentences. `api_vault` is unchanged (same-origin redirects only). Cleartext `@vault` goes only to a loopback literal or exact `localhost` (`is_loopback`).
  - **Rule 4:** search on `page_local` with `allow_private=true` is acceptable.
    - The base URL is operator config (`SB_SEARXNG_URL`, D-31), and the model controls only `q`.
    - The call still goes through `dispatch` as `Class::Read`.
  - **DNS overrides:** `cfg.resolve` overrides skip `PublicOnly`, but `Config::from_env` never sets them, so they exist only in tests.
- **Round-2 should-fix items that are closed:**
  - `PublicOnly` filters at connect, so there is no rebinding window. A name with mixed public and private answers keeps only the public ones.
  - `fetch("#id")` inherits private access only from a page whose URL was written as private. Tested at `round3_t12.rs:449`.
  - CGNAT, 198.18/15 and IPv4-compatible forms are private.
  - `is_loopback` accepts exact `localhost` only.
  - D-506 health-read failure removes earlier registrations.
  - One deadline covers the redirects and the body (`round3_t12.rs:581`).
  - The site output cap is tested.
  - iCal text fields are stripped.
- NAT64 (`64:ff9b::/96`) and 6to4 (`2002::/16`) addresses that embed private IPv4 are not in `private_ip`. This is minor.
- **Spec ids:** the title cites W-2..W-4 and D-508. The body also covers W-1 (search client) and W-7/D-506 (site) without naming them. `interfaces.yaml` is untouched. Rules 1, 3 and 6 still hold: no keys or fixtures, no solving, no sleeps in `src/`.

## Verdict

The round-2 bypasses are closed. The two new Blocking items both break rule 5 and are small to fix:
- `<option>` labels emit script, style and template source, markup included;
- the iCal `URL` property is echoed verbatim and uncapped.

Verdict: request changes
