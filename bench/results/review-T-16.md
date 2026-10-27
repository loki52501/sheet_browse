# Review T-16 round 1 (step 4: H-5, H-1.1, H-6, docs/02, T-6..T-8, G-3)

Range: `git diff main...HEAD` on swarm/core-T16 (530cd50). Covered: crates/core/src/{watch.rs, sched.rs, mcp.rs},
crates/harness/src/{reflect.rs, proposals.rs, mem_server.rs, launcher.rs, daemon.rs, cli.rs (T-16 part), bin/harnessd.rs},
tests accept_t16_{watch,contacts,mem,reflect}.rs, contract_paths.rs, mcp.example.json, D-140..D-142, B-189/B-197/B-198.
T-08b commits on the branch are out of scope. Not built here: the tester was building in the same worktree and host RAM
was nearly gone, so clippy and test status come from the tester.

Hard-rule pass:
- Rule 1: nothing model- or key-shaped added.
- Rule 2: every memory write (review item/answer, schedule prompt, watch_target), contact key, notification file,
  notifications.md line, baseline snapshot, ledger event, proposals line, replay prompt and reflect.md is scrubbed. One
  exception: watch.txt (Should-fix 3).
- Rule 3: not touched.
- Rule 4: all 8 mem tools go through `harness_core::serve` (mem_server.rs:67-72). Reflect's sweep and commit run inside
  harnessd, not through a tool server. That is harness-internal T-7 maintenance, the same as `harness tools retire`, and
  its tools.* events are ledgered through RunStore (reflect.rs:333). Acceptable.
- Rule 5: the replay prompt holds only url and args from scrubbed ledgers. The watch hash reads text the model wrote, not
  HTML.
- Rule 6: no sleeps in non-test code.
- Rule 7: the preamble test checks there are no em dashes.

Contacts (pr-ready 5):
- touch only after an Ok, unstopped Send with no held stop, under allow or answered_proceed (mcp.rs:1217).
- timeout_proceed, skip, fail and stop touch nothing, so it fails closed.
- Gate and touch both use the scrubbed recipients and `contact_key` (trim plus ASCII lowercase, D-62). Look-alikes and
  homoglyphs stay separate keys, so they ask.
- A touch error goes to stderr only. That also fails closed: the next Send asks.

Panics on external input:
- none in non-test code. proposals.rs:254 slices a suffix of the same string, and watch.rs:124 cuts at a char_indices
  boundary.

## Blocking
- none

## Should-fix
1. **Scheduled sessions cannot call any harness tool as wired.** At bin/harnessd.rs:36 and launcher.rs:139-141, the
   inner `ClaudeLauncher::print` passes no `--allowedTools`. Nothing in the repo grants `mcp__harness-*`:
   .claude/settings.json allows only Bash patterns. `claude -p` denies tools that need permission and have no allow
   rule, so a harnessd watch or cron session cannot call `watch.observe`, `fetch` or `fs.write`.
   bench/fixtures/ladder_session.py had to pass `--allowedTools mcp__harness-web mcp__harness-files mcp__harness-cli`
   for the same reason.
   - Fix: give the inner launcher the harness servers, including `mcp__harness-mem`. The gate is the permission layer
     (H-1).
   - This came from T-08, but rungs 7 and 8 through harnessd depend on it. If the tester's live rung 7 or 8 fails on
     permission denials, treat this as blocking.
2. **The watch preamble points at an ungated path.** run_preamble (sched.rs, the watch branch of `run_preamble`) says
   "write the current watched state as plain text to {files}/watch.txt". It does not name `watch.observe`. The model
   will reach for Claude Code's built-in Write: outside the gate, and refused in `-p`.
   - Fix: say "call harness-mem `watch.observe` with the state". Only schedule.add's description mentions it now, and a
     fired session never sees that.
3. **watch.txt is written unscrubbed (mem_server.rs:529).**
   - D-141 (4)'s reason is wrong. watch.rs:178 scrubs before hashing (`scrub(&normalise(&raw))`), so scrubbing at write
     changes no hash, baseline or notification. Unscrubbed storage buys nothing.
   - Meanwhile the file sits in the deliverables folder, the ToolOutput.files path puts it in report.md's Files line,
     and other tools can read it back.
   - The value can only arrive if the model typed it: args are unresolved, and `@vault(k)` stays literal. srv-files
     `fs.write` also writes content as given (fs.rs:512). So this is defence in depth, not a rule-2 break.
   - Fix: `self.vault.scrub(state)` before `write_atomic`, and correct the D-141 (4) text.
4. **The replay session is not actually limited to readers (G-3).**
   - `--allowedTools mcp__harness-web` adds an allow rule; it does not remove anything.
   - `command()` sets no `current_dir`, so the session inherits harnessd's cwd. From the repo root, that brings in
     .claude/settings.json's allow list: `Bash(python *)`, `Bash(node *)`, `Bash(cargo *)`, `Bash(git commit *)`,
     `Bash(psql *)`, `Bash(docker compose *)`. User-level allows and the other configured MCP servers also apply.
   - The prompt embeds url and args taken from earlier runs' ledgers, which are page-derived, so injected text reaches a
     session with those permissions.
   - Fix: `--strict-mcp-config --mcp-config` with harness-web only, cwd set to the run folder, and the built-ins
     restricted (`--tools ""` or deny Bash/Write/Edit). The doc claims at launcher.rs:21-23 and reflect.rs:21-26
     overstate what is enforced.
5. **`schedule.add` is Submit, so it is allowed by default (mem_server.rs:167).** A model-created schedule makes
   unattended runs persist, and one injected page can plant an hourly job with no card. Asking costs one card when the
   user did ask (efficiency is barely affected). File a blocker with a default: Account class, or a
   `('*','Submit:schedule','ask')` rule.
6. **Spec items neither implemented nor recorded as deferred:**
   - H-5/G-3 reflect "compare output shape"
   - T-6 growth and G-3 forge proposals
   - G-3 expectation learning (T-5)
   - G-3 "after each task"
   - H-5 "nightly": D-142 (1) creates no reflect schedule, so reflect never runs unless the user adds one.

   Write the deferrals into blockers/decisions with defaults. Consider having harnessd make sure one nightly reflect
   row exists.
7. **Reflect auto-commits unrelated work (reflect.rs:554-605 with archive.rs:144 `git add -A -- <dirs>`).** Every
   changed folder is committed whole, including untracked files (`-o`). That catches a user's in-progress edit or a
   forged tool that has not been reviewed yet, in the same site folder, and commits it to whatever branch the checkout
   is on. Fix: commit only the paths the sweep moved (plus srv-web's relocations, if wanted, by explicit list), or skip
   folders that have changes the sweep did not make.
8. **Stale contract doc (mcp.rs:356-357).** `ToolCtx::store` still says handlers call `touch_contact` after a send.
   With B-198 a handler that does this double-counts. Fix the doc line.

## Nits
- watch_target accepts interior newlines (mem_server.rs:442 `stored_text`; `target_key` only trims). The `Watch:`
  report line (watch.rs:95) and the notifications.md line (watch.rs:205) can then split or forge lines. Collapse
  whitespace in `target_key` or reject control characters.
- watch.rs:172 reads watch.txt with no size cap. Only `watch.observe` caps at 256 KiB; another writer may not.
- Two watch schedules with the same watch_target race `baseline_get`/`baseline_set` (watch.rs:185-245) and can notify
  twice. It is rare; document it or key the in-flight book by target.
- `write_proposals` (proposals.rs:303-341) reads, then appends, with no lock. Concurrent reflect passes, or an agent
  editing blockers.yaml at the same moment, can duplicate or lose lines.
- `LaunchRequest.schedule` is a new pub field on an I-13 type during the D-90 freeze. It is contained: no constructor
  outside core and harness on any swarm/* branch. Filed as B-197; interfaces.yaml is untouched. Fine as a recorded
  proposal.
- D-141: an Allow that comes from a gate rule (e.g. Send allowed on one domain) makes every recipient a global contact,
  because the gate ignores channel. This is documented; flag it to supreme.
- Postgres-backed tests (accept_t16_mem, the reflect Postgres variant) return green after a `SKIPPED` line when the DB
  is down. The tester should confirm no SKIPPED lines.
- MCP tool names with dots (`review.add`) follow the existing servers: srv-files `csv.clean`/`fs.write` and srv-web
  `<ns>.<tool>`. Claude Code ran dotted tools live: bench/results/ladder-phase1-servers.md:78 on
  swarm/servers-ladder-results shows `ashbyhq.list_jobs` called. Server-level permission rules (`mcp__harness-mem`) are
  unaffected.

## Verdict
No hard-rule break. Every step-4 acceptance item has a test that asserts behaviour:
- rung 8 notify once (accept_t16_watch.rs:311)
- missing, stopped and failed watch states (:442)
- watch canary (:522)
- rung 7 (:574)
- contacts touch then allow (accept_t16_contacts.rs:199)
- rung 6 restart (accept_t16_mem.rs:280)
- planted stale tool retired (accept_t16_reflect.rs:623/628)

Should-fix 1 and 2 decide whether live rungs 7 and 8 pass through harnessd.

Verdict: approve
