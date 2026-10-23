# START prompt for Claude Code

Paste everything below the line into Claude Code, opened in `F:\aside_mcp for claude\sheetbrowseV2_`.

---

You are the orchestrator for sheetbrowseV2_, a new project in this folder. It is an agent harness for Claude Code with a Rust browser inside it. Nothing has been built yet; the folder holds only specs and agent memory. You will build it in six steps, in order, and you will not start a step before the previous step's acceptance tests pass.

First, read these files completely, in this order, before writing any code:

1. CLAUDE.md
2. docs/00-OVERVIEW.md
3. docs/09-BUILD-ORDER.md
4. agent-memory/project.yaml, decisions.yaml, tasks.yaml, interfaces.yaml, blockers.yaml
5. Then, for the step you are on, the docs that step lists.

The old project at `F:\aside_mcp for claude\sheetbrowse` is reference only. You may read its `bench/results/` logs and `assets/snapshot.js` for ideas about what failed and why. Copy nothing wholesale. Do not modify it.

Hard rules, repeated because they are the ones that get broken: Claude Code is the only model, no API keys or .env anywhere; vault values never appear in any output, log, ledger row or test fixture, and every outgoing string is scrubbed; a captcha or human-verification step means stop, with no solving and no stealth; every side effect goes through the gate; the model never receives raw HTML or pixels except from the screenshot and video tools; no fixed sleeps in driver code; mail and calendar go through the browser, never an API; one database behind MemoryStore, never two.

How to work:

- Start with step 1 (docs/01 and docs/02): Cargo workspace, docker-compose for Postgres 16 with pgvector and pg_trgm, migrations, the `core` crate (MemoryStore trait, gate, vault, ledger, run folder, scheduler loop, the `core::mcp::serve` wrapper), the `core-pg` backend, the `harness` CLI, a fake `echo` tool server, and the step-1 acceptance tests exactly as listed at the end of docs/01 and docs/02. The vault-leak test must grep stdout, the ledger table, the jsonl mirror and `runs/` for a planted value and find it nowhere.
- Before a step, mark its tasks `doing` in agent-memory/tasks.yaml with you as owner. After, mark them `done` and write a handoff file from agent-memory/handoffs/TEMPLATE.yaml. Record every decision you make (including the four open ones D-31 to D-34 in decisions.yaml) with a one-line why. Write open questions to blockers.yaml with the default you took, then keep going. Do not stop to ask me unless a decision is irreversible and could reasonably go either way.
- Use sub-agents only where docs/09 says crates are independent. You own tasks.yaml. Each sub-agent gets: the crate, the doc section ids it implements, the interface ids from interfaces.yaml it must honour, and the acceptance tests it must make pass. It reports back with the handoff template.
- Every commit message names the spec ids it implements (H-3, M-2, T-4...). `cargo fmt`, `cargo clippy -- -D warnings` and `cargo test` must be clean at every commit. Commit at the end of every task, not at the end of every step.
- A step is done when its acceptance tests are green, not when the code compiles. When step 1 is green, continue to step 2 without asking. Continue through step 5. At the end of step 5, write bench/results/phase1-<date>.md and stop. Report the Phase 1 baseline numbers to me and wait for my go before starting step 6 (the browser).

What to send me when you stop: the baseline table from step 5, the list of decisions you made, the blockers you took defaults on, and the one command I can run to see everything passing.

Begin with step 1 now.
