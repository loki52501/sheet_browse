# Acceptance T-04: gate unit (docs/01-HARNESS-CORE.md H-1, H-1.1..H-1.4; docs/02 touch_contact; I-3)

Worktree `wt/core`, branch `swarm/core`, code under test from commit 3cd304a (`crates/core/src/gate.rs`, `crates/core/src/testing.rs`).
Every row runs through `Gate::check` with `harness_core::testing::MemStore`; rows that use the seeded store also run against `MemStore::empty()` where the row does not depend on a seed rule.
Test file: `crates/core/tests/gate_accept_t04.rs` (needs `--features testing`, D-108). Out of scope here: end-to-end `echo.send` through dispatch/ledger (T-09).

Full run: `SB_VAULT_CANARY=sbcanary-t04-<epoch>-q7x cargo test -p harness-core --features testing --test gate_accept_t04` gives `test result: ok. 14 passed; 0 failed; 0 ignored`.
Also: `cargo test -p harness-core --features testing --lib gate::` gives `11 passed; 0 failed; 0 ignored`; `cargo clippy -p harness-core --features testing --test gate_accept_t04 -- -D warnings` is clean; `rustfmt --check` on the test file is clean.

| item | command | result | note |
|---|---|---|---|
| H-1.1 Send to a recipient in contacts -> Allow | `cargo test -p harness-core --features testing --test gate_accept_t04 t04_send_known_recipient_allows` | pass | `test t04_send_known_recipient_allows ... ok`; seeded and empty store |
| H-1.1 Send to an unknown recipient -> Ask, on_timeout Proceed | `cargo test -p harness-core --features testing --test gate_accept_t04 t04_send_unknown_recipient_asks_proceed` | pass | `... ok`; also one unknown among known recipients asks Proceed |
| H-1.1 Send with empty recipients -> Ask | `cargo test -p harness-core --features testing --test gate_accept_t04 t04_send_empty_recipients_asks` | pass | `... ok`; card class Send |
| H-1.1/H-1.3 Pay -> Ask, on_timeout Skip | `cargo test -p harness-core --features testing --test gate_accept_t04 t04_pay_asks_skip` | pass | `... ok`; seeded and empty store |
| H-1.1/H-1.3 Delete -> Ask, on_timeout Skip | `cargo test -p harness-core --features testing --test gate_accept_t04 t04_delete_asks_skip` | pass | `... ok` |
| H-1.1/H-1.3 Account -> Ask, on_timeout Proceed | `cargo test -p harness-core --features testing --test gate_accept_t04 t04_account_asks_proceed` | pass | `... ok` |
| H-1.1 Read/Fill/Navigate/Submit -> Allow | `cargo test -p harness-core --features testing --test gate_accept_t04 t04_read_fill_navigate_submit_allow` | pass | `... ok`; all four classes, both stores |
| H-1.4/D-5 Challenge -> Stop even with ('*','Challenge','allow') force-written | `cargo test -p harness-core --features testing --test gate_accept_t04 t04_challenge_stops_despite_forced_allow_rule` | pass | `... ok`; forced row verified present via `gate_rule`; also domain-scoped forced allow; Stop carries class Challenge and element `#31`; domain=None also stops |
| H-1.2 ('bank.com','Submit','ask') asks on bank.com, other domains allow | `cargo test -p harness-core --features testing --test gate_accept_t04 t04_domain_submit_rule_scoped_to_domain` | pass | `... ok`; shop.example, notbank.com, bank.org all Allow |
| H-1.2/D-39 Submit with hint job_application -> Ask | `cargo test -p harness-core --features testing --test gate_accept_t04 t04_submit_job_application_hint_asks` | pass | `... ok`; without hint Allow; with hint Ask, on_timeout Skip (B-100 default) |
| H-1.3 strict run -> every ask times out Skip | `cargo test -p harness-core --features testing --test gate_accept_t04 t04_strict_run_every_ask_times_out_skip` | pass | `... ok`; Send new, Send empty, Pay, Account, Delete, bank.com Submit rule, job_application Submit |
| H-1.3 strict rule -> Skip | `cargo test -p harness-core --features testing --test gate_accept_t04 t04_strict_rule_times_out_skip` | pass | `... ok`; strict Send/Account/Pay rules skip; Account on a domain without the rule still Proceed |
| docs/02 touch_contact(person) then Send to that person -> Allow | `cargo test -p harness-core --features testing --test gate_accept_t04 t04_touch_contact_then_send_allows` | pass | `... ok`; Ask before, Allow after `touch_contact` via the `MemoryStore` trait (case-differing key) |
| H-1.3 ask card carries class, site, what, values, 60 s timeout | `SB_VAULT_CANARY=sbcanary-t04-<epoch>-q7x cargo test -p harness-core --features testing --test gate_accept_t04 t04_ask_card_contents` | pass | `... ok`; built card: values include `("card","@vault(card)")`, timeout == 60 s; Send card names recipient; classifier what/values used as given; canary absent from serialized and Debug card (gate holds no vault, so this is a sanity check, not a full vault flow) |

Verdict: green
