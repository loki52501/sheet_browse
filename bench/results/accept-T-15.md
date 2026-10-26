# Acceptance: T-15 (srv-cli, docs/04-TOOL-SERVERS-PHASE1.md)

Branch tested: worktree `.wt/T15` at 1221877 (srv-cli C-1..C-4) + 33aa903 (handoff).
Scope: the one docs/04 acceptance item that belongs to srv-cli (`gh run view --log`), plus
the srv-cli spec items C-1..C-4 that the acceptance rests on. The other docs/04 acceptance items
(`fetch`, `find`, `api`, `shell`, `history.search`) belong to srv-web/srv-files, not T-15.
Ladder rungs 3, 5 and 9 (docs/08) need a model run through Claude Code and are T-17's job, so
they are not rows here.

All runs set `CARGO_TARGET_DIR=F:/sb-swarm/target-servers` and
`SB_VAULT_CANARY=t15canary-<ns timestamp>-<random>` (a unique value; tests/common plants it
as the vault value).

| item | command | result | note |
|---|---|---|---|
| docs/04 acceptance: `gh("run view 123 --log")` returns the first failing step from a fixture log | `cargo test -p srv-cli --test gh_t15 acceptance_run_view_log_returns_first_failing_step` | pass | `gh_t15: 7 passed; 0 failed`. Returns `first failing step: job "test", step "Run cargo test"`, 12 lines or fewer, timestamps stripped, later failing step left out. Full log saved to `files/gh-run-123.log`. Ledger: Read/Allow, no card |
| C-1 reads named in the spec (`run list`, `run view`, `pr view`, `api` GET/HEAD) are Read and run without a card | `cargo test -p srv-cli --test accept_t15 c1_spec_reads_are_read_and_run_without_a_card` | pass | New test. `accept_t15: 2 passed; 0 failed`. 7 commands, each with ledger class Read, mode Allow, ok, and zero cards |
| C-1 writes (`api -X POST\|PUT\|DELETE`, `--method=PUT\|DELETE`, `pr merge`, `release create\|list\|upload`, unknown) reach the gate as Account or higher | `cargo test -p srv-cli --test accept_t15 c1_spec_writes_reach_the_gate_as_account_or_higher` and `--test gh_t15 writes_reach_the_gate_as_account_or_delete` | pass | New test. One card per call. POST/PUT are Account, DELETE is Delete. Skipped calls never ran gh (only core events in the ledger). Mutation check: changing the `release list` expectation to Read makes it fail (`left: Account right: Read`), so the test can catch a regression |
| C-1 `gh auth` refused; shell syntax refused before anything runs | `cargo test -p srv-cli --test gh_t15 auth_is_refused_and_nothing_runs` | pass | ToolError::Args. No card, and no ok event |
| C-1 vault: canary in the gh log is scrubbed from the returned step, the saved log, the ledger and the run folder | `cargo test -p srv-cli --test gh_t15 saved_log_and_returned_step_are_scrubbed` | pass | The returned text and the saved file both show `@vault(gh_canary)`. `everything_written()` (every ledger row and every file under the runs root, `ledger.jsonl` included) is clean |
| C-2 `dns(name, type)` formatting, name/type checks, bad args fail before lookup | `cargo test -p srv-cli --test dns_t15` | pass | `dns_t15: 4 passed; 0 failed; 1 ignored` (the ignored one is the live row below) |
| C-2 `dns` live lookup through the system resolver | `cargo test -p srv-cli --test dns_t15 -- --ignored` | pass | `live_lookup_example_com ... ok`; `1 passed; 0 failed`. Ran it explicitly: it needs a network, so the default run ignores it |
| C-2 `cloudflare(...)` via api with vault: token reaches the Authorization header, and the reflected token is scrubbed from text, errors and ledger. Writes ask Account, DELETE asks Delete, paths cannot leave the base | `cargo test -p srv-cli --test http_t15 cloudflare_` | pass | `http_t15: 6 passed; 0 failed`. The in-test server got `Bearer <canary>`. The returned text shows `Bearer @vault(cf_token)`. A 403 error is scrubbed. `//evil`, `..`, TRACE and `@vault` in the body are all refused, with nothing sent |
| C-3 `flags(provider, env)` read uses the provider header and is scrubbed; writes are Account, DELETE is Delete; unknown provider/env refused | `cargo test -p srv-cli --test http_t15 flags_` | pass | GET `/api/environments/production/flags` carries the canary in `X-Flags-Auth`. PATCH asks Account, DELETE asks Delete. Skipped writes send nothing |
| C-4 generic `cli(cmd)` behind `config/cli-allow.toml`: allowlisted read runs, everything else refused, missing allowlist allows nothing, write/delete entries ask | `cargo test -p srv-cli --test cli_t15` | pass | `cli_t15: 5 passed; 0 failed`. 8 refused commands produce 8 failed classify events and no cards. `allowed: none` when the file is absent |
| Server surface: 5 tools (`gh dns cloudflare flags cli`), Read floor, interface paths compile | `cargo test -p srv-cli --test contract_paths` and unit tests | pass | `contract_paths: 2 passed`, `src/lib.rs unittests: 20 passed` |
| Vault canary never in output: stdout/stderr of the full srv-cli run | `SB_VAULT_CANARY=<unique> cargo test -p srv-cli -- --nocapture > srvcli.out 2>&1; grep -c -F -f canary.txt srvcli.out` | pass | exit 0, 0 hits. Totals: 46 passed, 0 failed, 1 ignored (run separately above) |
| Vault canary never in the ledger table, jsonl mirror or `runs/` | in-test `assert_clean(&env.everything_written(), ..)` in gh_t15/http_t15 + `grep -rlF -f canary.txt runs/ $TEMP` + `psql -U sb -d <each db> -c "select count(*) from ledger l where l::text like '%<canary>%'"` | pass | The in-test ledger (MemStore) and `runs/<id>/` (with `ledger.jsonl`) are clean. Repo `runs/` does not exist. `$TEMP` has 0 hits (the only hit was my own canary.txt in the scratchpad). The srv-cli tests never touch Postgres, and no `sb*` database has a `ledger` table in the public path, so there were 0 rows to hit |
| `cargo clippy -p srv-cli --tests -- -D warnings` | same | pass | Clean, the new test included |

Outside T-15: in `SB_VAULT_CANARY=<unique> cargo test --workspace`, `core-pg --test accept_t03_r3 r3_rule2_constraint_errors_carry_no_bound_canary` panics with `SB_VAULT_CANARY must contain 'sbcanary' here`. That is a check on the canary's format in a core-pg test, not a leak (0 canary hits in the workspace output). T-15 does not touch core-pg. Report it to core: either core-pg makes its own canary when the env value lacks `sbcanary`, or the swarm canary convention includes `sbcanary`.

Verdict: green
