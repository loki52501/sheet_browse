"""PreToolUse for Write/Edit/MultiEdit: block forbidden paths and secret-looking content. Fails closed."""
from _common import *

d = read_input()
ti = d.get("tool_input", {})
path = ti.get("file_path") or ti.get("path") or ""
content = ti.get("content") or ti.get("new_string") or ""
if isinstance(ti.get("edits"), list):
    content += "".join(e.get("new_string", "") for e in ti["edits"])

p = Path(path).resolve() if path else None
root = ROOT.resolve()
if p:
    try:
        rel = p.relative_to(root)
    except ValueError:
        block(f"write outside this repo/worktree is not allowed: {p}")
    s = str(rel).replace("\\", "/")
    if "sheetbrowse/" in str(p).replace("\\", "/") and "sheetbrowseV2_" not in str(p):
        block("the old sheetbrowse folder is reference only; do not write there")
    if s == ".env" or s.endswith(".age") or s.startswith("tools/_archive/"):
        block(f"writing {s} is not allowed (secrets file or archive); see CLAUDE.md")
    if s == "agent-memory/interfaces.yaml" and area() != "supreme":
        block("only the supreme orchestrator edits interfaces.yaml; propose the change in blockers.yaml under interface_changes")
    if s.startswith("tools/") and s.endswith(".js") and "@gate" in content:
        # a planner must keep ?s before any submit-class click; crude but catches the common removal
        if "@kind planner" in content and "?s" not in content:
            block("planner tools must contain '?s' before a submit step (D-29)")
if content and KEY_RE.search(content):
    block("content looks like an API key or private key; the harness has no keys (D-1). Use @vault(name).")
canary = os.environ.get("SB_VAULT_CANARY")
if canary and canary in content:
    block("vault canary found in content being written: this would be a vault leak")
ok()
