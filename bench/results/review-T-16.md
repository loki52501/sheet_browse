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

## Round 3 (supreme hold, D-144)

Range: `git diff bd1f47c..HEAD`, non-merge commits 3c4f830 and be24f1e, limited to crates, migrations, bench/phase1
and agent-memory/decisions.yaml. Not built here, because a workspace test run held the build lock and host RAM was
short. Test claims are read from the code; the mutation claim is the coder's (handoffs/T-16-suborch-2.yaml).

Blocking for the whole review is still `- none`: no hard-rule break, and every supreme ask has a test that asserts the
behaviour.

### Asks (3), (4), (5) at HEAD
- **(3) watch.txt scrubbed: closed.** mem_server.rs:546 runs `self.vault.scrub(state)` before `write_atomic` at :548.
  The test is accept_t16_mem.rs:713 and :734-737, which checks that watch.txt never holds the canary. It fails if the
  scrub is removed. be24f1e (:697) gives that MCP session a yes, because its schedule.add now skips with no answer.
- **(4) replay session readers-only: closed.**
  - The argv is at launcher.rs:270-286: `--strict-mcp-config --mcp-config <run>/replay-mcp.json`, then
    `--setting-sources local --permission-mode default`, then `--tools ""`, `--allowedTools mcp__harness-web`, and
    `--disallowedTools` with DISALLOWED_TOOLS plus BUILTIN_TOOLS last.
  - cwd is the absolute run folder (:246).
  - The config names harness-web only (:168-177).
  - `the_replay_session_is_readers_only` (:577-611) compares the whole argv, including the empty string after
    `--tools`, and checks cwd and SB_TOOLS_ROOT. Removing any piece turns it red.
- **(5) no schedule without a yes: closed.**
  - mem_server.rs:255-262 hints schedule.add with `HINT_SCHEDULE`, and mcp.rs:1081/:1098 copy the hints into the
    `Call`.
  - The hit is found at the call's own class (gate.rs:459-468, `Account:schedule`), so `from_hint` stays true (:402).
    `may_proceed` is then false (:331), so strict is true and `on_timeout` is Skip (:340, :346, :375-378).
  - `ask_capped` can only tighten this: mcp.rs:564-573 turns a TimeoutProceed against a Skip card into a skip, and an
    asker that hangs is skipped.
  - Tests:
    - gate.rs:1361 checks Skip with the seed and Proceed with no seed, so it fails if the hint branch is removed.
    - accept_t16_mem.rs:540-561 runs on Postgres with the real migration. With no answer it expects a `skipped:` reply,
      still 1 row, no wake marker and ledger `TimeoutSkip`. It fails if the hint or the migration is dropped.
  - The key is spelled the same everywhere: migrations/0003_schedule_ask.sql:5-7 `'Account:schedule'`,
    testing.rs:44-49 SEED_RULES, accept_t03.rs:380. `rule_class_key("Account:schedule")` is that string, and
    PgStore::gate_rule canonicalises before it looks up (core-pg/src/lib.rs:791-797).

### Q2: can an unanswered or planted schedule.add still add a row?
Not through harness-mem.
- A failed rule lookup fails closed: gate error, then `ToolError`, then nothing runs (mcp.rs:1109-1112).
- `domain` is None, so no domain-scoped row can shadow the `*` rule.
- A user `('*','Account','allow')` loses to the hint rule.
- The class floor is Account (mcp.rs:1080).
- No model-reachable tool writes gate rules. Only cli.rs:309 does.
- The only fallback to "ask, timeout proceeds" is the user deleting or changing the row, which D-144 (1) documents.
- MemStore and the Postgres seed agree.
- 3c4f830 also changes harness-core, so core-pg is rebuilt and `sqlx::migrate!` picks up 0003 (see Note c).

Outside harness-mem, see Should-fix R3-1.

### Q3: `--tools ""` argv
- tokio delegates to std. On Windows, std quotes an empty argument as `""`, both for an .exe and, since Rust 1.77.2,
  through the batch-file path that the claude.cmd fallback (launcher.rs:304-311) takes. cmd's `%*` passes `""`
  through, and node reads it as one empty argv entry.
- Claude Code's `--tools` takes a list of values. The next token, `--allowedTools`, starts with `--`, so `--tools`
  ends at the empty value and the lists do not run into each other.
- The test reads argv from `as_std().get_args()`. That is the argument list, not the quoted Windows command line, so
  the quoting itself is not tested (Note a).
- The doc claims at launcher.rs:27-40 and reflect.rs:27-33 match the code, with two small gaps in Notes a and b.

### Should fix (round 3)
- **R3-1. Fired `claude -p` sessions still load project and user settings and keep Bash available.** This predates
  the delta and is outside it, but it is the remaining way a planted schedule (or gate rule) gets in.
  - Where: `ClaudeLauncher::print` sets no current_dir (launcher.rs:240-242), passes no `--setting-sources` and no
    `--tools`, and denies only `harness ask/gate/vault` (:56-60).
  - What happens: started from the repo root, a firing inherits .claude/settings.json's `Bash(psql *)` and
    `Bash(cargo *)`. Page-derived text in that session could then run `psql -c "insert into schedules ..."`, or
    `cargo run -p harness -- schedule add ...` / `-- gate set ...`. None of those match the deny patterns, and none
    of them reach `core::mcp::serve`.
  - Fix: give print sessions the replay session's isolation. That means `--tools ""` (or only the built-ins a
    firing needs), `--setting-sources local`, and cwd = the run folder. Also deny `Bash(harness schedule:*)`.
    Track it as a T-16b item or a blocker for supreme.
- **R3-2. `HINT_SCHEDULE` is a new pub item of I-3 that interfaces.yaml:28 does not list, and no blocker records it.**
  It is additive, like B-197. File it as an interface proposal for supreme.

### Notes (round 3)
- a. "Checked live on Claude Code 2.1.311" (launcher.rs:35, D-144 (2)) has no artifact in bench/results. Only the
  commit and the handoff say it. Before merge, record the session's tool list in accept-T-16.md. No test builds the
  Windows command line for the shim path.
- b. launcher.rs:36-37 says BUILTIN_TOOLS are denied "in case a Claude Code version ignores `--tools`". A version
  without the flag would reject it as an unknown option and the replay would fail, which fails closed. The wording
  should say that.
- c. core-pg has no build.rs with `cargo:rerun-if-changed=../../migrations`. A future change that only adds a
  migration (no Rust change in core-pg or its dependencies) would leave an incrementally built harnessd without it,
  and the sqlx docs warn about exactly this. It is not hit here, because harness-core changed in the same commit.
- d. accept_t16_mem and accept_t03 still pass with a SKIPPED line when Postgres is down. The tester must confirm
  there is no SKIPPED line for the new timeout-skip assertion.
- e. Hard rules on the delta:
  - no keys;
  - no vault value in the new seed note, card text or test fixtures; the canary test now answers yes rather than
    weakening its assertion;
  - no sleeps, no em dashes, no new `unwrap` outside tests;
  - rung-7.md's expected text matches D-144.

### Round 3 verdict
Asks (3), (4) and (5) are closed in code, and each has a test that fails if its fix is removed. R3-1 is a real
remaining bypass path for fired sessions. It predates and lies outside this delta, so it is should-fix for the
supreme to schedule, not a hold on PR #17.

Verdict: approve
