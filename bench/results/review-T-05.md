# Review T-05 round 4: core::vault (fc23dd2)

Scope:
- Code: `git diff a4f3224 fc23dd2 -- crates/core` (vault.rs, tests vault_accept_t05.rs, vault_accept_t05_r3.rs, vault_scrub_t05_fix.rs). crates/core is unchanged from fc23dd2 to HEAD 15e09d3.
- Callers: `crates/harness/src/cli.rs`, `crates/harness/src/lib.rs`, `crates/core-pg/src/lib.rs`.
- Records: D-76, D-118, interfaces.yaml I-4. fc23dd2 carried no handoff.
- Line numbers are `crates/core/src/vault.rs` at 15e09d3 unless another file is named.

Checks run (detached worktree of 15e09d3, own target dir, random `SB_VAULT_CANARY`):
- `cargo test -p harness-core --lib vault::`: 25 passed.
- `--test vault_accept_t05 / _r2 / _r3 / vault_scrub_t05_fix`: 9 / 14 / 9 (+2 ignored) / 16 passed.
- `--test vault_accept_t05_r3 -- --ignored`: r3_2 and r3_5 fail as announced (premise changed).
- `cargo clippy -p harness-core --all-targets -- -D warnings`: clean. `rustfmt --check` on all 5 vault files: clean.
- Canary: 0 hits in the combined output.

## Blocking

- **VaultError is not `#[non_exhaustive]`** (`:57-58`). D-76 and the I-4 notes in interfaces.yaml both say it is. This round adds a variant (`ValueInLiteral`, `:79-82`), which is exactly the breakage D-76 guards against. Fix: add the attribute. Integration tests are separate crates, so the exhaustive match in `tests/vault_accept_t05.rs:412-425` then needs a `_` arm. That is a tester change.

## Should fix

- **Round-3 item 3 is fixed in the library only. The user still cannot repair a vault.** `harness vault set` opens the vault with `Vault::open` (`crates/harness/src/cli.rs:612`), and `open` refuses a file with a broken entry. Both error texts tell the user to run `harness vault set <name>` (`:71-72`, `:79-80`), and that command fails with the same error, so the user goes round in a loop. The module doc (`:33-34`) says `open_for_set` "opens such a file so that command can repair it", but no caller uses it.
  - Fix: use `Vault::open_for_set` in `vault_set`. The process never installs that handle globally, so this is safe.
  - Add a CLI test: a file with a short entry, `vault set` on that entry, then `open` succeeds.
- **D-118 no longer matches the code. Amend it as follows:**
  - In (2), replace "and a match that only overlaps a literal is kept, so a value cannot hide behind one" with: "a match that only overlaps a literal is kept and widened to cover the whole literal; scrub repeats until a pass changes nothing (at most 8 passes, after which the whole output is withheld as the SCRUB_WITHHELD marker), so a literal scrub wrote cannot complete another value".
  - In (2), narrow "scrub output stays parseable by vault_refs" to "the @vault(name) literals of existing entries in the input stay parseable". Counterexample to the broad claim: the input `@vault(xxhunter22yy)`, with `pw = hunter22`, is a well-formed reference to an unknown name. It scrubs to `@vault(xx@vault(pw)yy)`, which gives `BadReference(0)`. That result is idempotent and leaks nothing, but it does not parse. The module doc `:22-24` needs the same narrowing.
  - Add a (3b): "set and open also refuse a non-empty value any of whose spellings occurs, ignoring ASCII case, inside `@vault(` or inside `@vault(n)` for any entry name n (VaultError::ValueInLiteral { name }, naming the entry whose value hides). Both errors name only the entry and point to `harness vault set <name>`. Vault::open_for_set skips these rules so that command can repair a file; set on that handle (and on in_memory) checks only the entry being set, and set on a handle from open checks every entry of the re-read file."
  - Fix (4) to match: the withheld marker is no longer only in_memory's backstop.
- **I-4 additions without a blocker.** `Vault::open_for_set` (`:297`) and `VaultError::ValueInLiteral` are new public API. They are not in interfaces.yaml, and no `interface_changes` B- entry asks supreme for them (B-150 was the earlier precedent). File one.
- **The pass cap is untested, and its marker text is misleading** (`:742-749`, `:787`). Nothing exercises hitting `MAX_SCRUB_PASSES`. Text built from a value C followed by the tail of `k = ")xyz1"` repeated 8 or more times (`C xyz1 xyz1 ...` written without spaces) costs one pass per repeat, so it hits the cap. The output is then withheld as "vault scrubber unavailable", which is wrong in this case. It fails closed, so nothing leaks.
  - Add a test for the cap.
  - Consider giving this case its own marker text, e.g. `@vault(?) [output withheld: scrub did not settle]`.

## Notes

- **Round-3 Should-fix status:**
  1. Values inside literals: **resolved.**
     - `check_entries` (`:127-164`) checks every spelling of every value, ignoring ASCII case, against `@vault(` and against every name's literal. Names with empty values are included, which is stricter than the scrubber's `names`.
     - It runs before the lock on the handle's entries and again on the re-read file (`:375-395`).
     - Tests cover `vault`, `AULT(`, `(test)`, `zip_94110`, the KELVIN SIGN, and a new name that would swallow an existing value.
  2. D-118 (2) idempotence when a value contains `( ) @`: **resolved.**
     - Widening (`:791-804`) leaves no literal fragment, and the fixed-point loop catches a literal that completes another value.
     - r3_6 is green.
     - The 3000-case property test in `vault_scrub_t05_fix.rs` checks no leak, the fixed point, and that known refs still parse.
     - Termination: each changing pass either removes non-literal text or merges two or more literals into one.
     - Parseability holds only in the narrowed sense above.
  3. Repairing a short value: library only. See Should fix.
- **New ValueTooShort text:** `vault entry "x" is shorter than 4 characters; ...; give it a longer value with \`harness vault set x\``. It names only the key (twice) and the floor, never the value. The r3_2 fixed string just needs updating.
- **ValueInLiteral** carries the name only. When it names a different entry than the one being set, the caller learns that entry's value is a substring of the chosen literal. The only caller is the user's own CLI, and scrub already gives the same membership oracle, so this adds no exposure.
- **Leak paths:**
  - `strict` is not in `Debug`.
  - No new log, panic or return path carries a value.
  - All new `unwrap`s are in `#[cfg(test)]`.
  - `open_for_set`'s scrubber is documented as never to be installed globally, and no caller does.
  - `mcp.rs` still passes `ToolError::Vault` through unscrubbed. That is fine for name-only variants.
- **Round-3 test gap** (wrapping forms with the real canary): closed by `canary_wrapped_in_literals_is_scrubbed_to_a_fixed_point`.
- **Cost:** any scrub with a match now takes one extra confirming pass. Text with no match still takes one pass.
- **Spec ids:** H-2 and D-118 behaviour is implemented, but D-118's text is stale (see above). I-4 is claimed, but `non_exhaustive` is missing and the additions are unfiled. Behaviour beyond the spec: `open_for_set` and the strict/only check modes.
- **Process:** fc23dd2 has no handoff, so this review's handoff records the state.
- **Hard rules 1 and 3-7:** this change does not touch them. There are no keys, sleeps, challenge code, gate paths or HTML. The error strings are not user drafts.

## Verdict

Request changes:
- One blocking item: `#[non_exhaustive]` on VaultError (D-76, I-4). It is a one-line fix plus a `_` arm in the row-5 test.
- The scrub and leak logic is sound, and round-3 items 1 and 2 are resolved and tested.
- Before merge, also:
  - wire `open_for_set` into `harness vault set`, so the repair hint works;
  - amend D-118 as written above;
  - file the I-4 additions.

Verdict: request changes
