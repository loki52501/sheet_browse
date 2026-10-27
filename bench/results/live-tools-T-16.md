# Live check: `--tools ""` in a `claude -p` session (T-16, D-144)

Date: 2026-10-26. Claude Code 2.1.311 (`claude --version`), Windows 11, run by the core
sub-orchestrator through Claude Code itself (no API key, rule 1).

MCP config (scratch file): one server, `harness-web`, command
`F:/sb-swarm/wt/core/wt/T16/target/debug/srv-web.exe`, no args.

Command (prompt on stdin):

```
echo "List the exact names of every tool you can call, comma separated, nothing else. If none, say NONE." |
  claude -p --strict-mcp-config --mcp-config <that file> --setting-sources local \
    --permission-mode default --tools "" --allowedTools mcp__harness-web
```

Answer:

```
mcp__harness-web__api, mcp__harness-web__feed, mcp__harness-web__fetch, mcp__harness-web__find, mcp__harness-web__search
```

So `--tools ""` removes every built-in tool (no Bash, PowerShell, Read, Write, Edit, WebFetch,
Task, ...) and keeps MCP tools. `ClaudeLauncher` passes it to every `-p` session (harnessd
firings and reflect's replay session); `launcher::tests` assert the argv.
