---
description: Run a doc's acceptance list via a tester subagent (usage /acceptance docs/01-HARNESS-CORE.md T-09)
argument-hint: <doc> <task>
---
Spawn the `tester` subagent with this brief: "Doc: $1. Task: $2. Run every item in that doc's Acceptance section, write bench/results/accept-$2.md, commit the tests you add." When it returns, print its verdict table and, if anything failed, list the exact missing behaviour so a coder can act on it.
