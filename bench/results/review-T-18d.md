# Review T-18d round 1 (area driver)

Scope: `git diff main...swarm/driver-T18d -- crates/ agent-memory/decisions.yaml`, code commit 3d5ae24.
Spec ids claimed: T-18d, B-2, B-199, D-630.
Checked here: `cargo clippy -j 2 -p sb-driver --all-targets -- -D warnings` clean; `cargo test -j 2 -p sb-driver --lib -- removal bench_profile` 6/6 pass.
Not run here: the real-Chromium tests (coder reports 10/10 in a loop; accept-T-18d.md row 1 reran them 10/10).

Saved by driver/suborch from the reviewer's report text: the reviewer's session could not write into the per-task worktree (guard_write.py roots writes at wt/driver; B-619). Text unchanged.

## Blocking

- none

## Should fix

- **Async callers block a runtime worker on drop** (crates/sb-driver/src/lib.rs:286 and :390, launch.rs:195-208). A `Driver` drop now blocks the dropping thread for up to 20 s in `kill` plus up to 19.9 s of removal waits (each removal attempt's own `remove_dir_all` time is not counted against the cap). If `kill` returned false and the `Pipe` Arc is the last one when the `pipe` field drops, `Pipe::drop` (sb-cdp pipe.rs:87) runs `kill(DROP_KILL_CAP)` again, up to 20 s more, about 60 s worst case. Today no crate outside sb-driver holds a `Driver`; its tests drop it on a `#[tokio::test]` current-thread runtime, which is acceptable for tests. Before sb-mcp (T-23+) holds one, `Driver::shutdown` should end with `tokio::task::spawn_blocking(move || drop(self))`, or the doc should say "drop off the runtime". The lib.rs:270 doc ("up to 20 s more") understates the worst case; mention the second kill and the attempt time.
- **No Drop-level test for the retry wiring** (launch.rs:544). The five `remove_dir_retrying` unit tests go red if the retry loop is removed (exact attempt counts, waits, the 19.9 s cap schedule, last error returned). But if `Drop for ProfileDir` called a single `std::fs::remove_dir_all`, every unit test still passes; only the flaky real-Chromium test would notice. Suggested: a `#[cfg(windows)]` test that opens a file in a Bench dir with `OpenOptionsExt::share_mode(0)` (std's `File::open` shares delete by default), drops the `ProfileDir` on another thread, releases the handle after the first failed attempt, and asserts the dir is gone.
- **Retry set on Unix** (launch.rs:230-234). `PermissionDenied` (EACCES) and `DirectoryNotEmpty` are retried on every OS; on Unix EACCES on a profile dir does not clear by waiting, so it silently costs the full 20 s. The measured cause is Windows only (OS error 32). Limit the kind-based arm to `cfg!(windows)`, or at least to `DirectoryNotEmpty`.
- **A final removal failure is invisible** (launch.rs:201). With `let _ =`, a permanent ACL denial or a hold past the cap leaves the dir behind with no trace outside the test's `leftover()` message. Report the path and `io::ErrorKind` on the final failure through whatever diagnostics channel sb-driver adopts (it has no tracing dependency today; a bench path and an error kind carry no vault data), or record that the T-18c sweep is the only signal.

## Notes

- Hard rules: rule 6 met. The only wait is the injected `sleep(wait)` at launch.rs:269, commented (Windows gives no event for a lock release) and capped in total at launch.rs:264; the real `std::thread::sleep` is passed in at launch.rs:206 under the Drop comment. Rules 1-5 and 7 untouched (no keys, vault, gate, HTML or user-facing text in this diff).
- Termination: the loop always ends; `waited` grows by a non-zero step whenever it repeats the last entry; an empty schedule or a zero last entry ends the loop (test at :509). Cap boundary exact: 38 steps of 500 ms reach 19.9 s, the next would exceed 20 s (test at :465). `waited + wait` (launch.rs:264) would panic on `Duration` overflow only with a cap near `Duration::MAX`, impossible with the `pub(crate)` const caller; `checked_add` would make it total.
- Me profile never removed: `Drop` matches only `ProfileDir::Bench` (launch.rs:197); the Bench path comes only from `tempdir_in(profiles, "bench-")` + `keep()` (launch.rs:181).
- kill() returned false: removal still runs afterwards by field order (lib.rs:278-286). Harmless for a bench profile; anything the browser recreates is retried, then left for the sweep. The field comment "removed only after the browser is gone" is slightly too strong in that case; the new Drop comment covers it.
- Masking real errors: NotFound is success; any non-retryable error stops at once (tests at :482, :520 with 87). Raw 5 and 145 are already covered by the ErrorKind arm on Windows; the raw list matters for 32 and 33, which std leaves uncategorized.
- Spec ids: B-2 and B-199 implemented as claimed. D-630 is new, unique on main, and matches the code (schedule, cap, retry set, NotFound, removal after a false kill, no panic). Nothing beyond D-630. B-199 is still open in blockers.yaml; the sub-orchestrator resolves it.
- Interfaces: no public signature changed; `ProfileDir`, `remove_dir_retrying` and the consts are `pub(crate)`; interfaces.yaml untouched.
- Residual risk: on a stalled host where `kill` hits its cap, a hold past 20 s is still possible (coder saw one at a 30 s cap). That is sb-cdp teardown or host time, not removal logic.

## Verdict

No rule break and no missing acceptance test. The retry is bounded and commented, the Me profile is safe, and the removal unit tests fail without the retry. Should-fix: a `spawn_blocking` drop path before async callers exist; a Windows held-file test through `Drop`; narrowing the Unix retry set; a trace for a final failure.

Verdict: approve
