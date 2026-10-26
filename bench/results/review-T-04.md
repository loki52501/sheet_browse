# Review T-04 (round 4): core::gate rule_domain via idna + MemStore cut-off (b829add; at 4201aae)

Scope: `git show b829add` (crates/core/src/gate.rs, testing.rs, Cargo.toml, Cargo.lock, tests/gate_accept_t04_r3.rs, and the core-pg lines). Spec: H-1.2, I-3. Decisions checked: D-110, D-113, D-120, D-125 (binding), D-126 (binding), B-190, D-81.

Runs, in a detached worktree at 4201aae with its own target dir, compose Postgres on 127.0.0.1:5433, `SB_VAULT_CANARY=sbcanary-<hex>-x`:
- `cargo test -p harness-core -p core-pg --no-fail-fast`: exit 0. lib 127, gate_accept_t04 14, gate_accept_t04_r2 7, gate_accept_t04_r3 20 (0 ignored), and every other binary passes.
- `cargo clippy -p harness-core -p core-pg --all-targets -- -D warnings`: clean. `cargo fmt --check`: clean.
- Cargo.lock: b829add adds one line, `"idna"` under harness-core's dependencies. idna 1.1.0 was already locked through `url`. No other lock change in the three commits.

## Round-3 items

- Should fix 1 (IDN mapping gaps): resolved by D-126.
  - `rule_domain` (gate.rs:146) is `Uts46::to_ascii(.., AsciiDenyList::URL, Hyphens::Allow, DnsLength::Ignore)` (gate.rs:156). That is the same call `idna::domain_to_ascii` makes, which is the WHATWG URL host path, so the stored key is the browser's host.
  - What idna refuses is refused (`map_err(|_| invalid())`).
  - After mapping, every label must be non-empty, at most 63 bytes, and only `a-z 0-9 - _` (gate.rs:167-173). So `*.bank.com`, `a!b.com` and empty labels stay refused, and the 253 limit holds. `*` alone still returns `*`.
  - No hand-written mapper is left. `map_host_char`, `host_char`, `compatibility_form` and `punycode` are deleted, and grep finds none of them or any `to_lowercase` in the domain path.
  - Test `d126_stored_key_is_the_idna_a_label` (gate.rs:1667) covers ΒΑΣ.gr (σ, not ς), U+1E9E, U+F900, U+3131, example.ΑΣ, ſ, ĳ, U+1D2C and a decomposed ü. Each is checked against a hard-coded A-label and against `idna::domain_to_ascii`, for idempotence, and for a rule written that way stopping the A-label call. ZWJ, U+202E, `xn--zz` and a tag character are refused. Reverting to a lowercase-only mapper fails the hard-coded rows.
  - The tester's `r3_b190_an_accepted_domain_is_its_uts46_a_label` is un-ignored and passes.
- Should fix 2 (I-3 names): filed as an interface change (agent-memory/interfaces.yaml:230). It waits on supreme. Nothing is owed by the coder.
- D-125 in MemStore: `WORD_SIMILARITY_THRESHOLD = 0.5` (testing.rs:263). `search_follows_d115` asserts 'Vasnth' hits at 0.5, and the tester's `r3_d115_vasnth_typo_inside_longer_subject_hits` is un-ignored and passes.

## Tester refusal-list edits (judged)

gate_accept_t04_r3.rs:153 `r3_rule_domain_refuses_what_it_cannot_map_safely`. Each removed entry, as UTS-46 (nontransitional, what Chrome, Firefox and the `url` crate apply) treats it:
- `bücher.de`: NFC gives bücher, so the host is xn--bcher-kva.de. Justified.
- U+200B, U+00AD, U+2060: status `ignored`, so the host is bank.com. Justified.
- U+2800: status `valid`, so the host is xn--bank-yj5b.com, a different host. Justified: a rule written with it now matches that exact host. A rule meant for bank.com, though, silently does not (see Notes).
- U+FB01 to fi, U+00B2 to 2, U+1D41B to b, U+24D1 to b: `mapped`. Justified.
- U+FF8A U+FF9F: maps to ハ plus U+309A, and NFC makes パ, which is valid. Justified.

r3_gate_rule_set_stores_rule_key_and_gate_matches_idn_hosts (gate_accept_t04_r3.rs:297) and core-pg accept_t03_r3.rs:589: the decomposed, ZWSP, fi and math-bold entries were replaced by U+202E and U+200D. These are justified for the same reasons.

What remains still tests refusal:
- the ASCII structure: empty, dots, port, `*`, space, `@`, `/`, and lengths, including a Punycode label over 63;
- two real UTS-46 refusals: U+200D (CheckJoiners) and U+202E (disallowed).

The coder also added `xn--zz`, a tag character, U+E0041 and `%2e` in gate.rs. The removals are correct. What the tester file has lost is breadth of UTS-46 refusal classes and a positive assertion for the removed inputs (see Should fix 1).

## Blocking

- none

## Should fix

1. crates/core/tests/gate_accept_t04_r3.rs:165-181 (tester-owned): the tester's refused list now holds only two UTS-46 refusal classes. Ask the tester to:
   - add a CheckBidi failure (an LTR label holding a Hebrew letter, for example `a\u{5d0}.com`), a label starting with a combining mark (`\u{301}bank.com`, CheckMarks), and a disallowed symbol (U+FFFD or U+2028);
   - move the removed entries into a positive "maps to" assertion. Today only the coder's gate.rs unit test pins them.
   The coder edited tester assertions. The edit was justified by binding D-126 and disclosed in the commit body and handoff, but a tester must still confirm it.

## Notes

- Invisible-but-valid code points: U+2800 (and any other code point UTS-46 calls valid but that renders blank) now produce a key for a lookalike host instead of a refusal, as D-120 did before. This is what D-126 asks for, and it is right when a user copies a phishing host into a stop rule. A rule typed to mean bank.com would not apply to bank.com, and the stored key, shown as `xn--bank-yj5b.com`, reveals that. If supreme wants it, refusing D-120's invisible set after mapping is compatible with D-126, which only says that idna refusals are refused.
- `resolve` (gate.rs:380-384) canonicalises `call.domain` through the same `rule_domain`, and a refused host falls back to `*` rules. That is unchanged, and consistent because a refused host can never have a stored rule.
- IPv4-looking names (`0x7f.1`) are stored as written, while the WHATWG parser would turn them into `127.0.0.1`. This predates the round and no realistic rule hits it.
- D-81 and interfaces.yaml:224 still say "refuses non-mappable Unicode". That is still true, since idna's refusals are refused. Supreme may want the I-3 note to cite D-126.
- Hard rules: no keys or sleeps, no HTML, and no vault path touched. The error text holds the user's own domain, not a vault value, and is scrubbed upstream as before. There is no non-test `unwrap`.
- Commit ids: H-1.2, D-120, D-125 and D-126 are claimed and implemented. There is no unclaimed behaviour. The I-3 `rule_domain` signature is unchanged.

## Verdict

The round-3 IDN should-fix is fully resolved by delegating to idna, as D-126 requires. The hand-written mapper is gone, the label restriction still refuses wildcards and punctuation, and the tests pin real A-labels. Each refusal-list removal follows UTS-46. The tester list needs broader refusal classes, but that is tester-owned and not a rule break.

Verdict: approve
