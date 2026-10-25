"""Rewrite fields of the supreme checkpoint (agent-memory/handoffs/supreme-wave<N>.yaml) safely.

  python scripts/swarm/checkpoint.py --state "text" [--not-done "text"] [--decision D-82 ...] [--commit "msg"]

Values are written as JSON strings, which YAML reads as double-quoted scalars, so backslashes,
quotes and colons in the text can never break the file (hand-written escapes did, twice).
The file is re-parsed before it is written; --commit stages only this file and decisions.yaml /
interfaces.yaml if they changed, and commits with the Co-Authored-By trailer.
"""
import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
TRAILER = "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"


def set_scalar(text, key, value):
    line = f"{key}: {json.dumps(value, ensure_ascii=False)}"
    new, n = re.subn(rf"^{key}: .*$", lambda _m: line, text, count=1, flags=re.M)
    assert n == 1, f"no top-level `{key}:` line"
    return new


def main(argv):
    p = argparse.ArgumentParser()
    p.add_argument("--file", default=None, help="checkpoint file (default: newest handoffs/supreme-wave*.yaml)")
    p.add_argument("--state")
    p.add_argument("--not-done", dest="not_done")
    p.add_argument("--decision", action="append", default=[], help="append an id to decisions_made")
    p.add_argument("--commit", help="commit message (staged: checkpoint + changed agent-memory contract files)")
    a = p.parse_args(argv[1:])
    path = Path(a.file) if a.file else sorted((ROOT / "agent-memory/handoffs").glob("supreme-wave*.yaml"))[-1]
    text = path.read_text(encoding="utf-8")
    if a.state:
        text = set_scalar(text, "state", a.state)
    if a.not_done:
        text = set_scalar(text, "not_done", a.not_done)
    if a.decision:
        cur = yaml.safe_load(text).get("decisions_made") or []
        ids = cur + [d for d in a.decision if d not in cur]
        text = re.sub(r"^decisions_made: .*$", lambda _m: f"decisions_made: [{', '.join(ids)}]", text, count=1, flags=re.M)
    yaml.safe_load(text)  # refuse to write a file that does not parse
    path.write_text(text, encoding="utf-8", newline="\n")
    print(f"updated {path}")
    if a.commit:
        for f in ("agent-memory/decisions.yaml", "agent-memory/interfaces.yaml"):
            yaml.safe_load((ROOT / f).read_text(encoding="utf-8"))
        paths = [str(path.relative_to(ROOT)), "agent-memory/decisions.yaml", "agent-memory/interfaces.yaml"]
        subprocess.run(["git", "-C", str(ROOT), "add", "--", *paths], check=True)
        subprocess.run(["git", "-C", str(ROOT), "commit", "-q", "-m", f"{a.commit}\n\n{TRAILER}"], check=True)
        print(subprocess.run(["git", "-C", str(ROOT), "log", "--oneline", "-1"], capture_output=True, text=True).stdout.strip())
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
