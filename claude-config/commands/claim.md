---
description: Claim a task for this area (usage /claim T-xx)
argument-hint: T-xx
---
Claim task $ARGUMENTS: in agent-memory/tasks.yaml set its status to `doing`, `owner` to `<area>/<your role>` where area is this worktree's name (or `supreme` at repo root), and add `claimed_at: <ISO date>`. Refuse if it already has a different owner with status doing; say who owns it. If the task has `blocked_by` tasks that are not `done`, say so and still claim only if the user confirms. Validate the YAML, commit `agent-memory: claim $ARGUMENTS`.
