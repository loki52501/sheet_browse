"""Stop: refuse (once) to end with uncommitted work or claimed tasks without a handoff."""
from _common import *

d = read_input()
if d.get("stop_hook_active"):
    ok()  # already refused once; let it stop
sid = session_id(d)
start = marked(f"session-{sid}")
st = git("status", "--porcelain", "--", "crates", "injected", "tools", "migrations", "Cargo.toml")
dirty = [l for l in st.splitlines() if l.strip()]
problems = []
if dirty:
    problems.append(f"{len(dirty)} uncommitted change(s) under crates/ injected/ tools/ migrations/. Commit with spec ids in the message.")
a = area()
tasks = load_yaml_lenient(ROOT / "agent-memory" / "tasks.yaml") or {}
mine = [t["id"] for t in tasks.get("tasks", []) if t.get("status") == "doing" and str(t.get("owner", "")).startswith(a)]
if mine:
    hd = ROOT / "agent-memory" / "handoffs"
    recent = [p for p in hd.glob("*.yaml") if p.name != "TEMPLATE.yaml" and p.stat().st_mtime > start]
    if not recent:
        problems.append(f"tasks {mine} are 'doing' and owned by {a} but no handoff was written this session. Run /handoff <task> or mark them done.")
if problems:
    block("Before stopping:\n- " + "\n- ".join(problems))
ok()
