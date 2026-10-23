# Acceptance: T-03 (core-pg MemoryStore on Postgres)

Spec: docs/02-MEMORY.md Acceptance (step 1 items), docs/01-HARNESS-CORE.md H-3, D-110, D-5.
Code under test: commit ea1befb (crates/core-pg). Tests: crates/core-pg/tests/accept_t03.rs (added by tester),
corroborated by crates/core-pg/tests/pg.rs (coder's suite, also 7/7 ok).
Database: every test creates its own throwaway `sb_t_a03_<random>` database on 127.0.0.1:5433 and drops it
(D-100); the shared `sb` database was not touched. No B-102 recovery-mode event during these runs.
No `sb_t_a03_*` database left behind afterwards (`sb_t_hunt_q03` is another area's).
Out of scope: the watch-schedule item (T-16, step 4); the gate half of the touch_contact item (T-04/T-09).
No vault items in T-03 scope, so no canary run.

Command for every row (run from F:\sb-swarm\wt\core):
`cargo test -p core-pg --test accept_t03 -- --nocapture --test-threads=2` -> `test result: ok. 7 passed; 0 failed; 0 ignored`

| item | command | result | note |
|---|---|---|---|
| remember + recall("Rust CDP crate") returns the chromiumoxide fact with why=both; query lacks "chromiumoxide" (docs/02) | `cargo test -p core-pg --test accept_t03 a03_recall_chromiumoxide_why_both` | pass | `a03 recall: top="chromiumoxide" why=Both score=0.032786883`; ranked first among 3 unrelated facts |
| 3-hop neighbors returns roles reachable from a person via met_at -> attended -> works_at (docs/02) | `cargo test -p core-pg --test accept_t03 a03_neighbors_three_hops_reach_role` | pass | depth 3 from Priya: `[event "Rust Bangalore meetup", person "Ravi", role "Staff Engineer, Acme"]`; a 4-hop role is excluded, start node excluded |
| grade_review SM-2: grade 5 roughly multiplies interval by ease; grade < 3 resets to 1 day (docs/02) | `cargo test -p core-pg --test accept_t03 a03_grade_review_sm2` | pass | `a03 sm2: 16 * 2.7999997 = 44.799995, got 45`; grades 0, 1, 2 each reset interval to 1.0 and due to now+24h (+/-0.5h) |
| UPDATE ledger.events fails, DELETE and TRUNCATE fail (docs/01 H-3) | `cargo test -p core-pg --test accept_t03 a03_ledger_update_delete_truncate_fail` | pass | update by id, update all, delete by id, delete all, truncate, truncate restart identity cascade: each -> `ledger is append-only`, both via the store pool and a direct connection; row unchanged after |
| touch_contact then contact() returns it (docs/02, store half) | `cargo test -p core-pg --test accept_t03 a03_touch_contact_then_contact` | pass | `Contact { person: "ravi@example.com", channel: Some("email"), count: 1, note: Some("meetup"), last: Some(..) }`; contact() was None before |
| gate_rule finds the seeded rules; gate_rule_set rejects a Challenge allow (docs/02 seeds, H-1.2, D-5) | `cargo test -p core-pg --test accept_t03 a03_seeded_rules_and_challenge_allow_rejected` | pass | seeds `*`/Pay, Account, Delete, Submit:job_application, Submit:signature = ask; Challenge = stop. Allow and Ask on Challenge rejected in 7 spellings (`challenge`, ` CHALLENGE `, `Example.COM.`, `Challenge:turnstile`, `challenge:hcaptcha`...) -> `Invalid("a Challenge rule must be stop (D-5)")`; no non-stop Challenge row in gate.rules; Challenge stop still accepted |
| gate_rule_set('Bank.COM.','submit',ask) is found by gate_rule('bank.com','Submit') (D-110) | `cargo test -p core-pg --test accept_t03 a03_d110_canonical_keys` | pass | `gate_rule(bank.com, Submit) -> Some(GateRule { mode: Ask, strict: false })`; stored key is `("bank.com","Submit")` |

Verdict: green
