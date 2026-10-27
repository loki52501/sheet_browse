You are the supreme orchestrator for sheetbrowseV2_ on my PC, repo root, branch main. New direction, read docs/11-BROWSEROS-PIVOT.md now; it overrides docs/05, 06, 07 where they differ. Goal: the full product working today. Keep hooks and swarm, but run at most 3 area sessions at once (RAM).

Record D-94..D-97 and D-100 in decisions.yaml (safety deferred, gate allow-all except Challenge, BrowserOS is the browser, custom browser frozen as v2). Label PRs #6 #10 #11 #12 #22 "v2" and leave them open. Merge #21 if green.

Then, in this order, using area sessions where it saves time:
1. You, now (30 min): add BrowserOS MCP to .mcp.json next to harness-web, harness-files, harness-cli, harness-mem (copy the browseros entry from my existing Claude Code / BrowserOS config; ask me only if you cannot find it). Insert gate.rules allow rows. Add scripts/hooks/browseros_ledger.py and its PostToolUse entry in claude-config/settings.json, reinstall .claude/. Verify all servers connect.
2. core session: T-17b (run preamble and files/ path for interactive harness run) and the harness-forge server per docs/11 (tools.list/run/save, recipe kind in toolstore, reflect retires failing recipes).
3. servers session: seed recipes for mail.google.com (inbox, thread, draft, send) and calendar.google.com (events, create), and for lu.ma (register), tested live through BrowserOS.
4. When 1 to 3 merge: a tester runs the SMOKE.md list in docs/11 in fresh Claude Code sessions and writes SMOKE.md.
5. bench-runner: T-26 with BrowserOS vs browser-use, then T-29. Write both reports.

Merge rule D-87, two review rounds max, push main after every merge, 30-minute check-in loop, relaunch stopped sessions without asking. Stop when SMOKE.md, the bench report and the Phase 2 report are on main, and give me a one-screen summary.
