# 11 · Pivot: BrowserOS as the browser (supersedes docs/05, 07, 06 where they differ)

Decision D-98 (2026-10-27): the product ships with **BrowserOS MCP** as its browser tool server.
The custom browser (sb-cdp, sb-driver, sb-act, injected.js, sb-mcp, sb-media) is frozen as v2 and
kept in the repo; it may replace BrowserOS later if the benchmark shows it wins. Safety is
deferred (D-95, D-96): gate allows everything except Challenge, which stays stop.

## System

```
Claude Code (only model, no API keys)
  │ MCP
  ├─ harness-web    srv-web     search · fetch · api · feed · read · find · site tools (phase 1)
  ├─ harness-files  srv-files   fs · shell · pdf/xlsx/docx/pptx · csv.clean · diff · history.search
  ├─ harness-cli    srv-cli     gh · dns · cloudflare · flags · allowlisted cli
  ├─ harness-mem    harness mcp memory recall/remember/link · contacts · reviews · schedules · watch
  ├─ browseros      BrowserOS   your real browser: navigate, read, click, type, tabs, screenshot
  └─ harness-forge  NEW         tools.list/run/save for tools/<site>/ recipes over BrowserOS
        │
  core (merged): gate (lenient) · vault · ledger · run folders · scheduler/harnessd · reflect
  core-pg: Postgres 16 + pgvector (memory, ledger, runs, tool health, gate rules)
  hooks + swarm: unchanged
```

BrowserOS calls are not routed through core::serve (it is an external server), so they are not
in the ledger. A PostToolUse hook (`scripts/hooks/browseros_ledger.py`, matcher `mcp__browseros__.*`)
appends one line per BrowserOS call with tool, args (scrubbed), and a 300-char result summary to
`runs/<run>/browseros.jsonl` (not ledger.jsonl, which mirrors the ledger table row for row, D-117), so
report.md and tools.save still see browser actions (D-98).

## What changes per area

| Area | Change |
|---|---|
| Browser | BrowserOS MCP added to `.mcp.json`; nothing to build. Profile = your normal BrowserOS profile (real logins, home IP). |
| Tool store (T-11, merged) | Adds a third tool kind, `recipe`: a JSON/YAML list of BrowserOS tool calls with `{arg}` placeholders and a `@match`. Readers/planners stay for fetched pages (phase 1). |
| Toolforge (T-28, new shape) | `harness-forge` server: `tools.list(domain)`, `tools.run(domain, name, args)` (returns the recipe steps for the model to execute through BrowserOS, in order), `tools.save(domain, name, steps)` (writes tools/<domain>/<name>.recipe.yaml from the session's successful BrowserOS calls, read back from browseros.jsonl). Reflect retires recipes that fail 3 times. |
| Mail + calendar (T-27, new shape) | No custom tools. Recipes in tools/mail.google.com and tools/calendar.google.com (inbox, thread, draft, send, events, create) seeded by hand, run through BrowserOS. |
| Benchmark (T-26) | Same 30 web tasks + hard set, run with harness + BrowserOS vs browser-use. Record success, steps, seconds, tokens. |
| Phase 2 ladder (T-29) | Q-2 rungs run with BrowserOS in place of sb-mcp. |
| Frozen (v2) | T-19..T-25, PRs #6 #10 #11 #12 #22 stay open, labelled v2. |

## Done means

1. `.mcp.json` has all six servers and Claude Code lists their tools.
2. SMOKE.md: research + xlsx · register on a Luma event via BrowserOS · read latest 5 Gmail threads · add a calendar event · schedule a daily watch that fires · save a recipe on one site and reuse it in a fresh session (zero forging the second time).
3. bench/results/bench-browseros-<date>.md and phase2-<date>.md written.
