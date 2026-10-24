# Review T-07: H-6 MCP wrapper, FileAsker, core-pg bootstrap (commit 7cb8691)

Scope: crates/core/src/mcp.rs, crates/core/src/ask.rs, crates/core/src/lib.rs, crates/core-pg/src/lib.rs (bootstrap), crates/core/tests/mcp_t07.rs.
Checked: `cargo test -p harness-core --lib --test mcp_t07` (85 + 18 pass); `cargo clippy -p harness-core --lib --test mcp_t07 -- -D warnings` and `cargo clippy -p core-pg --lib -- -D warnings` are clean.

## Blocking

- crates/core/src/ask.rs:152-159 and :166-176. FileAsker fails open (rules 4 and H-1.3). If it cannot create `asks/` or write the card, it returns `timed_out(card)`, and for Send(new) or Account cards that is `TimeoutProceed`. dispatch (mcp.rs:726-746) then logs `timeout_proceed` and runs the handler, even though the user was never shown a card. Proceeding on a timeout means "the user saw it and stayed silent", and here nobody saw anything. Return `TimeoutSkip` on these paths and add a test (for example, make `asks` a file so the create fails).
- crates/core/src/mcp.rs:284-289 (`ToolCtx::ask`). Mid-call asks never check `gate.rules`. Only `runs.strict` is read, so these are all ignored: a strict rule (H-1.3: "strict on a run **or a rule**"), a hint rule (D-110 (4), D-113), and a stop rule for (card.site, card.class). Example: a step escalates mid-call to Account on a domain with `('x.com','Account','ask', strict)` or `('x.com','Account','stop')`. The card is still shown with `on_timeout = Proceed` and runs after 60 s of silence. That is a side effect whose class was never checked against the gate (rule 4). Fix: run the card's class and site through `Gate::check` or the rule lookup. On Stop, skip the ask and log a stop. Treat a strict or hint hit as Skip.
- crates/core/src/mcp.rs:272-275 (the Challenge path in `ToolCtx::ask`) and :830 (`diag`). Both call `card_line` before `vault.scrub`, which is the wrong order (rule 2). Two ways a value leaks:
  - Truncation at 200 or 1000 chars can cut a vault value in half. The remaining prefix no longer matches, so it survives.
  - Whitespace collapse turns a multi-line value (a personal `address = "1 Main St\nSpringfield"`) into `1 Main St Springfield`. Neither this scrub nor `Ledger::write`'s global scrub matches that.

  The changed value then lands in the ledger (`result_summary`/`why` of the core stop event) and on stderr. Scrub first, then `card_line`, then scrub again. `clean_card` at :931 already scrubs first.

## Should fix

- Missing tests for behaviour the commit or handoff claims:
  - The `ToolCtx::ask` Challenge card is never shown and is logged as a stop (rule 3, mcp.rs:271-283).
  - Fail-closed paths: a ledger write failure on the Ask path means the handler does not run (mcp.rs:729-732); a gate or `run_get` error means it does not run (mcp.rs:693-720); a mid-call ledger failure turns proceed into skip (mcp.rs:293-302).
  - dispatch with `runs.strict = true` on Send(new) times out to skip.
  - A panicking handler gives `isError` (mcp.rs:607).
  - Two concurrent `tools/call` requests with out-of-order replies.
  - The FileAsker stderr line contains no canary.
- mcp.rs:595-610. A panicking handler gets an `isError` reply but writes no ledger event, although its side effect may already have happened (H-3, H-6). Catch the panic inside dispatch (spawn plus join, or `catch_unwind`) and write an `ok=false` tool event.
- mcp.rs:803 and :822-826. On the Allow path the ledger is written only after the handler runs. If Postgres is down, the side effect happens and is recorded only on stderr. The comment at :728 ("Nothing runs that the ledger does not show") is not true for Allow. Either check the ledger before the call or state the gap in the module doc and the handoff.
- mcp.rs:487 (`write_message`). Scrubbing runs over the whole JSON-RPC envelope: `id`, `jsonrpc`, `protocolVersion`, object keys. A short vault value (a PIN `2025`, a value `12`) rewrites `"id": 12` to `"@vault(pin)"` or `"2025-06-18"` to `"@vault(pin)-06-18"`, which breaks request correlation and the handshake. Scrub only the payload (`result.content[*].text`, `error.message`, tool list strings).
- mcp.rs:456. One non-UTF-8 input line makes `next_line()` return `InvalidData`, and serve_io then returns `Transport`. The server dies and replies for calls in flight are lost. Answer -32700 and keep reading.
- mcp.rs:726 and :291. There is no cap around `asker.ask`. An Asker that ignores `card.timeout` hangs the call forever. Wrap it in `tokio::time::timeout(card.timeout + margin)` and fall back to skip.
- ask.rs:234-249. `answer()` writes `<n>.answer` even after the ask has timed out or been answered, so the user believes they answered when nothing read it. Have FileAsker write a closed or outcome marker and have `answer()` refuse with a clear error.
- core-pg/src/lib.rs:1004-1012 with mcp.rs:698 and :285. `SB_RUN_ID` is checked only against the folder. If the run has no `runs` row, `run_get` returns `Ok(None)` and the run is read as `strict = false`, which fails open. Bootstrap should require the row, or a missing row should count as strict. `bootstrap()` has no test; the handoff acknowledges this, and the tester should run it with and without `SB_RUN_ID`.
- ask.rs:74, :124 and :152. Blocking `std::fs` calls run on the async executor. They are small, but `spawn_blocking` or `tokio::fs` is cheap here.

## Notes

- I-2 fidelity: the signatures of serve, dispatch, Harness, ToolRegistry, ToolSpec, ToolHandler, Classified, ToolCtx, ToolOutput and ToolError are unchanged against 275539d. The new private fields (ToolRegistry.version, ToolCtx.ledger and .call) do not change the API. The B-104 additions (serve_io, PROTOCOL_VERSION, scrub_value, the `ask` module) are additive and sound. B-103 (one card sanitiser shared with gate) is the right fix, and until then the copy is tested.
- H-6 duties are all present in dispatch: classify (`max(declared, classified)`, mcp.rs:691, so classification is never lowered), gate with run strict, vault only via `ToolCtx::resolve_into`, output/error/stop/files scrubbed with `harness.vault`, ledger through the shared `Ledger`, `duration_ms`, run id. Gate Stop and Challenge return before the handler and log a core stop with the element.
- MCP 2025-06-18: the initialize shape is correct (protocolVersion, `capabilities.tools.listChanged`, serverInfo). Notifications get no answer. -32602 for an unknown tool matches the spec, and -32601, -32700 and -32600 are used correctly. `tools/call` is spawned so calls run concurrently, and EOF drains calls in flight. The server always answers 2025-06-18; that is fine while it is the only version supported. `notifications/cancelled` is ignored, which is acceptable.
- Tool names such as `echo.send` and `fs.read`: check that Claude Code accepts or maps `.` in MCP tool names (the API tool-name pattern is `[a-zA-Z0-9_-]`). This is not T-07's choice, but serve publishes the names as they are.
- `ToolRegistry::add` (pre-existing contract) replaces a tool by name with no floor check, so a re-registered site tool with a lower class lowers its floor. Rule 4 says a repair or forge never lowers one. If toolstore does not enforce this, `add` could keep `max(old, new)`.
- The mid-call Challenge stop event has `consequences = null` (no element). A handler that then returns `stopped` writes a second core stop (mcp.rs:276 and :804-806).
- Rule 6: FileAsker waits on a `notify` watch with a cap. The fallback `sleep_until(deadline)` (ask.rs:203) has a comment explaining it; acceptable.
- Rule 1: bootstrap reads `DATABASE_URL` (the default has no password), `SB_VAULT_PATH` or the config dir, the OS keychain, `SB_RUNS_ROOT` and `SB_RUN_ID`. No API key is read. Rule 5 holds: the model gets text only. Rule 7 does not apply.
- docs/01 acceptance coverage (dispatch with MemStore): echo.send to a known contact gives allow (mcp_t07.rs:258), to an unknown recipient gives `timeout_proceed` (:370), forced Pay gives skip (:314). The vault grep covers the return value, ledger rows, the jsonl mirror, the `runs/` tree and serve_io stdout (:507, :617). The PgStore side of the grep belongs to T-09.
- Running cargo here updated Cargo.lock because of other coders' uncommitted edits to Cargo.toml and crates/harness/Cargo.toml (clap). That change is not from T-07.

## Verdict

Request changes. There are three blocking items: FileAsker proceeds on timeout when the card was never written; mid-call asks ignore strict, hint and stop gate rules; and vault values are cut or reshaped before scrubbing on the Challenge and stderr paths.

Verdict: request changes
