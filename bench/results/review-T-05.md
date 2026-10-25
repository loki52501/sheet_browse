# Review T-05 round 3: core::vault (bed9d02)

Scope:
- Code: `crates/core/src/vault.rs` and `crates/core/tests/{vault_accept_t05,vault_accept_t05_r2,vault_scrub_t05_fix}.rs` at bed9d02.
- Records: D-118 (f2dea11, f00c0b3) and B-150..B-152 (75b89c9).
- Spec: docs/01 H-2, I-4, D-1, D-101, D-102, D-112, D-118, CLAUDE.md rule 2.
- Handoff: `agent-memory/handoffs/T-05-rust-coder-3.yaml`.

Line numbers below are `vault.rs` at bed9d02 unless another file is named.

Checks run (detached checkout of bed9d02, TMP and target under F:/sb-swarm/tmp-core, random `SB_VAULT_CANARY`):
- `cargo test -p harness-core --lib vault::`: 21 passed, 0 ignored.
- `cargo test -p harness-core --test vault_accept_t05 --test vault_accept_t05_r2 --test vault_scrub_t05_fix`: 9, 14 and 11 passed, 0 ignored. r2_4b, r2_4c and r2_4d are now un-ignored and green.
- The canary had 0 hits in the combined output.
- `cargo clippy -p harness-core --lib --test vault_accept_t05 --test vault_accept_t05_r2 --test vault_scrub_t05_fix -- -D warnings` is clean.
- `rustfmt --check` is clean on all 4 files.
- I could not run any scratch probe. The write guard hook blocks writes outside the worktree. The two counterexamples below come from reading the code, step by step.

## Blocking

- none

## Should fix

- **Some values hide inside literals, and `set` accepts them.**
  - Where: the filter at `:627` with `literal_ranges` (`:451-474`), and `check_value` (`:78-86`).
  - How it works: a match is dropped only when its whole range lies inside fixed text. That text is `@vault(`, or `@vault(<existing name>)`, found case-sensitively. So a page cannot use the filter to hide an arbitrary reflected secret. Every wrapping form is still scrubbed: `@vault(<C>)`, `@vault(test<C>)`, `@vault(test)<C>`, `<C>@vault(test)` and `@VAULT(<C>)`. In each one the value lies in the unprotected name part, outside the range, or only overlapping it.
  - What hides: a value that is itself an ASCII-case-insensitive substring of that fixed text.
    - `set("pw", "vault")` passes the floor, and `scrub("@vault(")` then keeps `vault`.
    - `set("zip_94110", "94110")`: every `@vault(zip_94110)`, including the ones scrub writes, keeps `94110`.
  - Why it matters: H-2 says "every vault value found". Names are public through `names()`, so this exposure partly predates D-118. But D-118 (2) now skips such values on purpose.
  - Fix: one check in `set` and `open`. Refuse a value that occurs ASCII-case-insensitively in `@vault(` or in `@vault(n)` for any entry name `n`, which also covers a name that contains a value.
- **D-118 (2) promises more than the code does.** It claims that output "stays parseable by vault_refs and scrub(scrub(s)) == scrub(s)", which also appears in the doc at `:20-23` and `:613-617`. That is false when a value contains `)`, `(` or `@`.
  - Counterexample: entries `name = C` and `k = ")xyz1"`. Five characters, so `set` accepts it.
  - `scrub(C + "xyz1")` gives `@vault(name)xyz1`.
  - Scrubbing that again: `)xyz1` at [11,16) only overlaps the protected range [0,12), so it is kept. The output is `@vault(name@vault(k)`, and `vault_refs` on it gives `BadReference(0)`.
  - This is safe for rule 2, because it over-scrubs. But D-117's ledger re-scrubs scrubbed fields and fails closed when they change. Such a line becomes `Unscrubbable` and the H-3 event is lost.
  - Fix, either way:
    - When a kept match overlaps a protected literal, widen the span to cover the whole literal. The output stays parseable and nothing can hide.
    - Or narrow the claim in D-118 and the docs, and add this case as a test.
- **A short value in an existing file cannot be repaired.** `decrypt_entries` (`:1008-1013`) refuses it, which means:
  - `open` fails.
  - `set` fails too, because `current_locked` re-reads the file under the lock.
  - There is no remove API, so the user has to rebuild the whole vault and re-enter every secret.
  - No real vault predates the floor, so this is cheap to fix now. Let `set(name, …)` replace a short entry with the same name by skipping `check_value` for `name` in the re-read, or log the file-level check as a blocker.

## Notes

- **Round-2 items, all resolved:**
  1. The stale `incompatible_msrv` allows and the 1.85 comment are gone. A grep finds none in `crates/`.
  2. A failed build is now reported. `open` (`:211`) and both `set` paths (`:274`, `:281`) use `try_build`, and `set` builds before `encrypt_entries` and `write_atomic`. The marker is documented as a malformed reference (`:424-429`) and pinned in `failed_build_withholds_output`.
  3. Short values are handled: there is a floor (`:75-86`, counted in characters) and literal protection.
  4. D-102 now points to D-118, and D-118 matches the code.
  5. B-150 lists `default_path`, `default_key`, `MIN_VALUE_CHARS`, `ValueTooShort` and `ScrubUnavailable`.
  6. B-151 (step 2) and B-152 (step 3) have owners and deadlines, and HTML-safe JSON is fixed here.
- **ScrubUnavailable** cannot be reached through the public API. It is covered only by the `Scrubber::checked` unit test, and `vault_accept_t05.rs:409` accepts the variant without requiring it. That is acceptable: aho-corasick's limits are far beyond any vault.
- **The floor covers production.** `Vault::in_memory` (`:227`) is `pub` and exempt from the floor. Today it is used only in tests and in `#[cfg(test)]` (`mcp.rs:1163+`). The production paths use `open`: `harness/src/cli.rs:612` and `harness/src/lib.rs:65`. If a non-test caller ever appears, consider `#[doc(hidden)]` or a cfg/feature gate.
- **New spellings** (`:739-773`, `:812-875`):
  - They match Go `encoding/json` (`<>&`, U+2028/9, lower-case hex), Rails, and the System.Text.Json default encoder (`<>&'"+` and backtick, non-ASCII as UTF-16 `\uXXXX`; upper-case hex is folded by the case-insensitive automaton).
  - The nested forms (JSON in `{:?}`, `{:?}` in JSON, `{:?}` in `{:?}`) are correct, and the hand-written expectations in `html_safe_and_nested_json_debug_spellings` are independent of the code under test.
  - Two negligible gaps, which can go into B-151 or be ignored:
    - System.Text.Json also writes DEL as `\u007F`, but the dotnet style leaves it raw.
    - Go before 1.22 writes `\b` and `\f` as `\u0008` and `\u000c`.
- **Test gap:** the literal-protection tests (`short_values_never_touch_vault_literals`, and `vault_scrub_t05_fix.rs` section 9) use short in-memory values (`@vault(cab)`). Add one assertion with the real canary in the wrapping forms listed under Should fix, so the "cannot hide" property is pinned for a real-length value.
- **Error text:** `ValueTooShort` carries the name only. `mcp.rs:915-918` passes `ToolError::Vault(_)` through unscrubbed, which is fine for name-only variants.
- **I-4:** all wave-0 signatures are unchanged. The additions are only those in B-150. Supreme still has to fold B-150 into `interfaces.yaml`.
- **Spec ids:** H-2, I-4, D-102, D-112 and D-118 are implemented as claimed. D-101 is named, but no D-101 behaviour changed (doc comments only). D-118 first landed inside another coder's commit f2dea11. Say so in the PR description.
- **Hard rules 1 and 3-7:** this change does not touch them. There are no keys, sleeps, challenge code, gate paths or HTML.

## Verdict

Approve:
- No rule break. The literal filter cannot hide a reflected secret, only a value that is a substring of `@vault(` or of an entry's own literal.
- Every round-2 item is resolved.
- The acceptance tests are present, assert the behaviour, and are green with 0 canary hits.

Before step 2:
- Refuse values that sit inside `@vault(` or a name literal.
- Fix or narrow the idempotence claim, since the D-117 ledger depends on it.
- Let `set` repair a short entry.

Verdict: approve
