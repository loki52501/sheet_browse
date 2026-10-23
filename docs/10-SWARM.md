# 10 · Swarm: how many agents, in what shape, coordinated how

Goal: finish Phase 1 in days, not weeks, without the agents stepping on each other or
on the rules. Items are `S-n`.

## S-1 Topology (and the one limit that shapes it)

Claude Code subagents cannot spawn subagents. A tree deeper than two levels is therefore
built from **sessions**, not from nested subagents:

```
SUPREME ORCHESTRATOR          1 Claude Code session, repo root, main branch
  owns: agent-memory/tasks.yaml, merges, wave gates, acceptance verdicts
  │
  ├─ SUB-ORCHESTRATOR "core"      session in worktree wt/core      branch swarm/core
  ├─ SUB-ORCHESTRATOR "sheet"     session in worktree wt/sheet     branch swarm/sheet
  ├─ SUB-ORCHESTRATOR "store"     session in worktree wt/store     branch swarm/store
  ├─ SUB-ORCHESTRATOR "page"      session in worktree wt/page      branch swarm/page
  ├─ SUB-ORCHESTRATOR "servers"   session in worktree wt/servers   branch swarm/servers   (wave 2)
  └─ SUB-ORCHESTRATOR "driver"    session in worktree wt/driver    branch swarm/driver    (wave 2, Phase 2)
        each spawns subagents from .claude/agents/:
        rust-coder · js-coder · reviewer · tester · memory-keeper · spec-reader · bench-runner
```

Each sub-orchestrator owns one area (a set of crates), claims its tasks in `tasks.yaml`,
spawns coders per crate, a reviewer per PR, a tester for acceptance, and a memory-keeper
to write handoffs and decisions. It opens a PR to `main`; the supreme merges when the
reviewer and tester reports are green.

## S-2 Areas and waves

Interfaces are fixed in `agent-memory/interfaces.yaml` before wave 1, so areas code
against traits, not against each other's unfinished code.

| Wave | Area | Crates / files | Tasks | Needs |
|---|---|---|---|---|
| 0 (supreme, 1 hour) | contracts | workspace `Cargo.toml`, `core` trait + types only (compiles, `todo!()` bodies), `docker-compose.yml`, `migrations/0001`, stub crates for every box | T-01, T-02 | nothing |
| 1 | core | `core` (gate, vault, ledger, run, sched, mcp wrapper), `core-pg`, `harness` CLI | T-03..T-09 | wave 0 |
| 1 | sheet | `sb-sheet` (pure): index types, skeleton, find scorer, sheet, read renderer, data files | T-10 | wave 0 |
| 1 | store | `toolstore`: header parser, loader, fingerprint on index, lifecycle, git helper | T-11 | wave 0 (uses I-5 types from the stub) |
| 1 | page | `injected/injected.js` + `media.js` against fixture pages (jsdom + Playwright for tests), `sb-act` (pure) | T-19 (JS half), B-7 | wave 0 |
| 2 | servers | `srv-web`, `srv-files`, `srv-cli`, seeded `tools/ashbyhq.com` | T-12..T-15 | core, sheet, store merged |
| 2 | driver | `sb-cdp` Pipe, `sb-driver` registry/act/input/consequences/settle | T-18, T-20..T-22 | page merged |
| 3 | core | memory in use, scheduler, watch, reflect | T-16 | servers merged |
| 3 | driver | `sb-mcp`, `sb-media`, hatches, bench parity | T-23..T-26 | driver wave 2 |
| 4 | supreme | Phase 1 baseline (T-17), then Phase 2: mail/calendar tools (T-27), toolforge (T-28), Phase 2 ladder (T-29) | | all |

Phase 1 is done at the end of wave 3 for `core`/`sheet`/`store`/`servers` plus T-17.
`page` and `driver` are Phase 2 work that starts in wave 1 because nothing blocks it;
the supreme does not merge `sb-driver` into the Phase 1 baseline run.

## S-3 Coordination protocol (every agent follows this)

1. **Claim before coding.** `tasks.yaml`: set `status: doing`, `owner: <area>/<agent>`,
   `claimed_at`. The `/claim T-xx` command does it. A task with another owner is not touched.
2. **Interfaces are law.** A change to any `I-n` goes to the supreme as a one-line
   proposal in `blockers.yaml` under `interface_changes` and waits for a yes. Code against
   the current trait meanwhile.
3. **Decisions are written, not remembered.** Any choice not in the docs → `decisions.yaml`
   with a why, before the code that depends on it is committed.
4. **Handoff on every stop.** A subagent that finishes or is stopped writes
   `agent-memory/handoffs/<task>-<agent>-<n>.yaml` from the template. The `SubagentStop`
   hook refuses a stop without one.
5. **Commit per task, PR per area per wave.** Commit messages carry spec ids. The `Stop`
   hook refuses to end a session with uncommitted changes in `crates/` or `injected/`.
6. **Acceptance is a tester's job, not the coder's.** The coder says "done"; a `tester`
   subagent runs the acceptance list for that doc and writes `bench/results/accept-<task>.md`.
   The reviewer reads code, the tester runs it; they are different subagents.
7. **Blocked means default-and-go.** Open question → `blockers.yaml` with the default
   taken. Only an irreversible either-way decision stops an agent.

## S-4 Hooks (in `.claude/settings.json`, scripts in `scripts/hooks/`)

`.claude/` is generated from `claude-config/` by `scripts/swarm/install-claude-config.ps1`;
edit `claude-config/`, re-run the installer, commit both.

| Event | Matcher | Script | Does |
|---|---|---|---|
| SessionStart | | `session_start.py` | prints the area's claimed tasks, open blockers, interface changes pending, and the wave gate state into context; records session start time |
| PreToolUse | Write, Edit, MultiEdit | `guard_write.py` | **blocks**: writes outside this worktree, into `../sheetbrowse/`, into `.env`, `*.age`, `tools/_archive/`; any content matching key patterns (`sk-ant-`, `AKIA`, `ghp_`, `xoxb-`, `-----BEGIN`); edits to `agent-memory/interfaces.yaml` by anyone but supreme |
| PreToolUse | Bash | `guard_bash.py` | **blocks**: `rm` on `tools/`, `git push --force`, `git push` to `main` from a sub-orchestrator, `cargo publish`, anything writing under the old `sheetbrowse` path, `curl`/`wget` to non-allowlisted hosts |
| PostToolUse | Write, Edit | `post_write.py` | `.rs` → `rustfmt` the file; `agent-memory/*.yaml` → YAML parse, exit 2 with the error if invalid; `injected/*.js` → `node --check`; `tools/**/*.js` → header parse (`@name @kind @gate` present) |
| PostToolUse | Bash | `scan_output.py` | scans the tool output for the vault canary (env `SB_VAULT_CANARY`) and any key pattern; exit 2 with "vault leak" if found |
| Stop | | `stop_check.py` | exit 2 (refuse once) if `git status` shows changes in `crates/ injected/ tools/` not committed, or a `doing` task owned by this session has no handoff newer than session start |
| SubagentStop | | `subagent_stop.py` | exit 2 if no handoff file newer than the subagent's start |
| PreCompact | | `pre_compact.py` | appends a compact summary of claimed tasks + last 5 commits to `agent-memory/handoffs/_compact-<session>.md` so nothing is lost |

All scripts are Python 3 stdlib, read the hook JSON from stdin, fail **open** (exit 0)
on their own errors except the two guards, which fail **closed**.

## S-5 Subagents (`.claude/agents/`)

| Agent | Role | Tools |
|---|---|---|
| `rust-coder` | implements one crate against its doc ids and interface ids; runs fmt/clippy/test; commits per task | Read Write Edit Bash Grep Glob |
| `js-coder` | `injected/` and `tools/**/*.js`; tests with node + jsdom + Playwright fixtures | same |
| `reviewer` | reads a diff; checks every CLAUDE.md rule, spec ids claimed vs implemented, interface fidelity; writes `review-<task>.md`; never edits code | Read Grep Glob Bash(read-only git) |
| `tester` | runs the acceptance list for a doc; writes `bench/results/accept-<task>.md`; may write tests, not implementation | Read Write Edit Bash Grep Glob |
| `memory-keeper` | keeps `agent-memory/` true: tasks, decisions, blockers, handoffs; condenses when long | Read Write Edit |
| `spec-reader` | answers "what does the spec say about X" with doc ids; read-only | Read Grep Glob |
| `bench-runner` | runs ladders and bench sets, writes result tables | Read Write Bash |

## S-6 Slash commands (`.claude/commands/`)

`/status` tasks by area and status · `/claim T-xx` claim a task · `/handoff T-xx` write a
handoff from the template with git state filled in · `/acceptance <doc>` run that doc's
acceptance via a tester subagent · `/wave` print wave gate state (which PRs are merged,
which areas are unblocked) · `/review <task>` spawn a reviewer on the task's diff.

## S-7 Launch

```powershell
scripts\swarm\launch.ps1 -Areas core,sheet,store,page      # wave 1
scripts\swarm\launch.ps1 -Areas servers,driver             # wave 2, after merges
```
Creates `wt/<area>` worktrees on `swarm/<area>` branches, opens a Windows Terminal tab
per area, and starts `claude` in each with `prompts/suborch/<area>.md` as the first
message. The supreme session runs in the repo root with `prompts/SUPREME.md`.

## S-8 Merge and gate

Supreme merges an area's PR when: reviewer report has no blocking item, tester report is
green for the tasks claimed, hooks never fired a leak, `cargo test --workspace` passes
on the merge commit. Then it updates `agent-memory/swarm.yaml: waves` and tells the
waiting areas to pull. A wave gate is a line in `swarm.yaml`, not a conversation.

## S-9 Cost control

- Coders and testers may run as a cheaper model (`model:` in the agent file); reviewers
  and sub-orchestrators inherit. Change in one place.
- `spec-reader` exists so coders don't re-read all docs; they ask it.
- `PreCompact` hook means a compaction never loses the task state.
- Sub-orchestrators do not run bench sets; only `bench-runner` under supreme does.
