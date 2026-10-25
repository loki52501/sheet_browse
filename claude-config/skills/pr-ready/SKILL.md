---
name: pr-ready
description: Get a sheetbrowseV2_ area branch merge-ready in one pass - sync with main, commit hygiene, build gates, contract-path check, a self-review against the defects wave-1 reviewers kept finding, tests that can fail, and the merge gate run locally. Use before marking areas.<area>.status pr_open, and give it to every coder before it reports done.
---

# PR-ready checklist (area sessions and their coders)

Wave 1 areas needed 3 to 5 review rounds each, mostly for the same defect classes. Run this before you ask for a review, and again before `pr_open`. Run everything from your worktree with temp on its drive (`$env:TMP`/`TEMP`/`TMPDIR` = `F:\sb-swarm\tmp-<area>`).

## 1. Sync first

- `git status` clean, then `git merge main`. Resolve conflicts by keeping both sides' entries.
- `agent-memory/blockers.yaml` must have each top-level key once (`blockers:`, `interface_changes:`, `proposals:`) and every entry under the right one:
  `python -c "import sys,pathlib; sys.path.insert(0,'scripts/swarm'); import merge_gate as g; print(g.check_blockers_shape(g.load(pathlib.Path('agent-memory/blockers.yaml'))) or 'ok')"`
  blockers.yaml and decisions.yaml are append-only: fix structure by moving lines, never by editing or deleting entries (D-75).

## 2. Commit hygiene

- Stage explicit paths (`git add crates/<crate>/...`), never `git add -A` or `git commit -a` while subagents share the worktree.
- Before every commit: `git diff --cached --stat` and `git diff --cached --diff-filter=D --name-only`. Any deleted or shrunk file you did not mean to touch means a stale index (core's b7aa4f4 reverted two tasks' files this way). Unstage and recheck.
- Commit message: `<crate>: <what> (<spec ids>, <decision ids>)`.

## 3. Build gates (all must pass; the merge gate runs the last one)

```
cargo fmt --all -- --check
cargo clippy -p <crate> --all-targets -- -D warnings
cargo test -p <crate>
cargo test --workspace
```

## 4. Contract paths compile

Every item `agent-memory/interfaces.yaml` lists for your crates must exist at exactly that path. Keep one test file per crate, `crates/<crate>/tests/contract_paths.rs`, that `use`s every listed path (`use toolstore::header::parse_fingerprint;`, `use toolstore::ToolStore;` with a call to `ToolStore::try_tool_path`) so a wrong path fails compilation. Two wave-1 interface entries named paths that did not exist (D-74). When you file an `interface_changes` entry, copy paths from the code, not from memory, and add them to this file in the same commit.

## 5. Self-review: the defects reviewers found in wave 1

Check each against your diff and fix before the reviewer sees it:

- **Untrusted text reaches the model unescaped.** Page text, file contents, tool output must not be able to reproduce the harness's own syntax: link forms `[..](#id)`, code fences, `remaining:`/`truncated:` trailers, card rows (sheet B-32). Escape it the same way you escape your own structure, and cap length.
- **Fails open.** A missing row, timeout, parse error or unknown value must tighten, never relax: missing `runs` row reads as strict (D-79), unknown target classifies as Submit (D-402), unknown tool is a protocol error (D-71).
- **Look-alike keys.** Anything keyed by user or page text (contacts, domains, hints, tool names) is folded explicitly: ASCII lowercase (D-62), Punycode A-labels (D-81), and on Windows file names are case-insensitive (store B-31 item 1).
- **Vault values.** Never in an error, panic, `Debug`, log, ledger row or fixture. Scrub every spelling a consumer can produce: escaped, URL-encoded, base64/base64url (B-151), split or truncated (B-152).
- **Capability gain on repair.** A re-forge or relocate may never raise what a tool can do (`@phase`, `@kind`, new `@vault` keys, lower `@gate`) (B-31 item 2, G-4).
- **External input panics.** No `unwrap`/`expect`/indexing on JSON, files, URLs or model strings; slice UTF-8 only on char boundaries (`char_indices`, `floor_char_boundary`).
- **Filesystem races.** Rename then set_mtime, check then write: record state in data you control, not in a filesystem attribute that can fail to update (store B-31 item 4).
- **Silent defaults.** A documented default (`k = 3`, `chunk_tokens = 4000`) must be applied in code and asserted in a test; `0` must not silently mean "drop everything" (sheet B-32 item 5).

## 6. Tests that can fail

For each acceptance item, the test must fail when the behaviour breaks. Prove it once: break the code path (comment out the check, return early) and watch the test go red, then restore. Canary tests must feed the canary into the input (store B-31 item 3). An assertion on a value the test itself wrote proves nothing.

## 7. Reports the gate reads (exact names)

`scripts/swarm/merge_gate.py` reads, for every task in `swarm.yaml: areas.<area>.tasks`:
- `agent-memory/tasks.yaml`: the task's `status: done`;
- `bench/results/review-<TASK>.md`: a `## Blocking` section that is empty or `- none`, and a last line `Verdict: approve`;
- `bench/results/accept-<TASK>.md`: a table whose `result` column is `pass` on every row, and a last line `Verdict: green`.

Split reports (`review-T-19-js.md`) are fine as working files, but the gate only reads `review-T-19.md` and `accept-T-19.md`: write a combined one per task. Then run the gate on your own tree:

```
python scripts/swarm/merge_gate.py <area> --worktree .
```

It must print `gate ok`. Only then set `areas.<area>.status: pr_open`, commit it with the final handoff, and stop.
