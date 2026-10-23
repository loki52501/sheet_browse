"""SubagentStop: a subagent must leave a handoff file."""
from _common import *

d = read_input()
if d.get("stop_hook_active"):
    ok()
sid = session_id(d)
start = marked(f"session-{sid}") or (time.time() - 3 * 3600)
hd = ROOT / "agent-memory" / "handoffs"
recent = [p for p in hd.glob("*.yaml") if p.name != "TEMPLATE.yaml" and p.stat().st_mtime > start]
# also accept review/accept reports as the handoff for reviewer/tester agents
reports = list((ROOT / "bench" / "results").glob("*.md")) if (ROOT / "bench" / "results").exists() else []
recent += [p for p in reports if p.stat().st_mtime > start]
if not recent:
    block("Write a handoff first: copy agent-memory/handoffs/TEMPLATE.yaml to agent-memory/handoffs/<task>-<role>-<n>.yaml and fill it (state, not_done, gotchas, verify). Reviewers/testers may instead write their report under bench/results/.")
ok()
