# Review T-13c, round 2 (final, D-89)

Scope of round 2: `git diff 29862a0..HEAD`, commits ac60642 (B-562, D-575), f2f009e (B-560, D-565) and 35a2f88 (B-562, D-576). Plus D-575 and D-576 in `agent-memory/decisions.yaml`.

Round 1 summary: it reviewed 4b3c0f1 (srv-web), 0693620 (srv-files) and 2279f25 (agent-memory). It found one Blocking item: `@VAULT(` in another case, or an unclosed `@vault(`, passed both B-562 rules. Core resolves only exact lower-case `@vault(name)`, so an address or a raw token went out as typed with class Read. It also listed five should-fix items. Tests, clippy and fmt were green, and two break-the-code checks showed the tests can fail.

What I ran (CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2):
- `cargo test -p srv-web`: all green. lib 40, accept_t12 5 (+1 ignored), accept_t13 10 (+1), accept_w 6 (+1), contract_paths 1, live 0 (+3), round2_t12 7, round3_t12 10, t13c_api 11, web_tools 13.
- `cargo clippy -p srv-web --all-targets -- -D warnings`: clean. `cargo fmt -p srv-web --check`: clean.

## Blocking

- none

## Should fix

1. (T-13d) Classify and call can act on different files in srv-files (`fs.rs:375`, `util.rs:80-90`). `recheck` compares only the class. Recheck the target too, or reword D-572.
2. (T-13d) No size guard for 32 MB of JSON (`api.rs` `as_json`, `pick.rs`, `site.rs` page.api). Cap value count or depth before projecting, and run the live OpenAI check before closing B-560.
3. Fixed in f2f009e. `crates/srv-web/src/api.rs:493-495`: the pick cap applies only to JSON, text/plain or untyped answers. Test `pick_keeps_the_page_cap_for_an_answer_that_is_not_json` fails if the gate is removed (6000 bytes of HTML under a 1000-byte cap). An untyped answer still gets 32 MB, as D-576 states.
4. Fixed in 35a2f88. `api.rs:330`: an email in the path or query (including `%40`) gives Send, recipients [host], and the url on the card (`api.rs:373-375`, `385-401`). Small gap: `an_email_in_the_url_is_a_send_that_asks` checks the class, the `what` text and the url value, but not `recipients == [host]`. The header test checks it at `t13c_api.rs:211`. Add the same assertion.
5. (T-13d) Docs readers (`pdf.read`, `xlsx.read`, `docx.read`, `md.to_pdf`) have no bare-name fallback test.

## Notes

- **(a) The round-1 blocker is closed.** `check_vault_refs` (`api.rs:221-236`) accepts a template only when two things hold:
  - No `@vault(` in another ASCII case: the exact count equals the count after lower-casing.
  - `harness_core::vault::vault_refs` (core's own `parse_refs`, `core/src/vault.rs:732`) returns Ok with one ref per exact opener.

  For an accepted template, `literal_part` (`api.rs:191`) strips exactly core's refs. A name holds only `[A-Za-z0-9_.-]`, so the next `)` is the ref's close. `has_vault` is true exactly when core will resolve something. `ctx.resolve_into` (`core/src/mcp.rs:341`) calls the same parser on the same untrimmed string. A template with no case-insensitive opener skips the check, and core sees no ref in it either. Probes:
  - `@vault (x)`, `@va<ZWSP>ult(`, fullwidth `＠vault(`: not a ref to either side. In a credential header they are refused by `check_identity` because there is no real ref. Elsewhere the literal stays in the email scan. A non-ASCII template with no real ref fails `HeaderValue::from_str` in parse (`api.rs:310`). A non-ASCII template that does have a real ref fails after resolution in call (`api.rs:431-441`), before anything is sent.
  - `@vault(a(b))`, `@vault()`, `@vault(a.b@c.de)`, `@vault(tok`: BadReference in core, refused here.
  - `@vault(@VAULT(x))`: case counts differ, refused.
  - `@vault(TOK)`: accepted by both sides (an upper-case name is valid in core).
  - JSON `\u0040vault(` is decoded by serde before either side sees it. A header name cannot hold `@` or `(`.
  - Url and body are never resolved, and `no_vault` still refuses every spelling (`api.rs:238-245`).
  - URL userinfo is refused (`http.rs:567`), so reqwest cannot send basic auth around CREDENTIAL_HEADERS.
- **(b) No new blocking defect.**
  - The refusal names only the header and holds no value. The t13c test checks the literals are absent and that the message is one line.
  - The card shows the url (capped at 300) and the templates, never a resolved value. Cards are scrubbed by core.
  - Class is only raised (`req.class.max(Class::Send)`).
  - No new panic path. Slices end at ASCII `)` offsets, `has_email` bounds are unchanged, and `content_type` falls back to empty on a non-ASCII header.
- **Residual, not new.** A credential header with a raw literal next to a real ref still passes, e.g. `Bearer raw @vault(x)` (`api.rs:139`). Lower-case real refs allowed this in round 1 too. D-567 cannot tell a scheme word from a token. Candidate for T-13d: limit the literal part of a credential header to a short scheme or cookie-name prefix.
- **Nit.** `has_email(&literal_part(t))` joins the literal pieces. An address completed by the vault value, e.g. `x@example.c@vault(k)`, is not seen. This is contrived, and the value is the user's own vault entry.
- **Spec ids.** B-560, B-562, W-3, D-565, D-575 and D-576 match the code. The D-575 and D-576 ids are unique and not on main.
- **Hard rules.** No keys, sleeps, em dashes, captcha handling or `unwrap` outside tests were added.

## Verdict

The case-mismatch bypass is closed. srv-web now treats a header template as a vault reference exactly when core's parser does, and refuses every other spelling. Should-fix 3 and 4 are fixed. One recipients assertion is missing from the url test. Items 1, 2 and 5 move to T-13d.

Verdict: approve
