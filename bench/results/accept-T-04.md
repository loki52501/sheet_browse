# Acceptance T-04, round 3: gate unit (docs/01-HARNESS-CORE.md H-1, H-1.1..H-1.5; I-3; D-5, D-70, D-109, D-110, D-113, D-115, D-120, B-190, B-191)

Code under test: swarm/core at 15e09d3, which holds bae6c45. That commit changed `crates/core/src/gate.rs` (IDNA rule domains, ASCII hints, the Send new-recipients row, per-component card cleaning, public `clean_card`) and `crates/core/src/testing.rs` (MemStore parity with PgStore).
Run in a detached worktree F:/sb-swarm/tmp-core/wt-accept-t0304 at 15e09d3, with `CARGO_TARGET_DIR=F:/sb-swarm/tmp-core/target-accept-t0304`. Every command below runs from that worktree.
`SB_VAULT_CANARY` was a fresh `sbcanary-...` string for every run. The final run used `sbcanary-final0304-<epoch>-r9z`.

Every gate row goes through `Gate::check` with `harness_core::testing::MemStore`. Rules are written through the `MemoryStore::gate_rule_set` trait method, and timeouts are played out with `ScriptedAsker::new()`.

Test files:
- Round-1 rows: `crates/core/tests/gate_accept_t04.rs`. bae6c45 changed only its header comment and added one D-113 assertion (`Send([])` times out to skip).
- Round-2 rows: `crates/core/tests/gate_accept_t04_r2.rs`, unchanged.
- Round-3 rows: `crates/core/tests/gate_accept_t04_r3.rs`, new.
- The same behaviours over PgStore are in accept-T-03 (`accept_t03_r3.rs`).

Full run, `cargo test -p harness-core` (no `--features`, D-111):
- lib: 123 passed
- gate_accept_t04: 14 passed
- gate_accept_t04_r2: 7 passed
- gate_accept_t04_r3: 18 passed, 2 ignored (the red-first rows below)
- other binaries (T-05/T-06/T-07): 0 failed. Their 3 ignored tests belong to T-05/T-07, not this task.

The two ignored tests, run with `-- --ignored`: `test result: FAILED. 0 passed; 2 failed`.
`cargo clippy -p harness-core --test gate_accept_t04_r3 -- -D warnings` is clean, and `rustfmt --edition 2021 --check` on the new file is clean.
Vault canary: `grep -c` of the canary in the captured stdout/stderr of every run is 0. `runs/` does not exist in the worktree. The gate writes no ledger or jsonl (T-06/T-09).

Out of scope, not in the table:
- the end-to-end docs/01 items: echo.send through dispatch and the ledger, the vault fixture, ledger.jsonl row-for-row, `harness schedule add` (T-09);
- contact_key Unicode folding (B-101).

| item | command | result | note |
|---|---|---|---|
| H-1.1 Send to a recipient in contacts -> Allow | `cargo test -p harness-core --test gate_accept_t04 t04_send_known_recipient_allows` | pass | `test t04_send_known_recipient_allows ... ok`; seeded and empty store |
| H-1.1 Send to an unknown recipient -> Ask, on_timeout Proceed | `cargo test -p harness-core --test gate_accept_t04 t04_send_unknown_recipient_asks_proceed` | pass | `... ok` |
| H-1.1/D-113 Send with empty recipients -> Ask, times out to skip | `cargo test -p harness-core --test gate_accept_t04 t04_send_empty_recipients_asks` | pass | `... ok`; card class Send, on_timeout Skip |
| H-1.1/H-1.3 Pay -> Ask, on_timeout Skip | `cargo test -p harness-core --test gate_accept_t04 t04_pay_asks_skip` | pass | `... ok` |
| H-1.1/H-1.3 Delete -> Ask, on_timeout Skip | `cargo test -p harness-core --test gate_accept_t04 t04_delete_asks_skip` | pass | `... ok` |
| H-1.1/H-1.3 Account -> Ask, on_timeout Proceed | `cargo test -p harness-core --test gate_accept_t04 t04_account_asks_proceed` | pass | `... ok` |
| H-1.1 Read/Fill/Navigate/Submit -> Allow | `cargo test -p harness-core --test gate_accept_t04 t04_read_fill_navigate_submit_allow` | pass | `... ok` |
| H-1.4/D-5 Challenge -> Stop even with ('*','Challenge','allow') force-written | `cargo test -p harness-core --test gate_accept_t04 t04_challenge_stops_despite_forced_allow_rule` | pass | `... ok` |
| H-1.2 ('bank.com','Submit','ask') asks on bank.com only | `cargo test -p harness-core --test gate_accept_t04 t04_domain_submit_rule_scoped_to_domain` | pass | `... ok` |
| H-1.2/D-39 Submit hinted job_application -> Ask, Skip | `cargo test -p harness-core --test gate_accept_t04 t04_submit_job_application_hint_asks` | pass | `... ok` |
| H-1.3 strict run -> every ask times out Skip | `cargo test -p harness-core --test gate_accept_t04 t04_strict_run_every_ask_times_out_skip` | pass | `... ok` |
| H-1.3 strict rule -> Skip | `cargo test -p harness-core --test gate_accept_t04 t04_strict_rule_times_out_skip` | pass | `... ok` |
| docs/02 touch_contact(person) then Send to that person -> Allow | `cargo test -p harness-core --test gate_accept_t04 t04_touch_contact_then_send_allows` | pass | `... ok` (PgStore version in accept-T-03) |
| H-1.3 ask card has class, site, what, values and a 60 s timeout; canary absent | `SB_VAULT_CANARY=sbcanary-... cargo test -p harness-core --test gate_accept_t04 t04_ask_card_contents` | pass | `... ok`; `@vault(card)` kept, canary absent from serialized and Debug card |
| D-110 (1) non-canonical gate_rule_set still applies | `cargo test -p harness-core --test gate_accept_t04_r2 r2_gate_rule_set_noncanonical_domain_and_class_still_applies` | pass | `... ok` |
| D-110 (3) an unrelated hint allow does not undo job_application | `cargo test -p harness-core --test gate_accept_t04_r2 r2_unrelated_hint_allow_does_not_undo_job_application` | pass | `... ok` |
| D-110 (2)(4) job_application on Account asks and skips | `cargo test -p harness-core --test gate_accept_t04_r2 r2_job_application_on_account_asks_and_times_out_skip` | pass | `... ok` |
| D-113 ('*','Read:newsletter','allow') does not lower a higher class | `cargo test -p harness-core --test gate_accept_t04_r2 r2_lower_class_hint_allow_does_not_lower_higher_class` | pass | `... ok` |
| D-110 (4) user ask rule on Send to a known contact skips | `cargo test -p harness-core --test gate_accept_t04_r2 r2_user_ask_rule_on_send_to_known_contact_times_out_skip` | pass | `... ok` |
| H-1.3/D-113 Send(new) and Account proceed, Send([]) skips | `cargo test -p harness-core --test gate_accept_t04_r2 r2_send_new_and_account_time_out_proceed` | pass | `... ok` |
| D-110 (5) a card what with ESC/CR is sanitised; canary absent | `SB_VAULT_CANARY=sbcanary-... cargo test -p harness-core --test gate_accept_t04_r2 r2_card_what_with_esc_and_cr_is_sanitised` | pass | `... ok` |
| D-120 (1) rule_domain returns the A-label: trim, U+3002/U+FF0E/U+FF61, full-width ASCII, lowercase, one trailing dot, Punycode per label | `cargo test -p harness-core --test gate_accept_t04_r3 r3_rule_domain_maps_to_the_a_label` | pass | `BÜCHER.DE -> xn--bcher-kva.de`, `例え.テスト -> xn--r8jz45g.xn--zckzah`, RFC 3492 samples L, M, O, D match; full-width `ＢＡＮＫ.ＣＯＭ -> bank.com`; idempotent on its output |
| D-120 (1) rule_domain refuses decomposed, default-ignorable, compatibility and malformed input; labels <= 63, names <= 253 | `cargo test -p harness-core --test gate_accept_t04_r3 r3_rule_domain_refuses_what_it_cannot_map_safely` | pass | 24 refused inputs (combining U+0308, ZWSP, soft hyphen, ZWJ, word joiner, U+2800, U+FB01, superscript, math bold, circled, half-width katakana, U+202E, `..`, host:port, `*.x`...); 63/253 accepted, 64/254 and a Punycode label > 63 refused; KELVIN SIGN gives `kelvin.com` (its UTS-46 mapping) |
| B-190/D-120 (1) a domain the subset accepts is the A-label a URL host carries (UTS-46 non-transitional) | `cargo test -p harness-core --test gate_accept_t04_r3 r3_b190_an_accepted_domain_is_its_uts46_a_label -- --ignored --nocapture` | fail | `"ſhop.example" -> Ok("xn--hop-f3a.example"), URL host is "shop.example"`; `"ĳssel.nl" -> Ok("xn--ssel-uza.nl")` vs `ijssel.nl`; `"bᴬnk.example" -> Ok("xn--bnk-57x.example")` vs `bank.example`; `"example.ΑΣ" -> Ok("example.xn--mxa8a")` vs `example.xn--mxa0b`. `str::to_lowercase` turns a label-final Σ into ς (UTS-46 maps Σ to σ), and U+017F, U+0133 and U+1D2C have compatibility mappings outside the blocks `compatibility_form` lists. So these rules are stored, yet dead: the silent dead rule D-110 (1) and D-120 meant to close, and the B-190 promise "no stored key changes" breaks. Missing: refuse (or map) every character with a UTS-46 mapping, e.g. refuse a non-ASCII char unless `c.to_lowercase()` is itself and it has no compatibility decomposition, and lowercase without final-sigma context. Committed as `#[ignore = "red-first T-04 round 4: ..."]` |
| D-120 (1) rule_hint accepts only ASCII a-z 0-9 _ - . (KELVIN SIGN and full-width refused) | `cargo test -p harness-core --test gate_accept_t04_r3 r3_rule_hint_is_ascii_only` | pass | `" Job_Application " -> job_application`; 11 refused hints incl. U+212A, full-width, combining, ESC; `rule_class_key("Submit:\u{212a}elvin")` refused |
| D-113/D-120 every gate_rule_set backend stores rule_key's output; MemStore gate_rule canonicalises its lookup; the gate matches IDN hosts | `cargo test -p harness-core --test gate_accept_t04_r3 r3_gate_rule_set_stores_rule_key_and_gate_matches_idn_hosts` | pass | `rule_key("BÜCHER.DE","send") == ("xn--bcher-kva.de","Send")`; `gate_rule("Bücher.de.","SEND")` hits; call domains `xn--bcher-kva.de`, `BÜCHER.de` and `bücher。de` hit the rule, `buecher.de` does not; `BANK．COM` hits a bank.com stop; refused rules not stored; both stores |
| D-120 (2) what_line cleans each component (an OSC opened in one recipient cannot eat the next); U+2800 is invisible | `cargo test -p harness-core --test gate_accept_t04_r3 r3_card_components_are_cleaned_one_by_one -- --nocapture` | pass | `what: "echo.send to one@x.com 0;title, two@y.com: body=visible-body, ..."`; row lists `two@y.com`; on_timeout Skip (recipient not as written); `pay\u{2800}\u{2800}100` -> `pay 100 EUR` |
| D-120 (2) a stop reason cleans (and caps) the tool name | `cargo test -p harness-core --test gate_accept_t04_r3 r3_stop_reason_cleans_the_tool_name -- --nocapture` | pass | `why: "challenge at evil tool ttt...: stopped, ..."`, no ESC/CSI/OSC/U+202E/CR, element `#31`; rule stop clean too |
| D-120 (3) recipients blank after trim are not recipients | `cargo test -p harness-core --test gate_accept_t04_r3 r3_send_blank_recipients_are_not_recipients` | pass | only blanks -> `("new recipients (0)", "none")`, Skip, TimeoutSkip; known + blanks -> Allow; both stores |
| D-120 (3) new recipients deduped by contact_key, first spelling kept | `cargo test -p harness-core --test gate_accept_t04_r3 r3_send_new_recipients_deduped_by_contact_key` | pass | `["New@X.com","new@x.com"," NEW@x.com ",known]` -> `("new recipients (1)", "New@X.com")`, Proceed; both stores |
| D-120 (3)/B-191 values[0] is the gate's row; a classifier or arg value under that key is dropped | `cargo test -p harness-core --test gate_accept_t04_r3 r3_send_row_is_first_and_only_the_gate_writes_it -- --nocapture` | pass | forged `new recipients (0)`, `NEW RECIPIENTS`, ZWSP- and NBSP-spelled keys dropped: `[("new recipients (1)", "new@x.com"), ("subject", "Hi")]`; arg key dropped; Send rule on known contact -> `(0) none`, Skip; Pay card has no row |
| D-120 (3) row limits: 32 recipients, 2000 characters, 120 per recipient as head…(+K chars)…@domain, …(+K more); proceed only when complete | `cargo test -p harness-core --test gate_accept_t04_r3 r3_send_row_limits_and_proceed_only_when_complete -- --nocapture` | pass | 9 long recipients all listed, Proceed; 32 -> Proceed; 33 -> `…(+1 more)`, Skip; 20 x 110 chars -> 1914-char row with `more)`, Skip; 120 chars shown in full, Proceed; 130 -> `bbb…(+10 chars)…@example.com`, Skip |
| D-120 (3) a Send(new) ask proceeds on timeout only when every new recipient is shown in full and as written | `cargo test -p harness-core --test gate_accept_t04_r3 r3_send_proceeds_only_when_every_new_recipient_is_shown_as_written` | pass | `"  new@x.com  "` -> Proceed; ZWSP, U+2800, U+202E, double space, ESC, CRLF-injected bcc and an all-escape recipient each -> Skip and TimeoutSkip, row clean |
| D-120 (4) a lower-class hint that loses to the class level keeps the class-level timeout | `cargo test -p harness-core --test gate_accept_t04_r3 r3_losing_lower_class_hint_keeps_the_class_level_timeout` | pass | ('*','Read:newsletter',allow): Account and Send(new) hinted newsletter -> Proceed; Pay -> Skip; a tying lower-class ask hint, a same-class ask hint, and both hints together -> Skip; both stores |
| D-70 clean_card is public and cleans what, site, keys, values; drops a `new recipients` value; keeps class/timeout/on_timeout | `cargo test -p harness-core --test gate_accept_t04_r3 r3_clean_card_is_public_and_cleans_everything -- --nocapture` | pass | what `send to a@b.c www...` capped <= 200; site `site .example`; 42 values -> 32 + `...` row; `@vault(card)` kept; canary absent from the serialized card |
| MemStore search follows D-115 (equality 1.0, substring 0.9, word_similarity, ordered by score) | `cargo test -p harness-core --test gate_accept_t04_r3 r3_memstore_search_follows_d115 -- --nocapture` | pass | `[("Foo-Bar Inc", 1.0, Exact)]`; equality beats a newer mention at 0.9; `Vasanh` Trigram >= 0.6; `[("Vasanth Raghavan", 1.0, Trigram), ("reading list", 0.9, Exact)]`; identical to PgStore |
| MemStore D-115 example "'Vasnth' in 'Vasanth Raghavan' must still hit" | `cargo test -p harness-core --test gate_accept_t04_r3 r3_d115_vasnth_typo_inside_longer_subject_hits -- --ignored --nocapture` | fail | `r3 mem search Vasnth: []`, `left: None right: Some("Vasanth Raghavan")`. The pg_trgm port gives 0.5 (the same as the server, per testing.rs `word_similarity_matches_pg_trgm`), under WORD_SIMILARITY_THRESHOLD 0.6. Same root cause as the T-03 row; the fix belongs in both stores together. Committed as `#[ignore = "red-first T-04 round 4: ..."]` |
| MemStore SM-2 rounds a fractional interval up (D-115) | `cargo test -p harness-core --test gate_accept_t04_r3 r3_memstore_sm2_rounds_up -- --nocapture` | pass | `r3 mem sm2: [1.0, 6.0, 17.0, 48.0]` (equal to PgStore); exact 6 * 2.5 stays 15 |
| MemStore truncates timestamps toward zero from 2000 like sqlx | `cargo test -p harness-core --test gate_accept_t04_r3 r3_memstore_timestamps_truncate_toward_zero_from_2000 -- --nocapture` | pass | `1999-12-31T23:59:59.999999500Z -> 2000-01-01T00:00:00Z`, `...000001900Z -> ...000001Z`; equal to PgStore |
| MemStore add_contact and touch_contact refuse a blank person | `cargo test -p harness-core --test gate_accept_t04_r3 r3_memstore_add_contact_refuses_blank_person r3_memstore_touch_contact_refuses_blank_person` | pass | `add_contact("   ")` panics `blank person`; `touch_contact(" \t ")` -> Invalid |
| regression: the whole harness-core suite | `SB_VAULT_CANARY=sbcanary-... cargo test -p harness-core` | pass | every binary 0 failed; gate_accept_t04_r3 18 passed, 2 ignored (the rows above); canary count in output 0 |

Verdict: red
