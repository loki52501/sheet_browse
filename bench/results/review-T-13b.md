# Review T-13b, round 2 (final, D-89)

Scope: `git diff 0079779..HEAD -- crates/`. That is commits 0e416c4 (D-582..D-584), 4740ee3 (toolstore error text) and ff495c4 (srv-web), plus handoff T-13b-rust-coder-2.yaml.

Round 1 (3451796) approved with no Blocking items and seven should-fix items:
1. A redirect to a challenge URL ended as an ordinary failure.
2. Gaps in the D-580 tests.
3. page.api kept sending after a challenge.
4. D-577 over-claimed for two processes.
5. The relocate note was wrong after the Ok shortcut.
6. No test for the label's escaping and cap.
7. Runs of spaces in toolstore error text.

What I ran (CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2, worktree at 2464f3a):
- `cargo test -p srv-web -p toolstore`: all green, 0 failed.
  - srv-web: lib 40, accept_t12 5 (+1 ignored), accept_t13 10 (+1), accept_w 6 (+1), contract_paths 1, live 0 (+3), round2_t12 7, round3_t12 10, t13b 12, t13c_api 11, web_tools 13.
  - toolstore: lib 130, accept_t11 41, accept_t11b 9, contract_paths 4.
- `cargo clippy -p srv-web -p toolstore --all-targets -- -D warnings`: clean.
- `cargo fmt -p srv-web -p toolstore --check`: clean.

## Round-1 items

- **1. Fixed.** `http.rs:77` checks `html::challenge_url(attempt.url())` first in the shared api policy, so it covers the api, api_vault and page.api clients. It only ever returns `attempt.error`, never `follow`, so it cannot loop. It can only make the old policy stricter: the same-host, https-downgrade, vault same-origin and MAX_API_REDIRECTS checks at `http.rs:80-90` are unchanged. It fires only on CHALLENGE_HOSTS, google.com `/recaptcha/` and `/cdn-cgi/challenge-platform`, so an ordinary redirect is not stopped. `challenge_redirect` (`http.rs:189`) walks the source chain. The reason names the challenge host or the kind of page, never a header. The stop names the URL asked for (`api.rs:489`, `site.rs:636`).
- **2. Fixed.** `challenge_cases` (`t13b.rs:453`) has six cases, including the 503 without cf-mitigated and three redirect cases (cross-host x2, same-host path). The catching reader is tested at `t13b.rs:586`: it still stops, its return value is not shown, and exactly one request reaches the API host. The api tool redirect is tested at `t13b.rs:611`. Each test checks: no health failure, one core Stop, and `/cdn-cgi/` never requested.
- **3. Fixed.** The check at the top of `PageApi::api` (`site.rs:597`) refuses before any URL parsing or request.
  - The slot is a fresh `Arc` for each call (`site.rs:441`), so it cannot carry over to another call or run.
  - The JS has no handle on it. Only Rust `take()`s it, after the reader returns.
- **4. Deferred** by D-584. The wording now matches the code. See Should fix.
- **5. Fixed.** `file_carries` (`site.rs:299`) uses the same comparison as toolstore's `carries_fingerprint`. The two-server test checks both notes and that b says "patched nothing".
- **6. Fixed.** The test at `t13b.rs:779` covers the link form, brackets, a newline, U+2028/U+2029, runs of whitespace and a 400-char value. It checks for exactly 160 characters in both the summary and the ledger.
- **7. Fixed.** Three messages now use `\` continuations (`lifecycle.rs:410`, `:424`, `:545`; `invariants.rs:174`). Tests assert there is no double space. I found no other long run of spaces in a string in either crate.

## Blocking

- none

## Should fix

1. **Concurrent cross-process repair can still reach version 3** (round-1 item 4, deferred by D-584). `carries_fingerprint` and `fs::write` are a check-then-act (`lifecycle.rs:366`, `:431`), and the row update is a read-modify-write. A file lock or a compare-and-set on the row is still owed in a later task.
2. **Trailing-dot challenge hosts are missed** (`html.rs:289`). `host_str()` keeps a trailing dot, so `https://challenges.cloudflare.com./...` does not match. Since round 2 this function is also the redirect gate. Such a redirect is still not followed (it is cross-host), but it ends as the ordinary "another origin" failure, which round-1 item 1 was about. Fix: `trim_end_matches('.')`, as `toolstore::normalize_host` does.
3. **No test for the vault-header client** (`api_vault`, strict policy) on a redirect to a challenge. The code shares the closure, so it is covered by reading only. Add one case that sends an `@vault` header and asserts a stop and no request to the target.

## Notes

- **Cache::drop_site** (`cache.rs:80`) is correct.
  - **Same folding on both sides.** Both sides use `toolstore::registrable(host_of(Url))`. `Url::parse` gives a lowercased, punycoded host, and `registrable` trims the trailing dot and lowercases.
  - **No panic.** An unparsable URL is not matched. The poisoned lock is recovered.
  - **Other sites keep their pages.** Only matching pages leave `pages` and `by_folder`, and `last` falls back to the newest page left.
  - **Edge cases.** A page whose requested or final URL is on the site is dropped. That matches the D-582 text. Bare-suffix hosts such as `x.github.io` stay separate.
- **Pinned page.** classify pins the page (`site.rs:362`) before the gate check. A challenge on the same site between classify and call does not unpin it. Any page.api call still stops, so this is not a D-5 break, only a narrow window.
- **Text a server controls.** For a vault-header api call, the stop text now names the redirect target's challenge host. The server controls the subdomain label, the same as it controls the body. Core's outgoing scrub covers both. No vault value is added by srv-web.
- **D-583 TOCTOU.** `file_carries` reads the file, then `record` decides on its own. A write by another process in between can only make the note's wording wrong. It changes nothing that is written or recorded, and no gate class. The note shows only the version and status, never file content. An unreadable file reads as "not carried".
- **Rule 4.** classify still returns `gate.max(Read)`. The redirect check only refuses.
- **Hard rules.** No keys, `.env` files, sleeps, em dashes, captcha handling or raw HTML reaching the model were added. No `unwrap` or `expect` outside tests.
- **Spec ids.** Commit ids (W-3, W-7, T-4, D-5, D-579, D-580, D-582, D-583) match the code. Public signatures are unchanged: `ChallengeRedirect` and `challenge_redirect` are `pub(crate)`, and `Web` and `Cache` are crate-private.
- **Cosmetic.** The module doc line at `site.rs:37` runs past 100 columns.

## Verdict

Should-fix items 1, 2, 3, 5, 6 and 7 are fixed, and each has a test that checks the behaviour. Item 4 is deferred by D-584 with accurate wording. Round 2 adds no Blocking defect:
- The redirect check only refuses, and the cross-host and vault-header protections are intact.
- The challenge state lasts one call and the JS cannot reset it.
- drop_site cannot drop another site's pages.

Verdict: approve
