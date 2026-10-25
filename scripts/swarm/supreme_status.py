"""One-shot supreme check-in: everything /status and /wave need, read from the swarm/<area> branches.

  python scripts/swarm/supreme_status.py            # table + action list
  python scripts/swarm/supreme_status.py --json     # same data as JSON

Per area: branch head, ahead/behind main, minutes since its last commit, status on its branch,
merged on main, review/accept verdicts for its tasks, live session, uncommitted paths in its
worktree, and blockers.yaml damage (duplicate top-level keys hide entries from yaml.safe_load).
Interface changes are scanned from the raw text, so entries hidden under a duplicate key are still
found; one counts as decided when main's decisions.yaml mentions its id. Read-only: never edits.
"""
import json
import re
import subprocess
import sys
import time
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
WT_ROOT = Path(ROOT.drive + "/sb-swarm/wt") if ROOT.drive else ROOT.parent / "sb-swarm" / "wt"


def git(*args):
    r = subprocess.run(["git", "-C", str(ROOT), *args], capture_output=True, text=True, encoding="utf-8")
    return r.stdout if r.returncode == 0 else None


def show(ref, path):
    return git("show", f"{ref}:{path}")


def last_verdict(text):
    found = re.findall(r"^\W*Verdict:\s*(.+?)\s*$", text or "", re.M | re.I)
    return found[-1].strip().lower().strip("*_` .") if found else None


def raw_interface_changes(text):
    """(id, section) for every entry, by scanning lines, so a duplicate key cannot hide one."""
    out, section = [], None
    for line in (text or "").splitlines():
        m = re.match(r"^([a-z_]+):", line)
        if m:
            section = m.group(1)
            continue
        m = re.match(r"^  - id:\s*(\S+)", line)
        if m:
            out.append([m.group(1), section])
    # an entry is an interface change if it carries an `interface:` field, whatever section it sits in
    ids_with_iface = set(re.findall(r"^  - id:\s*(\S+)\n(?:    .*\n)*?    interface:", text or "", re.M))
    return [i for i, _ in out if i in ids_with_iface]


def live_sessions():
    if sys.platform != "win32":
        return {}
    ps = ("Get-CimInstance Win32_Process -Filter \"Name='claude.exe'\" | ForEach-Object { "
          "$_.ProcessId.ToString() + ' ' + [int]((Get-Date) - $_.CreationDate).TotalMinutes + ' ' + $_.CommandLine }")
    r = subprocess.run(["powershell", "-NoProfile", "-Command", ps], capture_output=True, text=True)
    alive = {}
    for line in r.stdout.splitlines():
        m = re.search(r"suborch/(\w+)\.md", line)
        if m:
            pid, up = line.split(" ", 2)[:2]
            alive[m.group(1)] = (pid, int(up))
    return alive


def main(argv):
    swarm = yaml.safe_load((ROOT / "agent-memory/swarm.yaml").read_text(encoding="utf-8"))
    decisions_text = (ROOT / "agent-memory/decisions.yaml").read_text(encoding="utf-8")
    alive = live_sessions()
    now = time.time()
    rows, actions = [], []
    for area, a in swarm["areas"].items():
        ref = f"swarm/{area}"
        if git("rev-parse", "--verify", "--quiet", ref) is None:
            continue
        head = git("rev-parse", "--short", ref).strip()
        ahead = int(git("rev-list", "--count", f"main..{ref}"))
        behind = int(git("rev-list", "--count", f"{ref}..main"))
        age = int((now - int(git("log", "-1", "--format=%ct", ref))) / 60)
        b_swarm = yaml.safe_load(show(ref, "agent-memory/swarm.yaml") or "{}") or {}
        status = ((b_swarm.get("areas") or {}).get(area) or {}).get("status")
        merged = bool(a.get("merged"))
        tasks = a.get("tasks", [])
        b_tasks = {t["id"]: t for t in (yaml.safe_load(show(ref, "agent-memory/tasks.yaml") or "{}") or {}).get("tasks", [])}
        reports = {}
        for t in tasks:
            rv = last_verdict(show(ref, f"bench/results/review-{t}.md"))
            ac = last_verdict(show(ref, f"bench/results/accept-{t}.md"))
            reports[t] = {"status": b_tasks.get(t, {}).get("status"), "review": rv, "accept": ac}
        ready = bool(tasks) and all(r["status"] == "done" and r["review"] == "approve" and r["accept"] == "green"
                                    for r in reports.values())
        blk = show(ref, "agent-memory/blockers.yaml") or ""
        top = re.findall(r"^([a-z_]+):", blk, re.M)
        dup_keys = sorted({k for k in top if top.count(k) > 1})
        undecided = [i for i in raw_interface_changes(blk) if i not in decisions_text]
        wt = WT_ROOT / area
        dirty = None
        if wt.exists():
            r = subprocess.run(["git", "-C", str(wt), "status", "--porcelain"], capture_output=True, text=True)
            dirty = len([l for l in r.stdout.splitlines() if l.strip()])
        row = {"area": area, "head": head, "ahead": ahead, "behind": behind, "last_commit_min": age,
               "status": status, "merged": merged, "ready": ready, "reports": reports,
               "session": (alive.get(area) or (None,))[0], "session_min": (alive.get(area) or (None, None))[1], "dirty": dirty, "dup_keys": dup_keys, "undecided": undecided}
        rows.append(row)
        if undecided:
            actions.append(f"{area}: decide interface changes {', '.join(undecided)}")
        if dup_keys:
            actions.append(f"{area}: blockers.yaml has duplicate keys {dup_keys} (D-75 repair before merge)")
        if not merged and (status == "pr_open" or ready) and ahead:
            actions.append(f"{area}: PR ready -> supreme-merge (status {status}, ready={ready})")
        if not merged and not alive.get(area) and ahead:
            actions.append(f"{area}: no live session (last commit {age} min ago)")
        elif not merged and alive.get(area) and min(age, alive[area][1]) > 90:
            actions.append(f"{area}: session up {alive[area][1]} min with no commit in that time (stuck on a prompt?)")
        if merged and ahead:
            actions.append(f"{area}: merged, {ahead} follow-up commit(s) on its branch (merge again when it reports the follow-up done)")
    waves = swarm.get("waves", {})
    merged_set = {r["area"] for r in rows if r["merged"]} | {k for k, v in swarm["areas"].items() if v.get("merged")}
    for area, a in swarm["areas"].items():
        need = a.get("needs_merged")
        if need and not a.get("merged") and all(n in merged_set for n in need) and area not in {r["area"] for r in rows}:
            actions.append(f"{area}: unblocked (needs {need} all merged) -> ask the user to launch")
    main_dirty = git("status", "--porcelain", "--untracked-files=no") or ""
    merge_head = (ROOT / ".git" / "MERGE_HEAD").exists()
    if merge_head:
        actions.insert(0, "main: MERGE_HEAD present (interrupted merge): git merge --abort, then redo")
    if main_dirty.strip():
        actions.insert(0, "main: uncommitted tracked changes")

    if "--json" in argv:
        print(json.dumps({"rows": rows, "actions": actions, "waves": waves}, indent=1))
        return 0
    print(f"main {git('rev-parse', '--short', 'main').strip()}  waves {waves}")
    print(f"{'area':7} {'head':8} {'+ahd':>5} {'-bhd':>5} {'age':>5} {'status':12} {'mrg':3} {'rdy':3} {'sess':6} {'dirty':>5}  reports")
    for r in rows:
        rep = " ".join(f"{t}:{(v['status'] or '-')[:4]}/{(v['review'] or '-')[:7]}/{(v['accept'] or '-')[:5]}"
                       for t, v in r["reports"].items())
        print(f"{r['area']:7} {r['head']:8} {r['ahead']:5} {r['behind']:5} {r['last_commit_min']:4}m "
              f"{str(r['status']):12} {'y' if r['merged'] else 'n':3} {'y' if r['ready'] else 'n':3} "
              f"{(r['session'] or '-'):6} {str(r['dirty']):>5}  {rep}")
    print("actions:" if actions else "actions: none")
    for x in actions:
        print("  - " + x)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
