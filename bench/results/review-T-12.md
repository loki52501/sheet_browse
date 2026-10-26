# Review T-12 round 2 (srv-web, docs/04 W-1..W-7)

Round 1 (579f466, on dbacdbb): request changes. There was 1 Blocking item (rule 5: HTML under a non-HTML or missing content type) and 6 should-fix items.

Round 2 covers b9ccca5 (fixes, D-505..D-507) and 5781f0d (handoff). Checks run:
- `cargo fmt --check -p srv-web`: clean.
- `cargo clippy -p srv-web --all-targets -D warnings`: clean.
- `cargo test -p srv-web`: lib 15, accept_t12 5 (+1 ignored), accept_w 6 (+1), contract_paths 1, round2_t12 7, web_tools 13. All green.
- `git stash list`: empty, so no T-12 entry is left.
- Probes ran in a scratch copy of 5781f0d (a `git archive` export with one extra test file), never in this worktree. A second scratch probe copied `looks_like_html` verbatim.

## Blocking

- **Rule 5 is still open: the sniffer misses full HTML documents (round-1 item only partly closed).** End to end, both `fetch` (text/plain) and `api` (text/plain, no content type, application/xml) returned `<!DOCTYPE html>...<script>stealCookies()</script>` verbatim in these cases:
  - **UTF-16 HTML.** The sniff reads the raw bytes, which start `FF FE 3C 00` (`http.rs:287`). The text the model gets comes from `http::decode` (`http.rs:412`), which honours the BOM, so the decoded text is plain HTML. The sinks are `fetch.rs:159-160` and `api.rs:375-382`.
  - **A leading comment that does not close inside the 8 KiB window**, or the empty comment `<!-->`. Both make the sniffer return `false` (`http.rs:287`, `http.rs:300`), so it fails open.
  - **Any non-tag prefix before `<!doctype html>`** (`http.rs:306`). Examples: a line of text, the XSSI guard `)]}'`, `{"ok":1}`, a Jinja `{% %}` line, U+200B, NUL. The same happens with `<!DOCTYPE  html>` (two spaces) and with `<!doctype\nhtml>`.
  - **Fragments whose first tag is not in `HTML_START_TAGS`** (`http.rs:276`), such as `<li>`, `<tr>`, `<td>`, `<em>`, `<pre>` or a custom element.

  The round-2 tests (`round2_t12.rs:84`) only cover documents that start with a doctype or a listed tag.

  Fix:
  - Sniff the decoded text, not the raw bytes.
  - Treat an unclosed leading comment as HTML.
  - In the two text sinks (fetch's `text/plain` branch and api's text fallback), look for `<!doctype html`, `<html`, `<head`, `<body`, `<script` or `<iframe` anywhere in the decoded window, not only at the start.
  - Add a test for each of the bypasses above.

## Should fix

- **The private-address guard covers redirects only, and it has a check-then-connect race.**
  - `host_is_private` resolves the name with `lookup_host` (`http.rs:246`), then reqwest resolves it again when it connects. A TTL-0 rebinding name passes the check and then connects to 127.0.0.1.
  - Two cases are not covered at all, because neither involves a redirect:
    - a public name whose A record is 127.0.0.1;
    - `fetch(url="#id")` on a link to `http://127.0.0.1:5433/`.
  - The guard also adds one DNS lookup per hop.
  - Better: give the page client a `ClientBuilder::dns_resolver` that drops loopback, private and link-local answers unless the URL as written is such a literal or `localhost`. That closes the race and needs no extra round trip.
  - `private_ip` (`http.rs:190-193`) does not cover:
    - 100.64.0.0/10 (CGNAT; Tailscale uses it for the user's own machines);
    - 198.18.0.0/15;
    - the IPv4-compatible form `::7f00:1`.
  - IPv4-mapped addresses, and decimal, octal and hex spellings (`url` turns them into `127.0.0.1`), plus `0.0.0.0` and `0/8`, are handled correctly. I checked these.
- **`is_loopback` accepts any `*.localhost` name** (`http.rs:263`, used at `api.rs:184`) for sending `@vault` headers over plain http. On this Windows host, getaddrinfo does not map `foo.localhost` to loopback: it goes to DNS, and NXDOMAIN-hijacking or search-suffix resolvers can answer with a public address. Accept only loopback IP literals and exact `localhost`, or resolve the name and require every address to be loopback.
- **D-506's fail-closed path leaves stale registrations.** When the health read fails, `site.rs:89` returns before line 100, so tools registered by an earlier fetch stay in the registry and stay runnable. The fail-closed path also has no test.
- **iCal `DESCRIPTION` does not go through `html_to_text`** (`feed.rs:331-348`). Google and Outlook `.ics` descriptions carry `<br>`, `<a href>` and `<b>`, and these reach the model in item summaries (capped at `SUMMARY_MAX`). RSS and Atom summaries are stripped.
- **`SITE_OUTPUT_MAX_CHARS` (`site.rs:41`) still has no test** (carried over from round 1). The 5 MB cap and the 15 s timeout are now covered by `accept_t12.rs:91` and `accept_t12.rs:190`. The stalled-body case maps to "did not answer in time" and passes.
- **The redirect chain plus the body can exceed W-2's 15 s.** `get_following`'s timeout (`http.rs:184`) bounds only the hops up to the response headers. The final body read then gets reqwest's own fresh 15 s. Bound the hops and the body read with one deadline.

## Notes

- Round-1 should-fix items that are closed:
  - **Vault cleartext and redirect origin.** `api_vault` keeps scheme, host and port; the non-vault client never downgrades https to http; `@vault` is refused on plain http to non-loopback hosts. Tested at `round2_t12.rs:158`.
  - **Challenge stops in feed and api.** `fetch::challenge_stop` runs before status and content checks. Tested at `round2_t12.rs:516`.
  - **Tool health honoured on registration**, and a miss disables the tool for the process (D-506). Tested at `round2_t12.rs:584`.
  - **D-505** (a partial fingerprint runs with a note) is recorded.
  - **Prose cannot forge `# ` or `- ` line starts.**
  - **JSON strings cannot spell `[x](#n)`.**
- **Pinning (D-507) is sound under `core::mcp::dispatch`.**
  - `ctx` is a local of the pinned dispatch future, so its address is stable from classify to call (`mcp.rs:1039`, `1056`, `1155`). `classify` always runs before `call`, and no other caller of `handler.call` exists in the workspace.
  - A pin left by a gate Stop, a skip or an Ask timeout cannot be taken by a later call that reuses the address. That call's own classify replaces the slot by key (`pin.rs:47`): fetch does so whenever `#id` resolves, and site tools always do.
  - Pins are bounded at 256 per handler (`pin.rs:21`). Eviction only causes a fail-closed "call again".
  - One residual: a stale site-tool pin holds an `Arc<Page>` until it is evicted, and a parsed page can be up to 5 MB. This is a memory concern only.
  - Concurrency is safe because every access goes through one mutex. Both tests (`round2_t12.rs:264`, `:360`) really interleave a second fetch while the first card is held.
- Rule 2 still holds: vault values are resolved only into a sensitive `HeaderValue`, and errors name only the host. Rule 3: no solving. Rule 4: every side effect goes through `dispatch`. Rule 6: no sleeps in `src/`. Rule 1: no keys and no new fixtures. Every `unwrap` in `src/` is in `#[cfg(test)]`. The sniffer's slicing stays on ASCII boundaries.
- Spec ids: the commit cites W-2..W-7, D-505 and D-506, and its body cites D-507. Every listed behaviour is in the diff. `interfaces.yaml` is untouched.
- api and page.api JSON can still carry HTML inside string values (e.g. GitHub `body_html`). This is data, not a page. Leave it for supreme together with B-500.
- B-500 and B-501 remain supreme's questions and do not block this review.

## Verdict

Most of the round-1 fixes are good. The rule-5 Blocking item is not closed: deterministic inputs, including UTF-16 and comment-prefixed full documents, still get raw HTML and `<script>` to the model through `fetch` and `api`, and there is no test for them.

Verdict: request changes
