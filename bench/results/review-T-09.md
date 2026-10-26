# Review T-09: echo server and step-1 acceptance

Commit 9fd42d7 on swarm/core: `crates/harness/examples/echo.rs`, `crates/harness/tests/step1_accept.rs`, `agent-memory/handoffs/T-09-rust-coder-1.yaml`. Spec: docs/01-HARNESS-CORE.md "Acceptance for this doc (step 1)", docs/02-MEMORY.md touch_contact item; H-6, H-2, H-3, H-5; D-100, D-121, D-122.

Verified in a detached worktree at 9fd42d7 with its own target dir, Postgres on 127.0.0.1:5433:

- `SB_VAULT_CANARY=<fresh> cargo test -p harness --test step1_accept -- --nocapture`: 7 passed, 0 failed (146 s). The canary had 0 hits in the full log.
- `cargo clippy -p harness --all-targets -- -D warnings`: clean. `cargo fmt -p harness -- --check`: clean.
- Throwaway mutation (reverted, never committed): `eprintln!("{}", v.to_ascii_uppercase())` inside the echo sink closure. b2 fails with `SB_VAULT_CANARY found in: ["echo stderr"]`, so the grep catches a case-flipped leak.
- With SB_VAULT_CANARY unset: `cargo test -p harness --test step1_accept b2_` reports `... ok` in 0.02 s, and the SKIPPED line does not show (see Blocking).

## Blocking

- `crates/harness/tests/step1_accept.rs:706-715`: b2, the only test for docs/01 bullet 2 (the vault grep), returns early and passes when SB_VAULT_CANARY is unset. libtest captures the `eprintln!` SKIPPED line for passing tests, so the default run prints `test b2_... ok` (checked: 0.02 s). Merge gates run without the variable, so bullet 2 is never checked there but still reports green. That counts as a missing acceptance test. Fix: when SB_VAULT_CANARY is unset, fall back to a runtime-unique canary (process id, nanos and random hex, at least MIN_VALUE_CHARS). T-05 and T-10 already do this (handoffs T-05-rust-coder-3, T-10-tester-2). Never skip.

## Should fix

- `crates/harness/examples/echo.rs:204-208` together with `:145`: without `--vault-key` and SB_VAULT_PATH, echo opens the user's real vault (`default_path()`) with the OS keychain key (`default_key()`). The sink then writes the exact length and an unkeyed FNV-1a 64 of every resolved body to the `--sink` file. FNV is fast and the floor is 4 chars, so a short secret with known text around it can be recovered from that line offline. A fingerprint of a real secret then sits in a plain file that scrub cannot catch. Fix: refuse to start unless both `--vault-key` and SB_VAULT_PATH are given (never the keychain, never the default path). With that, the hash is only ever of a test canary in a test temp dir. That file is the fake tool's destination, the stand-in for the site, so the hash is acceptable there and keeps the test strong (exact resolution, not only length). If the fallback stays, drop the hash and keep `bytes` + `unresolved`.
- `crates/harness/tests/step1_accept.rs:860-867`: `assert!(r.is_err())` accepts any error: a typo, a renamed column (`why`, `ok`), or a dropped connection would all pass it. Assert that the error text contains `ledger is append-only` (the trigger at `migrations/0001_init.sql:42`). Also add `truncate ledger.events`, which the migration header (`:4-5`, `:43`) says is blocked but nothing tests here.

## Notes

- Rule check, echo.rs: the resolved value exists only inside the `resolve_into` closure (`:137-149`). The return text, `consequences`, ledger args and the `main` error line (`:270`, scrubbed) never hold it. There is no `unwrap` on external input (args parse through `str_arg`/`amount_arg` into `ToolError::Args`), no sleep, no em dash, no captcha path. Every side effect goes through `serve_io` (H-6).
- `--ask-timeout-ms` (`echo.rs:178-191`, `:224-230`): the seam is built only in the example, from an example-only flag, and no production binary wires it. It uses `min`, so it can only shorten the timer. `on_timeout` is untouched, and `ask_capped` (`crates/core/src/mcp.rs:453-457`) still turns a TimeoutProceed on a skip card into TimeoutSkip, so the seam cannot loosen the gate. `Asker` was already public (I-3). Nothing in core changed.
- Grep coverage (`step1_accept.rs:628-643`, `:788-830`): echo stdout and stderr, the 4 tool results, every row of every base table in every schema (`select t::text`, ledger.events asserted present), every file and file name under runs/ (ledger.jsonl and report.md asserted present, asks/ included) and the sink file. It matches raw and JSON-escaped spellings, ignoring ASCII case. Percent-encoded, base64 (B-151) and non-ASCII case (B-152) spellings are not grepped. That is fine for an ASCII canary, but the tester should use one.
- Bullet 1 (`:454-591`): class/mode/ok, the card's class and `on_timeout`, `<n>.answer` = timeout, the core event plus the tool event (D-121 (3)) and the report lines are all asserted. "Class forced to Pay" is implemented as the classifier raising Send to Pay when `amount` is present, even for a known contact. That is a reasonable reading.
- Bullet 4 (`:902-996`) drives the real `harness` binary and an in-process Daemon with a ManualClock and a fake Launcher, as D-122 says. It asserts parent_run, report.md, the mirror, a single firing and no model call.
- docs/02 touch_contact (`:596-623`): asked before, allowed after, no second card. The other docs/02 step-1 items are in core-pg tests, as the header says.
- `echo_bin` (`:160-197`) runs a blocking, uncapped `cargo build --example echo` inside the test runtime. Rule 6 is about driver code, and a cap would fail cold builds for no good reason (here the build took more than 60 s), so I accept it. Each `#[tokio::test]` has its own runtime, so the blocked worker hurts no other test.
- Spec ids: the commit also exercises H-4 (run folder, report.md) without naming it, which is harmless. No pub API change, so `interfaces.yaml` is untouched, as the handoff says.

## Verdict

One blocking item: b2 silently passes when SB_VAULT_CANARY is unset, which is the merge-gate configuration. Everything else is sound, and the vault grep demonstrably catches leaks when it runs.

Verdict: request changes
