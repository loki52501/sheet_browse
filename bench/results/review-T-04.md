# Review T-04 (round 3): core::gate + harness_core::testing (bae6c45; at 15e09d3)

Scope: `git diff main...15e09d3 -- crates/core/src/gate.rs crates/core/src/testing.rs` (round-3 delta 7732264..15e09d3, which includes tests/gate_accept_t04.rs). Spec: H-1, H-1.1..H-1.4, I-3, I-1. Decisions checked: D-5, D-62, D-70, D-79, D-81, D-109, D-110, D-111, D-113, D-115, D-120. Blockers checked: B-190, B-191.

Runs, in a detached worktree at 15e09d3 with its own target dir:
- `cargo clippy -p harness-core --tests -- -D warnings`: clean (exit 0, clippy 0.1.99).
- `cargo clippy -p harness-core --all-targets --all-features -- -D warnings`: clean.
- `cargo fmt -p harness-core --check`: clean.
- `cargo test -p harness-core`: lib 123, gate_accept_t04 14, gate_accept_t04_r2 7, and every other binary pass.

The reported clippy failure ("items after a test module" in crates/core/src/testing.rs) does not reproduce at 15e09d3. `mod tests` is the last item (testing.rs:820-1479). It most likely came from uncommitted WIP in the shared checkout.

## Round-2 items

- Should fix 1 (`from_hint` set by a losing lower-class hint): resolved per D-120 (4).
  - gate.rs:523-536: when the class level is stricter, the hit takes the class-level mode and name with `from_hint = false`.
  - gate.rs:553-555: a tie keeps `from_hint`.
  - Test `a_hint_overruled_by_the_class_level_does_not_force_skip` covers four cases: Account/Send(new) under `('*','Read:newsletter','allow')` proceed, a tying hint skips, a raising hint skips.
- Should fix 2 (no IDNA): resolved by the D-120 subset (B-190). Changes:
  - The full stops U+3002/U+FF0E/U+FF61 and full-width ASCII are mapped.
  - Each label is Punycoded (RFC 3492, overflow-checked).
  - Invisible, compatibility and combining input is refused.
  - Tests: `unicode_domains_and_hints_are_canonical_or_rejected` and `punycode_matches_rfc_3492` (5 RFC vectors). Over PgStore: accept_t03.rs:347-362.
  - Gaps remain; see Should fix 1.
- Should fix 3 (`add_contact("")`): resolved. testing.rs:130-134 asserts a non-blank `contact_key`.
- Should fix 4 (stale header, missing `on_timeout` check): resolved. gate_accept_t04.rs:4-6 now names D-111, and :103-104 asserts `OnTimeout::Skip`.
- Note (StopReason embeds an uncleaned tool name): resolved. gate.rs:424 uses `card_text(&call.tool, 128)`, tested in `escapes_cannot_span_card_components`.

## Blocking

- none

## Should fix

1. crates/core/src/gate.rs:150-155 and :207-229: `rule_domain` still stores some keys that no URL host carries, which contradicts D-120 (1) ("refused, never mapped"). I checked each case with rustc 1.99:
   - `str::to_lowercase` applies Final_Sigma, so `ΒΑΣ.gr` becomes `βας`, but UTS-46 maps Σ to σ (`βασ`).
   - U+1E9E becomes `ß`, but UTS-46 maps it to `ss`.
   - `compatibility_form` misses most CJK compatibility ideographs (U+F900..U+FAFF, U+2F800..U+2FA1F, which NFC maps) and the Hangul compatibility jamo (U+3131..U+318E, which NFKC maps). All of them are alphanumeric, so they are accepted and Punycoded as written.

   Each case is the silent dead user rule D-110 (1) exists to prevent. The inputs are rare, so this is not blocking. Fix: lowercase per char (`char::to_lowercase`), refuse U+1E9E (or map it to `ss`), and add the ranges above to `compatibility_form`. Or take the `idna` crate as B-190 allows. Add these strings to the refusal or mapping tests.
2. agent-memory/interfaces.yaml:28 (I-3 `rust:`): the list does not name `ANY_DOMAIN`, `rule_domain`, `rule_hint`, `rule_class_key` or `rule_key`. D-113 makes them the one canonicaliser, and core-pg now calls them (crates/core-pg/src/lib.rs:778, :908), so their behaviour is contract in fact. D-81/B-191 records the behaviour but not the names. File an `interface_changes` entry so supreme adds them to I-3 and lists core-pg as a consumer.

## Notes

- D-120 (3) / D-81, the Send row (gate.rs:845-906, 908-942):
  - `values[0]` is always the gate's `new recipients (N)` row.
  - Blank recipients are dropped (gate.rs:431-436).
  - New recipients are deduped by `contact_key`, first spelling kept (gate.rs:625-639).
  - The caps are 32 recipients, 2000 characters, and a head + `…(+K chars)…` + `@domain` form for a recipient over 120 characters.
  - `complete` requires every new recipient to be listed and `clean == r.trim()`. `may_proceed` requires `complete` (gate.rs:451-459).
  - Tests: `send_card_lists_every_new_recipient` and `blank_recipients_are_not_recipients`. Both assert the row text and `on_timeout`.
- Two spoof shapes the user can still see. Neither changes the decision, because the gate's own row stays first and keeps the true count:
  - A classifier key with a homoglyph (Cyrillic `е` in `nеw recipients (0)`) passes the drop filter at gate.rs:951.
  - One recipient string holding `a@x.com, b@y.com` is shown as written. It counts as one recipient and can proceed.
- D-120 (2): `what_line` cleans every component before joining (gate.rs:826-843). This closes the OSC-across-recipients trick, and the test asserts both addresses survive and the card skips. `invisible` covers Default_Ignorable plus U+2800 (gate.rs:699-721).
- D-110 (2)(3), D-113: unchanged apart from (4) above. A same-class hint `allow` combined with a lower-class hint that the class level raises still resolves to the class-level ask, which is the safe direction. Challenge stops before any lookup (gate.rs:419-428), and `rule_key` refuses a non-stop Challenge rule, hinted or not (gate.rs:351-357). D-5 holds.
- D-109 / D-110 (4): `on_timeout` and `default_mode` keep their signatures. An ask proceeds only for a non-strict Account, or a non-strict Send(new) with a complete row, when no hint decided.
- MemStore parity (testing.rs):
  - `gate_rule` canonicalises its lookup like PgStore.
  - `micros` truncates toward zero from 2000-01-01 as sqlx does, including the `since`/`now` bounds.
  - `search` ports pg_trgm `word_similarity` with threshold 0.6 and matches PgStore's score (5/7) in both suites.
  - SM-2 uses `ceil_days`.
  - Each item has a regression test.
- Hard rules:
  - No keys and no sleeps.
  - No vault value reaches a card. Args are scrubbed upstream, and only `@vault(...)` literals appear.
  - No HTML or pixels.
  - Non-test `unwrap`/`panic!` appear only in the ScriptedAsker test double (testing.rs:809).
  - The `…` markers are U+2026, not em dashes, and cards are not user-sent drafts.
- Commit ids: bae6c45 claims H-1, H-1.1..H-1.4, I-1, I-3, D-62, D-70, D-109, D-110, D-113, D-115 and D-120. All are implemented. `clean_card` is public as I-3 lists it (D-70). There is no unclaimed behaviour.
- Lookup cost:
  - Each distinct recipient takes one `contact` round trip.
  - Each hint can take up to 2 x (class rank + 1) `gate_rule` calls.
  - These are local round trips, not model turns.

## Verdict

All round-2 should-fix items are resolved, and the D-120 behaviour is tested against real outcomes. There are no hard-rule breaks, I-3 signatures are intact, and clippy, fmt and the tests are green. What is left is a set of rare IDN mapping gaps and recording the canonicaliser names in I-3.

Verdict: approve
