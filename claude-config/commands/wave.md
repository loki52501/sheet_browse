---
description: Show wave gate state: which area PRs are merged, which areas are unblocked
---
Read agent-memory/swarm.yaml. For each area print: wave, branch, last commit on swarm/<area> (`git log -1 --oneline swarm/<area>` if the branch exists), PR state if `gh` is available, merged (y/n), and what it is waiting on. Then list areas whose wave dependencies are all merged and can be launched with scripts/swarm/launch.ps1.
