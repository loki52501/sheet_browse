---
description: Write a handoff for a task from the template (usage /handoff T-xx)
argument-hint: T-xx
---
Create agent-memory/handoffs/$ARGUMENTS-<role>-<n>.yaml from agent-memory/handoffs/TEMPLATE.yaml. Fill: state (what compiles, what tests pass, with the command), not_done (ordered), gotchas (things that cost time), decisions_made (ids you added), blockers_opened, files_touched (from `git diff --name-only main...HEAD`), verify (one command). Then update the task's status in tasks.yaml (`done` if acceptance is green, else leave `doing` with a `next:` line). Validate YAML and commit `agent-memory: handoff $ARGUMENTS`.
