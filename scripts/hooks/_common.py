"""Shared helpers for Claude Code hooks. Stdlib only. Fail open unless told otherwise."""
import json, os, re, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
STATE = ROOT / ".claude" / "state"
STATE.mkdir(parents=True, exist_ok=True)

KEY_PATTERNS = [
    r"sk-ant-[A-Za-z0-9_-]{10,}", r"sk-[A-Za-z0-9]{20,}", r"AKIA[0-9A-Z]{16}", r"ghp_[A-Za-z0-9]{20,}",
    r"github_pat_[A-Za-z0-9_]{20,}", r"xox[baprs]-[A-Za-z0-9-]{10,}", r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
    r"AIza[0-9A-Za-z_-]{30,}",
]
KEY_RE = re.compile("|".join(KEY_PATTERNS))

def read_input():
    try:
        return json.load(sys.stdin)
    except Exception:
        return {}

def session_id(d):
    return d.get("session_id") or os.environ.get("CLAUDE_SESSION_ID") or "unknown"

def area():
    # worktree folder name wt/<area> or env override; supreme at repo root
    env = os.environ.get("SB_AREA")
    if env:
        return env
    cwd = Path.cwd().resolve()
    if cwd.parent.name == "wt":
        return cwd.name
    return "supreme"

def git(*args, cwd=None):
    try:
        return subprocess.run(["git", *args], cwd=cwd or Path.cwd(), capture_output=True, text=True, timeout=20).stdout
    except Exception:
        return ""

def block(msg):
    # exit 2 = blocking error shown to Claude
    sys.stderr.write(msg.strip() + "\n")
    sys.exit(2)

def ok(msg=None):
    if msg:
        sys.stdout.write(msg)
    sys.exit(0)

def mark(name):
    (STATE / name).write_text(str(time.time()))

def marked(name):
    p = STATE / name
    try:
        return float(p.read_text())
    except Exception:
        return 0.0

def load_yaml_lenient(path):
    """Try PyYAML; if missing, do a crude validity check (balanced quotes/indent) and return None."""
    try:
        import yaml  # type: ignore
        with open(path, encoding="utf-8") as f:
            return yaml.safe_load(f)
    except ImportError:
        return None
