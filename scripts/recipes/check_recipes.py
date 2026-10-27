"""Static check for BrowserOS recipes, tools/<domain>/<name>.recipe.yaml (T-27).

  python scripts/recipes/check_recipes.py [path ...]     # default: every recipe under tools/

The format is defined in prompts/suborch/WAVE3.md (docs/11). Core's toolstore parser (T-28)
replaces this check once it merges; until then this is the gate for hand-written recipes.
Exit 0 when every recipe passes, 1 with one `path: problem` line per violation otherwise.

Rules (WAVE3 "Rules" plus what the format implies):
  * name is [a-z0-9_]+ and equals the file stem; domain is a valid site folder and equals the
    folder the file is in; kind is `recipe`; description is a non-empty string.
  * match: non-empty list of path globs, same rules as toolstore's @match (no scheme, no //,
    no .., no backslash, no encoded dot/slash/backslash, starts with /).
  * args: name -> {type: string|int|bool, default?, required?}; a default has the arg's type;
    required and default are not both given.
  * steps: BrowserOS tools only; `target` only on act, as {role, name?, nth?}; act kinds that
    address an element have a target, press has a key and no target.
  * placeholders are {name} for declared args only, and every declared arg is used.
  * no raw ref (`ref`/`targetRef` keys, `e12` or `[ref=e12]` values), no fixed sleep (wait is
    for text or selector, never time), no password or key-shaped string; @vault(key) only in
    act `text`/`value`.
  * navigate URLs are https on the recipe's domain, a subdomain of it, or a `_site.yaml`
    alias, with no placeholder before the path.
"""
import re
import sys
from pathlib import Path
from urllib.parse import urlsplit

import yaml

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts" / "hooks"))
from _common import KEY_RE  # noqa: E402  (the hooks' key patterns: one list for the repo)

SUFFIX = ".recipe.yaml"
TOP_KEYS = {"name", "domain", "kind", "description", "match", "args", "steps", "expects"}
ARG_KEYS = {"type", "default", "required"}
ARG_TYPES = {"string": str, "int": int, "bool": bool}
STEP_KEYS = {"tool", "args", "target", "returns"}
TARGET_KEYS = {"role", "name", "nth"}
EXPECT_KEYS = {"text", "selector"}
# BrowserOS neo 0.0.69 tools (WAVE3 "Browser"), minus the ones a recipe has no use for and
# that would hand control away: run/evaluate (arbitrary script), save_skill/mark_skill_run,
# request_human_help/await_human_help (a challenge is a stop, D-5), name_session.
TOOLS = {"tabs", "tab_groups", "history", "navigate", "snapshot", "diff", "act", "download",
         "upload", "read", "grep", "screenshot", "pdf", "wait", "windows"}
TARGETED = {"click", "fill", "type", "hover", "focus", "check", "uncheck", "select", "drag"}
UNTARGETED = {"press", "scroll"}
NAME_RE = re.compile(r"^[a-z0-9_]+$")
DOMAIN_RE = re.compile(r"^[a-z0-9][a-z0-9.-]*[a-z0-9]$")
PLACEHOLDER = re.compile(r"\{([^{}]*)\}")
RAW_REF = re.compile(r"^e\d+$|\[ref=e\d+\]")
VAULT = re.compile(r"@vault\(([^()]*)\)")
VAULT_KEY = re.compile(r"^[A-Za-z0-9_.-]+$")
SECRET_WORD = re.compile(r"passw(or)?d|passcode|\bpin\b|\botp\b|cvv|cvc|card.?number", re.I)


def match_violation(p):
    """toolstore::header::match_violation, ported (G-4, D-307)."""
    if not isinstance(p, str):
        return "is not a string"
    if "://" in p:
        return "has a scheme"
    if "//" in p:
        return "contains //"
    if ".." in p.split("/"):
        return "contains .."
    if "\\" in p:
        return "contains a backslash"
    if any(e in p.lower() for e in ("%2e", "%2f", "%5c")):
        return "contains a percent-encoded dot, slash or backslash"
    if not p.startswith("/"):
        return "does not start with / (a host or relative pattern)"
    return None


def site_aliases(folder):
    """aliases from tools/<folder>/_site.yaml (toolstore::domain::SiteConfig)."""
    f = folder / "_site.yaml"
    if not f.exists():
        return []
    doc = yaml.safe_load(f.read_text(encoding="utf-8")) or {}
    return [str(a).lower() for a in doc.get("aliases") or []]


def strings(v, path=()):
    """Every string in v with the key path that leads to it."""
    if isinstance(v, str):
        yield path, v
    elif isinstance(v, dict):
        for k, x in v.items():
            yield from strings(x, path + (str(k),))
    elif isinstance(v, list):
        for i, x in enumerate(v):
            yield from strings(x, path + (str(i),))


def keys(v, path=()):
    """Every mapping key in v with its path."""
    if isinstance(v, dict):
        for k, x in v.items():
            yield path, k
            yield from keys(x, path + (str(k),))
    elif isinstance(v, list):
        for i, x in enumerate(v):
            yield from keys(x, path + (str(i),))


def check_url(url, domain, aliases):
    if not isinstance(url, str):
        return "navigate url is not a string"
    head = url.split("/", 3)
    if len(head) < 3 or head[1] != "":
        return f"navigate url {url!r} is not an absolute https URL"
    if PLACEHOLDER.search("/".join(head[:3])):
        return f"navigate url {url!r} has a placeholder before the path"
    parts = urlsplit(url)
    if parts.scheme != "https":
        return f"navigate url {url!r} is not https"
    host = (parts.hostname or "").lower()
    if parts.username or parts.password or parts.port:
        return f"navigate url {url!r} carries credentials or a port"
    if not any(host == d or host.endswith("." + d) for d in [domain, *aliases]):
        return f"navigate url {url!r} leaves {domain}"
    return None


def check_step(i, step, domain, aliases):
    out = []
    where = f"steps[{i}]"
    if not isinstance(step, dict):
        return [f"{where} is not a mapping"]
    for k in step.keys() - STEP_KEYS:
        out.append(f"{where} has unknown key {k!r}")
    tool = step.get("tool")
    if tool not in TOOLS:
        out.append(f"{where} tool {tool!r} is not a BrowserOS recipe tool")
    args = step.get("args", {})
    if not isinstance(args, dict):
        return out + [f"{where} args is not a mapping"]
    target = step.get("target")
    if target is not None:
        if tool != "act":
            out.append(f"{where} has a target but tool is {tool!r}")
        if not isinstance(target, dict):
            out.append(f"{where} target is not a mapping")
        else:
            for k in target.keys() - TARGET_KEYS:
                out.append(f"{where} target has unknown key {k!r}")
            if not isinstance(target.get("role"), str) or not target.get("role"):
                out.append(f"{where} target has no role")
    if tool == "act":
        kind = args.get("kind")
        if kind in TARGETED and target is None:
            out.append(f"{where} act {kind} has no target")
        elif kind in UNTARGETED and target is not None:
            out.append(f"{where} act {kind} takes no target")
        elif kind not in TARGETED | UNTARGETED:
            out.append(f"{where} act kind {kind!r} is not allowed in a recipe")
        if kind == "press" and not args.get("key"):
            out.append(f"{where} act press has no key")
    if tool == "wait" and args.get("for") not in ("text", "selector"):
        out.append(f"{where} wait is for {args.get('for')!r}: a fixed sleep (wait for text or selector)")
    if tool == "navigate":
        why = check_url(args.get("url"), domain, aliases)
        if why:
            out.append(f"{where} {why}")
    if "returns" in step and not (isinstance(step["returns"], str) and NAME_RE.match(step["returns"])):
        out.append(f"{where} returns {step.get('returns')!r} is not [a-z0-9_]+")
    return out


def check_doc(doc, path, aliases=()):
    """Problems with one parsed recipe; path is where it lives (tools/<domain>/<file>)."""
    out = []
    if not isinstance(doc, dict):
        return ["is not a mapping"]
    for k in doc.keys() - TOP_KEYS:
        out.append(f"unknown key {k!r}")
    stem = path.name[: -len(SUFFIX)] if path.name.endswith(SUFFIX) else path.stem
    name, domain = doc.get("name"), doc.get("domain")
    if not (isinstance(name, str) and NAME_RE.match(name) and len(name) <= 100):
        out.append(f"name {name!r} is not [a-z0-9_]+")
    elif name != stem:
        out.append(f"name {name!r} is not the file stem {stem!r}")
    if not (isinstance(domain, str) and DOMAIN_RE.match(domain) and ".." not in domain):
        out.append(f"domain {domain!r} is not a site folder name")
    elif domain != path.parent.name:
        out.append(f"domain {domain!r} is not the folder {path.parent.name!r}")
    if doc.get("kind") != "recipe":
        out.append(f"kind {doc.get('kind')!r} is not 'recipe'")
    if not (isinstance(doc.get("description"), str) and doc["description"].strip()):
        out.append("description is missing")

    match = doc.get("match")
    if not (isinstance(match, list) and match):
        out.append("match is not a non-empty list")
    else:
        for p in match:
            why = match_violation(p)
            if why:
                out.append(f"match {p!r} {why}")

    args = doc.get("args") or {}
    declared = {}
    if not isinstance(args, dict):
        out.append("args is not a mapping")
        args = {}
    for a, spec in args.items():
        if not (isinstance(a, str) and NAME_RE.match(a)):
            out.append(f"arg {a!r} is not [a-z0-9_]+")
            continue
        if not isinstance(spec, dict):
            out.append(f"arg {a} spec is not a mapping")
            continue
        for k in spec.keys() - ARG_KEYS:
            out.append(f"arg {a} has unknown key {k!r}")
        ty = ARG_TYPES.get(spec.get("type"))
        if ty is None:
            out.append(f"arg {a} type {spec.get('type')!r} is not string, int or bool")
        elif "default" in spec and not (
            isinstance(spec["default"], ty) and not (ty is int and isinstance(spec["default"], bool))
        ):
            out.append(f"arg {a} default {spec['default']!r} is not a {spec['type']}")
        if "required" in spec and not isinstance(spec["required"], bool):
            out.append(f"arg {a} required is not true/false")
        if spec.get("required") and "default" in spec:
            out.append(f"arg {a} is required and has a default")
        declared[a] = spec.get("type")

    steps = doc.get("steps")
    if not (isinstance(steps, list) and steps):
        out.append("steps is not a non-empty list")
        steps = []
    for i, step in enumerate(steps):
        out += check_step(i, step, domain or "", aliases)

    expects = doc.get("expects")
    if expects is not None:
        if not isinstance(expects, dict) or not expects or expects.keys() - EXPECT_KEYS:
            out.append(f"expects {expects!r} is not {{text|selector: ...}}")

    used = set()
    body = {"steps": steps, "expects": expects}
    for kpath, s in strings(body):
        for ph in PLACEHOLDER.findall(s):
            if ph not in declared:
                out.append(f"placeholder {{{ph}}} at {'.'.join(kpath)} is not a declared arg")
            used.add(ph)
        if RAW_REF.search(s):
            out.append(f"raw ref {s!r} at {'.'.join(kpath)}")
        for key in VAULT.findall(s):
            in_act_text = (len(kpath) == 4 and kpath[0] == "steps" and kpath[2] == "args"
                           and kpath[3] in ("text", "value")
                           and steps[int(kpath[1])].get("tool") == "act")
            if not in_act_text:
                out.append(f"@vault at {'.'.join(kpath)} (only act text/value may use it)")
            elif not VAULT_KEY.match(key) or VAULT.sub("", s).strip():
                out.append(f"@vault at {'.'.join(kpath)} is not a bare @vault(key)")
    for a in declared:
        if a not in used:
            out.append(f"arg {a} is declared but never used")
    for kpath, k in keys(doc):
        if k in ("ref", "targetRef"):
            out.append(f"raw ref key {k!r} at {'.'.join(kpath) or 'top'}")
        if isinstance(k, str) and SECRET_WORD.search(k):
            out.append(f"secret-looking key {k!r} at {'.'.join(kpath) or 'top'}")
    for kpath, s in strings(doc):
        if KEY_RE.search(s):
            out.append(f"key-shaped string at {'.'.join(kpath)}")
    for i, step in enumerate(steps):
        if not isinstance(step, dict):
            continue
        t, a = step.get("target") or {}, step.get("args") or {}
        if (isinstance(t, dict) and SECRET_WORD.search(str(t.get("name", "")))
                and isinstance(a, dict) and a.get("kind") in ("fill", "type")):
            v = str(a.get("value", a.get("text", "")))
            if not VAULT.fullmatch(v):
                out.append(f"steps[{i}] types into {t.get('name')!r} without @vault(key)")
    return out


def check_file(path):
    path = Path(path)
    if not path.name.endswith(SUFFIX):
        return [f"file name does not end with {SUFFIX}"]
    try:
        doc = yaml.safe_load(path.read_text(encoding="utf-8"))
    except yaml.YAMLError as e:
        return [f"YAML: {e}"]
    return check_doc(doc, path, site_aliases(path.parent))


def recipes(root=ROOT / "tools"):
    return sorted(p for p in Path(root).rglob("*" + SUFFIX) if "_archive" not in p.parts)


def main(argv):
    paths = [Path(a) for a in argv] or recipes()
    bad = 0
    for p in paths:
        for why in check_file(p):
            print(f"{p}: {why}")
            bad += 1
    print(f"{len(paths)} recipe(s), {bad} problem(s)")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
