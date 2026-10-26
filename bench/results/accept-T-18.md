# accept T-18, round 1 (tester)

Task T-18 (area driver): sb-cdp pipe/ws transports and CDP client, sb-driver launch and registry.
Specs: B-2, B-3, B-4 #2, B-4 #11; decisions D-600, D-601, D-5. docs/05-BROWSER.md has no
numbered acceptance item for T-18, so the rows below test the behaviours T-18 delivers.

Tree: `swarm/driver-T18`. Implementation at 17c40e1; HEAD e9696f3 at the time of the run
(the reviewer's commit only touches `agent-memory/handoffs/` and `bench/results/`). Browser:
`C:\Users\Admin\AppData\Local\ms-playwright\chromium-1248\chrome-win64\chrome.exe`, file
version 156.0.8078.4. Every real-browser test ran (no `SKIP:` line in `--nocapture` output).

Tests added by the tester (no implementation change):
- `crates/sb-cdp/tests/accept_t18.rs`: 5 tests over an in-memory `Transport` written in the test,
  through the public API only.
- `crates/sb-driver/tests/accept_t18.rs`: 1 real-Chromium test (panics instead of skipping if no browser).

Main command (clean tree, last run):
`cargo test -j 2 -p sb-cdp -p sb-driver -- --nocapture` exited 0. sb-cdp: lib 16/16, accept_t18 5/5,
chromium_pipe 1/1, chromium_websocket 1/1, contract_paths 1/1. sb-driver: lib 26/26, accept_t18 1/1,
chromium_driver 1/1, contract_paths 1/1. 0 ignored.
`cargo fmt --all -- --check` exit 0. `cargo clippy -j 2 -p sb-cdp -p sb-driver --tests -- -D warnings` clean.

| row | what | result | evidence |
|---|---|---|---|
| 1 | Pipe launch of the D-34 build works; a session command returns | pass | `chromium_pipe::pipe_round_trip_then_kill_leaves_no_process` ok (Browser.getVersion + setDiscoverTargets over `--remote-debugging-pipe`). `sb-driver accept_t18::accept_d34_pipe_sessions_detach_and_cleanup` ok: `Browser.getVersion` product ends with `CHROMIUM_VERSION` = `156.0.8078.4`; `Runtime.evaluate` on a flattened page session returns 42. `chromium_driver::bench_headless_launch_registry_and_cleanup` ok (session `Runtime.evaluate` 1+1 = 2). |
| 2 | Pipelining: several commands in flight before any reply; out-of-order replies reach the right callers | pass | `sb-cdp accept_t18::accept_pipelined_commands_and_out_of_order_replies` ok: 20 `start`s, all 20 read off the wire and `pending() == 20` before any reply, replies in shuffled reverse order across browser and `S-odd` sessions, each caller gets its own echo. Unit `sends_pipeline_and_out_of_order_replies_reach_their_callers` ok. Real pipe: two commands started before either is awaited (chromium_pipe). Mutations M2a, M2b red (row 10). |
| 3 | Flattened sessions: events routed per session; a detached session's channel closes | pass | `sb-cdp accept_t18::accept_session_routing_and_detach_closes_only_that_channel` ok (A gets n=1,3, B gets n=2; `Target.detachedFromTarget{sessionId:A}` closes A's channel within 5 s; B still receives). Real Chromium (`sb-driver accept_t18`): `Page.navigatedWithinDocument` from tab A arrives on A's channel tagged with A's session, nothing from A on B's channel; `Target.closeTarget` on A closes A's channel, removes the tab from the registry, B still answers. Unit `events_route_to_their_session_and_to_all` ok. Mutations M3a, M3b red. |
| 4 | 10 s command timeout names the method; transport close fails pending callers; a cancelled caller's slot is removed | pass | `sb-cdp accept_t18`: `accept_timeout_is_10_s_and_names_the_method` ok (paused clock, elapsed exactly 10 s, `CdpError::Timeout{method:"DOM.getDocument"}`, Display contains the method, slot removed); `accept_transport_close_fails_pending_callers` ok (both callers get `Closed{method}` within 2 s, `pending()==0`); `accept_cancelled_caller_slot_is_removed` ok (drop one of two: `pending` 2 -> 1, late reply ignored, the other caller still answered). Unit tests `command_times_out_after_10_s_naming_the_method`, `transport_close_fails_every_pending_caller`, `cancelled_caller_entry_is_removed`, `cancelling_a_started_future_mid_wait_removes_the_entry` ok. Mutations M4a1, M4a2, M4b, M4c red. |
| 5 | Launch flags exactly D-601 (no stealth/UA/fingerprint, D-5); `me` + headless refused; chromium resolution order with a set-but-missing source erroring | pass | sb-driver lib: `bench_headless_argv_is_exactly_the_d601_flags` (exact argv `--remote-debugging-pipe`, `--user-data-dir=..`, `--no-first-run`, `--no-default-browser-check`, `--headless=new`, `about:blank`), `headed_argv_has_no_headless_flag`, `argv_has_no_stealth_ua_or_automation_hiding_flags`, `me_headless_is_rejected`, `tests::me_headless_is_rejected_before_anything_starts` (no profiles/ dir created), `explicit_path_wins_over_env_and_pinned`, `missing_explicit_path_errors_instead_of_falling_through`, `env_is_used_when_no_explicit_path`, `missing_env_path_errors_naming_sb_chromium` (pinned exists, still errors), `pinned_path_under_sb_home_then_cwd`, `tests::missing_chromium_names_sb_chromium` all ok. Extra mutations M5a, M5b red. |
| 6 | Registry: popup opener, same-origin iframe parent, cross-origin iframe opaque, navigation bumps generation | pass | sb-driver lib `registry::tests::*` 15 ok (`popup_records_its_opener_tab`, `child_frame_has_its_parent`, `cross_origin_child_is_opaque_and_about_blank_inherits`, `navigation_bumps_generation_so_old_ids_are_stale`, ...). Real Chromium `chromium_driver`: srcdoc child has parent 0 and is not opaque, `http://localhost:<port>/child` under `127.0.0.1` is opaque with its own OOPIF session, main-frame generation changes on `Page.navigate` and `is_current(gen0)` is false, `window.open` popup has `opener == tab`. Mutations M6a..M6d red. |
| 7 | Drop leaves no Chromium process and no bench profile dir | pass | `sb-driver accept_t18`: after `drop(driver)` every pid from `driver.pids()` (13 processes) is not running, the `profiles/bench-*` dir is gone and `profiles/` is empty. `chromium_driver` same check ok. `chromium_pipe` `kill` leaves no pid. Unit `bench_profile_is_a_fresh_dir_removed_on_drop_and_me_persists` ok. No `chrome.exe` with `bench-` in its command line remained after any run. Mutations M7a, M7b red. |
| 8 | WebSocket attach works | pass | `chromium_websocket::websocket_attaches_to_a_running_chrome` ok: `--remote-debugging-port=0`, `DevToolsActivePort` read, `ws://127.0.0.1:` connect, `Browser.getVersion`, `Target.getTargets`, flattened `attachToTarget` + `Runtime.evaluate` 6*7 = 42, `Browser.close` closes the socket. |
| 9 | No fixed sleeps in crates/sb-cdp/src and crates/sb-driver/src | pass | `grep -rn "sleep" crates/sb-cdp/src crates/sb-driver/src` -> one hit, `crates/sb-driver/src/lib.rs:2` (doc comment "No fixed sleeps"). No `thread::sleep`, `time::sleep`, `yield_now`, `park_timeout`. Only timed wait: `ws.rs:114` `tokio::time::interval(20 ms)` re-reading `DevToolsActivePort` under a caller cap, with a comment that Chrome emits no event for it (condition poll, not a fixed sleep). Pipe kill waits on process handles (`WaitForSingleObject`) to a deadline. |
| 10 | Tests can fail: one or more mutations per row 2, 3, 4, 6, 7, each red, file restored, `git status` clean of implementation changes | pass | See the mutation table. All 15 mutations red. After each, `git checkout -- <file>` ran and `git status --short` showed only the two untracked `tests/accept_t18.rs` files. |

## Mutations (row 10)

Each: edit one implementation line, run the command, `git checkout -- <file>`, `git status --short`.

- M2a (row 2, `crates/sb-cdp/src/client.rs`): reader routes each reply to the lowest pending id instead of its own id. Command: `cargo test -j 2 --no-fail-fast -p sb-cdp --lib --test accept_t18`. Result: red: `sends_pipeline_and_out_of_order_replies_reach_their_callers`, `cancelled_caller_entry_is_removed`, `accept_pipelined_commands_and_out_of_order_replies` (accept_t18.rs:106), `accept_cancelled_caller_slot_is_removed`.
- M2b (row 2, `client.rs`): `queue()` moved inside the returned future (nothing on the wire until awaited). Command: same. Result: red: 6 lib tests and 4 accept tests panic "client wrote a message within 5 s".
- M3a (row 3, `client.rs`): per-session broadcast `tx.send(event)` removed. Command: `cargo test -j 2 --no-fail-fast -p sb-cdp --test accept_t18`. Result: red: `accept_session_routing_and_detach_closes_only_that_channel` "event within 5 s" (accept_t18.rs:115). Note: first attempt also ran `--lib`; unit `events_route_to_their_session_and_to_all` hung on an uncapped `recv()` and was killed (red, but by hang).
- M3b (row 3, `client.rs`): detached session's channel not removed (`sessions.remove(&gone)` dropped). Command: same. Result: red: `accept_session_routing_and_detach_closes_only_that_channel` "A's channel resolved within 5 s: Elapsed".
- M4a1 (row 4, `client.rs`): Timeout error carries `"?"` instead of the method. Command: `cargo test -j 2 --no-fail-fast -p sb-cdp --lib --test accept_t18`. Result: red: `command_times_out_after_10_s_naming_the_method`, `accept_timeout_is_10_s_and_names_the_method`.
- M4a2 (row 4, `client.rs`): deadline `COMMAND_TIMEOUT * 2`. Command: same. Result: red: same two tests (elapsed != 10 s).
- M4b (row 4, `client.rs`): `Shared::close` no longer takes the reply map. Command: same. Result: red: `transport_close_fails_every_pending_caller` (after 10 s, Timeout != Closed), `accept_transport_close_fails_pending_callers` "pending callers failed well before the 10 s timeout: Elapsed".
- M4c (row 4, `client.rs`): `SlotGuard::drop` does not remove the slot. Command: same. Result: red: `cancelled_caller_entry_is_removed`, `cancelling_a_started_future_mid_wait_removes_the_entry`, `command_times_out_after_10_s_naming_the_method`, `accept_cancelled_caller_slot_is_removed`, `accept_timeout_is_10_s_and_names_the_method` ("timed-out slot removed").
- M6a (row 6, `crates/sb-driver/src/registry.rs`): navigation does not bump generation. Command: `cargo test -j 2 --no-fail-fast -p sb-driver --lib`. Result: red: `navigation_bumps_generation_so_old_ids_are_stale`.
- M6b (row 6, `registry.rs`): opener always `None`. Command: same. Result: red: `popup_records_its_opener_tab`, `target_detach_removes_the_tab_and_its_sessions`.
- M6c (row 6, `registry.rs`): `attach_frame` parents every child to frame 0. Command: same. Result: red: `child_frame_has_its_parent`, `cross_origin_child_is_opaque_and_about_blank_inherits`, `detach_removes_the_frame_and_its_subtree`.
- M6d (row 6, `registry.rs`): cross-origin no longer makes a frame opaque. Command: same. Result: red: `cross_origin_child_is_opaque_and_about_blank_inherits`.
- M7a (row 7, `crates/sb-driver/src/lib.rs`): `Driver::drop` does not kill the browser. Command: `cargo test -j 2 --no-fail-fast -p sb-driver --test accept_t18 --test chromium_driver`. Result: red: accept_t18.rs:141 "still running after drop: [13 pids]"; chromium_driver.rs:229 "pid 26788 still running after drop". No chrome left afterwards (Job kill-on-close at process exit).
- M7b (row 7, `crates/sb-driver/src/launch.rs`): bench profile kept (`TempDir::keep`). Command: `cargo test -j 2 --no-fail-fast -p sb-driver --lib --test accept_t18`. Result: red: `bench_profile_is_a_fresh_dir_removed_on_drop_and_me_persists`, accept_t18.rs:142 "bench profile ... left behind".
- M5a (row 5, `launch.rs`): `--disable-blink-features=AutomationControlled` added. Command: `cargo test -j 2 --no-fail-fast -p sb-driver --lib`. Result: red: `bench_headless_argv_is_exactly_the_d601_flags`, `argv_has_no_stealth_ua_or_automation_hiding_flags`, `headed_argv_has_no_headless_flag`.
- M5b (row 5, `launch.rs`): a set-but-missing `SB_CHROMIUM` falls through to the pinned path. Command: same. Result: red: `missing_env_path_errors_naming_sb_chromium`.

## Notes (non-blocking)

- Unit test `client::tests::events_route_to_their_session_and_to_all` awaits `s1.recv()` with no cap, so a
  routing regression hangs the test binary instead of failing it (seen under M3a). The tester's
  `accept_t18` routing test is capped at 5 s and fails cleanly.
- A `cargo test -j 1 --workspace --no-fail-fast` (not started by the tester) was running in this tree during
  the mutation window; any result it recorded between the tester's edits and restores may have
  compiled a mutated `client.rs`, `registry.rs`, `lib.rs` or `launch.rs`.
- No vault rows: T-18 has no vault path (no `@vault`, no insertText), so the canary check does not apply.

Verdict: green
