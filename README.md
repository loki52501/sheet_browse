# sheetbrowseV2_

An agent harness for Claude Code, with a browser inside it.

Claude Code is the only model. Everything it can do to the world goes through one
core (gate, vault, ledger, memory, scheduler) and out through tool servers:
web, files and documents, CLIs, and in Phase 2 a Rust browser (sheetbrowse v2)
that reads pages as short numbered text and acts on them. Mail and calendar are
reached through the browser, not through APIs or connectors.

Everything the model sees is text. Everything it does is logged. Every forged
tool is saved under the site it was forged for and maintains itself.

## Read in this order

| File | What it settles |
|---|---|
| `CLAUDE.md` | Rules every coding agent must follow in this repo |
| `docs/00-OVERVIEW.md` | The system in one page, the two phases, the vocabulary |
| `docs/01-HARNESS-CORE.md` | Gate (lenient), vault, ledger, run folder, scheduler |
| `docs/02-MEMORY.md` | `MemoryStore` trait, Postgres schema, swap targets |
| `docs/03-TOOL-STORE.md` | `tools/<site>/`, tool file format, lifecycle, maintenance loop |
| `docs/04-TOOL-SERVERS-PHASE1.md` | web (search, fetch, api, feed, read renderer), files + documents, CLIs |
| `docs/05-BROWSER.md` | sheetbrowse v2: crates, 11 components, read modes, find scoring, act, settle, media, transports |
| `docs/06-TOOLFORGE.md` | The decision ladder and the forge pipeline |
| `docs/07-MAIL-CALENDAR-VIA-BROWSER.md` | Gmail and Google Calendar as site tools over the browser |
| `docs/08-USE-CASES-AND-TESTS.md` | The 30 use cases by phase, the two test ladders, pass criteria |
| `docs/09-BUILD-ORDER.md` | Six steps, each with acceptance tests |
| `docs/10-SWARM.md` | Topology (supreme → sub-orchestrator sessions → subagents), waves, protocol, hooks |
| `agent-memory/` | YAML the coding agents read and write: tasks, decisions, blockers, interfaces, swarm, handoffs |
| `claude-config/` | Source of `.claude/`: `settings.json` hooks, `agents/` subagents, `commands/` slash commands. Install with `scripts\swarm\install-claude-config.ps1` (the remote bridge cannot write `.claude/` directly) |
| `scripts/hooks/` | The hook scripts (Python 3, stdlib) |
| `scripts/swarm/` | `launch.ps1` opens one terminal per area in its own worktree; `merge.ps1` gates merges |
| `prompts/SUPREME.md` | Paste into Claude Code at the repo root to start (wave 0, then coordination) |
| `prompts/suborch/<area>.md` | Launched automatically by `launch.ps1` in each worktree |
| `prompts/START.md` | Single-session alternative if you don't want the swarm |

## Fast path

```powershell
cd "F:\aside_mcp for claude\sheetbrowseV2_"
scripts\swarm\install-claude-config.ps1                  # creates .claude/ from claude-config/
git init; git add .; git commit -m "specs"
claude "$(Get-Content prompts\SUPREME.md -Raw)"        # wave 0: contracts + stubs, ~1 h, then it stops
scripts\swarm\launch.ps1 -Areas core,sheet,store,page    # wave 1: four sub-orchestrators in four tabs
# ... supreme merges as PRs go green, then:
scripts\swarm\launch.ps1 -Areas servers,driver           # wave 2
```

## Layout (target, after step 6)

```
sheetbrowseV2_/
  Cargo.toml                 workspace
  crates/
    core/                    gate, vault, ledger, run folder, scheduler, MemoryStore trait
    core-pg/                 Postgres backend for MemoryStore
    toolstore/               tools/<site>/ loader, headers, fingerprints, lifecycle
    srv-web/                 search, fetch, api, feed, read renderer      (MCP server)
    srv-files/               fs, shell, documents, csv, diff               (MCP server)
    srv-cli/                 gh, dig, provider APIs                        (MCP server)
    sb-cdp/                  CDP transport trait: pipe, websocket, napi, extension
    sb-driver/               registry, act+retry, input, consequences, settle, scheduler, recovery
    sb-sheet/                skeleton, find, sheet, read renderer (shared with srv-web)
    sb-act/                  act DSL parser, gate classification
    sb-media/                video frames, screenshots
    sb-mcp/                  the browser MCP server
    toolforge/               ladder, forge pipeline, reflect
  injected/
    injected.js              walk, ids, names, find, act checks, settle, text, fingerprint
    media.js
  tools/                     the site-scoped tool store (git-tracked)
    _generic/  _archive/
  runs/                      one folder per run (git-ignored)
  migrations/                Postgres
  docs/  agent-memory/  prompts/  bench/
```

## Non-negotiables

See `CLAUDE.md`. In short: Claude Code only, no API keys, vault values never leave
the tool that uses them, a captcha means stop, no stealth, everything through the
gate, and the model never receives raw HTML or pixels unless it asks for a
screenshot or a video frame.
