# Review T-15b, round 5 (confirmation, after round 4)

## Round 5 (confirmation)

Scope: d9c644c, which adds D-586. It also renumbers D-660 to D-585, moving it out of the driver range. `third_is_delete` (`crates/srv-cli/src/gh/mod.rs:456-463`) walks the words after the subcommand with `subcommand()` and requires exactly `delete`/`remove`/`rm`. Worktree F:/sb-swarm/wt/servers/.wt/T15b.

What I ran (CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2):
- `cargo test -p srv-cli`: all green. lib 40, accept_t15 2, accept_t15b 7, accept_t15b_open 3, accept_t15b_proxy 1, cli_t15 5, contract_paths 2, dns_t15 4 (+1 ignored, live DNS), gh_t15 7, gh_t15b 7, http_t15 6, http_t15b 1, proxy_t15b 1.
- `cargo clippy -p srv-cli --all-targets -- -D warnings`: clean. `cargo fmt -p srv-cli --check`: clean.
- gh 2.93.0 probed with `--help` targets only, from the scratchpad with a scratch GH_CONFIG_DIR and token variables unset. No write, no `auth token`.

**Q1. Is the round-4 Blocking item closed? Yes.**
- `third_is_delete` uses the same walk as the second word (`subcommand()`, `gh/mod.rs:401`), so a `-R`/`--repo` value is skipped.
- The unit test now asserts Delete for these three calls (`gh/mod.rs:1737-1739`):
  - `repo deploy-key -R o/r delete 5`
  - `repo autolink --repo o/r delete 7 --yes`
  - `repo autolink --repo=o/r delete 7`
- Each form, checked against what gh does:
  - **`-R=o/r` and `-Ro/r`.** Each is one word that starts with `-R`, so it is skipped as known, and `Known(delete)` gives Delete. gh: `gh repo deploy-key -R=o/r delete --help` and `-Ro/r delete --help` both print "Delete a deploy key".
  - **`--repo=o/r`.** Skipped as known, giving Delete (now tested).
  - **`-h` before the third word.** Skipped, so `repo deploy-key -h delete` is Delete. gh prints the group help and runs nothing. This over-classes a help call, which fails closed.
  - **An unknown flag first** (`repo deploy-key --bogus delete 5`). `Ambiguous([delete, 5])` gives Delete. gh refuses it ("unknown flag").
  - **An unknown flag before the subcommand** (`repo --bogus deploy-key delete`). The second-word `Ambiguous` arm already gives Delete.
  - **`--` before the third word.** It counts as an unknown flag, so the result is `Ambiguous` and Delete. gh stops the command lookup there and runs no delete.
  - **`-R delete 5`.** gh takes `delete` as the `-R` value and `5` as an unknown command. We give `Known(5)` and Account. That matches gh, and nothing is deleted.
- A third word reached through an alias:
  - `plan_at` (`gh/mod.rs:257-313`) classes the expanded argv through `group_first` and `classify_static`. So `alias dk='repo deploy-key'` called as `gh dk -R o/r delete 5` hits the same code.
  - `alias set`'s stored expansion goes through `plan_at(depth + 1)` too.
  - There is no dedicated test, but nothing on this path is specific to aliases (see Notes).
- gh 2.93 has five nested groups: `repo deploy-key`, `repo autolink`, `repo gitignore`, `repo license` and `codespace ports`. Their only deletion word is `delete`, and none of them has an alias. So requiring an exact `delete`/`remove`/`rm` as the third word misses no real deletion.

**Q2. New Blocking defect or lowered class in d9c644c? No.**
- The third-word arm only does `c.max(Class::Delete)`, so it can only raise a class.
- Narrowing the third word from `is_delete_word` to an exact match lowers nothing real:
  - no gh 2.93 command has a hyphenated delete word in third position (see the list above);
  - hyphenated deletes as the subcommand still go through `sub_class` -> `is_delete_word`.
- No `unwrap` on input, sleep, vault path or em dash was added.
- It touches only `gh/mod.rs` and `decisions.yaml`. There is no `interfaces.yaml` change.
- D-585 and D-586 are unique in the branch's `decisions.yaml` and do not appear on main. D-660 is gone from code and memory.

**Q3. The full T-15b decision set (D-590..D-599, D-585, D-586): anything Blocking left? No.**
- Each decision was confirmed against gh 2.93 in its round:
  - D-590..D-593 in round 1;
  - D-594..D-597 in round 2;
  - D-598 and D-599 in round 3;
  - D-585 in round 4;
  - D-586 in round 5.
- Every one only refuses a call or raises a class.
- The open items (Should fix 2, 3, 4 and 6) either fail closed or are gated by an ask, so none is Blocking.

## Blocking

- none

## Should fix

1. ~~`gh secret remove` / `gh variable remove` classed Account.~~ **Fixed in d02b19b (D-599).**
2. **`--hostname` on a command other than `api` is not host-checked** (kept).
   - `repo_host` (`gh/mod.rs`) reads only HOST/OWNER/REPO.
   - Since D-594, a leading `--hostname h attestation verify` also lands there.
   - The class is Account, so it asks.
3. **`-R o/r run view 5 --log` returns capped stdout, not the saved log** (kept). `is_run_log` needs `run view` as words 1 and 2.
4. **B-591 is an implicit contract** (kept). The D-595 key is the ToolCtx address. Supreme should answer B-591 before core changes dispatch.
5. ~~Hyphenated and three-word deletes classed Account (from D-540).~~ **Fixed in 005285c (D-585, first numbered D-660).** The `-R VALUE` form was fixed in d9c644c (D-586).
6. **The third-word rule is broader than gh's command tree. Partly fixed in d9c644c (D-586).**
   - Fixed: `repo view my-delete-tool` stays Read (asserted at `gh/mod.rs:1744`). `search repos delete-me` now stays Read the same way, but has no test.
   - Still raised to Delete:
     - a positional that is exactly `delete`/`remove`/`rm`, such as `pr view rm` for a branch named `rm`;
     - a delete word after an unknown flag (`issue list --label delete` is `Ambiguous([delete])`).
   - This fails closed and costs a turn on a read.
   - To remove it, apply `third_is_delete` only to the groups that nest (`repo deploy-key`, `repo autolink`).

## Notes

- **Round-4 notes are fixed.**
  - The `sub_class` doc comment is back above `sub_class` (`gh/mod.rs:465`).
  - The third-word comment in `classify_static` now cites D-585 (`:628`).
- **New doc nit.** The `third_is_delete` doc (`:454`) cites D-585 for "found the way the subcommand is", but that rule is D-586.
- **Test gap (not Blocking).** No test reaches the third word through an alias expansion. Two examples:
  - `alias dk='repo deploy-key'` called as `gh dk -R o/r delete 5`;
  - an `alias set` expansion that contains `deploy-key -R o/r delete`.

  Both go through the same `classify_static`, so such a test would only guard against regressions.
- **Older notes still hold.**
  - D-594's mover reads every leading-flag form gh reads.
  - D-595 fails closed only.
  - There is no new vault, fail-open or panic path.
  - A failed run's error still carries gh's own stderr, which may echo words from an expansion (D-596 covers only the server's own refusals).
- **Round-1 should-fix 2, 3 and 4 remain closed.**

## Round 4 (confirmation, kept for history)

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

**Round 4 Blocking item (closed in round 5 by D-586):**

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

D-586 closes round 4's Blocking item. The third word is now found with the same `subcommand()` walk as the second word. So every `-R`/`--repo` form reaches Delete: `-R o/r`, `-R=o/r`, `-Ro/r`, `--repo o/r` and `--repo=o/r`. An unknown flag before the third word reaches Delete too.

The change only raises a class. Narrowing to an exact `delete`/`remove`/`rm` misses no deletion in gh 2.93. Should-fix 6 is partly fixed. The items still open either fail closed or ask.

Verdict: approve
