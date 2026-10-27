# Review T-12b, round 3 (confirmation)

Scope: `git diff 8cc2328..HEAD -- crates/` on swarm/servers-T12b. Code commit a487bc7, plus ccf2cfb (D-519, D-526, D-527, handoff T-12b-rust-coder-3.yaml). Round 2 asked for changes on two Blocking items: (1) ~4000 nested elements overflow the recursive walk's stack and abort srv-web; (2) html5ever's tree build is quadratic in nesting depth, uninterruptible, and feed/search parsed on the async worker with no cap.

Gates (CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2): `cargo test -p srv-web --no-fail-fast` exit 0, 161 passed, 0 failed, 5 ignored (adds t12b_nesting 2, t13b +1, dom/feed/search/http unit tests). `cargo clippy -p srv-web --all-targets -- -D warnings` clean. `cargo fmt -p srv-web -- --check` clean.

Release probes (throwaway crate outside the repo, srv-web via the shared target; binaries removed afterward, repo clean). Each body is `<open>` x N around "before words" / "deep words", run on a 2 MiB thread with a 60 s watchdog:

| open | d=4k | d=40k | d=100k | d=1M | result |
|---|---|---|---|---|---|
| `<div>` html | 4.5 ms | 3.3 ms | 3.3 ms | 4.1 ms | cut=Deep, before shown, deep dropped |
| `<div>` plain | 3.8 ms | 8.2 ms | 14 ms | 118 ms | cut=Deep |
| `<b>` html/plain | ~1 ms | ~1-4 ms | ~1-9 ms | ~1-83 ms | cut=Deep |
| `<span>` html/plain | ~1 ms | ~1-6 ms | ~1-15 ms | ~1-125 ms | cut=Deep |
| `<div role=button>` | 38 ms | 33 ms | 43 ms | 37-187 ms | 541 control rows, cut=Deep |
| `<td>` (dropped outside a table, so nothing nests) | 1-2 ms | 12-16 ms | 33-41 ms | 323-421 ms | cut=None, linear |

- `raw_main_html` on 1M-deep `<div><i>`: 1.3 ms, keeps the lead.
- feed RSS with a 100k-level escaped `<description>`: 35 ms, one item, summary empty (deepest word is below the cap). iCal with a 100k-level description: 6 ms. search answer with a 100k-level snippet: 0.6 ms. None stalled.
- Coder's remaining slow case (500 `<div>` then 5 MB of `<p>x</p>`, past the walk cap but under the 512 parse cap, so only the time deadline bounds it): the unlimited parse finished in 7.2 s in release (handoff reported 64 s in debug), well inside the 15 s fetch budget. The feed loop checks the deadline before each 1 KB piece, and one piece at depth ~500 is thousands of scope walks (microseconds), so a spent deadline stops the parse within one piece and frees the thread (dom `a_spent_budget_gives_up` and feed/search zero-cap tests assert the stop). No probe timed out or disconnected; every thread returned.

## Blocking

- none

## Confirmation of the round-2 items

- **Blocking 1 (walk stack overflow) closed.** `Walker::walk` now counts depth and, past `WALK_DEPTH_MAX` = 256 (html.rs:636), reads the subtree as plain text via an iterative `collect_text` (html.rs:1439) with a one-line note; `nested_controls`, `collect_text`, `fragment_text_in` and `serialize` all use explicit stacks, so no tree depth touches the thread stack. The parse itself stops at `NEST_MAX` = 512 (dom.rs:34), so a tree is never deeper than ~512. `html::tests::the_walk_fits_a_small_stack` walks nine nesting shapes at ~470 levels on a 1 MiB thread (half the usual stack); a stack overflow would abort the binary, so it passing is the proof. My 1M-deep probes returned in milliseconds on a 2 MiB thread with no abort.
- **Blocking 2 (quadratic, uninterruptible parse; uncapped feed/search) closed.** Every html5ever parse in srv-web goes through `dom::document`/`dom::fragment` (grep confirms `parse_document`/`parse_fragment` appear only in dom.rs; html.rs, feed.rs, search.rs route through `dom` or `fragment_text_in`; `sb-sheet` carries no HTML parser). `feed` (dom.rs:127) processes 1 KB at a time and between pieces checks the deadline (`Budget`), the open-element depth vs `NEST_MAX`, and a node cap of 2/byte + 1024, keeping what it read. `feed::parse_with` and `search::parse_answer_in` now run in `spawn_blocking` under `fetch::WALK_MAX` with a task timeout (feed.rs:174, search.rs:205). Deep probes all returned in milliseconds; the 5 MB block-tag case stayed under budget.
- **No text lost in a way that breaks rule 5 or W-2.** A cut drops only what sits below 512 levels (Chromium's own `kMaximumHTMLParserDOMTreeDepth`) and adds a note under the skeleton; kept text still goes through `fragment_text_in`/escape, so no markup or pixels reach the model. Acceptable with the note.
- **No new Blocking defect.** No `unsafe` and no production `unwrap`/`expect` on parsed input in dom.rs, feed.rs or search.rs (unwraps are test-only or `unwrap_or`). `check`'s `borrow()` runs only between `process` calls, never during one. D-527 touches no gate class: it keeps `Relocated` (which toolstore refuses for a retired tool) and removes the tool from the registry, so a retired tool is never enabled or re-registered; `t13b::a_retired_tool_is_not_revived_by_a_second_repair` asserts runs/version unchanged and the tool gone from the registry. D-526 caps api's lookup at `LOOKUP_MAX` = 5 s within the request budget (http.rs); `the_api_lookup_cap_is_short` asserts the value and the shorter-timeout clamp.

## Should fix (follow-up T-12c candidates)

1. **render_read and find still run on the async thread after the walk, outside its cap**, and the walk deadline starts after an HTTP read that can itself take 15 s (round-2 should-fix 1). Handoff leaves this for a follow-up.
2. **Wall-clock asserts in debug tests may flake under workspace load** (`hostile_angle_runs_are_linear`, accept_t12b's 20 s) (round-2 should-fix 4).
3. **A body ~500 deep then megabytes of block tags is linear-times-depth in html5ever** (7.2 s release for 5 MB here, under budget, but a larger body leans on the time deadline alone). A lower `NEST_MAX` would halve it at the cost of real pages between 256 and 512 levels; the deadline already bounds it, so this is a tuning note.
4. **D-526's test checks the cap value, not a stalled resolver** (`PublicOnly` has no delay seam); the real slow-lookup path is unexercised.

## Verdict

Both round-2 Blocking items are closed and backed by tests that would fail if reverted (handoff break-checks 1 and 2 confirm this): the walk is depth-capped and iterative, every parse is bounded by depth, node count and a between-pieces deadline, and feed/search parse off-thread under the 15 s cap. Release probes at 4k, 40k, 100k and 1M depth and the 5 MB block-tag case all return quickly with no abort and no thread left busy. No parse path bypasses dom.rs, no text loss breaks rule 5, and no new Blocking defect. Four should-fix items remain for T-12c; none blocks the merge.

Verdict: approve
