---
description: Spawn a reviewer on a task's diff (usage /review T-xx)
argument-hint: T-xx
---
Find the commits for $ARGUMENTS (`git log --oneline main...HEAD --grep=$ARGUMENTS` plus any commit touching the crates in that task). Spawn the `reviewer` subagent with the diff range and the task's doc ids. Print the Blocking and Verdict sections of bench/results/review-$ARGUMENTS.md when it returns.
