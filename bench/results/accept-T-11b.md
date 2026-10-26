# Acceptance T-11b (B-33 items 1-3, D-315, D-316; docs/03 T-3, T-7, D-303)

Tester round 1, branch swarm/store-T11b, worktree F:\sb-swarm\wt\store, base 5089c47.
New test file: `crates/toolstore/tests/accept_t11b.rs` (9 tests, public API only, from
outside the crate, own fake `MemoryStore` whose `tools.health` read fails per folder). The
archive failure there is portable: `tools/_archive` is made a regular file, so the archive
step's `create_dir_all` fails after the stamp and the tool file stays in place. The
Windows share-lock variant is already in `accept_t11.rs`
(`d314_failed_retire_does_not_stop_sweep_and_next_sweep_archives`) and passes.

| # | item | test | result |
|---|------|------|--------|
| 1 | scan_live/load_live skip a tool whose own row is retired with its file still in tools/<domain>/, reported in skipped as `retired (v<N>)`; ephemeral, saved, repaired, needs_repair, ask and no row still load | accept_t11b `r1_retired_row_with_file_in_place_is_skipped_and_reported`, `r1_non_retired_statuses_and_no_row_still_load` | pass |
| 2 | a retired site tool does not shadow the _generic tool of the same name; a retired _generic row is skipped | accept_t11b `r2_retired_site_tool_does_not_shadow_the_generic_one`, `r2_retired_generic_row_is_skipped` | pass |
| 3 | a memory error from tool_health makes scan_live/load_live return Err, never a list | accept_t11b `r3_memory_error_is_err_never_a_list` | pass |
| 4 | retire with failed archive (record at 5 misses; sweep of idle ephemeral) leaves row retired, file in place, load_live omits it, sync load still lists it | accept_t11b `r4_r5_record_retire_with_failed_archive`, `r4_r5_sweep_retire_with_failed_archive` | pass |
| 5 | D-316 ledger: ok:true tools.retire (archived null, archive_failed set) then ok:false tools.archive naming file and error; record Ok; sweep Retired{archived: None}; next sweep ok:true tools.archive; success path one ok:true tools.retire with the archived path | accept_t11b `r4_r5_record_retire_with_failed_archive`, `r4_r5_sweep_retire_with_failed_archive`, `r5_successful_archive_is_one_ok_retire_with_the_path` | pass |
| 6 | contract_paths.rs names tool_path, archive_path, ApiFetch::api, root fingerprint, scan_live, load_live from outside the crate | contract_paths (4 tests) + accept_t11b `r6_live_loader_signatures` | pass |
| 7 | gates: fmt check, clippy -p toolstore all targets -D warnings, cargo test -p toolstore, cargo test --workspace -j 2 | see commands below | pass |
| 8 | `python scripts/swarm/merge_gate.py store --worktree .` output recorded | merge_gate.py | pass |

## Commands and output

- `cargo test -p toolstore --test accept_t11b -j 2`: `test result: ok. 9 passed; 0 failed; 0 ignored`.
- `cargo test -p toolstore --test contract_paths -j 2`: `test result: ok. 4 passed; 0 failed; 0 ignored`
  (toolstore_methods_exist, deprecated_path_builders_stay_public, api_fetch_signature,
  fingerprint_signature).
- `cargo fmt --all -- --check`: exit 0, no output.
- `cargo clippy -p toolstore --all-targets -j 2 -- -D warnings`: `Finished`, exit 0.
- `cargo test -p toolstore -j 2`: lib 122 passed, accept_t11 41 passed, accept_t11b 9 passed,
  contract_paths 4 passed, doctests 0; 0 failed, 0 ignored.
- `cargo test --workspace -j 2 --no-fail-fast` (background, no timeout wrapper): exit 0,
  72 test binaries, **722 passed, 0 failed, 5 ignored**. harness `step1_accept`
  `7 passed; 0 failed` in 95 s (the coder's earlier exit 143 was their own timeout). The 5
  ignored are outside toolstore: 3 red-first T-08 cli tests (`d124_1_*`, ignored with
  reason) and 2 child-process helpers (`d119_7_diag_child`, `file_asker_stderr_child`). The
  `panicked at crates\core\src\vault.rs:2042` line belongs to
  `global_scrub_before_and_after_install`, which reports `ok` (it tests the panic hook).
- `python scripts/swarm/merge_gate.py store --worktree .`: `gate ok for store: T-11`, exit 0.
  The gate judges swarm.yaml's area task T-11, not T-11b; swarm.yaml not edited.

## Mutation proof (each applied to src temporarily, test run, file restored with `git checkout --`; `git status` showed src clean after each)

- M1 retired check in scan_dir disabled (`retired(..).filter(|_| false)`) (loader.rs): red r1_retired, r2_shadow, r2_generic, r4 record, r4 sweep (5)
- M2 every status but saved counts as retired (loader.rs): red r1_non_retired_statuses
- M3 `_generic` filtered by the site folder's rows (loader.rs): red r2_shadow, r2_generic
- M3b retired site row also hides `_generic` (old shadowing) (loader.rs): red r2_shadow, r2_generic
- M4 `_generic` tool_health error swallowed (`unwrap_or_default`) (loader.rs): red r3
- M4b site tool_health error swallowed (loader.rs): red r3
- M5 failed archive returns Err from retire (pre-D-316) (lifecycle.rs): red r4_r5 record, r4_r5 sweep
- M6 ok:true tools.retire dropped on failed archive (lifecycle.rs): red r4_r5 record, r4_r5 sweep
- M7 ledger_failed written ok:true (lifecycle.rs): red r4_r5 record, r4_r5 sweep
- M8 success-path consequences gain `archive_failed` (lifecycle.rs): red r5_successful
- M9 `tool_path` made pub(crate) (archive.rs): red contract_paths: E0624 method `tool_path` is private
- M10 `archive_path` made pub(crate) (archive.rs): red contract_paths: E0624 method `archive_path` is private
- M11 `load_live` made pub(crate) (loader.rs): red contract_paths: E0624 method `load_live` is private

ApiFetch::api and the root `fingerprint` were not mutated: changing either signature also
breaks the crate's own callers, so there is no isolated mutation. They are pinned by the
`impl ApiFetch` plus `fn(&dyn ApiFetch, &str, Duration) -> Result<Value, String>` coercion
and by `let _: fn(&Tool, &[IndexRow]) -> FingerprintResult = toolstore::fingerprint`, which I
checked by reading.

## Notes (not failures)

- What the new tests cover beyond the coder's in-crate tests: a retired row with the same
  name in another site folder does not hide this site's tool; a site row named like a
  `_generic` tool does not hide the generic tool; a memory error on `_generic` only (site
  read fine) is still Err; a still-broken archive on the next sweep writes one more ok:false
  tools.archive and no second tools.retire; the success path's consequences are exactly
  `{archived: <path>}`; a failure from `create_dir_all` (portable) as well as from a
  share-locked rename (Windows, accept_t11).
- scan_live reports a retired file in `skipped` only when its `@match` fits the path; a
  retired file with a bad header is reported as `bad header`, not `retired`. Both are
  consistent with D-315 (the file is never loaded).
- A failing archive step appends an ok:false tools.archive on every sweep with no dedup
  (B-33 item 4, deferred to T-28 by the blocker's default; not in T-11b scope).
- No vault values are involved in T-11b paths; no canary run was needed.

Verdict: green
