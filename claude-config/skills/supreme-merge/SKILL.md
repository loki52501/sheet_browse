---
name: supreme-merge
description: Merge one sheetbrowseV2_ area PR (swarm/<area>) into main the safe way - second-opinion review, merge.ps1 as a background job, verify the gate and test totals, mark merged, file should-fix items, memory-keeper, checkpoint, report what unblocked. Use when supreme_status.py says an area's PR is ready.
---

# Supreme merge of one area

Preconditions: `python scripts/swarm/supreme_status.py` shows the area `pr_open` or ready, no duplicate keys in its blockers.yaml, `main` clean with no MERGE_HEAD, and no undecided interface changes for it.

## 1. Second-opinion review

- If the user opted into workflows, run `scripts/swarm/review_workflow.js` with the Workflow tool (`scriptPath`), args:
  `{area, tasks, crates, docs, contracts, focus}`, e.g. `{"area":"core","tasks":"T-03..T-09","crates":"core, core-pg, harness","docs":"docs/01-HARNESS-CORE.md and docs/02-MEMORY.md","contracts":"I-1, I-2, I-3, I-4, I-13","focus":"..."}`. Put known risks in `focus` (for core: the stale-index commit b7aa4f4 reverted nothing else; the vault-leak acceptance feeds the canary).
- Otherwise spawn one general-purpose agent with the `.claude/agents/reviewer.md` checklist, told to read the branch only via `git diff main...swarm/<area>` / `git show`, never the worktree, never cargo, and to return Blocking / Should fix / Notes / Verdict.
- Verify any blocking item yourself against the code before acting on it. If the blocker is in supreme-owned text (interfaces.yaml wording), fix the text on main with a decision (D-74 pattern). Otherwise send it back to the area with a handoff note and stop.

## 2. Merge (background job, no redirection)

Run with the PowerShell tool, `run_in_background: true`, and NO `*>`, `2>` or `| Out-File` on merge.ps1: under `$ErrorActionPreference = "Stop"` git's informational stderr becomes a terminating error after `git merge --no-commit` and leaves MERGE_HEAD on main.

```
Set-Location -LiteralPath '<repo root>'; $env:TMP='F:\sb-swarm\tmp-supreme'; $env:TEMP=$env:TMP; $env:TMPDIR=$env:TMP; $env:PYTHONIOENCODING='utf-8'; & .\scripts\swarm\merge.ps1 -Area <area>; "EXIT=$LASTEXITCODE"; git log --oneline -2
```

When it completes, check the output for `gate ok for <area>` and `merged swarm/<area>`, and sum the test lines (`grep -E "^test result" | awk '{p+=$4; f+=$6} END {print p, f}'`). Failed must be 0. If it printed `MERGE BLOCKED`, `main` is untouched; send the reason to the area.

## 3. Record

- `agent-memory/swarm.yaml`: set `areas.<area>.merged: true` with a comment `# <merge sha>, <date>: gate ok <tasks>, cargo test --workspace <n> passed 0 failed`.
- Should-fix items from the review: ONE new blocker under `blockers:` in `agent-memory/blockers.yaml` (insert right after the `blockers:` key line; never edit or delete existing entries) with the items, file:line, and a `default_taken` that says which item must land before which consumer merges.
- Append a handoff note to the area's suborch handoff: merged sha, the new blocker id, what to do next.
- Run `python -c "import sys,pathlib; sys.path.insert(0,'scripts/swarm'); import merge_gate as g; print(g.check_blockers_shape(g.load(pathlib.Path('agent-memory/blockers.yaml'))) or 'ok')"`, then commit.

## 4. memory-keeper

Spawn a general-purpose agent with `.claude/agents/memory-keeper.md`'s role plus: "blockers.yaml and decisions.yaml are append-only: never delete, move or rewrite an entry; list stale ones in your report instead." Scope it to `agent-memory/` on main, no commits, no other branches. Review its diff (`git diff --stat`) before committing it.

## 5. Checkpoint and report

`python scripts/swarm/checkpoint.py --state "..." --commit "supreme: <area> merged (<sha>)"`. Then tell the user: merged sha and test totals, review result (blocking / should-fix counts and the one that matters), and which areas are now unblocked (`needs_merged` in swarm.yaml) with the exact `scripts\swarm\launch.ps1 -Areas ...` command for them.
