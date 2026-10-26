# SUPREME resume prompt (fast rules)

Paste into Claude Code at `F:\aside_mcp for claude\sheetbrowseV2_` on `main`.

---

You are the supreme orchestrator for sheetbrowseV2_, resuming. Read, in this order: prompts/SUPREME.md (the original role), agent-memory/handoffs/supreme-wave1.yaml (where you stopped), then `git log --oneline -15 main`. Then read this message to the end before acting, because it changes the rules you were following.

The repo now has a remote: `origin = https://github.com/loki52501/sheet_browse.git`, with main and all four swarm branches pushed. From now on PRs are real GitHub pull requests opened with `gh pr create --base main --head swarm/<branch>`; areas push their branches; you push main after every merge. Record this as D-86 (supersedes D-46 local-only) and update `swarm.yaml: pr_rule` to say so.

NEW RULES, effective now. Record each as a decision with the why "speed: three days produced 2 merged areas; the old rule serialised everything behind core". They override docs/10-SWARM.md S-3 and S-8 where they differ.

D-87 Merge rule: a PR merges when its reviewer report has no Blocking items and `cargo test --workspace` passes on the merge commit. A red acceptance item does not block a merge; it becomes a follow-up task on main (T-xxb) owned by the same area. Supreme second-opinion reviews are non-blocking notes filed into blockers.yaml; they never hold a merge or a launch.
D-88 Holds are abolished: no wave-2 launch waits on a should-fix in another area. D-77's base64 scrub, B-32 item 1 and B-33 item 3 become ordinary follow-up tasks running in parallel with wave 2.
D-89 Review loops are capped at two rounds per PR: round 1 review + test, round 2 fixes. After round 2, merge and file the rest as follow-ups.
D-90 Interface freeze: no amendment to interfaces.yaml until T-17 is done. An area that needs a change works around it locally and files the proposal; you answer "deferred to wave 4" and move on. D-84's VaultEditor and D-85's before_run are the last accepted items; if core has not finished them, they become follow-up tasks, not merge conditions.
D-91 One PR per task from now on. Branch names swarm/<area>-T<nn>. core's remaining work after its merge, and every wave-2 and wave-3 task, each gets its own branch and merges alone.
D-92 You relaunch any stopped area session yourself, without asking, using the launch gotchas in supreme-wave1.yaml (TMP on F:/sb-swarm/tmp-<area>, Start-Process wt.exe form). The only things you still ask me about: a merge conflict you cannot resolve, a vault or captcha rule, or deleting something.
D-93 Checkpoint commits at most once per hour, one line. Decisions only when they change what someone else must do.

DO NOW, in this order, without stopping between steps:
1. Record D-86 to D-93 in decisions.yaml; update swarm.yaml (pr_rule, merge_rule, waves "2": open). Commit and push main.
2. Merge core: `git merge --no-ff swarm/core` into main (its review is approved). If the merge has conflicts in agent-memory/*.yaml, keep both sides' list items. Run `cargo test --workspace`; if a test fails, fix the test or the code yourself in under 30 minutes, otherwise mark the failing test `#[ignore = "T-0xb"]` and file the follow-up. Push main. Open and merge the PR on GitHub for the record (`gh pr create` then `gh pr merge --merge`), or if the branch is already merged locally, push and note the merge commit in swarm.yaml.
3. Merge page the same way (review said request changes: file those as T-19b and merge what is there if it compiles and tests pass; it is Phase 2 code and nothing in Phase 1 depends on it being perfect).
4. Create follow-up tasks in tasks.yaml: T-03b/T-04b/T-05b/T-07b (red acceptance items), T-05c (D-84 VaultEditor if missing), T-08b (D-85 if missing), T-05d (D-77 base64 scrub), T-10b (B-32), T-11b (B-33 item 3), T-19b (page review items). Each with the area as owner, wave 2, its own branch name.
5. Launch wave 2 and the follow-ups: run `scripts\swarm\launch.ps1 -Areas servers,driver` plus relaunch core (for T-16 prep and its follow-ups), sheet (T-10b) and store (T-11b), using the working Start-Process form. Give each session a one-paragraph brief that names its tasks, its branch per task, and the new rules D-87 to D-91.
6. Then run the check-in loop every 30 minutes: `python scripts/swarm/supreme_status.py`, merge any PR that meets D-87 with `gh pr merge --merge` after `cargo test --workspace` on a local merge, push main, relaunch stopped sessions. One-line checkpoint per hour.
7. When servers and T-16 are merged, run the Phase 1 baseline (T-17) with bench-runner, write bench/results/phase1-<date>.md, push, and stop with: the baseline table, the list of follow-ups still open, and the one command that proves it.

Rules that still override everything: CLAUDE.md hard rules (no keys, vault never leaks, captcha means stop, everything through the gate). The hooks stay on. Never force-push. Never push to main from an area session.

Begin with step 1.
