# Acceptance T-04, round 2: gate unit (docs/01-HARNESS-CORE.md H-1, H-1.1..H-1.5; I-3; D-5, D-39, D-109, D-110, D-111, D-113)

Worktree `wt/core`, branch `swarm/core`. Code under test is at commit c22ece0 (`crates/core/src/gate.rs`, `crates/core/src/testing.rs`, `crates/core/Cargo.toml`), the fix for the round-1 findings.
Every row goes through `Gate::check` with `harness_core::testing::MemStore`. Most rows run on both the seeded store (`MemStore::new`) and the empty one (`MemStore::empty`).
Round-1 rows: `crates/core/tests/gate_accept_t04.rs`, unchanged.
D-110/D-113 rows: `crates/core/tests/gate_accept_t04_r2.rs`, new. Its rules are written through the `MemoryStore::gate_rule_set` trait method. Its timeouts are played out with `ScriptedAsker::new()` (no scripted answer), which returns the outcome the card's `on_timeout` field gives.

Full run, plain and with no `--features` (D-111): `SB_VAULT_CANARY=sbcanary-t04r2-<epoch>-z9k cargo test -p harness-core` reports these binaries:
- lib: 83 passed
- `gate_accept_t04`: 14 passed
- `gate_accept_t04_r2`: 7 passed
- `accept_t06`: 9 passed
- `ledger_t06`: 6 passed
- `vault_accept_t05`: 9 passed
- `vault_scrub_t05_fix`: 9 passed

Every binary reported 0 failed and 0 ignored.
The canary appears 0 times in the captured stdout/stderr (`grep -c`). `runs/` does not exist in the worktree, so it was not grepped.
`cargo clippy -p harness-core --test gate_accept_t04_r2 -- -D warnings` is clean, and `rustfmt --edition 2021 --check` on the new test file is clean.
Out of scope:
- end-to-end `echo.send` through dispatch and the ledger (T-09)
- contact_key Unicode folding (B-101)
- core-pg's own `check_rule` (separate T-03 fix)

| item | command | result | note |
|---|---|---|---|
| H-1.1 Send to a recipient in contacts -> Allow | `cargo test -p harness-core --test gate_accept_t04 t04_send_known_recipient_allows` | pass | `test t04_send_known_recipient_allows ... ok`; seeded and empty store |
| H-1.1 Send to an unknown recipient -> Ask, on_timeout Proceed | `cargo test -p harness-core --test gate_accept_t04 t04_send_unknown_recipient_asks_proceed` | pass | `... ok`; one unknown among known recipients also asks Proceed |
| H-1.1 Send with empty recipients -> Ask | `cargo test -p harness-core --test gate_accept_t04 t04_send_empty_recipients_asks` | pass | `... ok`; card class Send |
| H-1.1/H-1.3 Pay -> Ask, on_timeout Skip | `cargo test -p harness-core --test gate_accept_t04 t04_pay_asks_skip` | pass | `... ok`; seeded and empty store |
| H-1.1/H-1.3 Delete -> Ask, on_timeout Skip | `cargo test -p harness-core --test gate_accept_t04 t04_delete_asks_skip` | pass | `... ok` |
| H-1.1/H-1.3 Account -> Ask, on_timeout Proceed | `cargo test -p harness-core --test gate_accept_t04 t04_account_asks_proceed` | pass | `... ok` |
| H-1.1 Read/Fill/Navigate/Submit -> Allow | `cargo test -p harness-core --test gate_accept_t04 t04_read_fill_navigate_submit_allow` | pass | `... ok`; all four classes, both stores |
| H-1.4/D-5 Challenge -> Stop even with ('*','Challenge','allow') force-written | `cargo test -p harness-core --test gate_accept_t04 t04_challenge_stops_despite_forced_allow_rule` | pass | `... ok`; the forced row is present per `gate_rule`; a domain-scoped forced allow also stops; Stop carries class Challenge and element `#31`; domain=None also stops |
| H-1.2 ('bank.com','Submit','ask') asks on bank.com; other domains allow | `cargo test -p harness-core --test gate_accept_t04 t04_domain_submit_rule_scoped_to_domain` | pass | `... ok`; shop.example, notbank.com and bank.org all Allow |
| H-1.2/D-39 Submit with hint job_application -> Ask | `cargo test -p harness-core --test gate_accept_t04 t04_submit_job_application_hint_asks` | pass | `... ok`; Allow without the hint; with the hint Ask, on_timeout Skip (D-109) |
| H-1.3 strict run -> every ask times out Skip | `cargo test -p harness-core --test gate_accept_t04 t04_strict_run_every_ask_times_out_skip` | pass | `... ok`; covers Send new, Send empty, Pay, Account, Delete, the bank.com Submit rule and job_application Submit |
| H-1.3 strict rule -> Skip | `cargo test -p harness-core --test gate_accept_t04 t04_strict_rule_times_out_skip` | pass | `... ok`; strict Send/Account/Pay rules skip; Account on a domain without the rule still Proceeds |
| docs/02 touch_contact(person) then Send to that person -> Allow | `cargo test -p harness-core --test gate_accept_t04 t04_touch_contact_then_send_allows` | pass | `... ok`; Ask before, Allow after `touch_contact` (key differs in case) |
| H-1.3 ask card has class, site, what, values and a 60 s timeout | `SB_VAULT_CANARY=sbcanary-t04r2-<epoch>-z9k cargo test -p harness-core --test gate_accept_t04 t04_ask_card_contents` | pass | `... ok`; values include `("card","@vault(card)")`; timeout == 60 s; the Send card names the recipient; canary absent from the serialized and Debug card |
| D-110 (1) gate_rule_set('Bank.COM.','submit',ask), then Submit on bank.com asks | `cargo test -p harness-core --test gate_accept_t04_r2 r2_gate_rule_set_noncanonical_domain_and_class_still_applies` | pass | `... ok`; stored as `gate_rule("bank.com","Submit") == Some(ask)`; Ask with on_timeout Skip and TimeoutSkip; call domains `BANK.com`, `bank.com.` and ` Bank.Com ` also ask; other domains Allow; `gate_rule_set` rejects `bank.com:443`, empty domain, `Sbumit`, `Submit:` and `Submit:job application`; Challenge/`challenge`/`Challenge:x` allow rules are rejected (D-5); both stores |
| D-110 (3) hints ['newsletter','job_application'] with ('*','Submit:newsletter','allow') still ask | `cargo test -p harness-core --test gate_accept_t04_r2 r2_unrelated_hint_allow_does_not_undo_job_application` | pass | `... ok`; Ask and TimeoutSkip in both hint orders, also with a domain-scoped `Submit:newsletter` allow; newsletter alone gives Allow; hint ` Job_Application ` is canonicalised and asks; both stores |
| D-110 (2)(4) job_application hint on an Account call asks and times out to skip | `cargo test -p harness-core --test gate_accept_t04_r2 r2_job_application_on_account_asks_and_times_out_skip` | pass | `... ok`; card class Account, on_timeout Skip, TimeoutSkip; the same Account call without the hint still Proceeds; both stores |
| D-113 ('*','Read:newsletter','allow') does not lower a Pay call hinted newsletter | `cargo test -p harness-core --test gate_accept_t04_r2 r2_lower_class_hint_allow_does_not_lower_higher_class` | pass | `... ok`; Pay gives Ask, Skip, TimeoutSkip; Account, Delete and Send(new) hinted newsletter still Ask; a domain-scoped `Read:newsletter` allow does not lower either; Read hinted newsletter gives Allow; both stores |
| D-110 (4) a user ask rule on Send to a known contact times out to skip | `cargo test -p harness-core --test gate_accept_t04_r2 r2_user_ask_rule_on_send_to_known_contact_times_out_skip` | pass | `... ok`; Allow before the rule; after `gate_rule_set(mail.example or '*','Send',ask)` the result is Ask, on_timeout Skip, TimeoutSkip; both stores |
| H-1.3/D-113 Send(new) and Account asks time out to proceed; Send with no recipients times out to skip | `cargo test -p harness-core --test gate_accept_t04_r2 r2_send_new_and_account_time_out_proceed` | pass | `... ok`; Send(new) and mixed known+new each give Proceed and TimeoutProceed; Account gives Proceed and TimeoutProceed; Send([]) gives Ask with Skip and TimeoutSkip; both stores |
| D-110 (5) a card what with ESC/CR is sanitised | `SB_VAULT_CANARY=sbcanary-t04r2-<epoch>-z9k cargo test -p harness-core --test gate_accept_t04_r2 r2_card_what_with_esc_and_cr_is_sanitised` | pass | `... ok`; a classifier `what` with CSI and CR keeps both visible phrases, with no control chars or `[2K`/`[32m` left; values and keys with CR, 8-bit CSI and U+202E are clean; `@vault(card)` is kept; a `what` that cleans to empty falls back to the line with `shop.checkout`; args-built values are clean; a 5000-char what is capped at <= 200 chars; a Send recipient with CR/ESC gives a clean card; canary absent from the serialized and Debug card |

Verdict: green
