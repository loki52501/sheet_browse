# Review T-13, round 1

Scope: `git diff swarm/servers-T12...d42e8b4` plus the claim commit c3f1ffc. Covers the seeded `tools/ashbyhq.com/list_jobs.js`, srv-web `site.rs` / `fetch.rs` / `lib.rs`, `tests/accept_t13.rs`, `tests/common/mod.rs`, the `round2_t12.rs` change, D-560, D-561 and B-560.

What I ran (CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2):
- `cargo test -p srv-web --test accept_t13 --test round2_t12`: 12 passed, 1 ignored (the live test).
- `cargo clippy -p srv-web --all-targets -- -D warnings`: clean.
- `cargo fmt -p srv-web --check`: clean.

## Blocking

- none

## Should fix

1. **Two repairs at once can record version 3** (`crates/srv-web/src/site.rs:300-311`, `:363-371`; handoff "Known edge"). Two concurrent calls on the same partial page can both see `disabled || needs_repair` and both compute `relocate`. Both then `record(Relocated)`. The second call's `check_repair(patched, patched)` passes, so the version is bumped twice, the ledger gets two `tools.relocate` events (the second reads "X -> X"), and the model gets two "relocated" notes. This is more likely than the handoff says: the model can call `ashbyhq.list_jobs` twice in parallel with different `org` args on one pinned page. It breaks no rule, but it breaks the "version = 2" acceptance property under concurrency. Two cheap fixes:
   - a per-tool repair `Mutex` in `Web`, re-checking the row after the lock, or
   - skip `Relocated` when the file's current `@fingerprint` already equals the new one, and record `Ok` instead.
2. **D-561 contradicts T-4's own text, and no blocker records it** (`agent-memory/decisions.yaml:460`, `site.rs:126-129`, `site.rs:300`, `site.rs:370`). T-4 says a miss leaves the tool "disabled for this run". D-561 re-enables a process-disabled tool within the same run once a later fetched page matches it. The behaviour is reasonable, because it saves turns and the fingerprint is re-checked on the call. Two problems remain:
   - It amends the spec, not just D-505/D-506. Write a `blockers.yaml` entry (or a supreme-acknowledged decision) so supreme sees it.
   - The acceptance item does not need the in-process re-enable. A `needs_repair` row from an earlier run, on a fixed page, already reaches relocate through the `listed(&broken, t)` branch.
3. **Rule 4 is not asserted after the re-register** (`crates/srv-web/tests/accept_t13.rs:283-292`). After the relocate, `reregister` (`site.rs:220-234`) replaces the spec. Class safety does hold today, through two mechanisms: `check_repair` refuses any `@gate` change on relocate (`toolstore/src/invariants.rs:193`), and the registry keeps a class floor (D-127). But the acceptance test never checks `e.registry.get("ashbyhq.list_jobs").unwrap().class == Class::Read` after the repair. One line would pin rule 4 on this new path.
4. **docs/03 acceptance item 4 is not mapped** ("never emits a password field's value, never emits content of a CHALLENGE frame"). The T-13 handoff covers items 1 to 3 and leaves item 4 out. Existing tests do cover the inputs: `html.rs` password tests and `web_tools.rs::challenge_pages_stop_the_call` (a challenge page is never cached, so no site tool can run on it). Either list those in the handoff for the tester, or add one site-tool-level test, e.g. `generic.read_table` / `page.find` on a page with `<input type=password value=...>`, asserting the value is absent.

## Notes

- **(a) Rule 4 and the relocate file write.** srv-web never runs git: there is no `Command`, `git_commit` or `process::` in `crates/srv-web/src` (D-560 holds). The relocate write happens inside the site-tool call, which is gated at `max(@gate, Read)` with the page's registrable domain (`site.rs:167`, classify). It goes through `ToolStore::record(Relocated)`:
  - `check_repair(Relocate)` runs first;
  - the file is then written;
  - the core `tools.relocate` ledger event follows (`toolstore/src/lifecycle.rs:347-375`).

  I judge this acceptable. T-4 requires the patch as part of the pre-run check. It is harness-internal tool-store state, like the `tools.health` write every run already does. It is invariant-checked and ledgered, and it touches nothing outside the process. Optional improvement: classify's `what` could say "may relocate (T-4)". The cost of D-560 is that the docs/03 rule "every change to tools/ is a commit" is deferred to T-16, which is documented.
- **(b) Capability gain on repair.** None found. `check_repair(Relocate)` compares every non-`@fingerprint` header line and the body byte for byte, and checks `@gate`, `@kind`, `@phase` and vault keys (`invariants.rs:177-235`). `reregister` also requires the same name, kind=reader and phase=1. Defence-in-depth gap in toolstore (pre-existing, not this diff):
  - `relocate` copies `index[i].name` raw into the file (`toolstore/src/fingerprint.rs:222`), and `FpItem` Display writes other characters raw (`toolstore/src/header.rs:63`).
  - Rust `header_lines` splits on `\n` only, but JS also ends a `//` comment at `\r`, U+2028 and U+2029. A name holding one of those would turn the rest of the line into reader code, and `check_repair` would not notice.
  - It is unreachable from srv-web today, because every index name goes through `escape::name` (`split_whitespace`).
  - Phase 2's live index must keep that guarantee. Better, `check_repair` or `FpItem` Display should refuse JS line terminators and control characters. Worth a toolstore follow-up (B-33 family).
- **(c) Seeded tool.** `page.api` is still limited to the page's registrable domain (`site.rs` PageApi, D-501), and `org` is `encodeURIComponent`'d. Output is capped: 60 jobs, 20 000 characters, and the runner's 64 KiB. There is no vault or header use. The http-follows-page scheme (`tools/ashbyhq.com/list_jobs.js:18-20,32`) is a test seam inside a production tool, but the risk is low:
  - it only applies when the final page URL is plain http, which Ashby redirects away from;
  - the API is a public read with no credentials;
  - its integrity is no worse than the http page the model already read.
- **(d) Rule 5.** The relocate note escapes page-derived fingerprint names with `escape::name` (`list()`). The `page.api` errors carry only status, host and cap (`http::map_err` holds just the host; `site.rs:458` names the cap). Tool throw text is escaped and capped at 300. No body text reaches the model.
- **D-505's reasoning is weakened, not answered.** A "clean run" verifies a relocated fingerprint only if the reader actually reads the index. `list_jobs` uses only `page.api`, so its clean run proves nothing about the patched selectors. This is acceptable because a relocate can only touch `@fingerprint`. Mention it in D-561's why.
- **(g) The `round2_t12` flip** (`crates/srv-web/tests/round2_t12.rs:620-622`) is consistent with D-561 and with D-304 ("a needs_repair tool that matches ... becomes repaired"), and with T-7's needs_repair exit. Coverage of "a `needs_repair` row whose fingerprint misses is not registered", in a fresh process without the in-process disable, is now only implicit: it shares the `is_disabled || listed(&broken)` branch at `site.rs:126`. A one-line case would make it explicit.
- **(f) The tests assert behaviour.** `accept_1` checks the exact skeleton line, exact JSON, the API request shape, the registry class and the ledger event. `accept_2` checks the row at v1 then v2, `why`, the patched file text, `@gate`/`@phase` kept, the ledger relocate event, that the tool is not offered on the still-broken page, and the plain match afterwards. Both are red without the change:
  - without the scheme change, accept_1's https call to the plain server fails;
  - without D-561, the fixed-page fetch never lists the tool.
- **Fail-closed check.** An unreadable health row gives "not broken" and no patch (`site.rs` `needs_repair`). Register still fails closed on a store error. A disabled tool reached through a stale registration is still refused by the call-time fingerprint check.
- The commit's spec ids (T-3, T-4, W-7, D-560) match the code. `is_multiple_of` is fine on rust-version 1.94. No `unwrap` on external input in `src`; the `lock().unwrap()` calls are only in test `common`.

## Verdict

No hard-rule breaks. Acceptance items 1 and 2 are tested and asserted end to end through dispatch. Should-fix items 1 to 4 are small and can go to a T-13b follow-up or a round 2.

Verdict: approve
