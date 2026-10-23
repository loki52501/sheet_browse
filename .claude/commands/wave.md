---
description: Show wave gate state: which area PRs are merged, which areas are unblocked
---
Read agent-memory/swarm.yaml on main. For each area print: wave, branch, last commit on swarm/<area> (`git log -1 --oneline swarm/<area>` if the branch exists), the area's own status from its branch (`git show swarm/<area>:agent-memory/swarm.yaml`, key `areas.<area>.status`; `pr_open` there is the PR in a local-only repo, D-46), PR state if `gh` is available and a remote exists, merged (y/n, from main), and what it is waiting on. Then list areas whose `needs_merged` are all merged and can be launched with scripts/swarm/launch.ps1.
