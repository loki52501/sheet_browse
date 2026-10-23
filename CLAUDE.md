# CLAUDE.md: rules for coding agents in sheetbrowseV2_

You are building an agent harness with a browser inside it. Read `docs/00-OVERVIEW.md`
first, then the doc for the step you are on (`docs/09-BUILD-ORDER.md` says which).
`agent-memory/` is your shared memory with other coding agents: read `tasks.yaml`,
`decisions.yaml` and `blockers.yaml` before starting, write to them when you finish.

## Hard rules (a PR that breaks one is rejected)

1. **Claude Code is the only model path.** No Anthropic API calls, no other model SDKs,
   no API keys, no `.env` files committed, no key-shaped strings in the repo. Tests that
   need a model are run by Claude Code itself through MCP, or are skipped with a reason.
2. **Vault values never leave the tool that resolves them.** `@vault(k)` is resolved
   inside `core::vault` at the last possible moment (the `insertText` call, the HTTP
   header). A vault value never appears in a return value, a log line, the ledger, a
   screenshot caption, a panic message, or a test fixture. Outgoing strings are scrubbed
   against every vault value before they leave the process.
3. **A challenge means stop.** Captcha, Turnstile, hCaptcha, "verify you are human",
   phone verification. Report it, hand back to the user. No solving, no third-party
   solvers, no stealth plugins, no fingerprint or user-agent tricks, no headless
   evasion. This is decision D-5 and it does not get revisited.
4. **Everything goes through the gate.** Every side effect from every tool server,
   including `eval`, `cdp`, forged tools, and site tools, is classified and checked.
   A repair or forge may never lower a gate class.
5. **The model never receives raw HTML or pixels** except from the explicit
   `screenshot` and `video.*` tools. Readers return short text. Passwords are never read
   from the DOM.
6. **No fixed sleeps.** Wait for a condition (`settle()`, navigation, an event) with a
   cap. `tokio::time::sleep` in driver code needs a comment saying why no condition exists.
7. **Text the user will send as themselves** (mail drafts, messages) contains no em
   dashes and no double-dash separators.

## Priorities when they conflict

efficiency (fewer, more correct steps) > latency > throughput > token cost.
A change that adds a round trip to save tokens is wrong. A change that adds tokens to
remove a wasted turn is right.

## Working rules

- Rust 2021, `tokio`, `serde`, `sqlx` (Postgres), `anyhow` at edges, typed errors inside.
  `cargo clippy -- -D warnings` clean. Format with `cargo fmt`.
- JS in `injected/` is plain ES2020, no bundler, no dependencies, one file per concern.
  It runs in an isolated world and must not touch `window` globals the site can see.
- One crate per box in the architecture. A crate may depend only on the crates below it
  in `docs/00-OVERVIEW.md`. `sb-sheet` and `sb-act` are pure (no I/O).
- Every spec item has an id (`H-3`, `M-2`, `T-7`, `B-14`...). Reference ids in commit
  messages and in `agent-memory/tasks.yaml`.
- A step in `docs/09-BUILD-ORDER.md` is done when its acceptance tests pass, not when
  the code compiles. Do not start step N+1's crates before step N's acceptance is green.
- When a spec is ambiguous, write the question into `agent-memory/blockers.yaml` with a
  proposed default, take the default, and keep going. Do not stop to ask.
- When you discover something the next agent needs, write it to `agent-memory/handoffs/`
  using the template. Decisions go in `decisions.yaml` with a one-line why.
- Benchmarks and acceptance runs write to `runs/` and `bench/results/`; never commit `runs/`.

## Swarm protocol (docs/10-SWARM.md)

- Supreme orchestrator at repo root on `main`; sub-orchestrators in `wt/<area>` on
  `swarm/<area>`; subagents from `.claude/agents/`. Only supreme edits
  `agent-memory/interfaces.yaml` and pushes to `main`.
- Claim before coding (`/claim T-xx`), handoff before stopping (`/handoff T-xx`),
  one commit per task with spec ids, one PR per area per wave.
- Hooks in `.claude/settings.json` enforce: no secrets written, no writes to the old
  sheetbrowse folder, no `rm` on `tools/`, no force push, YAML valid after every edit,
  Rust formatted, JS syntax-checked, tool headers present, vault canary never in output,
  no stop without commit + handoff. Do not disable or edit hooks; if one is wrong, write
  a blocker for supreme.
- Reviewer and tester are different subagents; a coder never accepts its own work.

## Things that look like shortcuts and are not allowed

- Reading the Gmail or Calendar API directly. Mail and calendar go through the browser
  (`docs/07-MAIL-CALENDAR-VIA-BROWSER.md`). The user decided this.
- Running two databases. Memory is Postgres behind `MemoryStore`. SQLite or SurrealDB
  are future backends of the same trait, not additions.
- Hand-writing HTML into the model's context "just for this test".
- Dry-running a planner tool against a live site. Readers only.
- Deleting a tool file directly. Tools retire, then archive, then delete, on the
  schedule in `docs/03-TOOL-STORE.md`.
