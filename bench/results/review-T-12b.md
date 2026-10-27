# Review T-12b, round 2 (final, D-89)

Scope: `git diff 9ee09bc..HEAD` on swarm/servers-T12b. That is code commit ed371aa, plus 6919eb3 (D-514..D-518, B-592, B-593, handoff T-12b-rust-coder-2.yaml).

Round 1 (4e3b601) asked for changes. Blocking: D-512 `plain_source` was quadratic (60 s for 320 KB of `<x`), which broke W-2's 15 s. Should-fix: (1) empty links got the feed URL, (2) override hosts were proxied, (3) no test for a proxied redirect to a private target, (4) IPv6 literal brackets skipped the proxy. Acceptance round 1 failed rows 3b, 3c, 4b, 6b, 7b and the two totals rows.

What I ran in F:/sb-swarm/wt/servers at 6919eb3 (CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2):
- `cargo test -p srv-web --no-fail-fast`: exit 0, 149 passed, 0 failed, 5 ignored (live). By binary: lib 56, accept_t12 5+1, accept_t12b 10, accept_t12b_proxy 1, accept_t13 10+1, accept_w 7, contract_paths 1, live 0+3, proxy_t12b 1, round2_t12 7, round3_t12 10, t12b 5, t13b 12, t13c_api 11, web_tools 13.
- `cargo clippy -p srv-web --all-targets -- -D warnings`: clean. `cargo fmt -p srv-web -- --check`: clean.
- Release timing: a scratch bin crate outside the repo, built in the scratchpad against this worktree's srv-web and sb-sheet. It calls `html::parse`/`parse_text`, then `sb_sheet::read`/`skeleton`/`sheet`/`find` and `feed::parse`, on a 2 MiB thread (tokio's default stack). Each body is about 5 MB unless stated.

## Blocking

- **Deep nesting aborts srv-web: the walker overflows its stack at about 4,000 levels** (html.rs:641 `Walker::walk` recurses at :663/:753/:757/:930/:938, and `collect_text` at html.rs:1397). On a 2 MiB stack, release build:
  - `<div>` x3500 is ok (72 ms). `<div>` x4000 (20 KB) overflows, and so does `<span>` x5000.
  - text/plain `<b>` x5000 (15 KB) overflows too, because known names go to the parser.
  - A stack overflow is a process abort, not a panic, so `walk_within`'s `JoinError` arm (fetch.rs:330) never sees it. Every web tool of the session goes down. So D-517's "the call returns whatever the input" does not hold.
  - This predates T-12b (main has the same recursion), but D-517 claims the guarantee, and any hostile page can do it.
  - Fix: a nesting cap. Either make the walk iterative, or stop descending past a fixed depth.
  - Test: fetch of `<div>` x10000 as text/html and of `<b>` x10000 as text/plain returns.
- **html5ever's tree build is quadratic in nesting depth and cannot be interrupted, so the D-517 cap returns the call but a thread stays pinned** (fetch.rs:314, html.rs:28). `Html::parse_document` alone, release:

  | `<div>` nesting | Body size | Parse time |
  |---|---|---|
  | 10k | 50 KB | 0.37 s |
  | 20k | 100 KB | 1.47 s |
  | 40k | 200 KB | 6.2 s |
  | 80k | 400 KB | 31 s |
  | 160k | 800 KB | over 60 s (killed) |

  D-517 says "the parser's own pass cannot be interrupted (it is linear)". It is not linear on this input. A body of 1 MB or less holds a blocking thread for minutes after fetch returns its 15 s error, and then hits the overflow above.
  - **feed is worse** (feed.rs:120, html.rs:459/468). `parse_with` and `fragment_text` run on the async worker with no `spawn_blocking` and no cap. `fragment_text` also walks `ancestors()` for every text node.
    - One RSS `<description>` of escaped `<div>x` x80k (about 1 MB) took 96 s; 40k took 18 s.
    - The feed call does not return, and a tokio worker is blocked for that time.
    - This is the same class as round 1's Blocking: a CPU stall far past the 15 s budget, from a hostile body.
  - Fix: bound element depth before html5ever builds the tree. One option is a linear pre-pass that counts unclosed non-void start tags and refuses or flattens past a cap of a few hundred. Also run feed's parse in `spawn_blocking` under the same deadline.
  - Tests: 400 KB of nested `<div>` through fetch, and a deeply nested feed summary through feed, each return within 15 s, and nothing keeps running after.

## Should fix

1. **render_read runs after the walk cap, on the async thread** (fetch.rs:447). In release, 1.67 M paragraphs (`a\n\n` x1.67M as text/plain) took 12.5 s in the walker (under the cap), then 2.6 s in `sb_sheet::read` and 1.0 s in `skeleton` on the tokio worker. The cost is linear and bounded, but the call can run past 15 s after the HTTP read. Also, the 15 s walk deadline starts after a read that may itself take 15 s. Run the render inside the same `spawn_blocking` and deadline, and count the walk against the request's start.
2. **api's name lookup is capped at the full request timeout** (http.rs:112, :246 `lookup_max: cfg.timeout`). With a proxy set and a resolver that drops packets, api waits up to 15 s for the lookup and then up to 15 s more for the request. Every proxied api call now also does one extra lookup. Cap the lookup at a few seconds, or take it out of the request's budget.
3. **D-518 edge case.** When the tool was retired between fetch and record, but its archive step failed (D-316, so the file is still in place) and already holds the fingerprint, srv-web now records `Ok` (site.rs:486). It then calls `enable` and `reregister`, so a retired tool comes back in this process. Before this change, toolstore refused `Relocated` on a retired tool. Fix: keep `Relocated` (or skip enable/reregister) when the returned row is `Retired`.
4. **Wall-clock asserts in debug tests may flake under workspace load**: `hostile_angle_runs_are_linear` (3 s per 4 MB, 12 s walk) and accept_t12b's 20 s. The t13b flake shows that the workspace load is real. Consider bigger margins or a step count.

## Notes

- **(a) Round-1 Blocking.** Closed for the `plain_source` path.
  - `plain_bracket`'s window is bounded at `PLAIN_BRACKET_MAX` bytes (html.rs:433). Known-name lookup is a binary search (html.rs:305), and the walker checks its deadline every 256 nodes (html.rs:641). `walk_within` uses `tokio::time::timeout` (fetch.rs:327).
  - Release, 5 MB text/plain, walk plus read:

    | Body | Walk + read |
    |---|---|
    | `<x` | 0.79 s + 0.35 s |
    | `<x ` | 0.68 s + 0.48 s |
    | `<x=` | 0.51 s |
    | `</q` | 2.3 s |
    | `&lt;` | 1.5 s + 0.12 s |
    | `&#60;` | 0.80 s |
    | one word | 0.11 s + 0.09 s |
    | one paragraph | 0.18 s + 0.29 s |
    | U+FDD0 x U+FDD1 | 0.14 s |

  - text/html, 5 MB:

    | Body | Walk |
    |---|---|
    | one `<p>` | 0.14 s |
    | 312k links | 3.6 s (sheet 0.57 s, find 0.72 s) |
    | 263k table rows | 2.8 s |
    | 1 M `<li>` | 8.2 s |
    | 555k sections | 5.4 s |
    | 147k labels | 2.7 s (find 2.5 s) |
    | `&lt;` entities | 1.5 s |

    All linear apart from the nesting cases above.
  - B-592 is not reachable through srv-web. `split_long` keeps every unit at 8000 chars or less, and table cells are capped. The reader stayed at 0.3 s or less on a single 5 MB paragraph, word, `<pre>` or `<td>`. The reader itself is still quadratic per unit for other callers, which is the sheet owner's item.
  - Search: SearXNG fields are short and go through `fragment_text`. The feed finding above applies if a backend returns deep markup.
- **(b) Should-fix 1-4 and the failing rows are fixed.**
  - 1: `link_url` returns `None` for empty, `#`, whitespace and self links (feed.rs:261).
  - 2: `overridden` sends override hosts direct (http.rs:162, :141).
  - 3: `a_proxied_redirect_to_a_private_target_is_refused` asserts `Private` and exactly one proxied request. The handoff says it was not break-checked.
  - 4: `host_kind` strips the brackets, and a public IPv6 literal is proxied (http.rs:149; test at :1131).
  - Rows 3b, 3c, 4b, 6b and 7b pass: accept_t12b is 10/10 and accept_t12b_proxy is 1/1.
  - D-510 tests now build real `\u003c` escapes from `char::from(92u8)`, with a `has_tag` pre-assert (api.rs:994).
- **Tester test edits keep their intent.** accept_t12b_proxy.rs:281 and proxy_t12b.rs:170 now assert that the override host `public.test` does not reach the proxy. proxy_t12b also checks the origin got it, with the count going from 6 to 7. That assertion is new and is D-515's behaviour.
  - The proxied control moved from a name to the public literal `192.0.2.10`. A public name through the pre-check is still covered at unit level (`a_proxy_never_fetches_a_private_target`, pub.test). The api control (`api.example.invalid`), the CONNECT check, the vault-in-proxy-heads check and the private-name violations of row 4b are unchanged.
  - Nothing was loosened.
- **(c) U+FDD0/U+FDD1 cannot forge or break escaping.**
  - `escape::angles` maps each mark to the same `&lt;`/`&gt;` as a literal bracket (escape.rs:75). D-514 already makes `&lt;` in the output ambiguous (`&` is not escaped), so a page that writes the marks gains nothing it could not get by writing `<`.
  - `plain_source` only looks at `<`, so a mark in the input never changes what is markup.
  - `split_long` cuts after escaping and never inside `&lt;` (html.rs:1330).
  - api text/plain (`text_checked`) shows a raw mark verbatim. A noncharacter is not a bracket, so that is harmless.
  - The probe body (U+FDD0 x U+FDD1, 5 MB) read back as `&lt;x&gt;` with no `<`.
- **(d) D-518 lowers no class and skips no needed patch.**
  - It records `Ok` only when srv-web's own read shows the file already holds the new fingerprint, which is exactly toolstore's D-577 conversion (lifecycle.rs:362). The `Ok` arm's status handling (NeedsRepair to Repaired with a bump, Ask held) is unchanged.
  - The `@gate` class is not touched, and no patch is skipped that the file does not already hold.
  - The only difference is the retired case in should-fix 3.
- **(e) Other checks.**
  - No raw HTML or tag-shaped text appeared: every probe's read markdown had no `<`.
  - No private target goes via the proxy: `api_never_hands_a_private_name_to_the_proxy` covers names, mapped literals, ULA and overrides.
  - No vault value appeared, no `unwrap` was added outside tests, and there are no sleeps (only `timeout` caps).
  - Spec ids in ed371aa match the code. `api_for` and `walk_within` are crate-private, so interfaces.yaml needs no change.
  - The scratch bench left `t12b-review-bench.exe` and `depth.exe` in target-servers/release; nothing in the repo changed besides this file.

## Verdict

Round 1's Blocking and should-fix 1-4 are fixed and tested, the five failing acceptance rows pass, clippy and fmt are clean, and the D-514 escaping and the U+FDD0/U+FDD1 marks hold. Two Blocking defects remain, both about deep nesting, found with this round's hostile-body timing. They predate T-12b, but they break D-517's "returns whatever the input":
- about 4,000 nested elements (20 KB) abort the srv-web process;
- html5ever's quadratic tree build pins a fetch thread for minutes, and stalls feed on an async worker with no cap (96 s for 1 MB).

Verdict: request changes
