# 06 · Toolforge (crate `toolforge`)

Forges site tools on the fly, saves them into the tool store (docs/03), and improves
them with use. Items are `G-n`. Carried over from the old TOOLFORGE.md (S-12..S-17),
rewritten for the site-scoped store.

## G-1 The decision ladder (fast; the decision must never take longer than the task)

| Rung | Try | Budget | Outcome |
|---|---|---|---|
| 1 | **registry**: a loaded site tool whose `@args` fit the request | 50 ms | run it (T-4 fingerprint first) |
| 2 | **relocate**: a site tool with a partial fingerprint | 200 ms | patch and run |
| 3 | **compose**: `_generic/` tools chained (paginate + read_table) | 300 ms | run the chain |
| 4 | **forge**: write a new tool from the current page and request | ≤ 10 s, max 2 per task | ephemeral tool, then run |
| 5 | **fallback**: plain `find` + `act` by the model | | normal operation, trace recorded |

Rungs 1–3 are pure lookups in Rust. Rung 4 is the only one that calls the model (Claude
Code, as a sub-request), and only when the task has already needed ≥ 2 manual acts on
the same form or list, so a one-off click never triggers a forge.

## G-2 Forge pipeline

1. **Gather**: the request, the skeleton, the `find` results and the manual act trace so far.
2. **Draft**: the model writes a tool file (T-2 format) as a reader or planner.
3. **Static check**: parse header; denylist (`document`, `window`, `fetch`, `eval`,
   `import`, `require`); planner output must contain `?s` before any Submit-class step;
   `@gate` present and ≥ classified class.
4. **Dry run**: readers run against the live index; **planners are parsed and
   classified only, never executed against a live site** (bench failure F-7).
5. **Register** as `ephemeral` with `@forged` metadata; commit to `tools/<domain>/`.
6. **Run** through the normal path (gate, ledger).
7. **Score**: success → `runs += 1`; failure → `fails += 1`, why recorded.

Caps: 10 s wall per forge, 2 forges per task, 1 forge per form per task.

## G-3 Reflect (nightly and after each task; H-5)

- Health refresh: fingerprints and readers only.
- Growth: T-6 candidates → forge proposals, or auto-forge if the trace is clean
  (every step ok, no asks denied) and the domain already has a folder.
- Merge: T-8 proposals.
- Expectation learning: T-5 auto `@expects` after 3 clean runs.
- Writes proposals to `agent-memory/blockers.yaml: proposals` and `tools.health`.

## G-4 Learn as data, never as permission

A forged or repaired tool changes **what** gets done on a page, never **whether** it is
allowed. The gate floor can only go up. A repair cannot remove `?s`, cannot add a vault
key the tool did not use, cannot widen `@match` to another domain. Violations are
rejected at the static check and logged.

## G-5 User loop

- `harness tools list luma.com` shows tools, status, runs/fails, why.
- `harness tools pin luma.com/register` protects it.
- Editing a tool file by hand is allowed; the next load re-parses it and resets status
  to `saved` with `version += 1`.
- Writing a tool by hand before the agent ever visits a site is the expected way to
  seed important sites (mail, calendar: docs/07).

## Acceptance (step 6)

- A task that needs 3 manual acts on one form produces exactly one forge; a second
  identical task on the same site uses rung 1 and makes zero forges (ledger proves it).
- A forge whose draft lacks `?s` before a submit is rejected at static check.
- A planner is never executed during forge (ledger shows no act from `toolforge`).
- Reflect on a fixture site with a changed button label repairs the tool via relocate
  and bumps the version.
