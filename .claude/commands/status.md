---
description: Show tasks by area and status, open blockers, wave gates
---
Read agent-memory/tasks.yaml, agent-memory/blockers.yaml and agent-memory/swarm.yaml. If you are the supreme (repo root on main), also read each existing `swarm/<area>` branch's copies (`git show swarm/<area>:agent-memory/tasks.yaml`, same for blockers.yaml): claims, statuses and interface_changes live on the branches until merge, and a branch's value wins for the tasks that area owns. Print a compact table: task id, step, status, owner, title (60 chars), and the branch the status came from. Then open blockers with their defaults, pending interface changes (with the branch they came from), and the wave gate lines. No commentary beyond the tables.
