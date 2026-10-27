# Review T-13b, round 1 (D-89)

Scope: `git diff swarm/servers-T13c...swarm/servers-T13b`. That is commits d3e9246 (D-577..D-581), a0b59c9 (toolstore), f150f5a (srv-web) and 88a27ee (handoff), plus the claim and progress commits. Sources checked: handoff T-13b-rust-coder-1.yaml, decisions D-577..D-581, docs/03 T-2..T-4, T-9 and step-2 item 4, docs/04 W-7, blockers B-561 and B-33.

What I ran (CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2, worktree at 88a27ee):
- `cargo test -p srv-web -p toolstore`: all green.
  - srv-web: lib 40, accept_t12 5 (+1 ignored), accept_t13 10 (+1), accept_w 6 (+1), contract_paths 1, live 0 (+3), round2_t12 7, round3_t12 10, t13b 8, t13c_api 11, web_tools 13.
  - toolstore: lib 130, accept_t11 41, accept_t11b 9, contract_paths 4.
- `cargo clippy -p srv-web -p toolstore --all-targets -- -D warnings`: clean.
- `cargo fmt -p srv-web -p toolstore --check`: clean.

## Blocking

- none

## Should fix

1. **The D-580 "challenge URL" case is only half covered.** `site.rs:579` catches a challenge URL only after a redirect that page.api followed, which means a same-host one such as `/cdn-cgi/challenge-platform/`. The api client refuses a cross-host redirect (`http.rs:84`). So a redirect to `challenges.cloudflare.com`, `hcaptcha.com` and the like ends at `site.rs:570` as "redirected somewhere this tool does not follow". That is an ordinary error, the tool's health records a `Failed` outcome, and nothing stops. `reqwest::Error::url()` gives the refused target, so this can be checked with `html::challenge_url`. The `api` tool has the same gap. No test anywhere covers a challenge URL redirect, same-host or cross-host. Fix both in T-13d, or narrow the D-580 text.
2. **Gaps in the D-580 tests** (`t13b.rs:430`). Three cases are missing:
   - A reader that catches the page.api error and returns normally. `list_jobs.js:55` does not catch, so the "whatever the reader made of it" line at `site.rs:446` is never tested on its own.
   - A non-2xx HTML challenge without `cf-mitigated`.
   - The redirect case from item 1.
3. **page.api keeps sending after a challenge.** Once the slot is set, later `page.api` calls in the same run still go to the network until the 2 s wall cap. A reader that catches the error can keep calling the host that challenged it. Check the slot at the top of `PageApi::api` and refuse.
4. **Concurrent repairs from two processes can still reach version 3.** `carries_fingerprint` (`lifecycle.rs:366`) and `fs::write` (`lifecycle.rs:431`) are a check-then-act with no lock across processes, and the row update is a read-modify-write. D-577 says it "covers two server processes". In fact it covers only repairs that run one after the other, which is all `two_servers_repairing_the_same_tool_leave_version_2` tests. Reword D-577, or add a file lock or a compare-and-set on the row.
5. **The relocate note is wrong after the Ok shortcut** (`site.rs:467`). When toolstore turned the `Relocated` into `Ok`, the second server still prints "fingerprint relocated ... patched to version 2", but it wrote nothing. This is the coder's open note (b). Tell the model the truth: compare the row's `why` or the version before and after, or have `record` return which path ran.
6. **No test for the label's escaping and cap** (`site.rs:239`, D-579). The label is built from model-supplied args and goes into the ask card and the ledger. Escaping and the cap look right: `escape::name` collapses whitespace, turns `[`/`]` into `(`/`)`, and caps the escaped text at 160 characters. But no test sends a long arg value, a newline or `](#`. Without one, a later edit that drops the escape or the cap stays green.
7. **Stray runs of spaces in error text.** Bad string continuations left long runs of spaces inside three messages: `lifecycle.rs:410`, `lifecycle.rs:423` and `invariants.rs:174`. These reach `tools.health.why` as written. The model's note collapses them through `escape::name`. Use `\` continuations.

## Notes

- **Rule 3 open note (a): not Blocking.** A site tool can still run on a page cached before a later fetch of the same site hit a challenge. I do not count this as a D-5 break:
  - The challenge page itself is never cached and offers no tool (`a_challenge_page_offers_no_site_tool`).
  - The reader works on bytes fetched before the challenge.
  - Any network call it makes goes through page.api, which now stops on a challenge.
  - Core does not end the run on any `stopped` output, fetch's included. The hand-back is the model's step, and the model could just as well call `fetch` again.

  Nothing here solves or gets around a challenge. Suggested for T-13d: when fetch hits a challenge, drop that site's cached pages (or mark the site as challenged for the run), so site tools also refuse there until the user says continue.
- **Rule 4: no class can be lowered.** classify still returns `gate.max(Read)`. The repair lock and the Ok shortcut write no header. `check_repair` now refuses more, never less.
- **The Ok shortcut compares only `@fingerprint`, but no other file content can pass through it.** The shortcut writes nothing. Even before this change, the `Relocated` path ran `check_repair(old, patch(old))` on whatever was on disk, so a body or `@gate` that changed under it was never checked here either. The run that gets recorded is the in-memory tool. A reforge in between moves the row out of needs_repair, so the shortcut then only adds a clean run. `!retired` still refuses a retired row. An `Ask` row stays `Ask`.
- **The lock is correct:**
  - The outer std `Mutex` is held only to clone the `Arc` (`lib.rs:79`), with poison recovered.
  - The tokio mutex does not poison. Its `lock_owned` is cancel-safe, so a dropped call releases the guard.
  - There is one lock per call and none nested, so it cannot deadlock.
  - The guard is held across the reader (2 s wall cap with an interrupt, page.api bounded by the remaining time), `record_row` and `reregister`. The only wait without a bound is a store call that hangs. That is the same for every tool and only blocks other repairs of the same tool.
  - The map is keyed by on-disk site tools, so its size is bounded.
  - The re-check after taking the lock is correct. The waiter runs as a healthy partial with a note (D-505).
- **Small race in one process.** If a `Match` call records `Ok` while a `Partial` call holding the lock is running, the `Partial` call then records `Relocated` and reaches version 3 with a real new fingerprint. This is rare, and it is a different fix.
- **D-578 holds.** `header_unsafe` covers all four JS line terminators (LF, CR, U+2028, U+2029) plus every other Cc character. Relocate skips unsafe names and unsafe form ids, and `fingerprint()` then returns Miss, not Partial. Record refuses the item and also checks that the patched line reads back as written. `check_repair` scans the comment block, including a line that starts with U+2028 before the `//`. The refusal names only the role, and the tests check that the page text is absent.
- **D-579.** The label uses only the args declared in `@args`. Site tool args are never vault-resolved, and core scrubs args, `what` and the summary against vault values. A cut label has no truncation marker. That is cosmetic.
- **Panics.** No new indexing or slicing on input. No `unwrap` or `expect` outside tests. serde serialising a `Value` is total.
- **Spec ids.** T-4, T-9, W-7, D-5, B-33 and D-577..D-581 match the code. D-577..D-581 are not on main. B-561 stays open for supreme, with D-581 recording the default. Public signatures are unchanged: the new helpers are `pub(crate)`, and `Web` is `pub(crate)`.
- **Tests fail when the behaviour breaks** (by reading the code, not by editing it):
  - Without the lock's re-check, the in-process test sees two relocate notes.
  - Without the Ok shortcut, the two-server test reaches version 3.
  - Without the challenge slot, both challenge cases return an error, not a stop.

  The coder's break checks agree. The rule-4 tests check the end-to-end property only (D-127 floors), as the handoff says.
- **Hard rules.** No keys, `.env` files, sleeps, em dashes, captcha handling or raw HTML reaching the model were added. The password test asserts that no value reaches the tool, the sheet, fetch or the ledger.

## Verdict

All six items are implemented with tests that check the behaviour, and clippy and fmt are clean. No hard-rule break. The gaps are in coverage of the D-580 claim: the redirect case, a reader that catches the error, and page.api calls after a challenge. Also D-577's two-process wording and the relocate note's wording. Items 1 to 4 should land in round 2 or move to T-13d.

Verdict: approve
