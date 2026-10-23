"""PreToolUse for Bash: block destructive or rule-breaking commands. Fails closed."""
from _common import *

d = read_input()
cmd = (d.get("tool_input", {}).get("command") or "").strip()
low = cmd.lower()

def has(*subs):
    return all(s in low for s in subs)

if re.search(r"\brm\b.*\btools/", cmd) or re.search(r"\bdel\b.*\\tools\\", low):
    block("tools are retired, archived, then deleted by the lifecycle (docs/03 T-7), never rm'd directly")
if has("git push", "--force") or has("git push", "-f "):
    block("force push is not allowed")
if re.search(r"git push\s+\S*\s+main\b", low) and area() != "supreme":
    block("sub-orchestrators push to swarm/<area> and open a PR; only supreme touches main")
if "cargo publish" in low:
    block("no publishing")
if re.search(r"sheetbrowse[\\/](?!v2_)", cmd, re.I) and re.search(r"\b(rm|mv|cp\s+\S+\s+|>|>>|sed -i|git (commit|checkout|reset))\b", low):
    block("the old sheetbrowse folder is reference only; do not modify it")
if re.search(r"\b(curl|wget|iwr|invoke-webrequest)\b", low):
    allow = ("github.com", "githubusercontent.com", "crates.io", "static.crates.io", "docs.rs", "registry.npmjs.org",
             "localhost", "127.0.0.1", "storage.googleapis.com/chromium-browser-snapshots", "huggingface.co")
    if not any(h in low for h in allow):
        block("network fetch from the shell is limited to package registries, GitHub, Chromium snapshots and localhost; use srv-web tools for pages")
if ("api_key" in low or "anthropic_api_key" in low) and ("export" in low or "set " in low or "$env:" in low):
    block("no API keys in this project (D-1)")
ok()
