# Review T-18 (area driver), round 1

Scope: `git diff main...HEAD -- crates/sb-cdp crates/sb-driver Cargo.toml` at 17c40e1 (net diff; client.rs at HEAD is the same as afbff77, so the 753dfb5 read_loop mutation is gone).
Spec: 05-BROWSER B-2, B-3, B-4 #1, #2, #11 (10 s timeout). Decisions D-5, D-34, D-600, D-601, D-90. Blockers B-600..B-603.
Gates run: `cargo clippy -j 2 -p sb-cdp -p sb-driver --all-targets -- -D warnings` clean (Windows target only; the tree also held a tester's untracked `tests/accept_t18.rs` files, which compiled too). `cargo fmt --check` clean. Tests were not run (low memory, per brief).

## Blocking

- none

## Should-fix

1. **Handle-inheritance comment is wrong, and the race it claims to prevent is real.** `crates/sb-cdp/src/pipe_windows.rs:191-195`: the comment says the handle list "keeps any other concurrently spawned child from inheriting them". `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` only limits *this* CreateProcessW. Any `std::process::Command` spawn on another thread between `SetHandleInformation` (l.194) and the drop at l.236 inherits `h_in`/`h_out`, because std passes `bInheritHandles=TRUE` with no list. That child then holds the pipe ends, so EOF stops tracking the browser and the client never closes (B-4 #11 "browser exit" depends on it). The coder's handoff gotcha already says Command and Pipe must not spawn concurrently, but sb-mcp and toolstore will spawn git and other processes in the same process. Fix the comment, and either serialize spawns behind a crate-wide lock that every in-repo spawner uses, or record the limit as a decision or blocker so T-22/T-26 do not trip on it.
2. **Unix: Chromium is orphaned if a bridge thread fails to spawn.** `crates/sb-cdp/src/pipe.rs:50-55`: `?` after `sys::spawn` drops `Proc`. On Windows that closes the kill-on-close job. `pipe_unix.rs` `Proc` has no `Drop`, so the browser and its process group keep running. Add `impl Drop for Proc` (kill the group) or kill on that error path.
3. **Windows `kill` loop has no total cap of its own.** `pipe_windows.rs:331-357`: every pass re-lists pids and waits per pid against `deadline`. If a listed pid's handle is already signalled, the loop spins without ever checking `deadline`. That should not happen with current kernel behaviour, but rule 6 wants every wait capped. Add `if Instant::now() >= deadline { return false; }` at the top of the loop. Related: `pids()` returns empty when `QueryInformationJobObject` fails (for example ERROR_MORE_DATA), and `kill` then reports "all gone". That fails open; return `false` on query failure instead.
4. **Registry: a second session on a known page target takes the tab over.** `crates/sb-driver/src/registry.rs:274-280`: a later `Target.attachedToTarget` for a target already in `by_target` (an explicit `Target.attachToTarget` with flatten emits one) replaces `t.session`. When that second session detaches, `detached()` (l.318-327) removes the whole tab, even though the auto-attach session is still live. Keep the first (auto-attach) session as the tab's session and track extra sessions as aliases, or ignore them. Add a unit test: attach S1, attach S2 for the same target, detach S2, tab still present with session S1.
5. **Registry: prerender targets become phantom tabs.** `registry.rs:273`: every `type: "page"` attach creates a tab, including `targetInfo.subtype == "prerender"`, which page-level auto-attach delivers (Speculation Rules; Google search uses them). Those show up in `tabs()`, take TabIds and would count toward B-4 #10's 6-tab cap. Skip (or separately track) subtype `prerender` until activation, and add a test.
6. **Registry misses frames of targets that already existed at attach time.** `Page.enable` does not replay `frameAttached`/`frameNavigated` for frames already loaded. A target attached with `waitingForDebugger: false` (the initial tab, tabs a `me` profile restores on startup) therefore has only frame 0 from `targetInfo`. Its child frames and their contexts are missing: `context_created` drops them at l.439 because the frame is unknown. Either seed with one `Page.getFrameTree` per non-paused attach (one round trip per attach, not per lookup, so B-4 #2's "zero round trips to look up" still holds), or file a blocker pointing at T-22 (registry complete, acceptance 4).
7. **The observer-ordering test cannot fail.** `crates/sb-cdp/src/client.rs:519-533` (`observer_sees_each_event_before_subscribers`): on the current-thread test runtime the reader finishes its loop iteration before the test task resumes. With the order swapped (broadcast first, observer second), `seen == 1` still holds. Make the observer itself check that no subscriber has the event yet, for example with `try_recv()` on a receiver it captured returning `Empty`. The handoff also says the pr-ready section 6 mutation run never completed. The tester should break each named behaviour once and record the result.
8. **Real-browser tests pass silently when no Chromium is present.** `crates/sb-cdp/tests/common/mod.rs:10-25` (duplicated in sb-driver): `chromium()` prints SKIP and the test returns green. That covers B-2 launch, the pipe, flattened sessions, OOPIF and opener registration, and drop leaving no process or profile. A machine without `C:\Users\Admin\...\chromium-1248` reports them as passing. Make the skip visible to the gate: fail when an env flag such as `SB_REQUIRE_CHROMIUM=1` is set (the acceptance run sets it), or `#[ignore = "needs Chromium"]` and run them with `--ignored`. The hard-coded per-user path should also move behind `SB_CHROMIUM` or `SB_HOME`.
9. **The ws test leaks Chrome on any failure.** `crates/sb-cdp/tests/chromium_websocket.rs:20-109`: the `std::process::Child` is killed only on the happy path. Any `unwrap`/`expect` panic before l.95, or the 30 s timeout panic, leaves a Chromium running against a temp profile. Wrap the child in a guard that kills it on drop.

## Notes

- **Rule 3 / D-5 / D-601:** `launch_args` (`crates/sb-driver/src/launch.rs:128-151`) emits exactly `--remote-debugging-pipe`, `--user-data-dir=`, `--no-first-run`, `--no-default-browser-check`, `--headless=new` (only when asked), and `about:blank`. There are no stealth, UA, automation-hiding or fingerprint flags. `me`+headless is refused twice, in `launch_with` (lib.rs) before any profile is touched and in `launch_args`. Tests assert the exact argv, the banned substrings and the refusal.
- **Rule 6:** no `sleep` anywhere. Waits use `timeout`/`timeout_at`, `recv_timeout` or `WaitForSingleObject` against a deadline. `devtools_active_port` (ws.rs:108-131) is a 20 ms poll under a cap, with a comment explaining why no event exists. It is not driver code.
- **Rule 2:** `CdpError` names the method, never params (client.rs:27-41). Nothing logs. No vault paths are touched in T-18.
- **Rule 1:** no keys, `.env` or model SDKs. New deps: futures-util, libc, tokio-tungstenite, windows-sys.
- **Reply map is correct.** `start` queues synchronously, so sends pipeline. `SlotGuard` removes the slot on cancel or timeout. `Shared::close` takes the map, so pending callers get `Closed` and later sends fail immediately. The ordering of the close flag against `session_events` is sound. No leaks found. Tests cover pipelining with out-of-order replies, the 10 s timeout naming the method, cancel before and during the wait, close failing all pending callers, and protocol errors. The session-routing test would have caught 753dfb5's dropped `tx.send`.
- **Small client gaps:** `session_events` for a session that already detached registers a channel that never closes until client close (client.rs:298-308). `From<CdpError>` maps `Closed` to `Transport(Closed)` and drops the method name (lib.rs). Both are minor.
- **Windows unsafe code reviewed:**
  - The CRT block layout (int count, u8 flags, intptr handles) matches UCRT.
  - `STARTUPINFOEXW.cb` is correct.
  - The attribute list is usize-aligned, deleted exactly once, and the `inherit` array outlives it.
  - Every error path closes the NUL handle, both pipe ends, the job, and the process and thread handles.
  - The suspended process joins the job before `ResumeThread`.
  - `pids()` buffer sizing and alignment are right for both pointer widths.
  - Inherited handles are exactly NUL (for fds 0-2) plus the two pipe ends.
  - Nit: `AttrList` stores a raw pointer to a borrowed slice without a lifetime tie (`PhantomData<&'a [HANDLE]>` would encode it).
- **Process lifetime:** Windows is covered even if the harness crashes (kill-on-close job). On Unix, a crashed harness leaves the group alive unless Chromium exits on pipe EOF. Unix `kill` waits for the leader only, then probes the group once, so it can report `false` spuriously. The Unix path is compile-checked only (handoff).
- **Drop blocks:** `Driver::drop` and `Pipe::drop` call `kill(DROP_KILL_CAP = 20 s)` synchronously, on a tokio worker when dropped in async code. That is fine for teardown, but blocking a worker for up to 20 s is worth a doc line or a `spawn_blocking` path later. Dropping a `me` driver without `shutdown()` terminates the browser without `Browser.close`, so cookies may not flush. Document on `Driver` that `me` callers must `shutdown`.
- **Stale bench profiles:** a crashed harness leaves `profiles/bench-*` behind, and nothing sweeps them at launch. D-601 is met as written ("removed when the driver drops").
- **Untrusted text:** `tabs()` titles and URLs are page-controlled and uncapped. The consumer that renders them (T-22, sb-mcp) must escape and cap them (pr-ready section 5).
- **Interfaces (D-90):** I-8 paths and the `Transport` signature are unchanged, and I-9's `Driver::launch` and every listed method signature are unchanged. `contract_paths.rs` in both crates `use`s every listed path. Additive items, all to be listed for the supreme:
  - sb_cdp: `Cdp`, `CdpError`, `CdpSender`, `Event`, `Observer`, `COMMAND_TIMEOUT`, `Pipe::{launch, pid, pids, kill}`, `DROP_KILL_CAP`, `process_running`, `WebSocket::connect`, `devtools_active_port`, `parse_devtools_active_port`.
  - sb_driver: `DriverError::Launch` (a new variant on an exhaustive enum; no in-tree consumer matches on it), `impl From<CdpError> for DriverError`, `Driver::{launch_with, cdp, with_registry, wait_registry, pid, pids, profile_dir, shutdown}`, `Registry`, `ExecContext`, `SB_WORLD`, `LaunchEnv`, `launch_args`, `resolve_chromium`, `platform_dir`, `binary_name`, `CHROMIUM_VERSION`.
- **Spec ids vs code:**
  - B-2: done (the pinned binary download is T-18b per D-601).
  - B-3: done (Pipe and WebSocket implemented; ElectronNapi and ExtensionRelay kept as `todo!()` stubs).
  - B-4 #1: done.
  - B-4 #2 skeleton: done (event-fed, opener, per-frame generation with `is_current`, zero-round-trip lookups).
  - B-4 #11: the 10 s timeout is done; crash and relaunch are not claimed.
  - Beyond spec, recorded in B-602: three extra events, `shutdown`, `wait_registry` and the ws port-file helper.
- **Commit hygiene:** the branch has three WIP commits plus 6045b0a. pr-ready section 2 wants one commit per task, so squash at merge.

## Verdict

No hard-rule break and no missing acceptance test. B-2, B-3 and B-4 #1/#2/#11 (timeout) are implemented as claimed, and the client's reply map and session routing are correct. The should-fix items are:

- a handle-inheritance race whose comment claims protection it does not give;
- a Unix orphan path;
- a Windows kill loop without its own cap check;
- three registry state-machine gaps (second session, prerender, pre-existing frames);
- one test that cannot fail;
- real-browser tests that skip silently.

Fix them in round 2 or carry them as T-18 follow-ups before T-22.

Verdict: approve
