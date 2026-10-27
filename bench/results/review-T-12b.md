# Review T-12b, round 1 (D-89)

Scope: `git diff origin/main...swarm/servers-T12b`. That is code commit 0a3d8b3 (merge 025bbc3 ignored), plus handoff T-12b-rust-coder-1.yaml and D-509..D-513. The decision ids are free on every branch I checked.

What I ran in F:/sb-swarm/wt/servers at 53ec8d3 (CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2):
- `cargo test -p srv-web`, every in-scope target: all green.
  - lib 46, accept_t12 5 (+1 ignored), accept_t13 10 (+1), accept_w 7 (the base64 test is no longer ignored and passes), contract_paths 1, live 0 (+3), proxy_t12b 1, round2_t12 7, round3_t12 10, t12b 5, t13b 12, t13c_api 11, web_tools 13.
  - An untracked tester file, `tests/accept_t12b_proxy.rs`, was being written in this worktree during the review. It is not in the diff, and it failed at its line 296 ("public api (no override) used the proxy"). I left it out of the in-scope run. The tester should decide whether that failure is in the code or in the test.
- `cargo clippy -p srv-web --all-targets -- -D warnings`: clean. `cargo fmt -p srv-web --check`: clean.
- I built a copy of `plain_source`/`plain_bracket` with `rustc -O` in my scratchpad to time hostile input (Blocking 1).

## Blocking

- **D-512 `plain_source` is quadratic on hostile input, which breaks W-2's 15 s timeout.** The cause is `html.rs:348` `let line_end = s.find(['\n', '\r'])`. For every `<name` that is not a known element, it scans to the end of the line, and the whole remainder can be one line. A text/plain (or untyped) body that is `"<x"` repeated takes:

  | Body size | Time |
  |---|---|
  | 20 KB | 0.25 s |
  | 80 KB | 4.1 s |
  | 320 KB | 60 s |

  At the 5 MB page cap it would take hours. The walk runs in `spawn_blocking` with no time cap (`fetch.rs:296`). So the fetch call does not return, and a blocking thread stays pinned after any client gives up. `cfg.timeout` covers only the HTTP read.
  - Fix: limit the `>` search to `PLAIN_BRACKET_MAX` bytes from the `<`, stopping at a newline, so each `<` costs O(200).
  - Add a test with a few MB of `<x` on one line that must finish within a small time limit.

## Should fix

1. **D-513: an empty link resolves to the feed's own URL** (`feed.rs:255`, called at `feed.rs:347` for iCal). `link_url("", Some(base))` runs `base.join("")`, which returns the base. So every VEVENT without a `URL` now shows the calendar URL as its link. Before, the link was empty. Many events have no `URL`. For a Google secret iCal address this repeats a long private-token URL once per event, which wastes tokens. It also points the model at a URL that `fetch` refuses, which wastes a turn. Whitespace-only `<link>`s and `href="#"` do the same.
   - Fix: return `None` for an empty or fragment-only href before joining.
   - Test: an ICS event without `URL` must give `"url": ""`.
2. **D-509: a host with a config DNS override goes to the proxy, and the proxy ignores the override** (`http.rs:121`). The proxy resolves the name itself, so the pinned address is lost. `proxy_t12b` asserts this behaviour (public.test goes to the proxy). The same happens in `api_for` for an override to a private address that is not loopback (`http.rs:79`).
   - Fix: send any host in `cfg.resolve` direct. reqwest's `resolve()` beats the custom resolver on the `page` client. In `api_for`, treat an override host as local.
3. **D-509: no test for a redirect from a public URL, through the proxy, to a private target.** `follow` checks every hop again through `page_for`, so the code looks right, but no test covers it. Add a case where the recording proxy answers 302 to `http://rebind.test/` and to `http://[::ffff:127.0.0.1]/`. Assert `HttpError::Private` and exactly one proxied request.
4. **D-509: IP literals in the pre-check** (`http.rs:119`). `host_of` keeps an IPv6 literal's brackets, so `resolver.public("[2606:4700::1]")` fails and the hop goes direct. A public IPv6 literal therefore skips the configured proxy. That is a proxy-policy bypass on networks that allow direct egress, and a failure on networks that do not. An IP literal that is not private needs no lookup: send it to `page_proxy` when `proxied()`.

## Notes

- **Rule 5, D-512.** I could not find a path where `plain_source` lets markup through. Everything it rewrites becomes `&amp;lt;`/`&amp;gt;`, and an escaped run never contains `<` (`find(['>','<'])`). So it cannot swallow or create a tag.
  - Known names, which includes every tag that changes tokenizer state (script, style, title, textarea, xmp, plaintext, noscript, noembed, noframes, iframe, svg, math), are left to the parser. So are custom elements, tags with attributes, comments, `<!`/`<?`/CDATA and `< x`.
  - Uppercase is lowercased before the lookup.
  - `<x\n a=1>` and `<foo<script>` become text or a real script element. Either way they fail closed.
  - Single-letter generics that are HTML names (`Option<A>`, `<B>`, `<S>`, `<I>`, `<P>`) are still dropped, as D-512 says.
  - A body with no content type that holds XML now shows `&lt;item&gt;`-style text. That is escaped, not markup, but it costs tokens.
- **Rule 5, D-510.** `text/plain` is checked again on the shown text, including picked text (`api.rs:619`). Under a JSON type only a top-level string counts as a document (`api.rs:708`). `["<!DOCTYPE html>..."]` or `{"d": "<html>..."}` under `application/json` is still shown, as D-508 data. That matches D-508, but the D-510 rule is easy to sidestep. Supreme may want D-508 reconsidered.
- **D-511.** `shown_mime` allows only RFC 6838 restricted-name characters and at most 80 characters. No `<`, brackets, parens, backticks or whitespace can get through. The tests cover tag, link and fence shapes.
- **D-513 schemes.** `javascript:` and `data:` links, including ones made by a hostile `xml:base`, are dropped by `clean_url`, which accepts http(s) only. A feed can still show links to private addresses, which the model may then fetch as written private URLs. Absolute links could already do that before this change, so D-513 adds no new exposure.
- **D-509 vault headers.**
  - Plain-http `@vault` is allowed only when `is_loopback` holds, and that always selects `api_vault_local`, which uses no proxy.
  - https goes through the proxy as a CONNECT tunnel, and proxy_t12b checks that the proxy never sees the canary. A TLS-intercepting corporate proxy trusted by the OS store would see the header. That is the user's environment, but worth a line in the docs.
  - Numeric forms (`2130706433`, `127.1`, `0`) are normalised by `Url`. IPv4-mapped/compatible addresses go through `private_ip`'s `to_ipv4`.
  - NAT64 (`64:ff9b::/96`) and IPv6 site-local `fec0::/10` are not treated as private. That gap predates this change.
- **D-509 other details.**
  - `proxied()` returns false when `http::Uri` rejects a URL `Url` accepts (`|`, `^`, `{` in the query). That is fail-safe for privacy, but such URLs skip the proxy.
  - The residual rebinding window and the fail-closed behaviour on proxy-only DNS are stated accurately in D-509.
  - `NO_PROXY` and the Windows `ProxyOverride` are honoured the same way on both sides, because reqwest 0.12.28 builds the same `hyper_util` `Matcher::from_system` (`reqwest/src/proxy.rs:517`).
- **Search behind a proxy.** Search runs with `allow_private=true`. A SearXNG reached by a compose service name therefore goes to the proxy when one is set, unless `NO_PROXY` lists it. This is unchanged from before.
- **Tests.** The t12b wiring tests go through `mcp::dispatch` and assert the actual output (resolved URLs, no `<`, the unusual-type label count), not just that nothing panicked. The handoff reports break-checks that went red.
- **Hard rules.**
  - No keys, `.env` files, sleeps, captcha handling or em dashes were added.
  - No new `unwrap` on external input outside tests.
  - No vault value can reach a return, log or the proxy: the test checks the canary in the output, the ledger and the proxy heads.
- **Spec ids and interfaces.**
  - The commit's ids (W-1..W-4, B-590, D-509..D-513) match the code.
  - `feed::parse` and `feed::ical::parse` keep their public signatures. `parse_with`, `parse_at`, `api_for` and the `Clients` fields are crate-private. No interfaces.yaml change is needed.
  - B-590 lives only in T-15b's blockers.yaml, so this branch records its resolution only in D-509.

## Verdict

Items 1, 2, 3, 5, 6 and 7 are implemented and tested. Item 4 (B-590) is implemented with sound logic but has the gaps listed above. One Blocking defect remains: the D-512 escaper adds a quadratic CPU path that a hostile text body can use to stall `fetch` far past W-2's 15 s limit.

Verdict: request changes
