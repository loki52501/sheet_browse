# Review: T-19 sb-act half, round 3 (B-7, I-10, D-406, D-407)

Scope: `crates/sb-act/src` at 3122050, focused on e16f9fc (D-406) and 3122050 (D-407).
injected/ and `tests/wire_from_injected.rs` were not reviewed (js-coder, in progress).
Uncommitted tester files (`accept_t19_round2.rs`, `common/mod.rs`, `accept_t19_round3.rs`)
were not reviewed either.

Gates (unpiped): `cargo fmt -p sb-act --check` is clean. `cargo clippy -p sb-act --all-targets
-- -D warnings` is clean. `cargo test -p sb-act` passes: lib 58, classify 16, forms 11,
quote_fuzz 6, round2 18 and wire 3, with 0 failed. I ran the probes as a temporary
`tests/zzz_reviewer4_tmp_probe.rs` and deleted it.

## Earlier rounds

- R2 Blocking 1, control-character keys gave Fill: **resolved** (D-406 (1)). In a card form,
  `\r`, `\n` and `\r\n` give Pay, and `\t`, DEL, BS and ETX give click(row). Test at
  classify.rs:2020.
- R2 Blocking 2, Enter on an activator in a form dropped the form max: **resolved** (D-406 (2)).
  Enter on a switch "Save card" in a checkout gives Pay. Enter on "Show password" in a login
  gives Submit. Test at classify.rs:2066.
- R2 Blocking 3, the one-time-code exemption hid CVC: **resolved** (D-406 (3)). A field "CVC"
  with one-time-code, then Next, gives Pay. Test at classify.rs:2167.
- R2 Should 1, challenge before MissingStop: **resolved** (lib.rs:186). `c#14; c#42; ?s`,
  `c#14; k#42=Enter`, `c#99; sc(#42)` and `c#14; ?s; w(1s); u#42=x` each return
  ChallengeTarget at the challenge step.
- R2 Should 2, Enter in a submitish field: **resolved**, now Pay. Test at classify.rs:2109.
- R2 Should 3, Enter perf: **resolved**. Debug builds now take 29 ms for 200 x 1,700 and
  38 ms for 500 x 2,500. Round 2 took 2.5 s and 8.7 s in release.
- R2 Should 4, Confirm under a "Delete account?" heading with no form: **resolved**, now
  Delete (D-406 (6)).
- R2 Should 5, Backspace and Gmail `#`: **closed by decision**. D-407 sanctions Backspace on a
  row as click(row). D-406 (7) leaves `#` to T-27.
- R2 Should 6, word gaps: **mostly resolved** (D-406 (7), D-407). Book now and Reserve are
  sanctioned as Submit. Leave group, Wipe device and the split-heading 2SV set-up are still
  open (Should 3).
- R2 Nits: twofa_setup is a single pass, the "never below a c#" doc is reworded
  (classify.rs:195-198), and cancel and/or is sanctioned (D-407). All resolved. The nit for a
  field named "New password" without autocomplete is still open.
- Interface: B-401 was accepted as D-66, and main's I-10 lists
  `PlannerError (incl. ChallengeTarget{step})`. The code matches. This worktree's
  `interfaces.yaml` still has the old line, so merge main before the PR (suborch note).

## Blocking

- none

## Should fix

1. classify.rs:239-258 (with injected.js:123-124): **a non-submit button with Pay or Account
   text is Navigate.** Probes: `type=button` "Complete purchase" inside a form with a cc-number
   field gives Navigate on `c#`, and also on `k#=Space`. Non-submit "Complete order",
   "Order now", "Place bid", "Renew", "Top up", "Send money", "Purchase", "Donate",
   "Create a passkey" and "Add passkey" all give Navigate. D-402 sanctions this ("a non-submit
   click is Delete by the same text rule ... else Navigate"), so it is not Blocking. But
   injected's `SUBMIT_WORDS_RE` is anchored at the start and lacks complete, purchase,
   donate, order now, bid, renew, top up, send and transfer. So the D-407 submit-name words
   never fire on React-style `type=button` checkouts, which is the common case. D-406 (7)
   already made this raise for "upgrade". Raise-only fix: run `pay_words`/`create_text`/
   `change_text` on non-submit button and menuitem rows the way `delete_text` already runs.
   Or have injected set submitish from the same lists. This needs a suborch ruling. A missed
   Pay costs money (D-58 why).
2. classify.rs:203-213: **Enter that is not the last chord segment is click(row).** In the
   card form, `Enter+Shift`, `Enter+a` and `Enter+` give Navigate on the Promo field. So do
   `Enter\t`, `\tEnter` and `Enter\0` (the control-char branch). D-433 defines Enter by its
   last segment, so this is as specified. Whether it is a lowering depends on sb-driver's
   chord semantics, and sb-driver is a stub today. With Playwright-style press order
   (segments down in order), or a driver that trims the key, Enter's keydown submits the
   form, and Pay would run as Navigate. This becomes Blocking once sb-driver behaves that way.
   Fix it raise-only now: treat any segment, or the trimmed key, equal to
   enter/return/numpadenter as Enter. Or open a blocker for I-9 so that sb-driver rejects
   non-modifier segments before the last, and control characters other than CR/LF.
3. Word gaps, Should per D-407: "Leave group" and "Wipe device" give Submit (submit) and
   Navigate (non-submit). A 2SV phone set-up gives Submit when the "2-Step Verification"
   heading and the "Set up your phone" heading are on different rows.

## Nit

- classify.rs:571: a 146-column doc line. rustfmt does not wrap comments.
- classify.rs:2503 and 2516: wall-clock bounds of 250 ms and 500 ms in a unit test can flake
  when swarm agents load the machine. The exact `evidence_builds() == 1` counter already
  proves the cache, so consider dropping or loosening the time asserts.
- Formless Enter: a formless "Coupon" textbox next to a formless SUBMIT "Place order" gives
  Submit (D-403 (2), B-431 synthetic form id still pending in injected). This is noted, not
  new.

## Notes

- D-406 (5) Ctx cache hunt found no lowering. Two frames each with form `f1` (checkout in
  frame 0, login in frame 1) give Pay and Submit, and either step order in one `classify()`
  or `check_planner()` gives the same per-step classes (cache key is `(frame, form id)`).
  Rows with no form id are their own context and are never cached (classify.rs:138). A
  duplicate ref takes the max, whether the formless copy comes first or second. A duplicate
  CHALLENGE copy wins (classify.rs:169). `evidence()` drops its `Ref` before `borrow_mut`, and
  `FormEvidence::of` does not re-enter Ctx, so there is no RefCell panic path. Ctx is per
  call, so nothing can go stale across calls. Injected only emits a form id from a real
  `<form>` (injected.js:1398), so an empty-string id cannot merge formless rows.
- check_planner (lib.rs:183-196) classifies once, scans for Challenge first, then applies the
  `?s` rule. `sc(#42)` and `u#42` are rejected too, as D-404 (4) requires for any step.
- Hard rules: the crate is pure, with no I/O, sleeps, keys or env. The only non-test
  `unwrap_or` is total (classify.rs:325). `passwords[0]` is guarded (406), and `verbs[from..]`
  has `from <= len` from `partition_point` (688). Error strings echo no values. Rules 3, 5 and
  7 do not apply.
- Spec ids: e16f9fc implements D-406 (1)-(7) as listed, and 3122050 implements D-407. The
  docs (lib.rs:9-26, classify.rs:1-19) name D-404 (1) as the only lowering and D-406 (3) as
  its bound. I found nothing beyond the decisions except the `upgrade` Pay on a menuitem,
  which only raises.
- Tests assert exact classes and steps, with negatives for every new word list
  (classify.rs:2271, 2388).

## Verdict

Every round-2 Blocking and Should item is fixed and tested, or closed by D-406/D-407. The
cache adds no structural lowering, and gates and tests are green. The open items are a
D-402-sanctioned gap (non-submit Pay buttons), a key-chord contract that sb-driver must
match, and word-list synonyms. All three are Should under D-407.

Verdict: approve

# Round 4 (reviewer-6): 0554c29, D-408

Scope: 0554c29 (`crates/sb-act/src/classify.rs`, `lib.rs`), with main merged in (73e2aa0).
Tester files under `crates/sb-act/tests/` were not reviewed.

Gates (unpiped, TMP on F:): `cargo fmt -p sb-act --check` is clean. `cargo clippy -p sb-act
--all-targets -- -D warnings` is clean. `cargo test -p sb-act` passes: lib 59, classify 16,
forms 11, quote_fuzz 6, round2 18, round3 15 and wire 3, with 0 failed.

Method: I exported 0554c29^ and HEAD with `git archive` into my scratchpad. I ran one probe
on both (separate target dirs). It covered every row of `common::ALL` plus 17 extra rows, as
`c#` and 31 key values, so 4,480 steps. I diffed the classes. Nothing was written to the
worktree.

## Round 3 items

- R3 Should 1, non-submit Pay/Account buttons: **resolved within D-408 scope**
  (`Ctx::form_button`, classify.rs:273-303). "Complete purchase", Confirm, Place bid,
  Order now, Top up, Renew, Create account and a passkey Add are raised. Show details and
  Back to top stay Navigate. Links are untouched. Test at classify.rs:2561.
- R3 Should 2, Enter not last or with control chars: **resolved** (`names_enter`,
  classify.rs:388-395). Enter+Shift, Enter+, Enter+a, `Enter\t`, `\tEnter` and `Enter\0`
  in a card form now give Pay. Enterprise is not Enter. But see Blocking 1.
- R3 Should 3: Leave group and Wipe device are **resolved** (classify.rs:801-802). The
  split-heading 2SV set-up is still open (Should 3 below).
- R3 Nits: the 146-column doc line is wrapped, and the unit-test time bounds are 5 s.
  **Resolved.**

## Blocking

1. classify.rs:203-205: **D-408 (2) lowers Enter+Delete chords from Delete.** The new
   `names_enter` branch returns `self.enter(row)` before the D-404 (3) Delete merge
   (classify.rs:234-238) and the DEL-character merge (classify.rs:208-210) can run.
   Old->new probe on a formless mail `row` "Invoice" (the Gmail case D-404 (3) exists for):
   `Enter+Delete`, `Enter+Del`, `Control+Enter+Delete`, `Return+Shift+Delete`,
   `Enter\u{7f}` and `\u{7f}Enter` go from Delete to Navigate. On non-text rows in a form
   they go from Delete to Submit or Account. The probe found 378 lowered steps over the
   fixtures, all from these 7 key shapes, and no other lowering. D-408 is raise-only, and
   D-407 makes a key-path lowering Blocking (hard rule 4). Fix: in the Enter branch, merge
   the old result. For example, merge Delete when any segment is delete/del or the key
   holds U+007F on a non-text row, or `merge(&mut out, <pre-D-408 path>)`. Add these
   chords to a lowering test.

## Should fix

1. classify.rs:277-288: **the heading-shared lookup is same-frame only, and a formless
   button with no form near it is never raised.** Probes: a formless "Complete purchase"
   under heading "Payment", with the cc-number field in a Stripe-style iframe (frame 2,
   heading "Payment"), gives Navigate. A formless "Pay $20.00" or "Place order" button with
   no form gives Navigate. D-408 scopes the raise to form context, so this is per spec.
   Hosted card iframes plus a formless parent-frame Pay button are a common checkout
   shape. Proposed raise-only rule for suborch: apply `pay_words` (the strict phrase list,
   not `PAY_TOKENS`) to any button or menuitem's own name, whatever its form context.
2. classify.rs:213-216 (pre-existing, not from D-408): **Delete in a non-last chord
   segment is lost.** `Delete+Enter` on the mail row is Navigate before and after this
   commit. This is the mirror of R3 Should 2 for Delete. Fix it in the same change as
   Blocking 1: any delete/del segment on a non-text row merges Delete.
3. Carried from R3 Should 3: the 2SV phone set-up is Submit when "2-Step Verification" and
   "Set up your phone" are on different heading rows. This is a word gap, Should per D-407.

## Notes

- Raise-only check for (1) and (3): with a click, the probe found no lowering. Every class
  that changed went up, and `form_button` only feeds `class.max` (classify.rs:261). The
  Class order is Pay > Delete > Account (core/src/class.rs:15-25), so returning Pay before
  checking Account never hides a higher class. No existing fixture lowers on any step
  except the Blocking 1 key shapes.
- `PAY_TOKENS` (classify.rs:660-679) includes order, place, submit, confirm and transfer.
  So in a form with Pay evidence, an "Order details" toggle or a "Transfer" tab button asks
  for Pay. That is a false-positive ask and only raises. "Continue" and "Next" in a card
  form stay Navigate, which matches D-408's "any Pay word".
- Spec ids: the commit implements D-408 (1)-(3) as written. Two extras go beyond the text,
  and both only raise. (a) Pay evidence is `ev.pay`, meaning card or IBAN field or checkout
  text, where D-408 says "card evidence". (b) Menuitems are covered too. The coder's note
  that a type=button "Create account" in a login form is now Account also only raises.
- Interface I-10: no pub item changed (lib.rs diff is docs only). Main's I-10 with
  `ChallengeTarget{step}` matches the code.
- Hard rules: the crate is still pure, with no I/O, sleeps or env. `form_button` and
  `names_enter` take no unwrap or index on input. Rules 1, 2, 3, 5 and 7 do not apply.
- Tests: `d408_form_buttons_enter_chords_delete_words` (classify.rs:2561) asserts exact
  classes, with negatives (Show details, Back to top, links, formless, Enterprise). It does
  not cover a Delete chord, which is how Blocking 1 got through. The tester's item-18
  lowering guard missed it too, because its fixtures lack Enter+Delete keys.
- Nit: lib.rs:132 is a 126-column doc line, and lib.rs:127 ends a doc line early at
  "(D-408 (1)); Fill on".
- The tester-owned `accept_t19_round3.rs` still has a 250 ms wall-clock bound (coder
  handoff not_done 2).

## Round 4 verdict

D-408 (1) and (3) are correct and raise-only. D-408 (2) closes the round-3 Enter gap but
opens a structural lowering: Enter+Delete chords and Enter plus DEL drop from Delete. That
breaks D-408's raise-only rule and hard rule 4.

Verdict: request changes
