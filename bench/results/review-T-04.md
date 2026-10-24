# Review T-04 (round 2): core::gate + harness_core::testing (3cd304a, c22ece0)

Scope: crates/core/src/gate.rs, crates/core/src/testing.rs, crates/core/Cargo.toml, crates/core/tests/gate_accept_t04.rs at HEAD (7732264). Spec: H-1, H-1.1..H-1.5, I-3, I-1, D-5, D-19, D-36, D-39, D-109, D-110, D-111, D-113.
Checked against a clean `git archive HEAD` export, so other coders' uncommitted WIP is excluded: `cargo test -p harness-core` with no features runs 5 binaries (lib 64, accept_t06 9, gate_accept_t04 14, ledger_t06 6, vault_accept_t05 9), all passing. `cargo clippy -p harness-core --all-targets -- -D warnings` and `--lib` are clean. `rustfmt --check` on the three files is clean.

## Blocking

- none

## Should fix

1. gate.rs:336-355: `from_hint` is set to true whenever any hint hits, even when a lower-class hint loses to the class level. Example: an Account call with the hint `newsletter` and the rule `('*','Read:newsletter','allow')`. D-113 keeps the class-level ask (the Account default). That ask does not come from a hint rule, so D-110 (4) says it should time out to proceed. The code times it out to skip, and gate.rs:1134-1136 asserts that. The deviation is in the safe direction, but an unrelated allow rule makes Account stricter, and the spec does not say so. Fix one of two ways: set `from_hint` only when the hint's own mode decided the result, or record the stricter reading in D-113.
2. gate.rs:131-152: `rule_domain` lowercases non-ASCII domains but does not run IDNA. A rule stored as `bücher.de` never matches a call domain `xn--bcher-kva.de`, which is the form URL hosts arrive in. That is the same silent dead rule that D-110 (1) set out to remove. Either reject non-ASCII or convert it to punycode. Write a blocker for supreme with a proposed default.
3. testing.rs:125-127: `MemStore::add_contact` still skips the empty-person check that `touch_contact` does. This was round-1 item 5 and it is not fixed. A test can still create a `""` contact that PgStore would refuse.
4. gate_accept_t04.rs:5-6: the header still says to run with `--features testing`, which is out of date after D-111. Also, `t04_send_empty_recipients_asks` (gate_accept_t04.rs:96-103) does not assert the D-113 `on_timeout == Skip`. The unit test gate.rs:760-762 covers it, so this is not blocking.

## Notes

- D-110 (1): holds. `rule_domain`, `rule_hint`, `rule_class_key` and `rule_key` (gate.rs:131-207) are pub as D-113 names them. MemStore `set_rule` stores `rule_key`'s output (testing.rs:194-197). `resolve` canonicalises `call.domain` (gate.rs:318-322) and `hint_hits` canonicalises and dedups hints (gate.rs:382-399). If the domain does not canonicalise, only `'*'` rules apply (D-113). Tests: gate.rs:946-1042, testing.rs rules_are_stored_under_the_canonical_key. core-pg's own check_rule is out of scope.
- D-110 (2): for each hint, the lookup runs C = call.class down to Read, and at each class tries `(domain, C:hint)` then `('*', C:hint)`, first hit only (gate.rs:386-397). Tests: gate.rs:1096-1122.
- D-110 (3): across hints the most restrictive mode wins and strict is OR-ed (gate.rs:357-363), and any hint hit beats the class level. Tests: gate.rs:1044-1094. One combination is untested: a same-class hint `allow` together with a lower-class hint that the class level overrides. That case resolves to the more restrictive mode, which is the safe direction.
- D-113 guard: a lower-class hit is max-ed with the class level and can never lower it (gate.rs:336-347). Test: gate.rs:1124-1140. Challenge rules are still refused on write (rule_key, gate.rs:199-205) and stopped before any lookup (gate.rs:253-262). Rule 3 / D-5 holds.
- D-110 (4) / D-109: `may_proceed` requires no hint, and either Account, or Send with more than 0 new recipients. Strict is OR-ed from the run and the rule (gate.rs:274-281). So an empty-recipient Send asks and skips, and a user ask on Send(known) skips. Test: gate.rs:1142-1176. `on_timeout`/`default_mode` signatures are unchanged.
- D-110 (5): `what` (from the classifier or generated), every value key and value, site and element pass through `card_text`. That covers CSI/OSC/DCS 7- and 8-bit, C0/C1, CR/LF, bidi and zero-width marks. Caps are 200/200/64/253 characters, plus 32 values and one summary row (gate.rs:452-640). An unterminated OSC keeps its text. Test: gate.rs:1178-1225. `StopReason.why` embeds `call.tool` without cleaning (gate.rs:256-258). D-110 (5) covers cards only, and tool names come from the registry, but T-07 should note it.
- MemStore fidelity: timestamps truncated to micros, neighbors ordered (depth, id) then capped, blank recall/search returns empty, and the Invalid checks on node/edge/tool/contact all have tests in testing.rs.
- ScriptedAsker: a queued `Timeout*` that contradicts `card.on_timeout` panics (testing.rs:649-662). Both directions are tested with `should_panic`.
- D-111: a self dev-dependency (Cargo.toml) with no `required-features`. Plain `cargo test -p harness-core` ran all 5 binaries.
- I-3: additions only (`ANY_DOMAIN`, the `rule_*` fns, all named by D-113). No I-3 signature changed. `check` is async.
- Hard rules: no keys or .env. No vault value reaches a return value or a fixture (only the literal `@vault(card)`). No sleeps, no em dashes, no HTML or pixels. The gate runs no side effects. Every `unwrap` is in `#[cfg(test)]`.
- Commit ids: c22ece0 claims H-1.2, H-1.3, D-110 and D-111, and all four are implemented. The D-113 guard shipped in c22ece0 and was recorded afterwards in 7732264.
- Lookup cost: each hint can take up to 2 x (class rank + 1) `gate_rule` calls on PgStore. That costs local round trips, not model turns. A list-rules method would collapse them into one query later.
- T-07: dispatch must pass eTLD+1 as `call.domain`. With `host:port` the code (as D-113 allows) silently skips a user's domain ask rule and falls back to `'*'` rules or the default.

## Verdict

D-110 (1)-(5), D-113 and D-111 are implemented and the tests check real outcomes. There are no hard-rule breaks and I-3 is intact. The should-fix items are a deviation in the safe direction (1), a gap in IDN canonicalisation (2), and small leftovers in tests and the fake (3, 4).

Verdict: approve
