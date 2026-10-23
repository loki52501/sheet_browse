# Review: T-19 sb-act half, round 2 (B-7, I-10, D-403, D-404)

Scope: `git diff 6e52d44 HEAD -- crates/sb-act`, focused on fix commits e0ee962 (D-403) and
51f1c7e (D-404), plus the tester's 46c92c4 tests. injected/ not reviewed.

Gates (unpiped): `cargo fmt -p sb-act --check` clean; `cargo clippy -p sb-act --all-targets
-- -D warnings` clean; `cargo test -p sb-act` 99/99 pass (lib 48, classify 16, forms 11,
quote_fuzz 6, round2 18). I re-ran the probes as a temporary integration test, then deleted it.

## Round 1 items

- Blocking 1, activating keys lowered to Fill: **resolved**. Enter/Space/NumpadEnter on
  "Delete repository" give Delete, Space/NumpadEnter on SUBMIT "Place order" give Pay, and
  Enter on a link gives Navigate. Tests are at classify.rs:1131 and 1255, plus round2.
- Blocking 2, Enter in a form with no submit row: **resolved**. A card-only form gives Pay
  (classify.rs:1167).
- Should, change-email in a login-shaped form: **resolved**, now Account.
- Should, Delete from the heading or form name: **resolved** for submits. For non-submits it
  is resolved inside a form or dialog only (see Should 4).
- Should, missing Pay/Account/Delete words: **resolved** for every probed word.
- Should, card fields CVC/CVV/security code/MM-YY: **resolved**, but see Blocking 3.
- Should, Enter in a formless textbox: **resolved** (Submit, D-403 (2)).
- Nit, docs name D-58: resolved. MAX_WAIT is in I-10: resolved (D-63). Duplicate ref takes the
  max: resolved. CHALLENGE row inside a form: recorded in D-403. Planner targeting a challenge:
  resolved (D-404 (4)). Positive cross-frame test: resolved (classify.rs:983).
- Nit, "cancel and/or" exemption: **open**. It is documented at classify.rs:554 but still has
  no decision.
- Nit, submitish fields: **open**, and it is worse than I rated it (Should 2).

## Blocking

- classify.rs:117-121, 158-165: **control characters in a key step classify as Fill (rule 4;
  D-403 (1) exempts only printable characters).** Any whitespace-only key becomes `"space"`, and
  any one-char key counts as "editing". Probe (Promo field in a card form with a Pay submit):
  `k#1="<CR>"`, `k#1="\n"`, `"\t"`, `"\u{7f}"`, `"\u{8}"` and `"\u{3}"` all give Fill. A
  quoted value can carry a raw CR, and D-432 allows `\n`. Blink submits a form implicitly on a
  keypress with charCode `\r`, and common key layouts map `"\r"`/`"\n"` to Enter. So this is a
  Pay that runs as Fill. Fix: send `"\r"`, `"\n"` and `"\r\n"` to `enter()`. Treat only U+0020
  (and NBSP) as Space. Any key with `char::is_control` is unknown, so it is `click(row)` at
  least (or reject it in parse). Add tests.
- classify.rs:270-272 (pinned by the test at 1159-1161): **Enter on an activator row inside a
  form dropped the D-433 form max, a lowering that no decision sanctions.** Round 1 (70957dd)
  gave Enter on any non-submit row in a form the max over the form's submits. Now button, link,
  menuitem, option, switch, tab and treeitem rows return `click(row)` only. Probes: Enter on a
  native `<input type=checkbox role=switch>` "Save card" in a Checkout form gives **Fill**. The
  same row with role checkbox gives Pay, and round 1 gave Pay. Enter on "Show password" in a
  login form gave Submit in round 1 and gives Navigate now. D-403's header says "never lowers a
  class", and D-404 (1) is the only sanctioned lowering. Fix: keep the form max for activators
  in a form. At the very least, send every native input (`state.input_type` set and not
  button/submit/reset/image/file) through implicit submission. Or get a decision that sanctions
  "Enter on a button or link is only its click" and names exactly which roles.
- classify.rs:369: **unsanctioned lowering of D-403 (5).** A field with autocomplete
  `one-time-code` is never a card field, whatever its label says. Probe: a field "CVC" with
  `one-time-code` under a neutral heading, then Next, gives Submit (it would be Pay without the
  exemption). The exemption is reasonable for "Security code" SMS prompts. But it appears only
  in the rust-coder-3 handoff, not in D-403/D-404, and it contradicts the "except D-404 (1)"
  claim at lib.rs:14-15 and classify.rs:3-4. Fix: limit it to fields whose only card evidence
  is the "security code" phrase (CVC/CVV/CSC/cardholder/MM-YY/card number still count), and
  record it in a decision.

## Should fix

1. lib.rs:164-172: ChallengeTarget wins over MissingStop only inside one step. Probes:
   `c#14; c#42; ?s` returns `MissingStop{1}`, and `c#14; k#42=Enter` returns `MissingStop{1}`,
   though step 2 targets a CHALLENGE row. The planner then "repairs" the `?s` and only learns of
   the challenge on its next turn. B-401 says "before the ?s rule". Fix: check every step for a
   challenge first, then apply the `?s` rule (one O(n) pass, saves a turn).
2. classify.rs:270: Enter on a submitish *field* returns early and skips the form's submit
   buttons. Probe: Enter in a submitish Email field next to SUBMIT "Place order" gives Submit.
   The same field when not submitish gives Pay. I-5 documents submitish as "submit button,
   Enter target", so a conforming producer can mark fields. Today injected marks only buttons.
   Fix: return early only for non-text rows.
3. classify.rs:276-281: Enter costs O(submits x rows) with re-tokenising. Release-build
   probes: a form with 200 submits and 1,700 rows takes **2.5 s** per Enter, and 500 x 2,500
   takes 8.7 s (debug is about 10x slower). Page-wide forms (ASP.NET, admin tables with a Save
   per row) are real. Compute the form evidence (card fields, form_texts tokens, password counts,
   hints) once per form, then fold in each button's own name.
4. classify.rs:237-246: a non-submit "Confirm" button whose own heading is "Delete account?"
   is Navigate when it sits in no form and no `dialog` landmark. Div modals without
   `role=dialog` are common. Raise-only fix: count the row's own heading even without a context.
5. classify.rs:167-171: only Delete/Del count as Delete. Backspace and Meta+Backspace on a
   selected file row (Drive, and the macOS delete key) give Navigate, or Fill on an option row.
   Gmail's `#` shortcut (trash) gives Navigate. Propose a blocker that extends D-404 (3) to
   Backspace on non-text rows. Printable shortcut keys on a non-text row are at least Submit.
6. Evidence gaps (each needs a decision, since the lists are spec): Pay misses "Submit order",
   "Complete order", "Order now", "Book now", "Reserve", "Place bid", "Upgrade to Pro", "Renew",
   "Top up" and "Send money" (all Submit, and a non-submit "Upgrade" is Navigate). Account
   misses "Set up authenticator app", "Create a passkey", "Add passkey", and a 2SV phone set-up
   where the verb is in a different heading from "2-Step Verification" (all Submit). Delete
   misses "Clear history", "Leave group" and "Wipe device".

## Nit

- classify.rs:508-515: `twofa_setup` is O(spans x verbs). Release probes: 16k tokens take
  0.37 s. Producer names are capped at 150 chars (NAME_CAP), where it takes 150 us, so this
  is bounded. A one-pass nearest-verb scan would remove it.
- classify.rs:142-144: the doc line is over 100 columns. "A key step is never below a `c#` on
  the same target" is false for typing into a text field (Space in a textbox is Fill, `c#` on
  it is Navigate). Reword it.
- A "Two-step verification" heading over a single password field labelled "New password" with
  no autocomplete gives Submit. A "Password" heading over the same field also gives Submit, so
  D-404 (1) adds nothing here. It is a general gap: a password field named "new ..." could count
  as new-password.
- Round 1 nit still open: record the "cancel and/or" exemption (classify.rs:554) in a decision.

## Notes

- Hard rules: no keys or .env, no I/O, no sleeps. There is no `unwrap` or slicing on input
  outside tests (`passwords[0]` at classify.rs:541 is guarded by `len() == 1`). Error text
  echoes no values. Rules 3, 5 and 7 do not apply.
- D-404 (1) abuse check: the narrowing touches only the 2FA-text Account evidence. Under a
  "Two-step verification" heading, a new-password field still gives Account, two password fields
  give Account, a card number gives Pay, Delete words give Delete and "Change ..." gives
  Account. It cannot hide a password-change or card form.
- Regex and word matching: everything works on `tokens()` whole words, with no regex. split_key
  on a 200k `+` key takes 1 ms and a 50k-modifier chain takes 15 ms. The quote fuzz tests cover
  parse panics.
- Spec ids: e0ee962 and 51f1c7e cite D-403 and D-404, and each numbered clause is implemented.
  Additions beyond the spec are the one-time-code exemption (Blocking 3), the activator early
  return (Blocking 2) and the 4-word 2FA verb window.
- Interface: the I-10 `rust:` names all exist, and `MAX_WAIT` is listed (D-63).
  `PlannerError::ChallengeTarget` is B-401, which supreme has not accepted yet (no decision
  names it). Supreme needs to accept it before toolstore or toolforge match on it.
- Tests assert exact classes, steps and hints. The round2 lowering guard compares every fixture
  submit against a field-only rule.

## Verdict

The round 1 items are fixed and tested. The fixes add three under-classifications: control-char
keys, Enter on activator rows in a form, and the one-time-code card exemption. Two of them are
unsanctioned lowerings and the third contradicts D-403 (1). Each has a small fix.

Verdict: changes
