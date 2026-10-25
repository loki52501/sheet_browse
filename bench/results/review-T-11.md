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


## Addendum (store sub-orchestrator, after this review)

- Should fix 1-3 were fixed in 87c725f with regression tests that fail on 7b44132: memory_limit 0 = default and the cap is armed only after the runtime, prelude and module compile (runner_small_memory_caps_never_crash runs each limit in a child process); read_table.js cell cut keeps surrogate pairs whole; reforge keeps the archived copy when the rollback fails.
- Should fix 4 is recorded as B-311 (sb_act::check_planner on the emitted program is the authoritative ?s check), 5 as IC-304, 6 as tasks.yaml T-11 shipped/deferred. The D-309 case note is B-312.
- cargo test -p toolstore: 95 lib + 29 integration pass; cargo test --workspace: 135 pass.

## Round: D-78 batch (93a2e4d)

Scope: 93a2e4d against D-78, D-72, D-59, D-312, D-313, D-314, B-31 (five items), B-300, I-7 and I-12. Reviewer: reviewer-4, 2026-10-25.

Commands at 93a2e4d: `cargo test -p toolstore` exit 0 (112 lib, 29 accept_t11, 1 contract_paths); `cargo clippy -p toolstore --all-targets -- -D warnings` exit 0; `cargo fmt -p toolstore -- --check` exit 0. Probe (PowerShell, scratch dir): a read-only file on NTFS cannot be opened for write (access denied) but can be renamed.

Checked and holding:

- Hard rules: no keys or .env, no vault value in any return, log, row or ledger (errors name keys only), no captcha code, no new side effect, no raw HTML, no sleeps, no em dashes. Every new `unwrap` is in a test.
- Interfaces: no pub item added, removed or changed. `archive_with` and `header::is_tool_name` are private. The only API change is the two `#[deprecated]` markers (archive.rs:40, :47). tests/contract_paths.rs imports every I-7 and I-12 path except the two deprecated builders, which tests_review.rs:333 pins under `#[allow(deprecated)]`. No caller outside toolstore uses the deprecated builders.
- Spec ids: B-31 items 1 to 5, D-72, D-59/B-300, D-312, D-313 and D-314 are all implemented. Nothing ships outside the claimed ids (contract_paths.rs is the D-74 pr-ready step; `new_row` moving to `try_tool_path` is D-72).
- (1) New tests can fail. `archive_fails_closed_when_mtime_cannot_be_set` fails without the rollback (tool missing), without the error return (`matches!`), and without the real set-mtime (200-day-old mtime survives the rename). `kind_change_is_rejected_both_ways` asserts on the D-313 message, so the later relocate header check cannot satisfy it, and a reader to planner reforge has no other check. `phase_lowered_is_rejected_raised_is_allowed` uses `with()`, which asserts that its pattern is present. `upper_case_name_is_refused` relies on `unwrap_err`. `reforge_refuses_a_new_name_that_is_not_the_called_name` uses an old file whose @name is already `signup`, so check_repair passes and only the D-312 check can refuse. The B-31 (3) canary is now fed: `assert!(more.contains(&canary))` (accept_t11.rs:961) guards the replace, and without the vault-key check the canary is written to login.js and found by the tree sweep. The one-test-per-alias tests for identity aliases (button, checkbox, radio, link, form, tab) pass on equality alone; only their negative asserts touch the table. That is inherent and fine.
- (2) Archive rollback, single failure: correct. The rename back uses the same `to`, so a `.<n>`-suffixed archive name (archive.rs:89) rolls back to `<name>.js` the same way. The double failure is covered under Should fix 2.
- (3) role_matches (fingerprint.rs:93) keeps every old alias (input to textbox/searchbox, textarea, select, button, checkbox, radio, link, form, tab, table/grid). The two disagreements are acceptable under D-49 and docs/03 T-4. roles.txt lists `alias spinbutton` before `alias number` and has `alias switch`, so sheet lines print `spinbutton` and `switch` for those rows. A fingerprint written from a sheet line already uses those names, and `find('input ...')` does not hit a spinbutton either. A hand-written `input`/`checkbox` item on such a row now misses, relocate cannot re-find it (role mismatch), the tool goes to needs_repair and gets re-forged. That fails closed, costs at most one repair, and is the single-table rule D-49 exists for. No fingerprint in tools/ or tests/fixtures uses either pair (the only one is fixtures luma.com/register.js:6, `form#f2 button"Register" input"Email"`).
- (4) Lower-case names strand nothing that exists. The seeds (tools/_generic/read_table.js, tools/ashbyhq.com/list_jobs.js) and every fixture are lower case. migrations/0001_init.sql seeds no tools.health rows. Sweep skips legacy upper-case rows (lifecycle.rs:528), and archived upper-case files are still deleted at 90 days by the name-agnostic loop (lifecycle.rs:598).
- (5) The D-313 checks (invariants.rs:200, :208) come after the @match, rename and @gate checks and before ?s, vault and relocate-only. They are inside check_repair, so reforge logs them through `log_invariant` (lifecycle.rs:381) and record(Relocated) logs them through its existing check_repair path, like every other G-4 refusal.

## Blocking

- none

## Should fix

1. **archive.rs:66-70 with lifecycle.rs:555 and :566: a read-only tool file on Windows can never be archived, and since D-314 that stops every sweep.** `File::options().write(true).open` is denied on a read-only file, which the probe confirmed; rename works. Before this change that failure happened once (and left an archive copy that could be deleted early). Now archive renames back every time, `retire`/`archive` return Err, and `sweep` propagates it with `?`. So every reflect sweep aborts at that tool, before the retires and 90-day deletes of later tools and domains run. D-314's "the caller retries at the next sweep" assumes the failure is transient. Fix: set the time through a handle that needs only FILE_WRITE_ATTRIBUTES (`std::os::windows::fs::OpenOptionsExt::access_mode(0x100)` on Windows), and/or have sweep record a per-tool failure and continue. Add a test with a read-only tool file.
2. **archive.rs:101-112: when the set-mtime and the rename back both fail, the tool stays in `_archive/` with its old mtime, so the next sweep's 90-day loop (lifecycle.rs:598) can delete it at once.** That is the exact hazard D-314 closes. The error text is accurate, but nothing stops the delete, and this path has no test. Unlikely in practice (two failures in a row). A simpler shape removes the rollback: set the mtime on `from` before the rename (rename keeps mtime), so a failure leaves nothing moved. If the double-failure branch stays, test it through the seam with a blocked `from`.
3. **lifecycle.rs:384-395: the D-312 name refusal is not logged.** check_repair's rename refusal one line above (lifecycle.rs:381) writes the reason to `tools.health.why` through `log_invariant`, which is what `harness tools health` shows and what T-9 means by "rejected and logged". The D-312 refusal returns without logging, and tests_review.rs:954 pins the row as unchanged. Either call `log_invariant` before returning (and assert the why in the test), or record in D-312 that this refusal is deliberately unlogged.

## Nits

- fingerprint.rs:93: `alias_for(&row) == item` is redundant, because `aria_roles(alias_for(r))` always contains `r` under the roles.txt first-listing rule. Harmless.
- invariants.rs:215: the kinds are equal after :200, so `o.kind == Planner || n.kind == Planner` can be just `o.kind == ToolKind::Planner`.
- archive.rs:239: add a `.<n>`-suffixed case to the rollback test (an existing register.1.js, then a failing set_mtime, should leave register.1.js and register.js and no register.1.2.js).
- lifecycle.rs:426: a run of spaces sits inside the double-failure message (lost line continuation). This predates the commit, and rust-coder-6 flagged it.
- Some roles.txt aliases are broader than the old map (`tree` covers treeitem, `status` covers alert, `cell` covers columnheader). This matches find (D-49), and a looser match only lets a tool run whose output is still classified and gated.
- A legacy upper-case tool file would be skipped by the loader (reported in LoadReport.skipped) and never archived by sweep. None exists, so nothing is needed now.
- B-31 and B-300 are still open in blockers.yaml. The sub-orchestrator should close them with this commit.

## Verdict

Whole branch: no Blocking items from this round or earlier ones. Every D-78 item is implemented, each with a test that can fail. No hard rule is broken, no pub API changed except the two `#[deprecated]` markers, and fmt, clippy and the toolstore tests are clean. Fix Should fix 1 before reflect runs sweep on a real Windows tools tree.

Verdict: approve
