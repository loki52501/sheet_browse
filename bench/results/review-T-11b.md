# Review T-11b round 1 (B-33 items 1-3, D-315, D-316, IC-305)

Branch `swarm/store-T11b`, `git diff main...HEAD -- crates/toolstore` (955326d, 9ef13ef, 6a447e4).
Line numbers are for HEAD (5cb3308 / cbba088).

## Blocking

- none

## Should-fix

1. **A fail-open on the site-folder read is not caught by any committed test.**
   `crates/toolstore/src/loader.rs:168-172` reads the site folder's rows and `:173` reads `_generic`'s.
   `load_live_fails_closed_on_a_memory_error` (`crates/toolstore/src/tests_review.rs:1415`) fails every
   `tool_health` call. For `luma.com` the `_generic` read still errors, so the test passes even when
   the site read fails open. I saw this directly. The worktree had an uncommitted mutation in it
   (`store.tool_health(&folder).await.unwrap_or_default()` at loader.rs:169, very likely the tester's
   mutation run). Under that mutation `cargo test -p toolstore` passed all 122 lib tests and all 41
   accept_t11 tests. Only the untracked `tests/accept_t11b.rs` caught it.
   - Fix: make the lib test's `fail_get` fail only for one folder (the site folder, then `_generic`).
   - Or: land the tester's accept_t11b with this case in it.

   The code is correct. Only the coder's mutation proof does not cover this half.
2. **contract_paths pins `scan_live` and `load_live` by name only.** See
   `crates/toolstore/tests/contract_paths.rs:46-47`. Changing the return type to `Vec<Tool>` or the
   store to `Arc<dyn MemoryStore>` still compiles there. Pin them with a typed call from outside the
   crate, for example `let _: Result<LoadReport, StoreError> = ts.scan_live(&mem as &dyn MemoryStore, "luma.com", "/").await;`,
   using accept_t11's fake store. This is not blocking: IC-305 is not yet in interfaces.yaml, and the
   other I-7 methods are pinned by name in this file too.

   The other pins do hold:
   - `tool_path` and `archive_path` are pinned as `fn(&ToolStore, &str, &str[, i32]) -> PathBuf` from
     outside the crate (`:67`), with `#[allow(deprecated)]` on that one fn only.
   - `ApiFetch::api` is pinned both ways, by an impl and by a `&dyn` call (`:79`).
   - The root `fingerprint` is pinned as `fn(&Tool, &[IndexRow]) -> FingerprintResult` (`:95`).
3. **`consequences["archived"].is_null()` also passes when the key is missing.** serde_json's `Index`
   returns `Null` for an absent key. See `tests_review.rs:894` and `tests/accept_t11.rs:2775`. D-316
   says the key is `archived: null`. Assert `r.consequences.get("archived") == Some(&Value::Null)`.

## Notes

- **Hard rules.** No keys or .env, no vault paths touched, no captcha code, no new side effects
  outside the store. No fixed sleeps. No em dashes. No `unwrap`/`expect` in non-test code in the diff.
  `LoadReport.skipped` gains `retired (v<N>)`: short text, no HTML.
- **Live pair, can a retired tool load?** No, on the committed code. The filter runs per file in its
  own folder (loader.rs:124), after the `@name` == file-stem check. `@name` is forced to `[a-z0-9_]+`
  (D-312) and rows are keyed by `check_tool_id` names, so the match is exact. On Windows, a file
  `Register.js` is skipped by the stem check, so this fails closed. Rows are an exact-match lookup:
  core-pg runs `where domain = $1` on `text`.
  - A retired site row no longer shadows `_generic`, and a retired `_generic` row is skipped on every
    site (both tested).
  - A bad folder gives `Ok(empty)`, which fails closed.
  - A missing row loads the tool. That is not fail-open: retire saves the row before it moves the
    file, so a retired tool always has a row.
  - The sync `load`/`scan` behave the same as before and are documented as not reading health.
  - No runtime caller exists yet. srv-web, sb-mcp and toolforge must use `load_live` (D-315).
- **Small race.** `scan_live` reads rows before files (loader.rs:168-173). A retire committed between
  the row read and the file read, whose archive also fails, is served for that one call. Reading
  files first and rows second would close this, at no extra cost.
- **Ledger agrees with the row on every retire path.** `record` and `sweep` share `retire()`
  (lifecycle.rs:274):
  - archive ok: ok:true retire with `archived: <path>`.
  - archive failed: ok:true retire with `archived: null, archive_failed`, then ok:false
    `tools.archive`, then `Ok(None)`.
  - no file: ok:true retire with `archived: null`.

  The next sweep writes ok:true `tools.archive`. The first two are tested for record and sweep, with
  the Windows share-lock and portable variants. The no-file event is not asserted anywhere (this is
  not new in this diff). `SweepAction::Retired { archived: None }` means both "no file" and "archive
  failed". Only the ledger tells them apart.
- **Sweep `?` is equivalent.** The old arms were: Memory, return; anything else, ok:false
  `tools.retire` and continue. After D-316, `retire` returns `Err` only in three cases:
  - BadPath from `check_tool_id` / `try_tool_path`;
  - Memory from `tool_health_set` / `append`;
  - the dead `Err(e @ Memory)` arm at lifecycle.rs:288 (`archive_with` is sync fs and never
    returns Memory).

  BadPath cannot happen there. `try_tool_path(&domain, &h.name)?` at the top of the loop runs the
  same `check_tool_id`, and `h.domain == domain` because the store filters by equality. So `?` only
  ever sees Memory, as before. One difference is harmless: on a failed archive, sweep now also pushes
  the action and re-saves the row.
- **Pre-existing, outside this diff, now easier to reach.** `reforge` (lifecycle.rs:441) has no guard
  for retired rows. Since D-316, "retired row with the file in place" is a normal outcome. A reforge
  in that state reads the file and sets the row to `repaired` at version+1. That skips
  `register_forged`'s ephemeral path, which is how a retired name is meant to come back (T-7: retired
  means moved to `_archive/`). The gate floor check still applies, so rule 4 holds. Suggest a
  follow-up before toolforge T-28: refuse to reforge a retired row, as `Relocated` already does
  (IC-304).
- **Interfaces.** Existing I-7 and I-12 signatures are unchanged (D-90). `record_with` is
  `pub(crate)`. `scan_live` and `load_live` are additive and proposed as IC-305. interfaces.yaml is
  untouched, which is correct for an area branch.
- **Spec claims.** B-33 (1), (2) and (3) are implemented as D-315 and D-316 describe. Two behaviours
  go beyond the B-33 text, and both are written in D-315 and D-316: a retired site tool stops
  shadowing `_generic`, and `SweepAction::Retired` is now emitted for a failed archive.
- **Not run by me.**
  - clippy: the shared worktree held another agent's uncommitted mutation in `loader.rs` and an
    untracked `tests/accept_t11b.rs`. I did not touch them.
  - `cargo test -p toolstore` on the clean committed tree: the coder reports it green, and the tester
    is running the gates.

Verdict: approve
