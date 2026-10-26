# Review T-05 round 6: core::vault marker words (3f6fea5; at 4201aae)

Scope: `git show 3f6fea5` (crates/core/src/vault.rs, D-118 (3b) in agent-memory/decisions.yaml). This round answers review-T-05 round 5. D-118 as amended in place is binding.

Runs, in a detached worktree at 4201aae with its own target dir, `SB_VAULT_CANARY=sbcanary-<hex>-x`:
- `cargo test -p harness-core -p core-pg --no-fail-fast`: exit 0. lib 127 (vault tests included, `set_and_open_refuse_values_inside_the_withheld_markers` passes), vault_accept_t05 9, _r2 14, _r3 11, _r4 5, vault_scrub_t05_fix 16. Canary: 0 hits in the output. The panic line from `global_scrub_before_and_after_install` is that test's intended panic, and the test reports ok.
- `cargo clippy -p harness-core -p core-pg --all-targets -- -D warnings`: clean. `cargo fmt --check`: clean.
- Cargo.lock: not touched by 3f6fea5.

## Round-5 items

- Should fix (a value inside a withheld marker breaks idempotence): resolved.
  - `check_entries` seeds its literal list with `REF_OPEN`, `SCRUB_WITHHELD` and `SCRUB_UNSETTLED` (vault.rs:149). So every spelling of every value is checked, ignoring ASCII case, against both marker texts, on `set` and on `open`. The error is the existing `ValueInLiteral { name }`, which names the entry only.
  - Test `set_and_open_refuse_values_inside_the_withheld_markers` (vault.rs:1855):
    - refused on `set`, each naming `w`: "withheld", "OUTPUT WITHHELD", "put withh", "Scrubber Unavailable", "did not settle", "settle]", "[output";
    - a refused set leaves the vault unchanged;
    - near misses "settled" and "outputs" are accepted, and both markers then scrub to themselves;
    - `open` on a file holding "Not Settle" fails with the repair hint naming `note`.
  - Removing the two markers from line 149 fails the first loop, so the test fails if the fix is broken.
  - D-118 (3b) now names both markers, so (2)'s `scrub(scrub(s)) == scrub(s)` holds without the exception.

## Blocking

- none

## Should fix

- none

## Notes

- The new refusal covers ordinary words that may be real values: "output", "with", "held", "able", "vault", "scrub", "settle". The refusal names the entry and says how to fix it, and these words are weak secrets. This is the cost D-118 accepted and is acceptable.
- vault.rs:34: the module doc line runs to about 135 columns. rustfmt does not wrap comments, so fmt passes. Cosmetic.
- Round-5 imprecisions in the module doc and D-118 (3b) ("checks only the entry being set" for `Check::Only`) were not reworded. They are carried as notes and do not block.
- Hard rules: no value reaches a return, log, panic or fixture. The error text is a constant plus the entry name. No keys or sleeps.
- Commit ids: H-2 and D-118 are claimed and implemented, and the decision text matches the code. There is no unclaimed behaviour. I-4 signatures are unchanged (no new variant).

## Verdict

The one round-5 should-fix is resolved with a test that fails when the fix is removed, and D-118 now matches the code with no exception.

Verdict: approve
