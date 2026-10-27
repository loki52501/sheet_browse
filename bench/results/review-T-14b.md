# Review T-14b, round 4 (confirmation)

Scope: `git show 7b4e2f7` (D-556). `shell::no_bare_discovery` (`shell/mod.rs:495`) sets `GIT_CONFIG_COUNT=1`, `safe.bareRepository=explicit` on every line the seal does not cover (`shell/mod.rs:717`). The seal keeps its own copy of the key (D-555). There is a new dispatch test, `tests/accept_t14b_bare.rs`, plus an unsealed case added to the D-555 unit test (`shell/mod.rs:896`).

What I ran (HEAD 7b4e2f7, CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2, git 2.50.1.windows.1):
- `cargo test -p srv-files -j 2 --no-fail-fast`: 125 passed, 0 failed, 4 ignored. That is lib 77, accept_t13c 6, accept_t14 22, accept_t14_history 1, accept_t14b 13, accept_t14b_bare 1 (new), accept_t14b_git 1, accept_t14b_home 1, accept_t14b_seal 1 and contract_paths 2.
- `cargo clippy -p srv-files --all-targets -- -D warnings`: clean. `cargo fmt -p srv-files --check`: clean.
- Probe setup:
  - A `git archive` copy of HEAD in the scratchpad, plus one probe test file.
  - Each line goes through `dispatch` with the real gate, a `ScriptedAsker` that answers nothing, and MemStore.
  - The probe records whether a card was shown, the ledger class and mode, the first output lines, and the marker files the plants wrote.
  - Input: `lines.txt`, `lines2.txt` (the round 1 to 3 probes) and two new files, `lines4q2.txt` and `lines4d.txt`. That is 212 lines, each group in a fresh base folder.
- Live control in bash, same plants, `GIT_CONFIG_COUNT=1 safe.bareRepository=<v>`:
  - With `all`: `diff --no-index; git init` fired the diff driver, `git gc` fired the hook, and `git add -A` (core.bare=false, core.worktree) fired the clean filter.
  - With `explicit`: none of them fired. So the plants in the probe were live.

## Q1: is the round-3 Blocking item closed?

Yes. Across all 212 probe lines, no marker was written.
- **The four round-3 lines** (group `m`, plus `c`/`c2`) now run Fill/Allow with no ask, and git refuses discovery:
  - `cd foo && git diff --no-index a b; git init -q` falls back to a plain diff, and the driver does not run.
  - `... && git add nothing-here` and `git log -p; git stash list; git commit --allow-empty -qm x` write no marker.
  - `cd foo && git gc` (Fill/Allow) ends "fatal: cannot use bare repository ... (safe.bareRepository is 'explicit')". So the env pair reached git on an unsealed line, and the hook did not run.
  - `cd foo && git add -A`, `git commit` and `git -C foo add -A` with the core.worktree and filter plant end with the same fatal error. The filter did not run.
- **Other unsealed git lines** end with the same fatal error: `git tag`, `git branch b z`, `git fetch . HEAD`, `git -C . gc`.
- **Read lines (sealed)** behave as in round 3. This covers groups a, a2, f (a bare-shaped base folder, from `sub/`) and `git submodule status`.
- **The dispatch test is a real test.** Its control forces `all` and must fire diff, filter and hook. The coder's break-check matches my control.

## Q2: can a line undo the pair without an ask?

No. Every form I tried asked (Delete, TimeoutSkip, no marker):
- **Inline assignments:** `GIT_CONFIG_COUNT=0 git ...`, `GIT_CONFIG_VALUE_0=all git ...`, `GIT_CONFIG_PARAMETERS=... git ...`, and a bare `GIT_CONFIG_VALUE_0=all;`.
  - Lower and mixed case (`git_config_count=0`, `Git_Config_Count=0`, `git_dir=.`) also ask. This matters on Windows, where env names are case-insensitive.
- **Removing or redefining the variable:**
  - `env -u`, `env -i`, `exec env -u`, `unset`, `unset -v`, `export GIT_CONFIG_COUNT=0`, `export -n`, `export git_config_value_0=all`;
  - `declare +x`, `typeset -n`, `local`, `readonly`, `read`, `mapfile`, `printf -v`, `getopts`, `let`, `for GIT_CONFIG_COUNT in`;
  - `(( ))`, `: ${X:=0}`, `set -a; X=0`, and a function definition.
- **git's own options:** `git -c safe.bareRepository=all`, `--config-env=...`, `--git-dir=.`, `GIT_DIR=.`, and `-c include.path=<planted>`.
- **Other programs that run git:**
  - `make` with a planted Makefile (D-550).
  - `sh s.sh`, `bash s.sh`, `. ./s.sh`, `source s.sh` and `./s.sh` with a planted script. `chmod +x` also asks.
  - `bash -c`, `sh -c`, `cmd //c "set ...&& git"`, `powershell`, `pwsh`.
  - The wrappers `timeout`, `nohup`, `nice`, `time`, `command`, `xargs` and `find -exec`.
- **Aliases:**
  - An alias in the planted bare `foo/config` is never read (discovery is refused), and `git d` asks ("not known to be safe").
  - An alias in a planted file reached by `GIT_CONFIG_GLOBAL=` or `-c include.path=` asks.
  - Writing an alias into a real `r/.git/config` asks (strict git name).
- **`git config --global safe.bareRepository all`** is Account class. The probe gate ran it on ask timeout (TimeoutProceed). It does not reopen the hole: command scope beats global, and the next line still got "cannot use bare repository". See Notes for the side effect on this machine.
- **git's implicit-bare exceptions** (`.../.git`, `/.git/modules/`, `/.git/worktrees/`) cannot be planted: `mkdir -p d/.git/modules/m/objects` and `mkdir -p wt/.git` both ask.

## Q3: new defects in 7b4e2f7

- No new rule break. There are no keys, `.env`, sleeps, em dashes, captcha handling, raw HTML or vault paths in the diff.
- The commit ids (T-14b, F-2, D-556) match the change and `decisions.yaml`. srv-files has no `interfaces.yaml` entries.
- **A user-approved Delete line that sets its own `GIT_CONFIG_*` pairs:**
  - An inline or exported `GIT_CONFIG_COUNT=N` with its own KEY/VALUE pairs replaces ours. That is the approved line's choice, and git reads it consistently.
  - A line that sets only `GIT_CONFIG_KEY_0`/`VALUE_0`, with no COUNT, now takes effect, because our COUNT=1 picks it up. It also replaces `explicit` for that approved line.
  - A script that appends at index `${GIT_CONFIG_COUNT:-0}` works correctly.
  - So nothing breaks silently. Should-fix 10 notes the one surprise.
- **Expected trade:** an approved line in a real bare repository (`cd mirror.git && git fetch`) now fails with "cannot use bare repository" and needs `--git-dir`, which asks. The error text says so, so it costs at most one turn.

## Blocking

- none

## Should fix (T-14c candidates)

Items 1 to 7 are carried from round 3, unchanged. See the round 3 record below.

8. **Carried, and now in two places:** the D-555 unit test panics when git is not on PATH. See `shell/mod.rs:849` (`.status().unwrap()`) and the new `c.output().unwrap()` at `shell/mod.rs:902`. Use `Runner::git_program()` and skip with a reason.
9. **Closed by `tests/accept_t14b_bare.rs`.** One gap remains: the test checks only that no marker was written (`accept_t14b_bare.rs:195`). It never checks that each line ran with Allow and showed no card. If the classifier later made these lines ask, the test would pass without testing D-556. Assert that `asker.cards()` is empty and that each ledger mode is Allow.
10. **`no_bare_discovery` sets index 0 unconditionally** (`shell/mod.rs:495`). An approved line that sets `GIT_CONFIG_KEY_0`/`VALUE_0` without a COUNT now has them applied, which it did not before. Document this in the tool description, or not; it is harmless for safety.

## Notes

- **My probe changed this machine's global gitconfig, and I restored it.** The probe line `cd foo && git config --global safe.bareRepository all` ran on ask timeout (Account, TimeoutProceed). The runner keeps HOME, so it wrote a new `[safe] bareRepository = all` section to `C:/Users/Admin/.gitconfig`. The section was new, at the end of the file. I removed it with `git config --global --unset safe.bareRepository`, and the file is back to its 12 earlier lines.
  - This is gate policy for Account on timeout, not something 7b4e2f7 introduced.
  - The supreme orchestrator may want to confirm that a timeout should proceed for `git config --global`.
- The control in `accept_t14b_bare.rs` asserts the union of markers, not one per line. `git log -p -1` fires nothing even under `all`, because the repository has no commits. That is fine as a regression guard.
- **Hook:** probe files were written into the session scratchpad with a heredoc. Nothing in the repo was edited except this report.

---

# Round 3 record (kept; headings demoted so the gate reads round 4)

Scope: `git show ad4ddf9` (D-555). The seal adds `safe.bareRepository=explicit` in command scope (`shell/mod.rs:541`). There is one unit test, `shell::tests::a_planted_bare_repository_runs_nothing_under_the_seal` (`shell/mod.rs:831`).

What I ran (HEAD ad4ddf9, CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2, git 2.50.1.windows.1):
- `cargo test -p srv-files -j 2 --no-fail-fast`: 124 passed, 0 failed, 4 ignored. That is lib 77 (+1, the new test), accept_t13c 6, accept_t14 22, accept_t14_history 1, accept_t14b 13, accept_t14b_git 1, accept_t14b_home 1, accept_t14b_seal 1 and contract_paths 2.
- `cargo clippy -p srv-files --all-targets -- -D warnings`: clean. `cargo fmt -p srv-files --check`: clean.
- Probe: a scratch copy of HEAD outside the repo, plus one extra test file. Each line goes through `dispatch` with the real gate, a `ScriptedAsker` that answers no ask, and the ledger. Real bash and git run. Planted drivers and hooks write marker files.

### Q1: is the round-2 chain closed under the seal?

Yes, for lines where every git is Read (the sealed path). Each step below was a separate call through the shell tool:
1. `git init --bare foo` (Fill/Allow), or `mkdir -p foo/objects foo/refs/heads` plus `foo/HEAD` (Fill/Allow).
2. Add `[diff "x"] command/textconv` to `foo/config`, and `* diff=x` to `foo/info/attributes` (both Fill/Allow).
3. In `foo`, these all run as Read/Allow and write no marker: `git diff --no-index a b`, `git log -p`, `git show`, `git blame a`, `git grep one`, `git diff HEAD HEAD`, `git -C foo diff --no-index a b` and `git -C foo log -p`. Discovery fails with "cannot use bare repository ... (safe.bareRepository is 'explicit')". `--no-index` falls back to a plain diff.
4. The same holds from a subfolder (`foo/refs`), and when the base folder itself is bare-shaped (`HEAD`, `objects/`, `refs/` and `config` written into the base).
5. `git cat-file --textconv` asks, as before.

The unit test is a real test. The control writes the marker and the sealed run does not. The control forces `safe.bareRepository=all`, so a machine whose global config already says `explicit` cannot hide a regression.

### Q2: other ways git reaches a config the classifier never scanned

- **Blocking: an unsealed line still finds the planted bare repository (see Blocking 1).** The seal is applied only when `Verdict::seal_git()` is true (`util.rs:46`, `shell/mod.rs:694`). So any line that holds one git above Read runs every git in it with git's default `safe.bareRepository=all`. That includes the Read gits.
- **`.git` FILE with `gitdir:` pointing to a planted folder: closed (asks).**
  - `find_repo` follows the gitdir (`classify.rs:4022`) and `repo_config_files` scans the target's config.
  - Every way to make such a file asks: writing `.git` (dotfile), `git init --separate-git-dir` ("not an option the classifier knows to be safe"), `git worktree add` ("writes files it cannot name in advance").
  - Should-fix 6 covers the fail-open fallback.
- **GIT_DIR / GIT_COMMON_DIR / GIT_CONFIG_*: closed.** git reads these from the environment only, never from a file. Every form I tried asks:
  - `GIT_DIR=foo git log`, `env GIT_DIR=...`, `export`, `declare -x`, `eval`;
  - `git --git-dir=foo` and `git -c safe.bareRepository=all`;
  - the `GIT_CONFIG_COUNT/KEY/VALUE` prefixes.
  - `sanitize_env` strips the inherited `GIT_*` variables.
- **`commondir` and worktree `gitdir` files: closed for Read.**
  - `repo_config_files` follows `commondir` (`classify.rs:4170`). A worktree's `gitdir` file only says where the worktree is, so git never reads config through it.
  - A `worktrees/<n>/commondir` inside a bare repository is a plain path (Fill). But it is reached only through a `.git` file, which `find_repo` follows and scans.
- **`core.worktree`: not a config route.** It does not change which config git reads. In a bare repository the seal refuses discovery anyway. It does make Blocking 1 worse: with `core.bare=false` and `core.worktree` in a planted bare config, `cd foo && git add -A` runs the planted clean filter.
- **Parent discovery: closed for Read.**
  - `find_repo` walks up as git does. A `.git` above the base folder is found by both and scanned.
  - Bare-shaped parents are refused by `explicit`. The one exception is a folder named exactly `.git`, which `find_repo` also finds from its parent.
  - One mismatch is left, should-fix 7.
- **safe.directory: not a route.** It is a kept key from the global config, and git ignores it in repository config. It relaxes the owner check only; it never adds a repository.
- **includeIf / include in repository config: closed.** `unsealable_key` (`classify.rs:4300`) makes `include.path` and `includeif.*.path` Delete, and `classify/tests.rs:1042` covers it. Reaching an unscanned repository to carry an include needs one of the routes above, and those ask or are refused.

### Q3: new defects in ad4ddf9

- No new rule break.
- The key comes after the kept keys, so command scope wins over a global `safe.bareRepository=all` that was copied in.
- The test's `GIT_CONFIG_COUNT=1` control pair is overwritten by `apply`, because the count is reset and index 0 is rewritten. So it does not leak into the sealed run.
- The minor test issues are should-fix 8 and 9.

### Blocking

- **A planted bare repository outside runs/ still runs its drivers and hooks with no ask when the line is unsealed (rule 4; same class as the round-2 item, not closed).**
  - D-555 only covers lines where every git is Read. If any git in the line is above Read, `git_above_read` is set (`classify.rs:4362`), the seal is skipped (`shell/mod.rs:694`), and git discovers `foo` as a bare repository again.
  - The planted files are plain paths, so Fill/Allow outside runs/: `foo/config`, `foo/hooks/*` and `foo/info/attributes`. `find_repo` (`classify.rs:4006`) never sees them.
  - Reproduced through dispatch. Each run line below was ledgered Fill/Allow, never asked, and wrote its marker:
    1. After the round-2 plant: `cd foo && git diff --no-index a b; git init -q` ran `diff.x.command`. This is the round-2 Read diff; an unrelated Fill git in the same line removes the seal.
    2. `cd foo && git diff --no-index a b && git add nothing-here`, and `cd foo && git log -p; git stash list; git commit --allow-empty -qm x`: same result.
    3. `printf '#!/bin/sh ...' > foo/hooks/reference-transaction` (Fill), then `cd foo && git gc` (Fill/Allow) ran the planted hook.
    4. `[core] bare=false, worktree=<base>/wt` and `[filter "x"] clean=...` added to `foo/config`, `* filter=x` in `foo/info/attributes`, then `cd foo && git add -A` (Fill/Allow) ran the planted clean filter.
  - A planted program runs with no ask. For a normal repository the strict git names block the same plant (`.git/config` and `.git/hooks` are Delete), so this sidesteps D-525/D-547.
  - **Fix (pick one, then test it through dispatch with the four lines above, each with a live control):**
    - (a) Set `GIT_CONFIG_COUNT`/`safe.bareRepository=explicit` on every git the shell tool runs, sealed or not. It is one pair on the unsealed env, after `sanitize_env`. A user who works inside a bare repository must then pass `--git-dir`, which asks. That is acceptable.
    - (b) Make `find_repo` treat a folder that git would take as a git dir (`HEAD` plus `objects/` plus `refs/`, as git's `is_git_directory` checks) as a repository. Scan its config. Make any git there above Read Delete, or make writes under such a folder's `config`, `hooks/`, `info/`, `worktrees/` and `commondir` strict git names.
    - (a) is smaller and also covers bare-shaped folders made after classification in the same line. Note that `git init --bare foo && ... && cd foo && git diff` in one line already asks today, because `cd` into a missing folder makes the cwd unknown. That holds only by accident.

### Should fix (T-14c candidates)

Carried from round 2 (unchanged):

1. **D-550 (2) cost** (`classify.rs:1697`).
   - Every version probe asks. A narrower Read rule could cover exactly `make|node|python|python3|java|gcc|clang|docker --version`. It needs all of: `which` resolves to a PE `.exe` (not a `.cmd`/`.bat` shim), the `.exe` is outside version-manager shim folders, and the line has no other command.
   - Keep Delete for rustup proxies, go, yarn/pnpm/npm, dotnet, mvn, gradle and pytest.
2. **CODE_DIRS lacks `.mvn`, `.yarn` and `.volta`** (`paths.rs:184`). Non-dot project files (`mise.toml`, `rust-toolchain.toml`, `global.json`, `pytest.ini`, `package.json`) are scratch in runs/ and Fill elsewhere. Add these before anything makes a consumer Read again.
3. **create_dirs off Windows** (`safeio.rs:459`). A folder made through a link swapped in during the syscall is refused but left in place. Fix the doc at `safeio.rs:144`, or use `mkdirat`.
4. **`create_dirs` asks for FILE_ADD_SUBDIRECTORY even when nothing is missing** (`safeio.rs:296`). Use `check_entry` when `missing` is empty.
5. **Deferred from round 1:**
   - The submodule walk cap ends silently.
   - Non-absorbed submodule `.git` folders, and `.git` files outside `modules/`.
   - Two git spawns per sealed call.
   - Global `filter.lfs` is dropped, and `git config --global` shows the empty seal file.

New in round 3:

6. **`find_repo` fails open on a gitdir it cannot resolve** (`classify.rs:4026`).
   - `paths::resolve` errors on `//?/` and `//./` paths, a `:` stream name and some `~` forms. The fallback is then `git_dir = <the .git file>`, so no config is scanned, while git may still follow the gitdir.
   - Return a repository that makes Read git Delete ("gitdir the classifier cannot follow") instead.
   - Not Blocking: writing a `.git` file asks.
7. **An invalid `.git` folder stops `find_repo`, but git skips it and walks on up.** git's `is_git_directory` fails, so it keeps going to a parent repository, and the classifier scans the wrong (empty) folder.
   - Fix: stop only at a `.git` that has `HEAD`, `objects/` and `refs/`, or at a gitfile. Otherwise continue.
   - Not Blocking: `mkdir x/.git` asks (probe), and the parent repository's config must already name a driver.
8. **The D-555 unit test panics when git is not on PATH** (`shell/mod.rs:837`, `.status().unwrap()`). The other git tests skip with a reason. Use `Runner::git_program()` and skip.
9. **D-555 has no dispatch-level test.** Round 2 asked for the chain as an accept_t14b case through dispatch. The unit test checks `GitSeal::apply` only. It cannot catch a line that is classified unsealed, which is exactly how Blocking 1 got through. Fold this into the Blocking fix's test.

### Notes

- **Side effect of D-555:** `cd my-bare.git && git log` (or the same through `-C`) now fails with "cannot use bare repository" under a Read line. `--git-dir` asks. This is the intended trade, but the tool description or the error text could say so, to save a turn.
- **Hard rules in ad4ddf9:** no keys, `.env`, sleeps, em dashes, captcha handling, raw HTML or vault paths. `unwrap` appears only in the test. The commit ids (T-14b, F-2, D-555) match the change and the D-555 entry in `decisions.yaml`. There are no `interfaces.yaml` entries for srv-files.
- **Hook:** the probe file was written with a shell heredoc into the session scratchpad, because the Write hook (`guard_write.py`) refuses paths outside the worktree. Nothing in the repo was edited except this report.

---

## Round 2 record (kept)

Round 1 (HEAD 0fe3918) asked for changes. The Blocking item was D-545: dotfiles in runs/ were scratch, so a Read line could write `.yarnrc` and a Read `yarn --version` would run it. Acceptance rows 4b, 6b, 7b and 7c failed.

Round 2 scope: `git diff 85cd79c..HEAD -- crates/`, code commit 9e7e981 (D-550..D-554). Tests: 123 passed, 0 failed, 4 ignored. Clippy and fmt clean.

#### Round-1 items (round 2 findings)

- **Blocking (D-545): closed.** `code_path` (`paths.rs:236`) makes every dotfile Delete again, runs/ included.
- **7c: closed.** `runs_code` (`classify.rs:1697`) makes every RUNS_CODE use Delete, `--version` included.
- **4b: closed.** `--pathspec-from-file` (`classify.rs:5050`) makes `tree_reaches_runs` true.
- **6b: closed on Windows.** `create_dirs` (`safeio.rs:144`) makes each missing level with NtCreateFile, relative to a verified handle.
- **Should-fix 2 (D-554) and 5 (D-553): closed.**
- **The coder's edit to the tester's test only tightens it.**

#### Round 2 Blocking (now partly closed by D-555, see round 3)

- A bare repository planted outside runs/ ran its own diff driver under a Read git. `find_repo` finds only a `.git` entry, and the seal left `safe.bareRepository` at `all`.

#### Round 2 notes

- The NtCreateFile FFI (`safeio.rs:306`) is sound: layouts, lifetimes, length, refused names, handle ownership and the error mapping.
- `open_existing`'s rights match std.
- No other Read program read cwd config that names a program. In the cmd fallback, only plain argv runs.

Round 2 ended with request changes.

### Round 3 verdict


D-555 closes the round-2 chain wherever the seal applies: every Read git line, from the folder, a subfolder or a bare-shaped base folder. Tests and clippy are green. The same plant still runs with no ask whenever the line holds a git above Read: a planted diff driver via `diff --no-index; git init`, a planted hook via `git gc`, a planted clean filter via `git add -A`. That is because the bare-repository refusal is set only on sealed lines. One more command-scope pair on every git the shell runs, plus dispatch tests, closes it.

Round 3 ended with request changes.

## Verdict

D-556 closes the round-3 item. Every git the shell tool runs refuses bare discovery, on sealed and unsealed lines alike. The four round-3 lines, and 208 more lines including the round 1 to 3 probes, wrote no marker through dispatch, while the same plants fire in a live control with `all`. No line undoes the pair without an ask. Tests (125 passed), clippy and fmt are green. The remaining items are should-fix (T-14c).

Verdict: approve
