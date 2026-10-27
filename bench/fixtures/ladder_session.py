"""Session wrapper for the Phase 1 ladder (docs/08 Q-1): the `claude -p` that `harness run`
starts when SB_CLAUDE points at this wrapper's .cmd shim.

`harness run "<prompt>"` creates the run the normal way (runs/<id>/ with prompt.md, a
`runs` row with status running), then spawns `$SB_CLAUDE " <prompt>" --disallowedTools
<harness ask/gate/vault>` with SB_RUN_ID and SB_RUNS_ROOT set, and after it exits writes
report.md from the ledger and ends the row. This wrapper turns that interactive launch into
a headless one:

  claude -p --output-format stream-json --verbose
         --mcp-config $SB_LADDER_MCP --strict-mcp-config
         --setting-sources local          (cwd is an empty scratch folder: no settings load)
         --permission-mode default
         --allowedTools mcp__harness-web mcp__harness-files mcp__harness-cli
         --disallowedTools <the three from harness> Bash PowerShell Edit Write MultiEdit
                           NotebookEdit WebFetch WebSearch Read Glob Grep Task Agent
                           CronCreate CronDelete RemoteTrigger ScheduleWakeup
                           PushNotification SendMessage Workflow Skill EnterWorktree
                           ExitWorktree DesignSync

with the prompt (runs/<id>/prompt.md, exactly what the harness stored) on stdin and a
wall-clock cap (SB_LADDER_CAP seconds, default 900; past it the process tree is killed).
The session's final text goes to runs/<id>/files/answer.md; the full stream-json
transcript goes to $SB_LADDER_LOGS/<run id>.jsonl (outside runs/, for scoring and the
canary grep). Claude Code's own session variables inherited from the calling session are
dropped; ANTHROPIC_* variables are dropped too (rule 1: no API key is set or read).

Two optional knobs (T-16 ladder rungs 6 to 8):
  SB_LADDER_ALLOWED  space-separated `--allowedTools` list replacing the three above (rungs
                     6 to 8 add mcp__harness-mem, the `harness mcp` mem server);
  -p in argv         the launch came from harnessd (`ClaudeLauncher::print`): the prompt is
                     what harnessd wrote on stdin (run preamble, blank line, task; B-563),
                     not prompt.md, and the final text goes to stdout, which
                     harnessd keeps as files/answer.md.

Model calls go through Claude Code only (CLAUDE.md rule 1).
"""

import json
import os
import shutil
import subprocess
import sys

ALLOWED = ["mcp__harness-web", "mcp__harness-files", "mcp__harness-cli"]
EXTRA_DENY = ["Bash", "PowerShell", "Edit", "Write", "MultiEdit", "NotebookEdit", "WebFetch",
              "WebSearch", "Read", "Glob", "Grep", "Task", "Agent",
              # Claude Code built-ins with effects outside the three servers' gate.
              "CronCreate", "CronDelete", "RemoteTrigger", "ScheduleWakeup", "PushNotification",
              "SendMessage", "Workflow", "Skill", "EnterWorktree", "ExitWorktree", "DesignSync"]
KEEP_CLAUDE_VARS = {"CLAUDE_CONFIG_DIR"}


def main() -> int:
    args = sys.argv[1:]
    harness_deny = []
    if "--disallowedTools" in args:
        harness_deny = args[args.index("--disallowedTools") + 1:]
    root = os.environ["SB_RUNS_ROOT"]
    run_id = os.environ["SB_RUN_ID"]
    run_dir = os.path.join(root, run_id)
    if "-p" in args:
        prompt = sys.stdin.buffer.read().decode("utf-8")
    else:
        with open(os.path.join(run_dir, "prompt.md"), encoding="utf-8") as f:
            prompt = f.read()
    allowed = os.environ.get("SB_LADDER_ALLOWED", "").split() or ALLOWED
    cap = int(os.environ.get("SB_LADDER_CAP", "900"))
    logs = os.environ["SB_LADDER_LOGS"]
    cwd = os.environ["SB_LADDER_CWD"]
    os.makedirs(logs, exist_ok=True)

    env = {k: v for k, v in os.environ.items()
           if not ((k.startswith("CLAUDE") and k not in KEEP_CLAUDE_VARS)
                   or k.startswith("ANTHROPIC"))}
    claude = shutil.which("claude") or "claude"
    cmd = [claude, "-p", "--output-format", "stream-json", "--verbose",
           "--mcp-config", os.environ["SB_LADDER_MCP"], "--strict-mcp-config",
           "--setting-sources", "local", "--permission-mode", "default",
           "--allowedTools", *allowed,
           "--disallowedTools", *harness_deny, *EXTRA_DENY]
    with open(os.path.join(logs, run_id + ".cmd.json"), "w", encoding="utf-8") as f:
        json.dump({"argv": cmd[1:], "cwd": cwd, "cap_s": cap, "prompt": prompt}, f, indent=1)

    transcript = os.path.join(logs, run_id + ".jsonl")
    with open(transcript, "wb") as out:
        p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=out, stderr=subprocess.PIPE,
                             cwd=cwd, env=env)
        try:
            _, err = p.communicate(prompt.encode("utf-8"), timeout=cap)
            code = p.returncode
        except subprocess.TimeoutExpired:
            subprocess.run(["taskkill", "/F", "/T", "/PID", str(p.pid)],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            _, err = p.communicate()
            code = 124
    with open(os.path.join(logs, run_id + ".stderr.txt"), "wb") as f:
        f.write(err or b"")

    answer = ""
    with open(transcript, encoding="utf-8", errors="replace") as f:
        for line in f:
            try:
                ev = json.loads(line)
            except ValueError:
                continue
            if ev.get("type") == "result":
                answer = ev.get("result") or ""
    if code == 124:
        answer += f"\n\n[ladder: session killed at the {cap}s cap]\n"
    if "-p" in args:
        # harnessd keeps the session's stdout as files/answer.md (scrubbed, D-124).
        sys.stdout.buffer.write(answer.encode("utf-8"))
        sys.stdout.flush()
        return code
    files = os.path.join(run_dir, "files")
    os.makedirs(files, exist_ok=True)
    if answer.strip():
        with open(os.path.join(files, "answer.md"), "w", encoding="utf-8") as f:
            f.write(answer)
    return code


if __name__ == "__main__":
    sys.exit(main())
