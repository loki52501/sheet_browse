# Review T-06 round 2: ledger, run folder, report.md (5bf1f37, f2dea11, 60217eb)

Scope: crates/core/src/ledger.rs, crates/core/src/run.rs, crates/core/tests/{ledger_t06,
ledger_t06_fix, accept_t06_r2, accept_t06_r2_year}.rs, D-117. Line numbers refer to 60217eb.
The checks ran in a detached worktree at 60217eb (CARGO_TARGET_DIR under F:/sb-swarm/tmp-core):
- `--lib ledger:: run::`: 22 passed.
- ledger_t06: 6 passed. ledger_t06_fix: 5. accept_t06: 9. accept_t06_r2: 11. accept_t06_r2_year: 1.
- None are ignored and none fail.
- `cargo clippy -D warnings` is clean on `--lib` and on the five test targets.
- `--lib --profile test` fails only at crates/core/src/testing.rs:667
  (`items_after_test_module`). That file belongs to another coder. With that one lint
  allowed, the ledger/run test modules are clean.
- `rustfmt --check` is clean on the changed files.

## Blocking

- none

## Should fix

- crates/core/src/ledger.rs:833-837 (Q-4, "stop listed once" hides a distinct stop): the
  dedupe key is `(tool, args)` over the whole run, but H-1.4 lets the user resume a stopped
  run ("tell the model to continue"). Failure case: `act.click {"ref":"9"}` is stopped by
  hCaptcha. The user clears it, the model retries the same call, and Turnstile stops it
  again. report.md then shows one `stopped at act.click`, carrying the old why. Result at
  :903-906 also takes the *first* core stop's why, so the reason the run is stopped now is
  missing from the report. Fix: dedupe only within one call. A tool stop event closes the
  group (core stop from ToolCtx::ask, tool stop, trailing core stop). Or reset the key
  when a non-stop event of the same `(tool, args)` lies between two stops. Add a test with
  two stops of the same call that have different whys.
- crates/core/src/ledger.rs:820-832 (Q-4, second way a stop is hidden): the tool stop event
  is skipped when *any* core stop has the same `tool`, whatever its args. Failure case:
  1. `act.click {ref:9}` gets a gate Stop (one core stop).
  2. Later, `act.click {ref:12}` is challenge-stopped. Its tool stop event is written, but
     the trailing `stop_event` write fails. mcp.rs:812-814 `write_or_diag` only logs that,
     e.g. on a transient store error or an `Unscrubbable` `element`.
  3. Stop 2 never appears under Not done.
  Fix: build `core_stops` from `(tool, args)` pairs, the same key that `listed` uses.
- crates/core/src/ledger.rs:204-207: the doc says `run_id` "is left alone ... the
  serialized check catches anything else". Since D-117, `check_clean` (:272-306) does not
  check `run_id`, so nothing catches it. Fix the comment (see the run_id note below).
- D-117 (agent-memory/decisions.yaml, f2dea11): coder-level decision that narrows hard rule 2
  ("outgoing strings are scrubbed") for the jsonl line's harness fields. The design is sound
  (see Notes), but because it carves into a hard rule, supreme should ratify it. Carry-over
  from round 1: the report event-shape contract (Read heuristic, stop grouping, core modes
  ignored) lives only in the ledger.rs header (:10-43). It is not yet in decisions.yaml, so
  T-07 and T-09 are not bound by it.

## Notes

- **D-117 leak analysis.** No path found for a value that came from outside to reach the
  table, ledger.jsonl or report.md.
  - All six data fields are scrubbed to a fixpoint first (:208-260), numbers included.
  - Each data field is then checked serialized exactly as in the line (:280-293), so
    key/value glue inside `args`/`consequences` is still refused. This is tested in
    ledger_t06_fix.rs:154 (GLUE).
  - The table row is the same scrubbed `e` (:158).
  - Exempt fields:
    - `class`, `mode` and `ok` are enums or bool.
    - `ts` and `duration_ms` are set by the wrapper. Ledger::write's only callers are in
      mcp.rs (:279, :293, :730, :823). So these fields can match a vault value only by
      coincidence, e.g. a "2026" PIN in `ts`.
    - Cross-field glue needs a vault value that contains a top-level key name. That is
      contrived.
  - **run_id caveat.** `run_id` is not harness-pure: its slug comes from the prompt. It
    was scrubbed in `slugify` (run.rs:187-200) against the vault as it was when the run
    was created. A value added later (`vault set` mid-run), or a tool server with a
    different vault, puts the slug unchecked into every row and line. The folder name
    already holds it, so this is accepted, but D-117 should say so.
  - **Postgres caveat.** jsonb text output re-spaces and reorders keys (`{"a": "b"}`). A
    value that contains JSON syntax could match the PG text form when it did not match
    the serde form. That too is contrived.
- report.md: every input goes through `clip`. `clip` scrubs, collapses whitespace,
  scrubs again, then cuts (:550-560). The prompt is scrubbed whole before its first line
  is taken (:892). The final whole-text scrub stays (:998). Harness fields in the report
  (`ts` as `%H:%M:%S`, run id) are covered by that final scrub, at worst by
  over-redaction.
- `is_failure` now excludes core events (:450-452). No core event is written with
  `ok=false` today (mcp.rs:313-325 defaults to `ok: true`). A future core repair/forge
  event with `ok=false` (H-3 lists them as core events) would be silent in the report and
  in `run_status`. That is fine as long as it stays documented (:42).
- Q-5 "fallback taken" is a heuristic: the next non-core event (:858-862), which may be
  unrelated to the failed step. Acceptable until T-07 records an explicit fallback.
- **Round-1 items:**
  - Blocking, numbers: resolved (:243-251; test ledger_t06_fix.rs:98).
  - Blocking, clip-before-scrub: resolved (:550-560, :892; test ledger_t06_fix.rs:190).
  - Should fix, cross-process order: resolved by sorting by id (:399); two-Ledger test at
    ledger_t06_fix.rs:265.
  - Should fix, `none captured` and fallback: resolved (:863-870).
  - Should fix, CORE_SERVER, `other` bucket and contract: resolved in code (:63,
    :797-800, :10-43). decisions.yaml is still missing, see above.
  - Should fix, ledger_t06.rs:280 `.ok()`: resolved (:280-283 unwraps with
    `run_id = None`).
  - Should fix, run_id scrub: WrongRun now scrubs it (:147). The comment is stale, see
    above.
  - Should fix, D-111 required-features: resolved (crates/core/Cargo.toml:37 self
    dev-dependency).
- Acceptance tests for the round-2 claims assert real behaviour:
  - year PIN written and still scrubbed in args: accept_t06_r2_year.rs:65.
  - mid-call challenge counted once: accept_t06_r2.rs:625. The why text is asserted in
    ledger.rs:1366.
  - a distinct-args stop is still listed: ledger.rs:1384.
- Spec ids: H-3, H-4, Q-4, Q-5, D-102 and D-117 are claimed and implemented. Rules 3-7:
  no change in scope. Library code has no unwrap on external input and no sleeps.
- Interface fidelity: neither `LedgerEvent` nor `MemoryStore` changed. `ledger`/`run`
  are area-owned and not in interfaces.yaml.

## Verdict

No rule break and no missing acceptance test. The two stop-dedupe gaps can hide a second,
distinct stop in report.md, and both have small fixes. D-117 holds up for every
data field.

Verdict: approve
