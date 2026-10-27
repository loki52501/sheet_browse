# Review T-15b, round 2 (final, D-89)

Round 1 (03dd5d5) asked for changes. The Blocking item: a gh flag before the group was not parsed. `gh --repo=o/r run delete 5` was classed Account, `gh --hostname github.com auth token` asked instead of being refused, the alias lookup was skipped, and the host was not checked. There were four should-fix items: (1) classify and call could run different expansions, (2) expansion text reached errors, (3) the failing `gh alias list` path had no test, (4) `alias import` was not checked.

Scope this round: `git diff e67f879..HEAD -- crates/ config/`. Commits: 2b35fe3 (D-594), 829d71f (D-595, B-591), 8d7200b (D-596, D-597). I also read the handoff agent-memory/handoffs/T-15b-rust-coder-2.yaml.

What I ran (CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2, after `cargo clean -p srv-cli`):
- `cargo test -p srv-cli`: 84 passed, 0 failed, 1 ignored (live DNS). By binary: lib 38, accept_t15 2, accept_t15b 7, accept_t15b_open 3, accept_t15b_proxy 1, cli_t15 5, contract_paths 2, dns_t15 4+1, gh_t15 7, gh_t15b 7, http_t15 6, http_t15b 1, proxy_t15b 1. All 5 red rows in accept-T-15b.md are now green.
- `cargo clippy -p srv-cli --all-targets -- -D warnings`: clean. `cargo fmt -p srv-cli --check`: clean.
- I probed local gh 2.93.0 from a scratch dir with a scratch GH_CONFIG_DIR and no token variables. Every target was `--help`, and nothing wrote or ran `auth token`.

## Blocking

- **gh: an empty word in front of the command does the same thing the round-1 leading flag did. It skips the auth and config refusals and the alias expansion, it lowers a Delete to Account, and it misses the `--hostname` check (rule 4; D-540, D-591, D-594).**
  - Where: `group_first` (`crates/srv-cli/src/gh/mod.rs:161`) returns the argv unchanged when `argv[0]` is not a flag. `""` is not a flag, so `""` becomes the group. `classify_static` (`gh/mod.rs:554-628`) never sees `auth` or `config` as the group, and `sub_class("", Some("auth"))` is Account. The same thing happens inside a group: `subcommand()` (`gh/mod.rs:392`, `:411`) returns `Known` for an empty word, so `repo "" delete` has the subcommand `""`, which is Account.
  - What gh does: cobra's `stripFlags` drops `""` (`case s != "" && !strings.HasPrefix(s, "-")`), so the next word is the command. Each of these reached the named command in my probes: `gh "" auth token --help`, `gh auth "" token --help`, `gh "" config get --help`, `gh "" api --help`, `gh repo "" delete --help`, `gh "" repo delete --help`, `gh --repo=o/r "" run delete --help`. With alias `tk: auth token --help`, both `gh "" tk` and `gh "" "" tk` printed `auth token`'s help.
  - What srv-cli does with the real calls:
    - `args: "'' auth token"` (or `["", "auth", "token"]`) is classed Account and asks. After one yes, gh prints the token.
    - `'' config get oauth_token` is the same.
    - `'' tok` gets no lookup for `tok`. gh expands it itself, which D-591 and D-595 say cannot happen.
    - `'' repo delete --yes` and `repo '' delete --yes` are Account. gh's `repo delete` gets `RepoArg ""`, falls back to the base repo, and deletes it.
    - `'' api --hostname other.host /x` is never host-checked. `repo_host` sees `other.host` as a lone word, and the gate domain is github.com.
    - An alias expansion has the same hole. `alias set e "'' auth token"` is checked through the same `plan_at`, so it is Account.
  - Fix: in `group_first`, and in `subcommand()` before the subcommand is known, refuse an empty word or skip it the way cobra does. Keep `""` valid as a flag value (`--body ""`). Add the cases above to `a_flag_before_the_group_is_read_where_gh_reads_it` and gh_t15b.

## Should fix

1. **`gh secret remove NAME` and `gh variable remove NAME` are classed Account, but each runs a delete.** These two cobra aliases (`USAGE: gh secret delete`, `gh variable delete`) were confirmed with `--help`. The bug is in `sub_class` (`gh/mod.rs:438-446`), which matches the literal `delete` only. It dates from D-540 (T-15) and is not new in T-15b. Fix: add `remove` for `secret` and `variable`, plus a test.
2. **`--hostname` on a command other than `api` is not host-checked.** `gh attestation verify ... --hostname other.host` goes to that host, but `repo_host` (`gh/mod.rs:868-939`) reads only HOST/OWNER/REPO, so the gate domain stays github.com. D-594 now also moves a leading `--hostname other.host attestation verify ...` to this unchecked spot. The class is Account and it asks, so this is not blocking. Fix: in `repo_host`, read `--hostname[=]VALUE` the way `api_host` does.
3. **`-R o/r run view 5 --log` returns capped stdout, not the saved log** (handoff not_done 5). `is_run_log` (`gh/mod.rs:960-966`) needs `run view` as words 1 and 2, and the moved argv is `run -R o/r view ...`. The class is right. Only the log saving is missed.
4. **B-591 is an implicit contract.** The D-595 key is the ToolCtx address. I checked `crates/core/src/mcp.rs:1039-1155`: one `ctx` local goes to classify (`:1056`) and then to call (`:1155`). If core ever rebuilds the context, every gh call refuses, so supreme should answer B-591 before core changes dispatch.

## Notes

- **(a) D-594, apart from the empty word.** I could not break the mover. The moved argv is what is classified, shown on the card and run (`gh_argv`, `plan_at:254`, `call:1114`). So any gap between how the mover and gh read a call can only change the user's intent. It cannot change what runs against what was classified. The forms the mover accepts are the forms cobra's `stripFlags` reads the same way at root: root knows only `--help` and `--version`, so an unknown `-R`, `--repo` or `--hostname` takes the next word, and `--x=v` takes none. Cases I checked:
  - `--repo -R o/r run delete 5`: the group becomes `o/r`, and gh fails with `unknown command "o/r"`.
  - `-R=o/r ...`, `-Ro/r ...`, `--help ...`, `--version ...`, `-h`, `-`, `--=x` and `--` are all refused.
  - `--help=false run delete 5` is moved and classed Delete. gh does route it to `run delete`.
  - `---repo=x` is moved. Ambiguous words give Account or higher, and gh errors on the flag syntax.
  - `--hostname=` (empty) is moved. On `api` the empty host is refused, and on `auth` the call is refused.
  - A repeated `--repo` with another host is refused by `repo_host`, which checks every token.
  - A moved flag plus an alias (`-R o/r rd 5`, `-R o/r tok`) works: the alias is found after the move, and gh would also hand the moved flags to the alias as its arguments.
  - A value that looks like a group name (`--repo auth tok`) works: gh and the mover both take `auth` as the value.
  - Case and unicode look-alikes (`--Repo`, en dash `–repo`) are refused, or become an unknown group that is looked up as an alias and that gh rejects.
- **(b) D-595 fails closed only.** Call runs only when all of these hold: an entry has this ctx's address, the same run id, the same named argv, and the same plan as call's fresh plan, and the class is at or under `ctx.class`.
  - A stale entry, from a call the gate refused, is replaced by the next classify at that address (`remember` retains, then pushes). classify always runs before call in dispatch.
  - Even a stale match would run only the plan that call just recomputed, at a class the gate approved.
  - Two live dispatches cannot share an address. Eviction past 64 and a moved or rebuilt context both refuse.
  - A poisoned mutex is handled with `into_inner`, and nothing in it panics.
  - The test swaps an alias to the same class on another GHE host and asserts the refusal and `.ran` absent.
- **(c) No new vault, fail-open or panic path in the round-2 code.** `argv::from_value` still refuses `@vault(`, and the kept plans hold only argv words. `group_first` slices are bounded (`argv[0]` only when the argv is non-empty). There is no `unwrap` or `expect` outside tests.
  - One gap in D-596: a failed run's error still includes gh's own stderr (`call:1128-1133`). That stderr may echo words from an expansion, such as an unknown flag. D-596 covers only the server's own refusals.
- **should-fix 2, 3, 4 from round 1 are closed.**
  - 2: `command_words` and the quiet host refusals are tested. The unit test asserts that `any.host`, `secret.host`, `zz9` and `--bogus` are absent from the error text.
  - 3: `a_failing_alias_list_refuses_the_call` asserts the message, that no card is shown, that no ok event appears, and that the call runs once the list works.
  - 4: `alias import` is refused, and so is `alias set` or `alias import` after an unskipped flag. The coder's change to accept_t15b.rs (import now expected refused with no card) makes the test stricter and matches D-597.
- **Spec ids.** D-594..D-597 and B-591 are new and unique against main, and each commit's ids match its code. There is no `interfaces.yaml` change (B-591 is a proposal for supreme).
- **Hard rules.** No keys, sleeps, em dashes or captcha handling were added.

## Verdict

D-594 closes the leading-flag hole for every flag form gh reads. D-595, D-596 and D-597 are correct and tested, and the class only ever stays or rises. But an empty word in front of the command reaches the same unparsed path. `'' auth token` asks instead of being refused and prints the token after one yes. `'' tok` runs an alias the server never expanded, and `'' repo delete --yes` is an Account-class delete. This is the round-1 Blocking defect in another form, so the round cannot pass.

Verdict: request changes
