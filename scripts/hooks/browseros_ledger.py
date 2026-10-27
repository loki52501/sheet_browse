"""PostToolUse for mcp__browseros__.*: record each BrowserOS call (D-98, docs/11).

BrowserOS is an external MCP server, so its calls never pass core::serve and are not in the
ledger. This hook appends one JSON line per call to runs/<SB_RUN_ID>/browseros.jsonl (under
SB_RUNS_ROOT), or to .claude/state/browseros.jsonl outside a harness run. It is a sibling of
ledger.jsonl, not part of it: ledger.jsonl mirrors the ledger table row for row by id (D-117).
harness-forge's tools.save and report.md read these lines.

Line fields follow LedgerEvent: run_id, ts, server ("browseros"), tool (name without the
mcp__browseros__ prefix), class (null: the gate never saw it), mode ("external"), args
(scrubbed), result_summary (first 300 chars, scrubbed), ok, why (error text when not ok).

Scrub: BrowserOS never resolves @vault, so the model cannot hand it a vault value it does not
hold; what is redacted here is what a hook can see: the planted canary, key patterns, and the
values of arguments whose names look like secrets. Fails open: a hook error never blocks a call.
"""
from _common import *

PREFIX = "mcp__browseros__"
SECRET_KEY = re.compile(r"pass(word)?|secret|token|api[_-]?key|otp|cvv|cvc|pin\b|card", re.I)
MAX_SUMMARY = 300


def scrub_text(s):
    if not isinstance(s, str):
        return s
    canary = os.environ.get("SB_VAULT_CANARY")
    if canary and canary in s:
        s = s.replace(canary, "***")
    return KEY_RE.sub("***", s)


def scrub_args(v, key=""):
    if isinstance(v, dict):
        return {k: scrub_args(x, k) for k, x in v.items()}
    if isinstance(v, list):
        return [scrub_args(x, key) for x in v]
    if isinstance(v, str) and SECRET_KEY.search(key or ""):
        return "***"
    return scrub_text(v)


def summary(resp):
    if resp is None:
        return ""
    if isinstance(resp, dict) and isinstance(resp.get("content"), list):
        parts = [c.get("text", "") for c in resp["content"] if isinstance(c, dict) and c.get("type") == "text"]
        text = " ".join(p for p in parts if p) or json.dumps(resp, ensure_ascii=False)
    elif isinstance(resp, str):
        text = resp
    else:
        text = json.dumps(resp, ensure_ascii=False)
    text = scrub_text(" ".join(text.split()))
    return text[:MAX_SUMMARY]


def is_error(resp):
    return isinstance(resp, dict) and bool(resp.get("isError") or resp.get("is_error"))


def target():
    run, root = os.environ.get("SB_RUN_ID"), os.environ.get("SB_RUNS_ROOT")
    if run and root:
        return Path(root) / run / "browseros.jsonl", run
    return STATE / "browseros.jsonl", None


def main():
    d = read_input()
    name = d.get("tool_name") or ""
    if not name.startswith(PREFIX):
        ok()
    resp = d.get("tool_response")
    err = is_error(resp)
    path, run = target()
    line = {
        "run_id": run,
        "ts": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "server": "browseros",
        "tool": name[len(PREFIX):],
        "class": None,
        "mode": "external",
        "args": scrub_args(d.get("tool_input") or {}),
        "result_summary": summary(resp),
        "ok": not err,
        "why": summary(resp) if err else None,
        "session_id": session_id(d),
    }
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        with open(path, "a", encoding="utf-8") as f:
            f.write(json.dumps(line, ensure_ascii=False) + "\n")
    except Exception:
        pass
    ok()


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception:
        ok()
