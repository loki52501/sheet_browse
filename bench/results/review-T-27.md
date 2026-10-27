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

# Round 2 (confirmation, D-89)

Range: `git diff f2c2fb0..HEAD -- scripts/recipes tools agent-memory/{blockers,decisions,tasks}.yaml`
(850dcb8, 70f8eb3, 4a2888f, 0d40e06). Ran `python scripts/recipes/check_recipes.py` (7 recipes,
0 problems, exit 0) and `python -m unittest discover -s scripts/recipes -v` (26 tests OK). I also
ran a probe of `check_doc` and a mutation run: in a scratch copy of the suite I reverted each fix
one at a time. I did not use the browser and did not run cargo.

Round-1 items confirmed fixed, each with a test that fails when the fix is reverted:
- 1 (@vault outside act text): the scan now covers the whole file (check_recipes.py:311-322).
  test_vault_in_an_arg_default_is_refused fails when it is reverted to the `body` scan. The
  arg-value half is now B-599(1).
- 2 (URL tricks): check_recipes.py:131, :138-147. The backslash, tab, `%2f`, bad-port and `x@`
  cases each fail the suite when their check is removed.
- 3 (other tools): D-1503 cuts TOOLS down to 7 (check_recipes.py:47). The old set fails
  test_tools_are_browseros_recipe_tools.
- 7 (odd types): `one_of` and `text` (:153-160), nth/name/value/expects/read/grep checks
  (:177-221). Plain `in` in place of `one_of` raises inside the suite. Removing any one of the
  other checks fails the suite.
- 8 (aliases): check_recipes.py:81-100, and problems are surfaced at :354-355. Dropping the dot
  rule fails test_site_aliases_are_hosts_with_a_dot.
- Wait timeout cap: 1..30000, bool and str refused (:212-214). Removing the check fails the suite.

Two mutants survived, and both are harmless. (a) `"@" in netloc` replaced by `parts.username`:
the only input that behaves differently is `https://@host/`, an empty userinfo that Chrome drops.
(b) The scalar-alias line :92 has no effect of its own, because :94 already wraps a non-list.

## Blocking

- none

## Should fix

1. **Whole-file @vault scan crashes on a mapping `steps`** (check_recipes.py:313-318).
   `steps: {x: {tool: act, args: {text: "@vault(k)"}}}` raises `ValueError` from `int('x')`, and
   `steps: {"0": ...}` raises `IndexError`. Line :292 resets `steps` to `[]`, but `strings(doc)`
   still walks the original mapping. The check fails closed (exit 1) but prints a traceback,
   which is the item-7 class this round set out to remove. Guard the index with
   `kpath[1].isdigit() and int(kpath[1]) < len(steps)`.
2. **Step args have no per-tool key allowlist. This is the one fail-open left**
   (check_recipes.py:174-225). All of these pass: navigate `{url, tabId: 3}`, navigate
   `{url, newTab: true}`, `read {url: https://evil.com/}`, `snapshot {url: ...}`,
   `read {path: C:/Users/x/.ssh/id_rsa}`, and an args key `"@vault(pw)"`. I could not check here
   which of these BrowserOS 0.0.69 honours. Either way, D-1503's narrowing holds only as far as
   the arg surface of the 7 tools it keeps. Allow only the keys the recipes use (navigate: url;
   wait: for, value, timeout; act: kind, value, text, key; grep: over, pattern, limit; read:
   format, selector; snapshot/diff: none) and add a test.
3. **Item 9 is handled by description text only.** create.recipe.yaml:5 states the format, but
   B-599 has no run-time rule that enforces it. Add B-599(8): tools.run checks `start`/`end`
   against `^\d{8}T\d{6}$` (or add an optional arg `pattern:` field). Without it, a model that
   misreads the description still saves an event at Google's default slot.

## Notes

- D-1503 matches the code. The commit messages name T-27, T-27b, D-1503, B-599 and B-1500, and
  each matches the diff. tasks.yaml marks T-27 done, and T-27b is blocked_by T-28. The diff has
  no em dashes, ` -- `, key-shaped strings or vault values.
- A fullwidth backslash (U+FF3C) passes check_url: `https://evil.com\uff3c.mail.google.com/`.
  Chrome's host parser maps it to `\` and rejects the URL, so the browser fails closed. Python's
  NFKC netloc check already catches the fullwidth `/ ? # @`. Round 1 asked for an
  `[a-z0-9.-]` host allowlist, and adding it would close this case in the checker as well.
- Items 4 and 5 are handled by the send, draft and register descriptions plus B-599(6) and (7).
  That is acceptable for now. The prefix-match half of round-1 item 6 (`Send`, `Save` and
  `Register` as name prefixes) is not in B-599: only the nth scope, B-599(2), was carried over.
  B-599(7)'s `refuse_if` partly covers register.
- Waits: `timeout: 30000` is set on the first wait after each navigate in all 7 recipes and on
  draft's wait after its second navigate. All are condition waits within the checker's cap
  (rule 6). The waits after a click (Event saved, Message sent, You're In, thread's Print all)
  keep the 10 s default, which is fine for in-page transitions. The thread wait on
  `[role=main] [aria-label="Print all"]` is sound for opening a thread. Its live check is the
  caller's (I did not run it). Like the events grep and every name target, it depends on an
  English UI.
- B-1500's note that inbox snippets can carry one-time codes to the model is for core and
  supreme, and it is worth acting on.

## Verdict

Round-1 items 1, 2, 3, 7 and 8 are fixed, and each fix is held by a test that fails when the fix
is reverted. Items 4, 5 and 9 are mitigated by description text, and B-599 covers 4 and 5. No
hard rule is broken and no new defect blocks. Should-fix 2 is the one fail-open left in the
checker. Close it before T-28 replaces the checker with toolstore's parser, or carry it into T-28.

Verdict: approve
