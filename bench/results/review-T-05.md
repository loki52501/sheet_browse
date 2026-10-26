# Review T-05 round 5: core::vault (5cec6ef)

Scope:
- Code: `git diff 5cec6ef~1 5cec6ef`. Files: `crates/core/src/vault.rs`, `crates/core/tests/vault_accept_t05.rs`, D-118 in `agent-memory/decisions.yaml`, B-194 in `agent-memory/blockers.yaml`, and handoff `T-05-rust-coder-5.yaml`.
- This round answers review-T-05 round 4.
- Out of scope, as briefed:
  - `harness vault set` using `open_for_set`, which the T-08 coder owns.
  - The r3_2 and r3_5 ignores, which the tester owns.
- Line numbers refer to `crates/core/src/vault.rs` at 5cec6ef unless another file is named.

Checks run (detached worktree at 5cec6ef, own target dir, unique `SB_VAULT_CANARY`):
- `cargo clippy -p harness-core --all-targets -- -D warnings`: clean.
- `cargo check --workspace --all-targets`: clean. `#[non_exhaustive]` breaks no downstream match. The only one outside vault.rs is `mcp.rs:1136`, which matches a single variant.
- `cargo test -p harness-core --lib vault::`: 26 passed. The new test `scrub_pass_cap_withholds_unsettled_output` passes on its own.
- Test targets:
  - `vault_accept_t05`: 9 passed.
  - `vault_accept_t05_r2`: 14 passed.
  - `vault_accept_t05_r3`: 9 passed, 2 ignored (r3_2 and r3_5, owned by the tester).
  - `vault_scrub_t05_fix`: 16 passed.
- `rustfmt --edition 2021 --check` on both changed Rust files: clean.
- `blockers.yaml` and `decisions.yaml` both parse as YAML.
- Canary: 0 hits across all test output, with `--nocapture`.

## Blocking

- none

## Should fix

- **The idempotence claim has one exception: a vault value that occurs inside a marker's text.**
  - Where: D-118 (2), the module doc at `:25`, and `:553`, `:558`, `:763`.
  - `scrub` returns `SCRUB_UNSETTLED` (and the in_memory backstop returns `SCRUB_WITHHELD`) verbatim, without checking it against the vault.
  - Example: an entry with value `settle`, `output` or `withheld` passes every D-118 rule. A cap-hitting input then returns a string that holds that value's bytes, and `scrub(marker) != marker`.
  - Effects:
    - D-118's unconditional `scrub(scrub(s)) == scrub(s)` is false in this case.
    - The D-ledger rule (`decisions.yaml:331`) would refuse a line that carries the marker.
  - The marker is a constant, so nothing about the value leaks. That is why this is not blocking.
  - The test asserts `scrub(SCRUB_UNSETTLED) == SCRUB_UNSETTLED` only for its own two entries (`:1680`).
  - Cheapest fix: extend (3b) so that `check_entries` also refuses a value any of whose spellings occurs, ignoring ASCII case, in either marker. Add one test for it, and add "or inside a withheld marker" to D-118 (3b).
  - Alternative: narrow D-118 (2) and the module doc to name the exception.

## Notes

Round-4 items:
- **Blocking (`#[non_exhaustive]`): resolved** (`:66`).
  - `vault_accept_t05.rs:425` gained `_ => "other"`. The "variant not exercised" list still names every current variant, including `ValueInLiteral`.
  - Side effect: the `_` arm means a future variant no longer fails to compile in that test. An exhaustive match in a vault.rs unit test (same crate, so `non_exhaustive` does not apply) would restore that tripwire. Optional.
- **D-118 amendment: resolved.** I checked the text against the code:
  - (2) Widening matches `widen_over_literals`. The pass cap matches `:751-763`: the first pass plus at most 7 more is 8 passes, counting the confirming pass. The withheld text is exactly `SCRUB_UNSETTLED`. Parseability is narrowed and gives the `@vault(xxhunter22yy)` counterexample.
  - (3b) The literal list holds every entry name, valued or not (`:146-147`). `Only` versus `All` matches `set` (`:376-404`). Both error texts name only the entry and point to `harness vault set <name>`.
  - (4) The two markers and their triggers match the code.
  - The coder deviated from my round-4 wording by naming the cap marker by its own text instead of SCRUB_WITHHELD. That is correct, because round 5 gave the cap its own marker.
  - Imprecision, not wrong:
    - (3b), B-194 and the module doc `:39-40` say `set` on an `open_for_set` or in_memory handle "checks only the entry being set". `Check::Only` also checks every other value against the new name's literal (`:148-151`, `:157`), so the error can name a different entry. The `Check::Only` doc comment (`:115-117`) says this correctly. Consider "checks only the rules involving the entry being set".
    - The module doc `:41` says "The withheld marker" (singular), but there are now two distinct texts.
    - The `MAX_SCRUB_PASSES` doc (`:799-801`) says "Ordinary text takes 1, a value completed by a literal takes 2 or 3". Counting the confirming pass, text holding one plain value takes 2 and a completion takes 3. Text with no value takes 1.
- **interface_changes: resolved.** B-194 (`blockers.yaml:155`, unique at HEAD) files `Vault::open_for_set` and `VaultError::ValueInLiteral { name }` as additive I-4 items and records `non_exhaustive`. interfaces.yaml I-4 still lacks both items, which is expected until supreme accepts B-194.
- **Pass cap test: resolved** (`:1652-1682`). The test asserts:
  - the exact boundary: 6 repeats settle, 7, 8 and 64 are withheld;
  - the exact marker text, and that it holds neither value;
  - `vault_refs` and `resolve_into` give `BadReference(0)`, and the sink is never called;
  - the marker is a fixed point for that vault, and it differs from the failed-build marker.
  - The coder's handoff says it was proven red once.

SCRUB_UNSETTLED safety (asked in the brief):
- It is a `const`, built from nothing derived from a value. Apart from the Should-fix collision above, it holds no value.
- It cannot become a resolvable reference:
  - `?` fails `is_name_byte`, so `parse_refs` fails at byte 0 for any template that contains the marker. `resolve_into` parses before it looks anything up, so the sink never runs.
  - `set` cannot create an entry named `?` (`invalid_name_at`). Even a hand-edited file with that name would not help, because the parser rejects `?` whatever the entries are.
  - A template that embeds the marker next to valid refs fails as a whole, which is fail closed.

Other notes:
- Only adversarial text reaches the cap: values that start with `)` or end with `@`, repeated 7 or more times. The cost is the whole output, which fails closed. That is acceptable.
- Leak paths are unchanged from round 4:
  - No new log, panic or return path carries a value.
  - `strict` is not in `Debug`.
  - The new `unwrap`s are confined to `#[cfg(test)]`.
- Carried from round 4, out of scope here:
  - `harness vault set` must call `Vault::open_for_set`, plus a CLI repair test (T-08 coder). Until then the repair hint in both errors loops for the user.
  - r3_2 and r3_5 stay ignored (tester). HEAD also has tester commit 0790269 (T-05 round 4 acceptance), which I did not review here.
- Spec ids:
  - H-2 and D-118 are implemented, and D-118 now matches the code apart from the Should-fix exception.
  - I-4 and D-76: `non_exhaustive` is present, and the additions are filed as B-194.
  - No behaviour goes beyond the spec, now that the strict and only modes are in D-118 (3b).
- Hard rules 1 and 3-7: this change does not touch them. There are no keys, sleeps, challenge code, gate paths or HTML, and the error and marker strings are not user drafts.

## Verdict

Approve:
- The round-4 Blocking item and all three in-scope Should-fix items are resolved and tested.
- D-118 and the module doc match the code.
- The new marker cannot resolve and carries no value-derived data.
- Before merge, either refuse values that occur in the withheld markers or narrow the idempotence claim. Wiring the CLI to `open_for_set` remains owned by T-08.

Verdict: approve
