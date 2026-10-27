# Wave 3 brief (BrowserOS pivot): rules, recipe format, tasks

Overrides your launch brief, WAVE2.md and docs/05, 06, 07 where they differ. Sources: docs/11-BROWSEROS-PIVOT.md and decisions D-95..D-99. Merge rule D-87 (review with no Blocking + `cargo test --workspace` green), two review rounds max (D-89), one PR per task on `swarm/<area>-T<nn>` (D-91), interfaces frozen (D-90) except what this brief defines. Run the `pr-ready` skill before review and before the PR. Temp and builds stay on F: (your launcher sets TMP, CARGO_BUILD_JOBS=2, line-tables-only debug).

## Browser: BrowserOS

`.mcp.json` registers `browseros` (BrowserOS neo 0.0.69, `http://127.0.0.1:9010/mcp`, the user's real profile and logins). Its tools, as Claude Code names them `mcp__browseros__<tool>`: `tabs`, `tab_groups`, `history`, `navigate`, `snapshot` (accessibility tree with refs), `diff`, `act` (click/type/... on a ref from the last snapshot), `download`, `upload`, `read` (markdown/text/links), `grep`, `screenshot`, `pdf`, `wait`, `windows`, `evaluate`, `run`, `name_session`, `save_skill`, `mark_skill_run`, `request_human_help`, `await_human_help`. Refs change between snapshots, so nothing may store a raw ref. Every BrowserOS call is recorded by the PostToolUse hook in `runs/<SB_RUN_ID>/browseros.jsonl` (or `.claude/state/browseros.jsonl` outside a run), one LedgerEvent-shaped JSON line per call (D-98; see scripts/hooks/browseros_ledger.py).

## Recipe format (tool kind `recipe`, defined here; core implements it, servers writes recipes)

File: `tools/<domain>/<name>.recipe.yaml`.

```yaml
name: inbox                      # [a-z0-9_]+, equals the file stem
domain: mail.google.com          # registrable host, same folder rules as other tools (toolstore::domain)
kind: recipe
description: "List the latest N inbox threads: sender, subject, date, snippet"
match: ["/mail/*"]               # @match path globs, same validation as other tools (no scheme, host, //, ..)
args:
  count: {type: int, default: 5}
  to: {type: string, required: true}
steps:                           # executed in order by the model through BrowserOS
  - tool: navigate
    args: {url: "https://mail.google.com/mail/u/0/#inbox"}
  - tool: wait
    args: {for: "selector", value: "[role=main]"}
  - tool: act
    target: {role: button, name: "Compose"}   # resolved to a ref from a fresh snapshot/grep at run time
    args: {kind: click}
  - tool: act
    target: {role: textbox, name: "To recipients"}
    args: {kind: type, text: "{to}"}           # {arg} placeholders are filled by tools.run
  - tool: read
    args: {format: markdown, selector: "[role=main]"}
    returns: threads                          # optional: name of the value this step yields
expects: {text: "Message sent"}               # optional success check after the last step
```

Rules: placeholders are `{name}` for declared args only; `@vault(key)` may appear only in `act` type text and is resolved by the model's harness, never written to a recipe file with a value; a recipe never contains a raw ref, a password, or a fixed sleep (`wait` instead).

## Tasks

- **core** (one session):
  1. **T-17b**: interactive `harness run` sessions get the same run preamble as `-p`/harnessd sessions, including the run id and the absolute `runs/<id>/files/` path, and file-writing tools default relative paths to that folder (fixes T-17's deliverables-outside-the-run failures, also T-17c).
  2. **T-28 (new shape)**: (a) toolstore gains the `recipe` kind: parse and validate the format above (header-safe names, @match rules, placeholder check against `args`, no refs or secrets), load it beside readers/planners, health counters as for other tools. (b) new MCP server `harness-forge` (binary in the harness crate, served through `harness_core::serve` so its calls are in the ledger): `tools.list(domain)`, `tools.run(domain, name, args)` (returns the filled steps for the model to execute in order through BrowserOS; records a run of the tool), `tools.save(domain, name, steps, description?, args?)` (writes the recipe from the session's successful BrowserOS calls, read back from browseros.jsonl, with refs replaced by `target` locators from the matching snapshot/grep; git-commits it with a task id as toolstore requires). (c) report.md includes the run's browseros.jsonl lines in order. (d) reflect retires a recipe after 3 failed runs (existing lifecycle rules). (e) the `-p` launcher's `--allowedTools` and `--strict-mcp-config` sets (D-144) include `browseros` and `harness-forge` for normal runs; the reflect replay session stays harness-web only. Branch per task: `swarm/core-T17b`, `swarm/core-T28`.
- **servers** (one session), **T-27 (new shape)**: hand-written recipes, tested live through BrowserOS in the user's real browser:
  - `tools/mail.google.com/`: `inbox` (latest N threads), `thread` (open one by subject or index, return its messages), `draft` (compose and save a draft), `send` (compose and send).
  - `tools/calendar.google.com/`: `events` (upcoming N events), `create` (title, start, end, optional description).
  - `tools/lu.ma/`: `register` (register for an event by URL with the user's saved name and email).
  - Write them against the format above; until core's recipe kind merges, validate them with a small test that parses the YAML and checks the rules listed above, then switch to toolstore's parser after T-28 merges. One branch: `swarm/servers-T27`.

## Live-test limits (the gate is lenient now, D-96, so these are yours to hold)

- Mail: `send` tests send only to the logged-in account's own address, subject starting `[sbtest]`. Drafts are left as drafts. Never reply to or forward someone else's thread in a test.
- Calendar: test events only on the user's primary calendar, titled `[sbtest] ...`, in the next 7 days; delete each test event you created after the test passes.
- Luma: register only for a **free** public event; record its URL in the PR. Never complete a payment anywhere: a checkout, ticket purchase or "Pay" button is a stop for you even though the gate allows Pay.
- A captcha, login wall or 2FA prompt: stop and say so (Challenge stays stop, D-5); do not use `request_human_help` to get around a challenge, report it instead.
- No test deletes or archives the user's own mail, events or data except the `[sbtest]` items it created.
