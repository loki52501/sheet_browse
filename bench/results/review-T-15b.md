# Review T-15b, round 3 (confirmation, after D-89 round 2)

## Round 3 (confirmation)

Scope: d02b19b (D-598 empty words refused in `group_first`; D-599 `remove`/`rm` are Delete). Worktree F:/sb-swarm/wt/servers/.wt/T15b.

What I ran (CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2):
- `cargo test -p srv-cli`: all green. lib 40 (the 38 from round 2 plus `empty_words_are_refused_anywhere` and `remove_and_rm_are_deletes`), accept_t15 2, accept_t15b 7, accept_t15b_open 3, accept_t15b_proxy 1, cli_t15 5, contract_paths 2, dns_t15 4 (+1 ignored, live DNS), gh_t15 7, gh_t15b 7, http_t15 6, http_t15b 1, proxy_t15b 1.
- `cargo clippy -p srv-cli --all-targets -- -D warnings`: clean. `cargo fmt -p srv-cli --check`: clean.
- I probed local gh 2.93.0 with `--help` targets only, from the scratchpad with a scratch GH_CONFIG_DIR and no token variables. I ran no write and no `auth token`.

**Q1a. Is the round-2 empty-word Blocking item closed? Yes.**

Every path to gh goes through `group_first`, and `group_first` now refuses any `""` before it does anything else (`crates/srv-cli/src/gh/mod.rs:160-166`):
- The call's argv: `gh_argv` (`:137-143`) runs `group_first` for both classify (`:1080`) and call (`:1104`).
- Classify without the alias list: `classify_args` and `classify_with` go to `plan`, then `plan_at` (`:263`).
- Every alias-expansion depth: `alias::expand` feeds `plan_at(depth + 1)` (`:282`), which runs `group_first` again.
- `alias set`'s stored expansion: `alias_set_class` (`:519`, `:530`) splits it and calls `plan_at(depth + 1)`.
- What actually runs is `p.argv` (`:1124-1126`), the output of the innermost `group_first`, so it was checked. Only the `cfg.gh` prefix (from config) and the fixed `alias list` call (`:1017-1018`) are added outside that path.
- `needs_aliases` and `aliases()` only ever see argv that has already passed the check.

The unit test covers all seven round-2 fixtures plus `pr view 5 ''` and `alias set e "'' auth token"`. Each is tested both with the alias list (`p`) and without it (`c`), and it asserts the message.

**Q1b. Does any other word reopen the same class of bug? Yes, a lone `-` after the group. This is the new Blocking item below.**

cobra's `stripFlags` keeps a word as a command only if `s != "" && !strings.HasPrefix(s, "-")`. A lone `-` is not a two-letter flag, so cobra drops it just as it drops `""`.
- At the root, `group_first` already refuses `-` (it starts with `-` and is not a movable flag).
- Inside a group, `subcommand()` (`gh/mod.rs:401`) decides what is a flag with `t.len() > 1 && t.starts_with('-')`. So `-` becomes `Sub::Known(0)`, the subcommand is `-`, and the class is Account.

Other candidates I checked do not reopen it:
- A lone space or a tab word is not dropped. `gh label ' ' delete --help` and `gh label $'\t' delete --help` both printed `gh label <command>`, so gh never reached delete.
- `--` inside a group stops command lookup. `gh repo -- delete --help` printed `unknown command "delete"`, which fails closed. At the root, `--` is refused.
- A NUL word cannot reach gh. Rust's `Command` refuses NUL in an argument, so the spawn fails.
- Other control characters and look-alikes are ordinary words that match no command.
- Two-word aliases (`issue mine`) do not expand in gh 2.93: `gh issue mine` and `gh issue - mine` both went to `issue`, not to the alias.

**Q2. Did d02b19b add a Blocking defect? No.**
- The refusal names no word, so it is safe at depth > 0 (D-596).
- D-599 only raises a class.
- An empty flag value now has to be written `--flag=` (the message says so). This costs some convenience, not correctness.
- No `unwrap` on input, no vault path, no sleep.
- D-598 and D-599 are unique against main.
- The round-2 brief also asked for gh_t15b cases. Those were not added, but the server path uses the same `group_first`, so the unit test covers it.

## Blocking

- **gh: a lone `-` after the group is dropped by gh but read as the subcommand by srv-cli. A Delete is lowered to Account, and the `alias set`/`alias import` dispatch is skipped (rule 4; D-540, D-597, D-598).**
  - Where: `subcommand()` at `crates/srv-cli/src/gh/mod.rs:401` (`is_flag` needs `len() > 1`), and from there `sub_class`, `alias_refusals` and the `("alias", Some(i)) if rest[i] == "set"` arm (`:626-630`).
  - What gh does (help only): `gh label - delete --help` printed `gh label delete <name>`. The same happened for `repo - delete`, `cache - delete`, `secret - delete`, `alias - set` and `alias - import`: each reached the named subcommand.
  - The `-` is passed on as a positional argument:
    - `label - delete --yes` deletes the label named `-`.
    - `cache - delete` deletes the cache keyed `-`.
    - `repo - delete --yes` deletes `<user>/-`.
  - srv-cli classes each of these Account, not Delete. This matters at the gate: `Gate::on_timeout` (`crates/core/src/gate.rs:370-375`) lets an unanswered Account ask proceed, but an unanswered Delete ask is skipped.
  - In 2.93 the `alias - set`/`alias - import` forms do no harm, because `-` becomes the alias name, or import reads stdin, which is null. Even so, they skip the D-591/D-597 checks.
  - Fix: in `subcommand()`, skip a lone `-` the way cobra does (or refuse it, as D-598 refuses `""`). Since `proc::run` closes stdin, refusing every lone `-` word in `group_first` is also safe. Add `label - delete --yes`, `repo - delete --yes` and `alias - set e "auth token"` to the unit test.

## Should fix

1. ~~`gh secret remove` / `gh variable remove` classed Account.~~ **Fixed in d02b19b (D-599).** `remove` and `rm` are now Delete, with a test.
2. **`--hostname` on a command other than `api` is not host-checked** (kept from round 2). `repo_host` (`gh/mod.rs:878`) reads only HOST/OWNER/REPO. Since D-594, a leading `--hostname h attestation verify` also lands there. The class is Account and it asks.
3. **`-R o/r run view 5 --log` returns capped stdout, not the saved log** (kept). `is_run_log` needs `run view` as words 1 and 2.
4. **B-591 is an implicit contract** (kept). The D-595 key is the ToolCtx address. Supreme should answer it before core changes dispatch.
5. **New, dates from D-540 (T-15), not from T-15b.** Some deletes are Account, because `sub_class` looks only at the second word and only at the literal names.
   - Deletes with a third word: `repo deploy-key delete`, `repo autolink delete`.
   - Hyphenated deletes: `release delete-asset`, `project item-delete`, `project field-delete`.
   - All of these were confirmed with `--help`.
   - Fix: class Delete when any word that is not a flag before the positionals is `delete`/`remove`/`rm`, or ends in `-delete`.

## Notes

- **Round 2 Blocking (empty word) is closed by D-598.** Q1a lists the paths. Round 2's description: cobra drops `""`, so `'' auth token`, `auth '' token`, `'' tok`, `'' repo delete --yes`, `repo '' delete --yes`, `'' api --hostname other.host /x` and `alias set e "'' auth token"` ran commands srv-cli classified differently.
- **Round 2 notes still hold.**
  - (a) D-594's mover reads every leading-flag form gh reads.
  - (b) D-595 fails closed only.
  - (c) There is no new vault, fail-open or panic path. A failed run's error still carries gh's own stderr, which may echo words from an expansion (D-596 covers only the server's own refusals).
- **Round-1 should-fix 2, 3 and 4 remain closed.**
- **Hard rules.** d02b19b adds no keys, sleeps, em dashes or captcha handling, and touches nothing outside srv-cli and decisions.yaml. There is no `interfaces.yaml` change.

## Verdict

D-598 closes the empty-word hole on every path an argv or alias expansion takes to gh, and D-599 fixes should-fix 1. But a lone `-` after the group is the same defect one character over. gh drops it and runs `label delete` or `repo delete`, while srv-cli classes the call Account, and an unanswered Account ask proceeds. The fix and its test are each a few lines.

Verdict: request changes
