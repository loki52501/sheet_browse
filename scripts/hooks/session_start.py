"""SessionStart: print this area's state into context and record the start time."""
from _common import *

d = read_input()
mark(f"session-{session_id(d)}")
a = area()
out = [f"[swarm] area={a} branch={git('rev-parse','--abbrev-ref','HEAD').strip()} root={ROOT}"]
tasks = load_yaml_lenient(ROOT / "agent-memory" / "tasks.yaml") or {}
mine, doing, blocked = [], [], []
for t in tasks.get("tasks", []):
    o = str(t.get("owner", ""))
    if t.get("status") == "doing":
        (mine if o.startswith(a) else doing).append(f"{t['id']} {t['title'][:60]} ({o})")
    if t.get("status") == "blocked":
        blocked.append(f"{t['id']} {t['title'][:60]}")
out.append("claimed by this area: " + (", ".join(mine) or "none"))
out.append("in progress elsewhere: " + (", ".join(doing) or "none"))
if blocked:
    out.append("blocked: " + ", ".join(blocked))
b = load_yaml_lenient(ROOT / "agent-memory" / "blockers.yaml") or {}
if b.get("blockers"):
    out.append(f"open blockers: {len(b['blockers'])} (see agent-memory/blockers.yaml)")
if b.get("interface_changes"):
    out.append(f"interface changes awaiting supreme: {len(b['interface_changes'])}")
sw = load_yaml_lenient(ROOT / "agent-memory" / "swarm.yaml") or {}
if sw.get("waves"):
    out.append("waves: " + "; ".join(f"{k}={v}" for k, v in sw["waves"].items()))
out.append("rules: CLAUDE.md. protocol: docs/10-SWARM.md S-3. claim with /claim, hand off with /handoff.")
ok("\n".join(out) + "\n")
