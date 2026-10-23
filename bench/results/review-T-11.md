# Review T-11 (toolstore), combined re-review at 37ab7b0 (swarm/store)

This covers e5305f7 and 955ce52, the fix commits f67a09f (runner, D-308) and 4a61935 (core, D-309..D-311), and agent-memory 37ab7b0. It supersedes review-T-11-runner.md and review-T-11-core.md.

## Commands

All run at HEAD 37ab7b0:

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy -p toolstore --all-targets -- -D warnings` | exit 0 |
| `cargo test -p toolstore` | exit 0. lib 91 passed, accept_t11 23 passed |
| `cargo test --workspace` | exit 0. No failures in any crate |

## Probes

I re-ran the original probes in a scratch crate in my temp dir, against the public API. For comparison I also built the pre-fix tree (41ec965) from `git archive`.

## Prior items: fixed, and the test that would fail without the fix

**Core blocking 1, git pathspec magic: fixed** (archive.rs `git()` and `under_root`).
- Probe: `:/`, `:(top)x`, `:(glob)**`, `:(top)outside.txt`, `:!x`, `../outside.txt` and `luma.com/../../outside.txt` all return Err with no HEAD.
- An absolute `luma.com\*` is a literal path, so git fails on it.
- Test: tests_review.rs:224. The `a*` case exercises `--literal-pathspecs` on its own.

**Core blocking 2, traversal: fixed** (domain.rs `check_tool_id`, used by every path builder and mutator).
- Probe: every `try_tool_path` probe gives BadPath: `../../evil`, `..\..`, `\windows`, UNC, `.git`, `_archive`, `C:\x`, `c:`, NUL in the name, `CON`, `COM1`.
- `folder_for` gives `""` for all of those inputs, and `"c"` for `C:\x`, which is harmless.
- record, reforge, register_forged, log_invariant and archive all refuse bad input, and 0 rows were written.
- Tests: tests_review.rs:266, :320, :338, :376.

**Core blocking 3, vault key by concatenation: fixed** (invariants.rs, D-309 check in `planner_act_class`).
- Probe: check_repair still returns Ok, as D-309 says it will. `planner_class` rejects `@vault(bank_pin)`, `@vault( bank_pin )` and the nested `@vault(a@vault(bank_pin))`.
- Test: tests_review.rs:446.

**Core blocking 4, the three escapes from ask: fixed** (lifecycle.rs `held`).
- Probe (a): a re-forge at the same floor stays Ask.
- Probe (b): Relocated stays Ask.
- Probe (c): a miss then Ok stays Ask. So do 8 misses then 8 Oks, a sweep 400 days later, and pinning.
- register_forged on an Ask row returns Err. A re-forge back down to Submit returns Err (gate lowered).
- Tests: tests_review.rs:495, :508, :535, :558.

**Core should-fix items: fixed, with tests.**
- `?s` in a comment and `?sort` no longer count (:580). Partly open, see Should fix 4.
- Sweep idle clock is now max(last_ok, last_fail) (:614).
- `@match` rejects `\`, `%2e`, `%2f` and `%5c` in any case (:663).
- read_table has caps (:725). The caps add a new bug, see Should fix 2.
- Write order: retire persists the row first (:759), and reforge rolls back (:783). The rollback adds a new bug, see Should fix 3.
- `folder_for`: private suffixes, IDN, `.git` and `_archive` are covered (:338).
- Relocated on a retired row is refused (:558).

**Runner should-fix items 1-6 and nits: fixed.**
- `erase` is an `unsafe fn` with SAFETY at its one call site (runner.rs:309, :448). The argument is sound.
- API deadline. Probe with a 200 ms cap and a host that overruns: Ok, a caught error, a second call, and a promise chain all return Timeout(200ms) in about 231 ms. The second call never reaches the host. Test: runner/tests.rs `runner_api_past_the_deadline_is_timeout`.
- Output cap. Probe: a 5M-char reader gives OutputTooLarge{got: 5000002}. A 5M-byte planner and 40k `é` (80002 bytes) are also refused. Test: `runner_output_cap`.
- OOM. Probe: `'ab'.repeat(2**28)` and 16 x 1 MiB through json_stringify both give OutOfMemory. `throw new Error('out of memory')` gives Threw. Tests: `runner_oom_in_json_stringify_is_out_of_memory`, `runner_thrown_out_of_memory_text_stays_threw`.
- Globals equal the allowlist: `runner_globals_are_exactly_the_allowlist`. A global that cannot be removed fails the load: `runner_prelude_fails_on_a_global_it_cannot_remove`.
- q(undefined/null) throws. The import and stack tests are tighter. rquickjs comes from workspace deps. D-308 is recorded, and IC-302 covers part of the surface (see Should fix 5).

## Blocking

- none

## Should fix

1. **runner.rs:417: the CappedAlloc switch makes small memory limits crash the process (a segfault from a safe pub fn).**
   - Failing input: `run_reader_with(&t, &page, &json!({}), None, &RunOpts { memory_limit: 0, ..Default::default() })` exits with SIGSEGV (139).
   - The same happens for 1024, 16384, 32768 and 40000. 44000 exits 127.
   - Before f67a09f, limit 0 meant "no cap" and returned Ok, and 1024..28000 returned `Err(Engine(..))`.
   - Root cause: `Runtime::new_with_alloc` in rquickjs 0.14 crashes when the allocator refuses during runtime creation. I reproduced this with a bare capped allocator outside toolstore.
   - Small caps were already unsafe before the fix (41ec965 crashed at 29500). The new code also crashes mid-startup at 165000 and 168000 for a small tool.
   - A tool cannot trigger this at the 32 MiB default. Tool-driven fill probes at 0.3, 1, 4, 8 and 32 MiB all ended in a clean OutOfMemory.
   - Fix: admit every allocation until the context and prelude exist (for example a `limit: Rc<Cell<usize>>` that is raised to `memory_limit` just before the tool module is evaluated), or clamp `memory_limit` to a floor such as 1 MiB and map 0 to the default. Add a test for 0 and 1024.
2. **tools/_generic/read_table.js:49: `s.slice(0, MAX_CELL - 1)` can split a surrogate pair, and the whole read then fails.**
   - Failing input: a cell of 198 x `a` + U+1F600 + `tail` + 50 x `b` gives `bad return: unexpected end of hex escape at line 1 column 262`. serde_json rejects the lone surrogate.
   - Any table with emoji or astral CJK near position 199 fails.
   - Fix: cut by code points (`Array.from(s).slice(0, MAX_CELL - 1).join('')`) or step back one unit when the cut lands on a high surrogate. Add a test with an emoji at the boundary.
3. **lifecycle.rs:389-396: reforge's `undo` can delete the only copy of the old tool.**
   - When `fs::write(&path, new_src)` fails (disk full, ACL), undo retries `fs::write(&path, &old)`, which likely fails for the same reason, and ignores that error.
   - Undo then runs `remove_file(archived)` anyway. The tool is gone from both `tools/` and `_archive/`, which is a direct delete (CLAUDE.md: retire, archive, then delete).
   - Fix: restore with `fs::rename(archived, &path)`, and remove nothing unless the restore succeeded.
   - Found by reading the code. Probing it needs a write failure after the move.
4. **invariants.rs:144 (`stop_count`): still gameable. This carries over from the core review and is partly fixed.**
   - Replacing the real stop with a dead string gives stop_count=1, and `check_repair` returns Ok. Example: `c#${btn}\`;` plus `const unused = "?s";`.
   - A regex literal confuses the lexer: `const re = /"/; // ?s` is counted.
   - The run-time PlannerCheck (D-50) is still the backstop, and check_repair still never calls it, although D-307 lists "fail sb-act's ?s rule".
   - Fix: either say in D-307 that check_repair is advisory for `?s`, or count only `?s` inside act template or quoted text.
5. **IC-300..IC-303 (agent-memory/blockers.yaml:94-113) do not cover the whole public surface.**
   - Public but unrecorded in the runner: `ALLOWED_GLOBALS`, `FIND_ALL_K`, `table_json` (runner review 5b).
   - Public but unrecorded free fns and items: `stop_count`, `vault_keys`, `has_task_id`, `normalize_host`, `registrable`, `SiteConfig`, `fingerprint_items`, `role_matches`, `norm`, `parse_fingerprint` / `render_fingerprint` / `patch_fingerprint`, `namespace`, `glob_match`, `url_path`, `skeleton_line`, and the lifecycle consts.
   - IC-303 also omits three behaviour changes:
     - `tool_path` / `archive_path` now return an empty PathBuf on bad input.
     - `record(Relocated)` on a retired row now errors.
     - A reforge of an ask row stays ask (D-310).
   - Record these, or make the items `pub(crate)`.
6. **Spec-id over-claim is still open.**
   - 955ce52 claims T-1..T-9, and 4a61935 claims T-5. T-6, T-8, T-5's auto `@expects` and G-5's hand-edit reset are not implemented (handoff T-11-rust-coder-4 not_done 1).
   - tasks.yaml T-11 does not list the ids that shipped. Write them there and in the next handoff.

## Notes

- **The rest of CappedAlloc is sound.**
  - All pointers come from and go back to `RustAllocator`. realloc of null goes to alloc, and calloc checks for overflow.
  - Byte accounting stays stable over 50 runs and over churn loops.
  - Minor points:
    - The calloc overflow path (runner.rs:361) returns null without setting `hit`, so that case reports Threw and not OutOfMemory.
    - The OOM flag is sticky. A tool that catches an OOM and then throws its own error gets OutOfMemory. This is harmless.
    - The new code reports a tool-driven OOM that escapes as OutOfMemory. Before the fix it was `Threw("uncaught null")`.
- **Vault (D-309).** `vault_keys` is case-sensitive, so a program with `@VAULT(bank_pin)` passes `planner_class`. This is fine only if the resolver is case-sensitive too. Switch both checks to `harness_core::vault::vault_refs` when T-05 lands, so there is one grammar. srv-web's ApiFetch must not resolve `@vault(` in tool-supplied URLs (W-3 resolves headers only, and readers pass only a URL).
- **`@match`.** `/%252e%252e/x` (double-encoded) and `/\t/evil.com` (tab) pass. This is harmless while `@match` is only a path filter.
- **Sweep.** A failed archive in `retire` or in the Retired arm (lifecycle.rs:521, :532) aborts the whole sweep. While `_archive` is blocked, every sweep errors before other domains' 90-day deletes run.
- **Loader.** loader.rs:129: hosts that map to `""` (IPv6 literal, raw Unicode host) also get no `_generic` tools.
- **read_table.** `max: 0.5` floors to 0 rows.
- **Ask row.** A re-forge of an ask row back to its confirmed floor is refused as lowering the gate (probe (g)). This is conservative and acceptable.
- **Hard rules hold.**
  - No keys or `.env`.
  - No vault values. Errors and `why` carry key names only.
  - No captcha code.
  - No side effect outside the caller's gated ApiFetch.
  - No raw HTML. Output is capped.
  - No sleeps. The only wait is a spin in a test fake.
  - No em dashes. read_table uses `…`.
- **Layering and interfaces are unchanged.** D-38 layering holds: toolstore depends on core, sb-sheet and sb-act. There is no diff to crates/core, crates/sb-sheet, crates/sb-act or interfaces.yaml, so I-1 (`MemoryStore` / `ToolHealth`) and I-5 (`IndexRow`) are untouched. The I-7 signatures (`folder_for -> String`, `load`, `record`, `fingerprint`) are unchanged.
- **Carried notes from the earlier reviews, still open and not blocking:** relocate synonyms, the G-2 static denylist, sb_act::parse / SheetPage `todo!`, and list_jobs `decodeURIComponent`.

## Verdict

All four core blocking items and every runner and core should-fix are fixed, each with a test that fails on the old code. No hard rule is broken, and acceptance (23/23) and the workspace are green. Should fix 1, the crash for small or zero memory limits, is the most important item for the next round. It is a regression in the range of crashing limits, but not in kind, and a tool cannot reach it at the default limit.

Verdict: approve
