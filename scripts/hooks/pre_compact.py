"""PreCompact: snapshot task state and recent commits so compaction loses nothing."""
from _common import *

d = read_input()
sid = session_id(d)
a = area()
tasks = load_yaml_lenient(ROOT / "agent-memory" / "tasks.yaml") or {}
mine = [f"{t['id']} {t['title']}" for t in tasks.get("tasks", []) if t.get("status") == "doing" and str(t.get("owner", "")).startswith(a)]
log = git("log", "--oneline", "-8")
st = git("status", "--short")
out = ROOT / "agent-memory" / "handoffs" / f"_compact-{a}-{sid[:8]}.md"
out.write_text(
    f"# compact snapshot · area {a} · session {sid}\n\n## doing\n" + ("\n".join(mine) or "none") +
    f"\n\n## last commits\n{log}\n## uncommitted\n{st or 'clean'}\n\n"
    "## next\nRe-read CLAUDE.md, docs/10-SWARM.md S-3, your area prompt in prompts/suborch/, and this file.\n",
    encoding="utf-8",
)
ok(f"[swarm] wrote {out.relative_to(ROOT)}\n")
