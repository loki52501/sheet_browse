# Review T-09 round 2: echo server and step-1 acceptance

Commit fc3b6e5 on swarm/core (`git diff fc3b6e5~1 fc3b6e5`): `crates/harness/examples/echo.rs`, `crates/harness/tests/step1_accept.rs`, `agent-memory/handoffs/T-09-rust-coder-2.yaml`. This round answers round 1 (9fd42d7): Blocking b2 skip, Should fix echo vault fallback and the loose append-only assert. Spec: docs/01-HARNESS-CORE.md "Acceptance for this doc (step 1)"; H-2, H-3, H-6; D-122.

Verified in a detached worktree at fc3b6e5 with its own target dir, Postgres on 127.0.0.1:5433:

- `cargo test -p harness --test step1_accept` with SB_VAULT_CANARY unset: 7 passed, 0 failed (274 s of test time). b2 now takes as long as the others, not milliseconds.
- The same with a fresh SB_VAULT_CANARY (`--nocapture`): 7 passed, 0 failed (148 s). The canary had 0 hits in the full log.
- `cargo clippy -p harness --all-targets -- -D warnings`: clean. `cargo fmt -p harness -- --check`: clean.
- Throwaway mutation (reverted, never committed, worktree clean after): `eprintln!("{}", v.to_ascii_uppercase())` in the echo sink closure, SB_VAULT_CANARY unset. b2 FAILED with `SB_VAULT_CANARY found in: ["echo stderr"]`. So the fallback canary catches a case-flipped leak.
- The built echo binary: with no `--vault-key` it is a clap error (exit 2). With no SB_VAULT_PATH, or SB_VAULT_PATH set to whitespace, it prints `echo: SB_VAULT_PATH is required (a test vault; no default path)` and exits 1. No sink file is written in any of these cases.

## Blocking

- none

## Should fix

- none

## Notes

- Round 1 Blocking, resolved. `step1_accept.rs:702-725` `canary()` returns SB_VAULT_CANARY when set. Otherwise it builds `cnryT09q<pid hex>z<nanos hex>k<RandomState hash hex>`. That value is unique per run (pid, nanos and a per-process random seed), ASCII only (a fixed prefix plus hex), and asserted `>= MIN_VALUE_CHARS` (`:723`; it is about 40 chars against a floor of 4). It cannot collide with literal text in the test. Every needle is the whole canary (`needles`, `:628-635`), and no literal in b2 (`pin `, ` end`, `for you: `, `raw `, ` here`, the addresses, `@vault(test)`, `vault canary`) contains it. b2 (`:731`) has no early return left. The mutation above shows it fails on a leak without the env var.
- Round 1 Should fix (echo vault), resolved. `echo.rs:66` makes `vault_key: PathBuf` a required clap arg. `:206-211` requires SB_VAULT_PATH (trimmed, non-empty) and builds only `KeySource::IdentityFile`. `default_path` and `default_key` are no longer imported (`:43`). So the sink's chars/bytes/fnv1a64 line (`:140-152`) can only ever be about a test vault value. The header doc (`:15-30`) matches.
- Round 1 Should fix (append-only), resolved. `step1_accept.rs:875-893` panics on `Ok` and requires the error text to contain `ledger is append-only`, the exception raised by `migrations/0001_init.sql:42`. `truncate ledger.events` is added, and the trigger at `:43` covers truncate. The row count is still checked unchanged after the loop.
- Minor, no action needed: a tester-supplied SB_VAULT_CANARY is not checked for ASCII (B-152 covers non-ASCII case folding). It is also returned untrimmed when it has surrounding whitespace. `v.set` already enforces the length floor, with a panic message that names only the variable. The fallback path, which is what merge gates run, is ASCII by construction.
- Rule check of the diff: no keys or .env. The vault value never reaches a return, log, ledger or panic: the leak list prints only locations (`:870`), and the plant panic prints only the vault error. No sleeps, captcha paths or em dashes were added. Side effects still go only through `serve_io` (H-6). No pub API change, so `interfaces.yaml` is untouched.
- Spec ids: the commit claims H-2, H-3 and D-122, and the changes match (vault scrub test, ledger append-only, echo fake server). H-6 is unchanged this round.
- Still true from round 1: each run spends 1 to 5 minutes in `echo_bin`'s nested `cargo build --example echo` when crates/core changed. libtest's "running for over 60 seconds" lines are expected.

## Verdict

All three round-1 items are fixed, and each one is backed by a test that fails if the fix breaks. Step-1 acceptance is green with and without SB_VAULT_CANARY, and clippy and fmt are clean.

Verdict: approve
