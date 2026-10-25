---
name: supreme-checkin
description: Run one supreme check-in for the sheetbrowseV2_ swarm in a few commands - status of every swarm/<area> branch, undecided interface changes (including ones hidden by a broken blockers.yaml), PRs ready to merge, stalled sessions - then act and checkpoint. Use for every scheduled or requested supreme check-in, and first thing when resuming as supreme.
---

# Supreme check-in

You are the supreme (repo root, branch `main`). Goal: a check-in costs one status command plus only the actions it lists.

## 1. Status (one command)

```
python scripts/swarm/supreme_status.py
```

It reads every `swarm/<area>` branch through git and prints a table plus an `actions:` list. Trust the list; do not re-derive it with ad-hoc `git show` loops. Use `--json` when you need a field it does not print.

If `actions: none` and nothing moved since the last check-in, reply with one line and stop.

## 2. Act on each action, in this order

- **`main: MERGE_HEAD present`**: an interrupted merge. `git merge --abort`, confirm `git status` is clean, then redo the merge per the `supreme-merge` skill.
- **`<area>: decide interface changes X, Y`**: for each id, read the entry raw from the branch (`git show swarm/<area>:agent-memory/blockers.yaml`, find `- id: X`). Never use `yaml.safe_load` alone on a branch's blockers.yaml: a duplicate key hides entries.
  - Check claims against the code before accepting: names and module paths must exist where the entry says (`git show swarm/<area>:crates/<crate>/src/lib.rs`, grep for `pub fn`). Wave 1 accepted two wrong paths (D-74) by skipping this.
  - Decide yes, or yes with a condition, or no. Append `- id: D-<next>` with `text` and `why` to the END of `agent-memory/decisions.yaml` (append-only; supreme ids are D-31..D-99; check the last id first).
  - Edit `agent-memory/interfaces.yaml`: append to the entry's `rust:` or `notes:` string, or add a new `- id: I-<n>` entry. Do it with a small Python edit that re-parses the file with `yaml.safe_load` before writing. Inside double-quoted YAML use `/` or `\\`, never a single `\`.
  - Append one pull-note comment line to the area's newest suborch handoff in its worktree, `F:/sb-swarm/wt/<area>/agent-memory/handoffs/<task>-suborch-<n>.yaml`, starting `# supreme <yyyy-mm-dd hh:mm>:`, naming the decision ids and "merge main before your PR". Re-parse that file after appending.
- **`<area>: blockers.yaml has duplicate keys`**: the area must repair it (D-75): move lines, never edit entries, then merge main. If its handoff has no supreme note about it yet, append one.
- **`<area>: PR ready -> supreme-merge`**: follow the `supreme-merge` skill.
- **`<area>: no live session` / `session alive but no commit for N min`**: tell the user. Relaunching is the user's call: `scripts\swarm\launch.ps1 -Areas <a,b> -Restart`. A live session idle at its prompt will not read new handoff notes by itself; tell the user what to type in that tab.
- **`<area>: unblocked ... -> ask the user to launch`**: tell the user the exact `launch.ps1 -Areas ...` command. Never launch or push yourself.

Blockers that are not interface changes (`needs_user: false`, default taken) need no answer unless the default is unsafe or mistimed (D-77 was both). Read new ones when the status table shows the branch moved.

## 3. Checkpoint (one command)

After any decision or merge:

```
python scripts/swarm/checkpoint.py --state "<what is true now: heads, merged areas, holds, open blockers>" --decision D-<n> --commit "supreme: <ids>, check-in <hh:mm>"
```

It writes the state as a JSON-quoted string (YAML-safe), re-parses the file, stages only the checkpoint, decisions.yaml and interfaces.yaml, and commits. Do not hand-edit the checkpoint's quoted strings.

## 4. Report

Short: what changed per area, each decision with its one-line why, what needs the user. No commands the user does not need to run.
