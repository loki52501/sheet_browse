# Acceptance: T-18d (B-199 fix, D-630; spec docs/05-BROWSER.md B-2), round 1

Verdict: green

Worktree F:/sb-swarm/wt/driver-T18d, branch swarm/driver-T18d, HEAD 53a2d8b (fix commit 3d5ae24). Host: Windows 11, loaded, TEMP=F:\sb-swarm\tmp-driver, SB_CHROMIUM_TESTS unset (real Chromium ran; no SKIP line in any log).

Saved by driver/suborch from the tester's report text: the tester's session could not write into the per-task worktree (guard_write.py roots writes at wt/driver; B-619). Text unchanged. The tester's proposed Chromium-free test (tests/accept_t18d.rs: launch_with on a non-executable chromium file must keep a `me` profile's contents and leave no bench-* dir) was never saved or run and is not counted; it is filed in T-18e.

| item | command | result | note |
|---|---|---|---|
| 1. bench_headless_launch_registry_and_cleanup passes repeatedly (10 runs) | `cargo test -j 2 -p sb-driver --test chromium_driver -- --test-threads=1` (x10, loop, after one `--no-run` build of 31.7 s) | pass | 10/10 passed, 0 failed, 0 ignored; each log has `test bench_headless_launch_registry_and_cleanup ... ok`. Wall / test time per run: 1: 15.8 s/10.82 s, 2: 10.5/7.50, 3: 36.7/33.21, 4: 11.7/8.06, 5: 8.8/6.21, 6: 5.4/2.55, 7: 7.6/3.90, 8: 8.5/5.82, 9: 11.9/8.14, 10: 24.5/11.35. Run 3 (33 s) is the slowest and still inside the caps. |
| 2. No bench-* folder left under the profiles root | `ls -ld --time-style=full-iso F:/sb-swarm/tmp-driver/.tmp*/profiles/bench-*` before and after the 10 runs, then `diff before.txt after.txt` | pass | The test's profiles root is `<tempfile::tempdir() in %TEMP%>/profiles`. Before and after identical (`diff` empty). The 5 old leftovers are all dated 16:29 to 17:07 on 2026-10-26, before the fix commit at 17:17 (.tmp30CYPs/bench-qMSeFx, .tmpDZPlhn/bench-7jW8iK, .tmpO7aH8G/bench-3C8wVt, .tmpgW32mB/bench-LNz8Ie, .tmpywIWfH/bench-yDW5Vg); no new .tmp* or bench-* dir appeared. The in-test assert `!profile.exists()` runs before `home` is dropped and held in 10/10 runs. Old leftovers are the T-18c sweep's job. |
| 3. A `me` profile folder is never removed by a drop | `cargo test -j 2 -p sb-driver --lib -- --test-threads=1` (`launch::tests::bench_profile_is_a_fresh_dir_removed_on_drop_and_me_persists`) | pass | Existing unit test, no Chromium: prepares `ProfileDir::prepare(Profile::Me)`, drops it, asserts `me_path.is_dir()`. `ProfileDir::drop` only acts on `Bench`. Gap: the me dir is empty in that test and there is no Driver-level me test (T-18e). |
| 4. Retry helper unit tests pass | `cargo test -j 2 -p sb-driver --lib -- --test-threads=1` | pass | `test result: ok. 40 passed; 0 failed; 0 ignored`, including removal_retries_a_held_file_on_the_d630_schedule_until_released, removal_gives_up_at_the_d630_cap_with_the_last_error, removal_not_found_is_success_and_other_errors_stop_at_once, removal_with_no_or_zero_schedule_does_not_spin, removal_retries_each_windows_held_file_code (ran on Windows). |
| 5a. clippy clean | `cargo clippy -j 2 -p sb-driver --all-targets -- -D warnings` | pass | rc=0, 0 lines matching warning or error. |
| 5b. fmt clean | `cargo fmt --all -- --check` | pass | rc=0, no output. |
| 6. Rule 6: every sleep has a no-condition reason | `grep -rn -i "sleep" crates/sb-driver` | pass | 5 hits. The only real wait is launch.rs:269 `sleep(wait);`, commented at launch.rs:267-268 ("Windows gives no event when another process releases a file lock, so there is no condition to wait on (rule 6): back off on a schedule, capped in total."). launch.rs:206 passes `std::thread::sleep` into that function; launch.rs:245 is the parameter, launch.rs:425 a test doc comment, lib.rs:2 the crate doc. No `tokio::time::sleep` in sb-driver. |

Verdict: green
