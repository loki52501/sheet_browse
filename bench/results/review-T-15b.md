# Review T-15b, round 4 (confirmation, after round 3)

## Round 4 (confirmation)

Scope: 005285c. D-660 makes `group_first` refuse a lone `-` anywhere, next to D-598's empty words. It also adds `is_delete_word` and a third-word rule, so hyphenated and three-word deletes are at least Delete. Worktree F:/sb-swarm/wt/servers/.wt/T15b.

What I ran (CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2):
- `cargo test -p srv-cli`: all green. lib 40, accept_t15 2, accept_t15b 7, accept_t15b_open 3, accept_t15b_proxy 1, cli_t15 5, contract_paths 2, dns_t15 4 (+1 ignored, live DNS), gh_t15 7, gh_t15b 7, http_t15 6, http_t15b 1, proxy_t15b 1.
- `cargo clippy -p srv-cli --all-targets -- -D warnings`: clean. `cargo fmt -p srv-cli --check`: clean.
- gh 2.93.0 probed with `--help` targets only, from the scratchpad with a scratch GH_CONFIG_DIR and token variables unset. No write, no `auth token`.

**Q1. Is the lone-`-` Blocking item closed on every path? Yes.**
- The check is in the same place as D-598, before any flag moving or classing (`crates/srv-cli/src/gh/mod.rs:161`: `t.is_empty() || t == "-"`).
- The round-3 path list still holds. These all go through `group_first`:
  - the call's argv, via `gh_argv` (`:142`);
  - classify without aliases, via `plan` then `plan_at` (`:254`, `:264`);
  - every alias-expansion depth (`:283`);
  - `alias set`'s stored expansion (`:536`).
- What runs is the output of the innermost `group_first`.
- The unit test adds `label - delete --yes`, `repo - delete --yes`, `alias - set x 'auth token'` and `alias - import f.yml`. Each is checked with and without the alias list, and the test asserts the message.
- The `alias_import_is_refused` change (`tests/gh_t15b.rs:287-289`) is a fair loosening. `alias import -` is now refused one step earlier, and still nothing runs.

Are there other words cobra drops or reads as "no word"? None found.
- cobra's `stripFlags` keeps a command word only if `s != ""` and it does not start with `-`.
- `""` and `-` are now refused. `--` was handled in round 3.
- Every other `-x`/`--x` word is a flag. An unknown flag makes `Sub::Ambiguous`, which fails closed.
- Round 3's probes (space, tab, NUL, look-alikes) still apply.
- Case and prefix forms do not route. `gh label DELETE --help` and `gh label del --help` print the group help.
- In 2.93 the only leaf aliases of a delete are `secret remove` and `variable remove` (D-599).
- Group aliases (`cs`, `at`, `rs`, `ext`, `agents`) can only raise a class to Account, because no group on the read allowlist has an alias.

**Q2. Does `is_delete_word` or the third-word rule lower a class or cause a refusal that matters? Neither lowers a class or adds a refusal. The third-word rule is incomplete (Blocking below) and too broad (should-fix 6).**
- They only raise. `sub_class` returns only Read, Account or Delete, and `Delete > Account` in `core::class::Class` (`crates/core/src/class.rs:21-22`). So both `c.max(Class::Delete)` and the new early Delete arm can only raise.
- Too broad: the third word is "the first later word that does not start with `-`" (`gh/mod.rs:618`). That word is often a flag value or a positional, so some reads get raised to Delete:
  - `issue list --label delete`
  - `repo view my-delete-tool`
  - `pr view rm` (a branch named `rm`)
  - `search repos delete-me`
- This fails closed: an unanswered Delete ask is skipped. It costs a turn on a read and is not Blocking.

**Q3. New Blocking defect? Yes. D-660's third-word rule misses a `-R`/`--repo` value placed before the third word.**
- See Blocking. It is not a regression: before 005285c these deletes were Account in every form.
- But D-660 claims to cover this case, and the gate effect is the same as round 3's Blocking item.

## Blocking

- **gh: `repo deploy-key -R o/r delete 5` and `repo autolink --repo o/r delete 7 --yes` are still classed Account. D-660 says they are at least Delete (rule 4; D-660 claim vs code).**
  - Where: `crates/srv-cli/src/gh/mod.rs:618`. `rest[*i + 1..].iter().find(|w| !w.starts_with('-'))` returns `o/r`, the value of `-R`, not `delete`.
  - What gh does (help only):
    - `gh repo deploy-key -R o/r delete --help` and `gh repo deploy-key --repo o/r delete --help` both print "Delete a deploy key from a GitHub repository".
    - `gh repo autolink --repo o/r delete --help` prints "Delete an autolink reference for a repository."
    - `gh repo deploy-key -R o/r -R o/r delete --help` also reaches delete.
  - Today only the `--repo=o/r` and `-Ro/r` forms are classed Delete.
  - Why it matters (same as round 3): an unanswered Account ask proceeds, while an unanswered Delete ask is skipped (`crates/core/src/gate.rs:370-375`).
  - Fix: find the third word with the same parser as the second, `subcommand(&rest[*i + 1..])`.
    - Raise to Delete on `Known(j)` when `is_delete_word(&rest[*i + 1 + j])`.
    - Raise to Delete on `Ambiguous(ws)` when any of `ws` is a delete word.
    - This skips `-R VALUE`/`--repo VALUE` the way gh does.
    - Add `repo deploy-key -R o/r delete 5` and `repo autolink --repo o/r delete 7 --yes` to the Delete list in the unit test.

## Should fix

1. ~~`gh secret remove` / `gh variable remove` classed Account.~~ **Fixed in d02b19b (D-599).**
2. **`--hostname` on a command other than `api` is not host-checked** (kept).
   - `repo_host` (`gh/mod.rs`) reads only HOST/OWNER/REPO.
   - Since D-594, a leading `--hostname h attestation verify` also lands there.
   - The class is Account, so it asks.
3. **`-R o/r run view 5 --log` returns capped stdout, not the saved log** (kept). `is_run_log` needs `run view` as words 1 and 2.
4. **B-591 is an implicit contract** (kept). The D-595 key is the ToolCtx address. Supreme should answer B-591 before core changes dispatch.
5. ~~Hyphenated and three-word deletes classed Account (from D-540).~~ **Fixed in 005285c (D-660)**, with tests, for:
   - `release delete-asset`, `project item-delete`, `project field-delete`;
   - `repo deploy-key delete`, `repo autolink delete`.

   The `-R VALUE` form is still open; it is the Blocking item above.
6. **New: the third-word rule is broader than gh's command tree** (`gh/mod.rs:618`).
   - It raises reads to Delete when a flag value or a positional is a delete word (`issue list --label delete`, `repo view my-delete-tool`).
   - This fails closed.
   - The Blocking fix (reusing `subcommand()`) narrows it. Applying the rule only to the groups that nest (`repo deploy-key`, `repo autolink`) would remove the rest.

## Notes

- **Round 3 Blocking (lone `-`) is closed by D-660.** Q1 lists the paths.
- **Code comments.**
  - At `gh/mod.rs:447`, the doc line "The class of `gh <group> <sub> ...` from the static rules (D-540) alone." now sits above `is_delete_word`, so `sub_class` has no doc.
  - The comment at `:615-616` cites D-599 for the third-word rule, but that rule is D-660.
- **Round 2 and 3 notes still hold.**
  - D-594's mover reads every leading-flag form gh reads.
  - D-595 fails closed only.
  - There is no new vault, fail-open or panic path.
  - A failed run's error still carries gh's own stderr, which may echo words from an expansion (D-596 covers only the server's own refusals).
- **Round-1 should-fix 2, 3 and 4 remain closed.**
- **Hard rules.** 005285c adds no keys, sleeps, em dashes or captcha handling. It touches only srv-cli and decisions.yaml, with no `interfaces.yaml` change. D-660 is unique against main.

## Round 3 (confirmation, kept for history)

Scope: d02b19b (D-598 empty words refused in `group_first`; D-599 `remove`/`rm` are Delete).

- Ran `cargo test -p srv-cli`: all green, lib 40. Clippy and fmt were clean.
- Probed gh 2.93.0 with `--help` targets only.

**Q1a. Round-2 empty-word Blocking item: closed.**
- Every path to gh goes through `group_first`, which refuses any `""` first. The paths are the call argv via `gh_argv`, classify without aliases via `plan`/`plan_at`, every alias-expansion depth, and `alias set`'s stored expansion via `alias_set_class` -> `plan_at(depth + 1)`.
- What runs is the innermost `group_first` output. Only the `cfg.gh` prefix and the fixed `alias list` call are added outside that path.

**Q1b. Same class of bug: a lone `-` after the group (round 3's Blocking item; closed in round 4 by D-660).**
- cobra keeps a word as a command only if `s != "" && !strings.HasPrefix(s, "-")`, so it drops a lone `-`.
- `subcommand()` treated `-` as the subcommand, so `label - delete --yes`, `repo - delete --yes` and `cache - delete` were Account, and the `alias - set`/`alias - import` forms skipped the D-591/D-597 checks.
- Checked and not affected:
  - a space or tab word (not dropped);
  - `--` inside a group (stops command lookup, fails closed);
  - NUL (spawn fails);
  - look-alikes (ordinary words);
  - two-word aliases (they do not expand in 2.93).

**Q2. d02b19b added no Blocking defect.**
- The refusal names no word (D-596), and D-599 only raises a class.
- An empty flag value must now be written `--flag=`.
- No `unwrap` on input, vault path or sleep was added.

## Verdict

D-660 closes round 3's lone-`-` hole on every path, and no other word that cobra drops is left open. Should-fix 5 is fixed for every form it named.

One gap remains. The new third-word rule takes a `-R`/`--repo` value as the third word, so `repo deploy-key -R o/r delete` is still a Delete classed Account, although D-660 says this case is covered. The fix is to reuse `subcommand()` there and add two test lines.

Verdict: request changes
