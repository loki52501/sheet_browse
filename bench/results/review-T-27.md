# Review T-27 round 1 (servers, swarm/servers-T27)

Range: `git diff main..HEAD` (8c203df, 6540707). 7 recipes, `tools/lu.ma/_site.yaml`,
`scripts/recipes/check_recipes.py` + `test_check_recipes.py`, B-595..B-598, D-1500..D-1502.
Ran: `python -m unittest discover -s scripts/recipes -v` (20 tests OK) and
`python scripts/recipes/check_recipes.py` (7 recipes, 0 problems). I also ran a probe script
against `check_doc` (results quoted below). I did not use the browser. Live runs belong to the tester.

Hard rules: no keys or .env, no vault values, nothing that solves a challenge, no `evaluate`/`run`
in recipes, readers return text/ax lines only, no fixed sleeps, no em dashes or ` -- ` in the diff.
WAVE3 rules, each checked in every recipe and each with a test that fails when the rule is
broken: name = stem and `[a-z0-9_]+` (test :61), domain = folder (:66), @match port of
`toolstore::header::match_violation` is exact (:79), placeholders only for declared args
(:92), declared args used (:96), raw ref (:99), fixed sleep (:104), vault only bare in act
text (:135), password/key-shaped (:144). Spec deviations (register takes a slug, thread takes
query+index) are recorded in D-1502.

## Blocking

- none

## Should fix

1. **B-596 needs a vault rule for substituted values** (agent-memory/blockers.yaml:320). The
   checker only stops `@vault` written in the recipe file. Nothing stops a caller passing
   `body: "@vault(gmail_pw)"` to `send`. If tools.run resolves `@vault` after it fills the
   placeholders, the secret is typed into a mail and sent (rule 2). Add to B-596: `@vault` is
   resolved only where it appears literally in the recipe's act text, and a placeholder value
   that contains `@vault(` is refused or passed through as plain text. Arg defaults are not
   scanned either: `default: "@vault(pw)"` used in a navigate URL passes
   (check_recipes.py:205-230 and :245, where the scan covers only `steps` and `expects`).
2. **The navigate host check fails open on a backslash** (check_recipes.py:118-125).
   `https://evil.com\.mail.google.com/x` passes, because `urlsplit` takes the whole
   `evil.com\.mail.google.com` as the hostname and it ends with `.mail.google.com`. WHATWG
   parsing treats `\` as `/`, so Chrome goes to evil.com. The Rust port already rejects `\`
   for @match (header.rs:181). Reject `\`, whitespace and anything outside `[a-z0-9.-]` in the
   authority, and add a test.
3. **Other tools can still leave the site or touch local files** (check_recipes.py:46-47,
   :165). Only `navigate` URLs are checked. These all pass: `tabs {action: new, url:
   https://evil.com/}`, `windows {url: ...}`, `download {url: https://evil.com/x.exe}`,
   `history`, and `upload {files: [C:/Users/x/.ssh/id_rsa]}` (flagged only when it has a
   target). None of the 7 recipes uses them. Drop them from `TOOLS`, or run every `url` arg
   through `check_url` and refuse `upload`. D-1502's "the checker enforces" claim holds only
   for navigate.
4. **`send`/`draft` can reach the wrong recipient** (send.recipe.yaml:16-20,
   draft.recipe.yaml:16-20). Pressing Tab with Gmail's suggestion list open commits the
   highlighted suggestion. If `to` is a name or a partial address, mail goes to the top
   contact match, which the caller did not name. The description should say `to` is one or
   more full email addresses. Better: add a wait for text `{to}` (the chip) before Send.
5. **`register`'s "free only" is a comment, not a check** (register.recipe.yaml:1-3, :14-17).
   Nothing in the steps refuses a paid or approval event whose button name starts with
   "Register". B-596(1) uses prefix matching, so a button named e.g. "Register - $25" would
   match too. Either make side-effect clicks match the exact name (see 6), or add a step that
   fails closed unless the free registration block is present. Also put "free events only;
   stops at a ticket or payment step" in the `description`, which is the text the model sees.
6. **B-596(1): prefix matching for buttons that cause side effects.** "Send" matches
   anything that starts with "Send", "Save" matches "Save ..." and so on. Make exact match
   the default, and either strip Gmail's shortcut suffix (`Send (Ctrl-Enter)`, which has bidi
   marks around it) or add an explicit `name_prefix`. Also scope `nth`: thread.recipe.yaml:16-17
   uses `{role: row, nth}` with no name, so it counts rows across the whole snapshot. Any row
   outside the result grid shifts the index, and the wrong thread is opened (and marked read).
7. **The checker crashes on odd types instead of reporting them.** `tool: [act]`, `kind:
   [click]` and `type: [string]` each raise `TypeError: unhashable type`
   (check_recipes.py:137, :155, :219). The exit is non-zero, so this fails closed, but the
   output is a traceback. The checker also does not reject: `nth: -3`, `target.name: 5`, a
   newline in `target.name` (toolstore rejects header-unsafe characters), `expects: {text: 5}`
   or `{text: ""}`, or `wait {for: text}` with no `value` (all pass in the probe).
8. **`_site.yaml` aliases are not validated** (check_recipes.py:78-84). `aliases: [com]`
   would make every `.com` host count as on-site. A scalar `aliases: luma.com` is iterated
   one character at a time (toolstore's `yaml_list` reads it as one item, domain.rs:66-67),
   and a non-mapping file crashes. Require each alias to match `DOMAIN_RE` and contain a dot.
9. **`create` with a malformed `start`/`end`** (create.recipe.yaml:8-9, :14). Google's
   eventedit most likely ignores a bad `dates=` value and opens its default slot, and then
   the Save click creates an event at a time nobody asked for. Check the format
   (`^\d{8}T\d{6}$`) in tools.run, or wait for the expected date before Save.

## Notes

- B-596(7) plus each recipe's own last `wait` makes the same wait run twice
  (create:19-22, register:18-21, send:29-32, draft:31-34). It costs little, but drop one of the two.
- B-596 should also say: (a) a challenge seen during a recipe (D-5) ends the run as
  "challenge", and reflect's retire-after-3-failures does not count it as a failure;
  (b) tools.run refuses an em dash or `--` in `send`/`draft` subject or body (rule 7), since no
  recipe can check this; (c) how placeholders are escaped inside grep `pattern` and CSS
  selector `value`. None of the recipes puts a placeholder there yet.
- check_recipes.py:52: `\{([^{}]*)\}` also matches regex quantifiers, so a grep pattern
  `\d{2}` is reported as an undeclared placeholder. This fails closed, but the format needs
  an escape.
- check_recipes.py:254-256 allows `@vault` in act `value` (fill) and for any act kind. WAVE3
  says "act type text". The broader reading is reasonable, but write it down in D-1500 or B-596.
- The checker does not limit `read` formats (rule 5). WAVE3 lists markdown/text/links. Add an
  allowlist so a future `read {format: html}` is refused.
- events.recipe.yaml:15: the grep `[A-Z][a-z]+day, ` pattern only works with an English UI.
  thread's description does not say that opening a thread marks it read (it is only in the
  file comment at :1-2).
- B-597 (needs_user): live `register` is still open. That is correct, not a workaround.
- B-598: valid hooks finding for supreme.
- The tasks.yaml `blocked_by: [T-26]` waiver comment is fine. The commit message names
  T-27, D-1500..D-1502 and B-595..B-598, and they match the diff.

## Verdict

Every hard rule and every WAVE3 recipe rule holds, and each rule has a test that fails when
it is broken. Should-fix items 1-3 are fail-open gaps in the checker and in B-596. Fix them
before core builds T-28 on B-596.

Verdict: approve
