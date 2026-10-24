# Review T-06: ledger + jsonl mirror, run folder, report.md (commit 2e91956)

Scope: crates/core/src/ledger.rs, crates/core/src/run.rs, crates/core/src/lib.rs,
crates/core/Cargo.toml, crates/core/tests/ledger_t06.rs.
Checked: `cargo test -p harness-core --features testing --lib --test ledger_t06` passes
(49 lib + 6 integration), clippy `-D warnings` clean on lib and ledger_t06, and rustfmt
--check is clean on the three files. (`tests/accept_t06.rs`, an untracked tester file that
is not part of this commit, currently fails to compile at :324. That is not a T-06 code
issue, but it blocks a plain `cargo test -p harness-core --features testing`.)

## Blocking

- crates/core/src/ledger.rs:164 (rule 2, scrub bypass): `scrub_value` passes `Value::Number`
  through unchanged (`other => other`), so the store and ledger.jsonl both keep a numeric
  vault value as it is: a personal entry like a phone number, zip, card number or PIN sent
  as a JSON number in `args` or `consequences`. The serialized line at :111 is never
  scrubbed as a whole, so nothing catches it later. Fix: scrub the number's text and turn
  it into a String if it changed. Then, before append, check that `scrub(line) == line`
  and fail closed if it does not. Add a test with a numeric canary.
- crates/core/src/ledger.rs:298-306, 432, 327, 344, 358 (rule 2, scrub bypass in
  report.md): `clip()` collapses whitespace and cuts to N chars *before* the single
  `scrub(&text)` at :584. Two failure modes: (a) a vault value with a double space, tab
  or newline gets collapsed into a spelling that scrub no longer matches, so it leaks
  whole; (b) a value that crosses the 97/117-char cut leaks its prefix. The `prompt` passed
  to `report`/`write_report` is raw user text (only prompt.md is scrubbed), and `report`
  is `pub` over arbitrary events. Fix: scrub each input (prompt line, why,
  result_summary, target, consequence) before clip/normalise, and keep the final scrub.
  Add a test with a long prompt and a canary that contains whitespace.

## Should fix

- crates/core/src/ledger.rs:56-62, 109-116 (H-3 row-for-row across processes): the order
  mutex is per `Ledger`, but the doc says each tool server process holds its own Ledger
  on the same run. Two processes can insert ids 5 and 6 and then append 6 before 5.
  `read_jsonl` returns file order, while `ledger_events` is ordered by id, so the T-09
  `jsonl == table` comparison fails intermittently. Fix: sort `read_jsonl` output by id
  (or document that the comparison sorts), and add a two-Ledger test on one run.
- crates/core/src/ledger.rs:340-343, 566-573 (Q-5): a failed step with no consequences
  prints `consequence: ok` (asserted at :758). Use `none captured` instead. Q-5 also asks
  for "the fallback taken", and the event-shape contract has no slot for it, so T-07
  cannot record one. Name the field (e.g. `consequences.fallback`, or `why` suffix) in
  the header contract.
- crates/core/src/ledger.rs:7-18, 276-296 (T-07 contract): `Read:` counts come from
  substring heuristics on server/tool names (`contains("web")`, `contains("page")`,
  token `sb`...). A Read tool that does not match (srv-cli, memory.recall) silently
  disappears from both counts and refs. `"core"` is a bare literal and no const is
  exported. The contract also does not say "exactly one stop event per stop" (a core Stop
  plus a tool-call event with mode=stop is listed twice in Not done). It does not say that
  core `allow`/`ask` decision events are ignored. Fix: export `pub const CORE_SERVER`,
  write the shapes plus the name heuristic into decisions.yaml so that T-07 and T-09 are
  bound by it, and add a fallback bucket (e.g. count unknown reads as pages, or `n other`).
- crates/core/tests/ledger_t06.rs:280: `ledger.write(fetch.clone()).await.ok()` swallows a
  `WrongRun` (fetch carries run a's id), so case (b) never contains the fetch it intends
  to test. Use `.unwrap()` with `fetch.run_id = None`.
- crates/core/src/ledger.rs:145-153: `run_id` is the one string field `scrub_event`
  skips, although the doc comment says "every string field". This is harmless today,
  because a mismatched run id is rejected and a matching one came from a scrubbed slug,
  but either scrub it or fix the comment.
- crates/core/Cargo.toml (`[[test]] required-features = ["testing"]`): the acceptance
  binary is silently skipped by `cargo test --workspace`. D-111 replaces this with a self
  dev-dependency, and another coder is on it. Do not merge until that lands.

## Notes

- H-3 order is correct: the DB insert happens first (:110), then the jsonl append (:115),
  both under the lock. The id is carried in the line. ts is truncated to micros with no
  re-stamp, which matches ts_now/Postgres, and the integration test compares full
  `(id, event)` equality over 25 rows and 80 concurrent writes. The row-for-row check runs
  against MemStore only. The Postgres check belongs to T-09.
- If the insert succeeds and the append fails, the result is a permanent table/jsonl
  mismatch (documented at :93-94). T-07 should surface this error, not swallow it.
- H-4 matches: the folder layout and the UTC `%Y-%m-%dT%H-%M-%S-<slug>` id. `create_dir`
  (not `_all`) claims a name atomically, collisions get `-2`, `-3`, and both the slug and
  prompt.md are scrubbed (the slug is scrubbed before it is truncated, so no partial
  value). A slug that itself ends in `-2` makes ids ambiguous to parse, but they stay
  unique. A failure after `create_dir` leaves a partial folder behind.
- RunFolder::open (run.rs:78-102) correctly rejects empty, `.`, `..`, `/`, `\`, `:` and
  multi-component ids. It does not reject Windows reserved or trailing-dot/space names
  (`CON`, `abc.`), and `is_dir` follows a junction or symlink out of `root`. That is low
  risk, because ids come from `create` or from the session.
- Files: line (ledger.rs:381-411): `..` components are rejected and the marker must sit
  after a separator, so another run's `<id>-2/files/` does not match. These strings are
  only rendered and never opened. `walk_files` uses `file_type()` (no symlink following),
  so it cannot loop. `s.find` only looks at the first marker occurrence.
- Q-4 shape: all seven lines match the spec in order. The fixed plurals ("1 pages") are
  intentional. `run_status` marks a run partial when any step fails, even if a later
  fallback succeeded. Q-5 may want that distinguished.
- Interface fidelity: `ledger`/`RunFolder` are not in interfaces.yaml (area-owned). The
  commit does not touch `LedgerEvent`, `RunId` or `MemoryStore`. Q-4/H-3/H-4/D-102 are
  claimed and implemented. Q-5 is referenced only in a comment and is partial (see
  above).
- Rules 3, 4, 5, 6 and 7 are not applicable or clean: library code has no sleeps and no
  unwrap/expect, and adds no keys, captcha logic or side effects outside the gate.
  Blocking std I/O under the tokio mutex has a comment saying why.

## Verdict

Two rule-2 bypasses: numbers in the ledger, and clip-before-scrub in report.md. Both are
small fixes, and each needs a canary test.

Verdict: request changes
