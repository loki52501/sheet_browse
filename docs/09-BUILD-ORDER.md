# 09 · Build order

Six steps. A step is done when its acceptance tests are green and `agent-memory/tasks.yaml`
says so. Do not open the next step's crates before that. Each step lists the docs that
govern it.

## Step 1 · Harness core + memory
Docs: 01, 02. Crates: `core`, `core-pg`, `migrations/`, `harness` CLI.
Build: Postgres 16 + pgvector + pg_trgm (Docker compose file in repo), schema, `MemoryStore`
trait and backend, gate with `gate.rules`, vault (age + keychain), ledger (table + jsonl
mirror), run folder, scheduler loop, `core::mcp::serve` wrapper, fake `echo` server.
Done when: acceptance in 01 and 02 (step-1 items) pass; `cargo test -p core -p core-pg` green.

## Step 2 · srv-web + read renderer + tool store loader
Docs: 03, 04 (srv-web), 05 B-6 (shared scorer and renderer live in `sb-sheet`).
Crates: `sb-sheet` (pure), `toolstore`, `srv-web`.
Build: fetch → index; `read()` renderer with chunking; `find()` scorer with BM25 +
synonyms + roles (data files); `api`, `feed`, `search`; tool store loader, header parser,
fingerprint check on fetched index, lifecycle table updates; one seeded tool
`tools/ashbyhq.com/list_jobs.js`.
Done when: 03 (step-2 items) and 04 srv-web acceptance pass; Phase 1 ladder rungs 1, 2, 4 pass.

## Step 3 · srv-files + srv-cli
Docs: 04. Crates: `srv-files`, `srv-cli`.
Build: fs, shell with classification, docs wrappers over existing skill scripts, csv.clean,
diff, history.search, gh, dns, cli allowlist.
Done when: 04 acceptance passes; ladder rungs 3, 5, 9 pass.

## Step 4 · Memory in use, scheduler, watch, reflect
Docs: 01 H-5, 02, 03 T-6..T-8, 06 G-3.
Build: reviews (SM-2), contacts touch/check in gate, baselines, `harnessd` loop, watch
diff + single notification, reflect (readers only) updating `tools.health`, proposals file.
Done when: ladder rungs 6, 7, 8 pass; reflect retires a planted stale ephemeral tool.

## Step 5 · Phase 1 baseline
Docs: 08. Run all 10 rungs × 3 variants and the A/B use cases. Record in
`bench/results/phase1-<date>.md`: pass counts, evidence completeness, asks, timeouts,
vault scan clean. This is the number step 6 must not lower.
Done when: ≥ 27/30 rung variants pass; zero vault leaks; every run has a complete `report.md`.

## Step 6 · Browser: sheetbrowse v2, then mail/calendar, then Toolforge
Docs: 05, 06, 07. Crates: `sb-cdp`, `sb-driver`, `sb-act`, `sb-media`, `sb-mcp`, `toolforge`, `injected/`.
Order inside the step (each with its acceptance in 05):
1. `sb-cdp` Pipe transport + launch + registry skeleton (B-3, B-4 #1 #2)
2. `injected.js` identity + walk + names (B-4 #4, B-5) → acceptance 1
3. `act(id)` + Rust retry (B-4 #6) → acceptance 2
4. input synthesis + consequence capture (B-4 #7 #9) → acceptance 3
5. registry complete (#2) → acceptance 4; settle (#8) → acceptance 5
6. skeleton + find + sheet (B-6) → acceptance 6; read on live index → 7
7. sb-media → acceptance 8
8. scheduling, errors (#10 #11); escape hatches (B-10)
9. bench parity → acceptance 9
10. `me` profile + login flow; mail and calendar site tools (07) → 07 acceptance
11. Toolforge ladder + forge pipeline + reflect integration (06) → 06 acceptance
12. Re-run the Phase 1 baseline: it must not regress. Run the Phase 2 ladder.
Done when: all of the above green and `bench/results/phase2-<date>.md` written.

## Agents

Use several Claude Code agents in parallel only where crates are independent: in step 1,
`core` and `core-pg` are sequential (trait first); in step 2, `sb-sheet` and `toolstore`
can proceed in parallel, `srv-web` after both; in step 3, `srv-files` and `srv-cli` in
parallel; in step 6, `injected.js` and `sb-cdp` in parallel, everything else sequential
in the listed order. One orchestrator owns `agent-memory/tasks.yaml`.
