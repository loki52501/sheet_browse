"""Merge gate (S-8), run by merge.ps1 on the merged-but-uncommitted tree.

  python scripts/swarm/merge_gate.py <area> [--worktree <path>]

Passes (exit 0) only when, for every task the area owns this wave (agent-memory/swarm.yaml
areas.<area>.tasks, plus later.tasks once that wave is open):
  * tasks.yaml says status done;
  * bench/results/review-<task>.md has a Blocking section that is empty or `- none`, and its
    last `Verdict:` line is `Verdict: approve`;
  * bench/results/accept-<task>.md has every table result cell `pass` and ends with a
    `Verdict: green` line;
and agent-memory/decisions.yaml + blockers.yaml load without duplicate keys or ids (the
union merge driver can produce both), every blockers.yaml entry sits under the section its
fields belong to (a merge can move entries between sections, D-75), and the area's worktree has no leak log
(.claude/state/leaks.log, written by the leak hooks).
"""
import re
import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
NONE_RE = re.compile(r"^\s*(-\s*)?(none|nothing|n/a)\.?\s*$", re.I)


class StrictLoader(yaml.SafeLoader):
    pass


def _no_dup_mapping(loader, node, deep=False):
    keys = set()
    for k, _ in node.value:
        key = loader.construct_object(k, deep=deep)
        if key in keys:
            raise yaml.constructor.ConstructorError(None, None, f"duplicate key {key!r}", k.start_mark)
        keys.add(key)
    return loader.construct_mapping(node, deep)


StrictLoader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, _no_dup_mapping)


def load(path):
    return yaml.load(path.read_text(encoding="utf-8"), Loader=StrictLoader)


def sections(text, title):
    """Every `## <title>` section body, heading-line remainder included, in file order."""
    return [m.group(1) + m.group(2) for m in re.finditer(
        rf"^##\s*{title}\b(.*?)$(.*?)(?=^##\s|\Z)", text, re.M | re.S | re.I)]


def last_verdict(text):
    found = re.findall(r"^\W*Verdict:\s*(.+?)\s*$", text, re.M | re.I)
    return found[-1].strip().lower().strip("*_` .") if found else None


def check_review(path):
    if not path.exists():
        return f"missing {path.relative_to(ROOT)}"
    text = path.read_text(encoding="utf-8")
    blocking = sections(text, "Blocking")
    if not blocking:
        return f"{path.name}: no '## Blocking' section"
    lines = [l for l in blocking[-1].splitlines() if l.strip()]
    if lines and not all(NONE_RE.match(l) for l in lines):
        return f"{path.name}: Blocking items present"
    if last_verdict(text) != "approve":
        return f"{path.name}: last Verdict line is not 'Verdict: approve'"
    return None


def check_accept(path):
    if not path.exists():
        return f"missing {path.relative_to(ROOT)}"
    text = path.read_text(encoding="utf-8")
    rows = [l for l in text.splitlines() if l.strip().startswith("|")]
    if len(rows) < 3:
        return f"{path.name}: no result table"
    header = [c.strip().lower() for c in rows[0].strip().strip("|").split("|")]
    if "result" not in header:
        return f"{path.name}: table has no 'result' column"
    col = header.index("result")
    for r in rows[2:]:
        cells = [c.strip().lower() for c in r.strip().strip("|").split("|")]
        if col >= len(cells) or cells[col] != "pass":
            return f"{path.name}: row not pass: {r.strip()[:120]}"
    if last_verdict(text) != "green":
        return f"{path.name}: last Verdict line is not 'Verdict: green'"
    return None


def area_tasks(swarm, area):
    a = swarm["areas"][area]
    tasks = list(a.get("tasks", []))
    later = a.get("later") or {}
    if later and str(swarm.get("waves", {}).get(str(later.get("wave")), "")).startswith("open"):
        tasks += later.get("tasks", [])
    return tasks


BLOCKER_SECTIONS = {"blockers": ("question", "default_taken"), "interface_changes": ("interface", "change")}


def check_blockers_shape(doc):
    """Entries filed under the wrong section (a merge can move them) and duplicate ids."""
    problems, ids = [], []
    for section, keys in BLOCKER_SECTIONS.items():
        for item in (doc or {}).get(section) or []:
            ids.append(item.get("id"))
            missing = [k for k in keys if k not in item]
            if missing:
                problems.append(f"blockers.yaml: {item.get('id')} under {section} lacks {', '.join(missing)} (misfiled?)")
    dups = sorted({i for i in ids if ids.count(i) > 1})
    if dups:
        problems.append(f"blockers.yaml: duplicate ids {dups}")
    return problems


def main(argv):
    area = argv[1]
    worktree = Path(argv[argv.index("--worktree") + 1]) if "--worktree" in argv else None
    problems = []
    try:
        for name in ("decisions.yaml", "blockers.yaml", "tasks.yaml", "swarm.yaml"):
            load(ROOT / "agent-memory" / name)
        dec = load(ROOT / "agent-memory" / "decisions.yaml")["decisions"]
        ids = [d["id"] for d in dec]
        dups = sorted({i for i in ids if ids.count(i) > 1})
        if dups:
            problems.append(f"decisions.yaml: duplicate ids {dups}")
        problems += check_blockers_shape(load(ROOT / "agent-memory" / "blockers.yaml"))
    except yaml.YAMLError as e:
        problems.append(f"agent-memory YAML after merge: {e}")
        print("gate BLOCKED:\n  " + "\n  ".join(problems))
        return 1

    swarm = load(ROOT / "agent-memory" / "swarm.yaml")
    tasks = {t["id"]: t for t in load(ROOT / "agent-memory" / "tasks.yaml")["tasks"]}
    mine = area_tasks(swarm, area)
    results = ROOT / "bench" / "results"
    for tid in mine:
        status = tasks.get(tid, {}).get("status")
        if status != "done":
            problems.append(f"{tid}: status {status}, not done")
        for p in (check_review(results / f"review-{tid}.md"), check_accept(results / f"accept-{tid}.md")):
            if p:
                problems.append(f"{tid}: {p}")
    if worktree is not None:
        leaks = worktree / ".claude" / "state" / "leaks.log"
        if leaks.exists() and leaks.read_text(encoding="utf-8", errors="replace").strip():
            problems.append(f"leak hook fired in {worktree} (see {leaks})")
    if problems:
        print("gate BLOCKED:\n  " + "\n  ".join(problems))
        return 1
    print(f"gate ok for {area}: {' '.join(mine)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
