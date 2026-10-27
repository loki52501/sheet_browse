# Review T-15b, round 1 (D-89)

Scope: `git diff main...HEAD` on swarm/servers-T15b, commits a2f5b59 (D-590, B-590), 3ab364c (D-592 cli), 2db382e (D-591, D-592 gh), 82ba951 (D-593) and 21dc6ea (agent-memory). The merge ad5f8a0 is not reviewed.

What I ran (CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2, after `cargo clean -p srv-cli`):
- `cargo test -p srv-cli`: all green, 68 passed, 1 ignored (live DNS). By binary: lib 37, accept_t15 2, cli_t15 5, contract_paths 2, dns_t15 4 (+1 ignored), gh_t15 7, gh_t15b 3, http_t15 6, http_t15b 1, proxy_t15b 1.
- `cargo clippy -p srv-cli --all-targets -- -D warnings`: clean. `cargo fmt -p srv-cli --check`: clean.
- I probed local gh 2.93.0 with a scratch GH_CONFIG_DIR and no network. I used only `--help` targets, except one `auth token` run, whose output I redacted.

## Blocking

- **gh: a flag before the group is not parsed. This lowers classes, skips the host check, skips the auth refusal, and skips the new alias lookup (rule 4; D-540, D-591, D-592).**
  - Where: `classify_static` (`crates/srv-cli/src/gh/mod.rs:406-431`) takes `argv[0]` as the group and checks hosts only in `argv[1..]`. `needs_aliases` (`gh/mod.rs:246`, new) returns false for a group that starts with `-`.
  - What gh does: cobra strips such flags to find the command, then hands them to that command. Each of these reached the named subcommand: `gh --repo=o/r run delete --help`, `gh --yes=true repo delete --help`, `gh --method=DELETE api --help`, `gh --hostname github.com auth token --help`.
  - What srv-cli does with the real calls:
    - `gh --yes=true repo delete o/r` is classed Account. gh runs a Delete.
    - `gh --repo=o/r run delete 5` is classed Account. D-592 fixed only `run --repo=o/r delete 5`.
    - `gh --hostname github.com auth token` is classed Account, so it asks instead of being refused. gh 2.93 printed a token-shaped credential here (redacted in my probe).
    - `gh --hostname github.com tok` gets no alias lookup. gh expands the alias itself. With alias `tk: auth token --help`, `gh --hostname github.com tk` printed `auth token`'s help. This is exactly what D-591 says cannot happen ("what the gate saw is what runs").
    - `gh --repo=other.host/o/r pr view 5`: the host is never checked, and the gate domain is github.com.
  - Fix: refuse any gh call whose first word is a flag (except a lone `--help`/`--version`), or skip leading flags the same way `subcommand()` does. Add a test with the cases above.

## Should fix

1. **Classify and call can run different expansions.** `call` (`gh/mod.rs:891-896`) re-reads `gh alias list` and re-plans, but compares only the class. If an alias changes between the ask and the run, the call runs a different command, possibly on a different gate domain (another configured GHE host via `-R`/`--hostname`), at an equal or lower class. D-591's "why" says this window is closed; it is closed only for the class. Fix: keep the classify-time plan (argv and domain) and refuse when the call's plan differs. Same shape as T-13c should-fix 1.
2. **Expansion text reaches errors.** The alias.rs module doc (`gh/alias.rs:9-10`) says no expansion goes into an error message. D-591 says errors name an expansion by its first two words only. In fact, `in_alias` (`gh/mod.rs:231-238`) appends the inner message, and the `api_host`/`repo_host` refusals quote up to 200 chars of the expanding token. Example: `far: api https://any.host/x?...` puts the whole URL in the error. Fix: drop the inner message for alias refusals (give the kind only), or fix the claim. Add an assertion on the message text.
3. **The fail-closed path has no test.** Nothing tests that a failing `gh alias list` (non-zero exit without "no aliases configured", output past the cap, or unparseable output) refuses at the handler and nothing runs (`gh/mod.rs:828-853`). The fake gh always exits 0. Fix: add a fixture mode that fails, and a test that asserts `cannot tell whether this names a gh alias` and that no event shows a run.
4. **`gh alias import` is not checked.** `gh alias import FILE` stays Account, with no `alias_set_class` check. So `tok: auth token` can still be stored behind one ask. Today that only matters through the Blocking path, because every planned run refuses it. Refuse it, or class it like `alias set` for each entry.

## Notes

- **D-590 holds.** `Client::for_url` (`http.rs:39`) gives loopback (127/8, ::1, `localhost`) a `no_proxy()` client. `localhost` is pinned to 127.0.0.1, and redirects stay off. `secure_base` still allows plain http only to loopback. `proxy_t15b.rs` sets every proxy variable, sees both loopback GETs reach the origin with the canary header and none reach the proxy, and has a working CONNECT control. B-590 is a sound hand-off for srv-web.
- **D-592 cli holds.** `cluster_has` (`cli.rs:229`) checks the denied letter before the value letter, so `-cS`, `-pc` and `-S x -c` are still refused. I checked the shipped git letters (S G L O U M B) against status, log, diff, show, rev-parse, ls-files and blame. Each takes a value, takes an optional attached value, or is an unknown switch (git errors). In every case the letters after it are its value or harmless sub-level `-c`/`-C`. The global `git -c` cannot follow a subcommand. The dig letters match dig's parser. No class goes down.
- **D-592 gh holds for flags after the group.** `subcommand()` skips `-R/--repo VALUE`. An unknown flag first gives the highest class of any later word, never below Account. The per-command `-w` tables only lower Navigate to Read on read-allowlist commands. `-cRHOST` checks the host.
- **D-591 alias parsing and expansion.**
  - Parsing: plain and quoted values, `''` escapes, a short set of `\` escapes, and continuation lines made unreadable all fail closed.
  - Expansion: `$N` follows gh's ReplaceAll order (`$1` also hits `$10`, as gh does). `$@` is kept as text, and `argv::split` refuses it outside quotes.
  - Nesting stops at depth 4 (`loop: loop`). Shell aliases are refused.
  - Shadowing: an alias that shadows a built-in is looked up only off the read allowlist, so `pr: issue list` makes `gh pr merge 5` run `issue list merge 5` (Read). Wrong, but it runs what was classified.
  - The card names the expansion by two words, and the class is on the card.
- **D-593.**
  - Covered: `markup` (`text.rs:111`) folds case, finds a tag anywhere (BOM and leading whitespace do not matter), and does not count a tag cut at the body end. A doctype or any earlier document tag still counts.
  - Small gaps, none blocking: `<!doctype\thtml>` or two spaces is not a document marker (the `<html>` tag usually is). Tags whose attributes hold `<` or run past 2000 bytes are not counted. A JSON string holding a whole page is shown, as D-593 states. gh's non-`api` output is filtered only for whole pages.
  - The title is cut to 120 chars by `line`. No panic paths: slices sit on ASCII bytes.
- **cli output of an .html file (handoff not_done 4).** Agree it is local file content the user's command printed, not a web answer, so no filter is needed now.
- **Hard rules.** No keys, sleeps, em dashes or captcha handling were added. No `unwrap`/`expect` was added outside tests. No `@vault` resolution is in gh or cli (`argv::from_value` still refuses it).
- **Spec ids.** D-590..D-593 and B-590 are new and unique against main. Each commit's ids match its code. There is no `interfaces.yaml` entry for srv-cli internals, and tool names are unchanged.

## Verdict

The proxy, cli value-letter and HTML fixes are correct and their tests fail when the behaviour breaks. The gh alias work is careful, but a flag before the group still sends gh to the subcommand unparsed. That under-classes deletes, skips the auth refusal and host check, and lets gh expand an alias the server never looked up, which is the case D-591 was written to close.

Verdict: request changes
