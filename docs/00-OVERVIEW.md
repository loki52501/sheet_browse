# 00 · Overview

## One sentence

A harness around Claude Code where every action in the world goes through one gate,
one vault and one ledger, memory lives in one database behind a trait, tools forged
for a site are saved under that site and maintain themselves, and the browser is one
tool server among several, built in Rust on CDP, that shows the model short numbered
text instead of HTML.

## The picture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ Claude Code · the only model · skills · agent-memory YAML                     │
└────────────────────────────────────┬────────────────────────────────────────┘
                                     │ MCP
┌────────────────────────────────────▼────────────────────────────────────────┐
│ HARNESS CORE (crate core)                                                    │
│  Gate (lenient)  Vault  Ledger  Run folder  Scheduler + watch + reflect      │
│  MemoryStore trait ──► core-pg: Postgres 16 + pgvector                      │
└───────┬───────────────┬───────────────┬───────────────────┬──────────────────┘
        │               │               │                   │
┌───────▼──────┐ ┌──────▼──────┐ ┌──────▼──────┐   ┌────────▼───────────────────┐
│ srv-web      │ │ srv-files   │ │ srv-cli     │   │ sb-mcp · BROWSER (Phase 2) │
│ search fetch │ │ fs shell    │ │ gh dig      │   │ open find sheet read act   │
│ api feed     │ │ pdf xlsx    │ │ provider    │   │ video screenshot eval cdp  │
│ read()       │ │ docx csv    │ │ APIs        │   │ sb-driver sb-sheet sb-act  │
└───────┬──────┘ └──────┬──────┘ └──────┬──────┘   │ sb-media sb-cdp  toolforge │
        │               │               │          └────────┬───────────────────┘
┌───────▼───────────────▼───────────────▼───────────────────▼──────────────────┐
│ tools/<site>/  site-scoped tool store · git files · health rows in Postgres  │
│   mail.google.com/  calendar.google.com/  luma.com/  ashbyhq.com/  _generic/ │
└──────────────────────────────────────────────────────────────────────────────┘
        │               │               │                   │ CDP
   HTTP, feeds      your disk       CLIs/APIs     ┌─────────▼──────────────────┐
                                                  │ Chromium · persistent      │
                                                  │ profile · injected.js in   │
                                                  │ every frame (world "sb")   │
                                                  └────────────────────────────┘
```

## Two phases

**Phase 1, no browser.** Core, memory, tool store, srv-web, srv-files, srv-cli,
scheduler. Proves the gate, vault, ledger, memory and scheduler on real tasks before any
browser code exists. Covers 22 of the 30 use cases fully or in a browser-free form.

**Phase 2, browser.** sheetbrowse v2 as a tool server. Adds the 8 use cases that need
a real page (forms, uploads, downloads, logged-in sites, flow screenshots) and, because
the user chose it, **mail and calendar through Gmail and Google Calendar in the browser**.

The browser reuses the core unchanged. Its own new work is the Chromium column.

## Vocabulary

| Term | Meaning |
|---|---|
| **sheet** | Numbered text lines describing page elements: `#12 input "Email" empty` |
| **skeleton** | ~100-token outline of a page returned by `open()`: title, landmarks, forms, headings, counts, warnings, site tools |
| **find(q)** | Returns the few elements that match a comma-separated query, scored in-page |
| **read(tab)** | Whole page as clean chunked prose with ids on actionable things |
| **act(program)** | Runs the act DSL: `t#7=$name; s#12=Basic; c#14; w(3s); ?s` |
| **gate class** | Read, Fill, Navigate, Submit, Send, Pay, Account, Delete, Challenge |
| **mode** | What the gate does with a class: allow, ask, stop |
| **vault** | Encrypted secrets and personal data; `@vault(k)` resolved inside tools only |
| **ledger** | Append-only record of every call, decision and consequence |
| **run** | One task execution with its own folder under `runs/` |
| **site tool** | A file in `tools/<domain>/` that is a reader (returns data) or a planner (returns an act program) |
| **reader / planner** | A tool never gets hands: readers read, planners emit programs that `act()` runs through the gate |
| **fingerprint / expects** | What a tool requires on the page / what should happen after it runs |
| **reflect** | Idle-time pass: refresh tool health, propose forges and merges |
| **settle()** | DOM quiet 150 ms + no pending network, cap 3 s |
| **consequence** | What an act caused: navigated, opened tab N, dialog, download, no visible change |

## Crate dependency order (a crate depends only on crates below it)

```
sb-mcp  srv-web  srv-files  srv-cli  toolforge
        │
toolstore   sb-sheet   sb-act   sb-media
        │
     sb-driver
        │
      sb-cdp
        │
   core-pg ─► core
```

`sb-sheet` and `sb-act` are pure. `srv-web` uses `sb-sheet`'s read renderer on fetched
HTML; `sb-mcp` uses it on the live index. Same code, two inputs.

## Decisions carried into this repo (see agent-memory/decisions.yaml)

D-1 Claude Code only · D-2 Rust for everything that touches the browser · D-5 challenge
means stop · D-6 model never sees HTML/pixels unless asked · D-7 priority order ·
D-19 gate lenient by default · D-20 mail and calendar via browser · D-21 Postgres behind
MemoryStore, never two DBs · D-22 tools saved per site · D-23 harness first, browser as
module · D-24 new folder, start from scratch (old sheetbrowse is reference only).
