"""PostToolUse for Write/Edit: format Rust, validate YAML and tool headers, syntax-check JS."""
from _common import *

d = read_input()
path = d.get("tool_input", {}).get("file_path") or ""
if not path:
    ok()
p = Path(path)
s = str(p).replace("\\", "/")
try:
    if s.endswith(".rs") and p.exists():
        subprocess.run(["rustfmt", "--edition", "2021", str(p)], capture_output=True, timeout=30)
    elif "/agent-memory/" in s and s.endswith((".yaml", ".yml")):
        try:
            import yaml  # type: ignore
            with open(p, encoding="utf-8") as f:
                yaml.safe_load(f)
        except ImportError:
            pass
        except Exception as e:
            block(f"agent-memory YAML is invalid after this edit, fix it now: {e}")
    elif s.endswith(".js") and ("/injected/" in s or "/tools/" in s) and p.exists():
        r = subprocess.run(["node", "--check", str(p)], capture_output=True, text=True, timeout=20)
        if r.returncode != 0:
            block(f"JS syntax error: {r.stderr[:800]}")
        if "/tools/" in s and not s.endswith("_site.yaml"):
            head = p.read_text(encoding="utf-8")[:2000]
            missing = [k for k in ("@name", "@kind", "@gate", "@match") if k not in head]
            if missing:
                block(f"tool file header missing {missing} (docs/03 T-2)")
            if "@kind planner" in head and "?s" not in p.read_text(encoding="utf-8"):
                block("planner tools must stop with ?s before submit (D-29)")
except SystemExit:
    raise
except Exception:
    pass
ok()
