"""PostToolUse for Bash: scan output for the vault canary and key patterns."""
from _common import *

d = read_input()
resp = d.get("tool_response")
text = json.dumps(resp) if not isinstance(resp, str) else resp
canary = os.environ.get("SB_VAULT_CANARY")
if canary and canary in (text or ""):
    block("VAULT LEAK: the canary value appeared in command output. Stop, find the path it took (result, log, ledger, panic), fix scrub(), rerun.")
if text and KEY_RE.search(text):
    block("command output contains something that looks like an API key or private key. Do not proceed; remove it and report in blockers.yaml.")
ok()
