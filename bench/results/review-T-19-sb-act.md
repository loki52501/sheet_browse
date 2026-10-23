# Review: T-19 sb-act half (B-7, I-10)

Scope: `git diff 6e52d44 HEAD -- crates/sb-act` (commits 706dc5a, c92695b). injected/ and the
uncommitted `crates/sb-act/tests/` + Cargo.toml dev-dep were not reviewed.

Checked: `cargo clippy -p sb-act --all-targets -- -D warnings` clean; `cargo test -p sb-act --lib`
35/35 pass; `cargo fmt --check` clean for `src/`. A scratch probe outside the repo (200k random
programs through parse/classify/check_planner) found no panics, and every `ParseError.at` landed
on a char boundary. Quote/parse round trip holds for t, s, k and u.

## Blocking

- classify.rs:47-51, 54-59, 97-103: **keypresses that activate an element skip the gate's
  classification (rule 4).** Only `Enter` on a submit row, or in a form that has a submit, is
  raised. Any other key on any row is Fill, and the B-4 #7 driver sends every key through
  `dispatchKeyEvent`. Probe results:
  `k#1=Enter` and `k#1=Space` on a non-submit "Delete repository" button: Fill (`c#1` is Delete).
  `k#2=Space` and `k#2=NumpadEnter` on a SUBMIT "Place order": Fill (`c#2` is Pay).
  `k#13=Enter` on a link: Fill (should be Navigate). In each case `check_planner` returns Ok
  with no `?s`, and the D-434 hints are dropped. So a planner or the model can lower any
  Delete/Pay/Account target to Fill by writing a key step instead of `c#`. Fix: a key on a
  button, link, submit or checkable row (Enter, Space, `" "`, NumpadEnter, any `+Enter`/`+Space`)
  classifies as `click(row)`. Better still, invert `is_enter` into a list of keys known not to
  activate (Tab, Escape, arrows, modifiers alone), so an unknown key is never Fill. Add tests
  for each of the cases above.
- classify.rs:104-113 (pinned by the test at classify.rs:752): **Enter in a form with no
  indexed submit row is Fill.** B-7 says "Enter on a form → Submit", and browsers submit on
  Enter (implicit submission) even without a submit button. Probe: a form with only a card
  field and no submit row gives `k#card=Enter` → Fill, which should be Pay. Fix: Enter on a
  row in any form is at least Submit and still goes through the form evidence (card/IBAN, D-58
  text, Account, Delete), even when the form has no submit row. Update the `keys()` test.

## Should fix

- classify.rs:257-260: the login exemption (D-433) also throws away change-email evidence.
  A "Change email" form with New email + one `current-password` + "Save" classifies as Submit,
  but H-1 says Account (probe `c#6`). Proposed blocker for supreme: ignore only
  create-account/sign-up heading text for logins, and keep change password/email heading text.
- classify.rs:126: Delete is read only from the button name. A "Delete account?" heading or
  form name with a "Confirm" button is Submit (or login-exempt Submit). Proposal: apply
  `delete_text` to `form_texts` too. It only raises the class.
- classify.rs:209-216, 225-235, 264-272: words H-1 covers that the matchers miss (probe-confirmed):
  "Submit payment", "Proceed to payment", "Confirm payment" (`payment` is not `pay`), "Buy",
  "Buy with 1-Click", bare "Purchase", "Donate" are Submit, and should be Pay. "Enable two-factor
  authentication" / 2FA / 2-step is Submit, and should be Account (H-1 lists it). "Deactivate
  account", "Close account", "Discard", "Erase", "Revoke" are Submit, and should be Delete.
  D-58 says a false Pay costs one ask and a missed Pay costs money, so these lists should grow
  through a decision.
- classify.rs:194-197: a card field is recognised only by `cc-*` autocomplete, card number or
  IBAN. A form whose card inputs are named CVC, CVV, "Security code" or "Expiry (MM/YY)" and
  carry no autocomplete is not seen as a card form.
- classify.rs:101-103: Enter in a textbox with no form (chat composers, the SPA case in B-431)
  is Fill, while H-1 treats a message send as Send. At least note this on B-431. Proposed
  default: Submit.

## Nit

- lib.rs:4-11 and lib.rs:92-102: the module and `classify_step` docs do not mention the D-58
  checkout-text rule.
- lib.rs:50: `pub const MAX_WAIT` is public but not in the I-10 `rust:` list. Supreme should
  add it to interfaces.yaml.
- classify.rs:270: the "cancel and/or" exemption ("Cancel and close" is Navigate) is not in
  D-402, which says "cancel <something>" is Delete. Record it in a decision.
- classify.rs:39: when an ElemRef appears twice in the index, the first row wins. Take the max
  over all matching rows, so a duplicate CHALLENGE row cannot be shadowed.
- classify.rs:43: a submit in a form that contains a CHALLENGE row (a visible reCAPTCHA) is
  Submit. The driver's H-1.4 mid-program stop covers it at runtime; worth a note.
- lib.rs:139-146: `check_planner` accepts a step that targets a Challenge row as long as `?s`
  follows. Under D-5 a planner should never target one, so consider rejecting it outright.
- classify.rs:84: `State.submitish` also marks "Enter target" fields, so `c#field` on such a
  field is Submit and the planner needs a `?s` after just focusing it. This is over-classification
  (it only costs efficiency).
- classify.rs:654-668: tests cover only the negative cross-frame case. There is no positive test
  that a submit in frame 2 sees frame 2's own card field.

## Notes

- Hard rules: no keys or .env files, no I/O, no sleeps, no unwrap or slicing on input outside
  tests (`passwords[0]` at classify.rs:259 is guarded by `len() == 1`). Vault literals stay
  literal, and error messages echo at most one char or the ref/op token, never a value. Rules
  3, 5 and 7 do not apply.
- Interface: every I-10 name is present and the signatures match the 6e52d44 stub. I-5 fields
  are used read-only.
- Spec ids: D-50, D-52, D-402, D-430..D-434 and D-58 are all implemented as claimed. Additions
  beyond the spec, all raise-only except the cancel exemption: Account words
  update/passcode/mail, the `signatures` token, and filler windows in near().
- Tests assert exact classes, offsets and hints, not just "does not panic". The quote property
  test runs 3000 cases over an adversarial alphabet.

## Verdict

Two ways a target's class comes out lower than B-7 and H-1 require (activating keypresses;
Enter in a form with no submit row). Both are rule 4 issues with simple fixes.

Verdict: changes
