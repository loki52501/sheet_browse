# Review T-14b, round 2 (final, D-89)

Round 1 (HEAD 0fe3918) asked for changes. The Blocking item was D-545: dotfiles in runs/ were scratch, so a Read line could write `.yarnrc` and a Read `yarn --version` would run it. Acceptance rows 4b, 6b, 7b and 7c failed.

Round 2 scope: `git diff 85cd79c..HEAD -- crates/`. That is code commit 9e7e981 (D-550..D-554), with the handoff `agent-memory/handoffs/T-14b-rust-coder-2.yaml`.

What I ran (CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2, HEAD 782b959):
- `cargo test -p srv-files -j 2 --no-fail-fast`: 123 passed, 0 failed, 4 ignored.
  - lib 76, accept_t13c 6, accept_t14 22 (+4), accept_t14_history 1, accept_t14b 13, accept_t14b_git 1, accept_t14b_home 1, accept_t14b_seal 1, contract_paths 2.
- `cargo clippy -p srv-files --all-targets -- -D warnings`: clean.
- `cargo fmt -p srv-files --check`: clean.
- A scratch probe binary that calls `srv_files::shell::classify::classify` against a temporary base folder. It lives in the scratchpad, outside the repo.
- Real git 2.50.1 with the seal's env from `GitSeal::apply`: `GIT_CONFIG_NOSYSTEM`, an empty `GIT_CONFIG_GLOBAL`, `GIT_ATTR_NOSYSTEM` and the command-scope keys.

## Round-1 items

- **Blocking (D-545): closed.** `code_path` (`paths.rs:236`) makes every dotfile Delete again, runs/ included, and `RUNS_STRICT_NAMES` is gone. `item7_adversarial_tools_that_read_cwd_dotfiles_are_delete_in_runs` passes.
- **7c: closed.** `runs_code` (`classify.rs:1697`) makes every RUNS_CODE use Delete, `--version`/`version` included.
  - `item7_adversarial_rust_toolchain_file_in_cwd_does_not_run_a_program` passes end to end.
  - The probe confirms the same for `cargo.exe`, `CARGO` and `npm.cmd --version`. `rustfmt`, `rustdoc`, `cargo-clippy`, `corepack`, `mise`, `asdf`, `volta` and `conda` are unknown programs, so Delete. A path-run cargo is Delete.
- **4b: closed.** `classify.rs:5050` adds `--pathspec-from-file`, and `tree_reaches_runs` returns true for that flag. `item4_stash_and_commit_all_with_unignored_runs_are_delete` passes.
- **6b: closed on Windows.** `create_dirs` (`safeio.rs:144`) checks the nearest existing ancestor, then makes each missing level with NtCreateFile relative to the verified handle. `check_parent` (`safeio.rs:98`) no longer skips a missing parent. `item6_adversarial_new_subfolder_race_never_creates_outside` and the two new safeio unit tests pass.
- **Should-fix 2 (D-554) and 5 (D-553): closed.**
  - `config_text` (`classify.rs:4313`) fails closed, and there is a unit test.
  - `links_ok` (`safeio.rs:109`) uses `is_some_and`, and csv.clean out now has the recheck.
- **The coder's change to the tester's test only tightens it.** It was `item7_dotfiles_in_runs_scratch_but_git_names_stay_delete`, now `item7_dotfiles_in_runs_ask_and_git_names_stay_delete` (`tests/accept_t14b.rs`).
  - Every former "wrote" dotfile case must now ask, must not be written, and must be ledgered Delete. `.yarnrc`, `.yarnrc.yml` and `.pytest.ini` were added.
  - The shell dotfile write must ask. The former Read shell list must be Delete, with `.tool-versions` and `.pytest.ini` added.
  - The strict git-name list is unchanged.
  - The only Read assertions left are plain-name controls (`files/something`, `pip.conf`, `b.txt`), which were Read before too.
  - No other tester assertion changed: the diff touches only that test.

## Blocking

- **A bare repository planted outside runs/ runs its own drivers under a Read git, with no ask (rule 4).** This is round-1 should-fix 4, which round 1 rated too low. It is now reproduced, and it is the git analog of 7c.
  - **Why it is missed.**
    - `find_repo` (`classify.rs:4006`) finds only a `.git` entry, so `repo_in_runs` (`:3971`) and `unsealable_config` (`:4328`) see no repository.
    - The seal (`shell/mod.rs:525`) leaves `safe.bareRepository` at git's default `all`. Its value is only copied from the global config (`KEPT_KEYS`, `:422`).
  - **The chain (classes from the probe).** Each step is a separate call:
    1. `git init --bare foo3` (Fill), or `mkdir -p foo/objects foo/refs foo/info` plus `printf 'ref: refs/heads/main\n' > foo/HEAD` (Fill).
    2. `printf '[diff "x"]\n\tcommand = <prog>\n' >> foo/config` (Fill).
    3. `echo '* diff=x' > foo/info/attributes` (Fill).
    4. `cd foo && git diff --no-index a b` (Read).
  - The same Read class holds for `git log -p`, `git show`, `git diff A B`, `git blame` and `git grep` in that folder. Fill outside runs/ does not ask (Mode::Allow).
  - **Reproduced with real git under the seal env.** git discovers `foo` as a bare repository (`rev-parse --is-bare-repository` prints `true`). `git diff --no-index a b` then ran `diff.x.command` and wrote the marker file. Textconv drivers (`log -p`, `show`, `blame`) are reached the same way once objects exist (`git init --bare`, a Fill command, makes the folders).
  - **Fix: one seal key, then a test.**
    - Add `("safe.bareRepository", "explicit")` to the command-scope pairs in `GitSeal::apply`. Command scope is protected config and comes after the kept keys.
    - Checked: with that key, the same `rev-parse` fails with "cannot use bare repository" and `diff --no-index` runs without the repository's driver.
    - Alternatively, make `find_repo` treat a folder with `HEAD`, `objects/` and `refs/` as a git dir and scan its `config`.
    - Either way, add the chain above as an accept_t14b case: no marker is written, through dispatch.

## Should fix (T-14c candidates)

1. **D-550 (2) cost** (`classify.rs:1697`).
   - Every version probe now asks. An ask is latency and maybe a wasted turn, but a wrong Read breaks rule 4, and rules outrank the priority order. `which <prog>` stays Read for presence checks. So the cost is acceptable for now.
   - A narrower rule looks safe and could restore Read for exactly `make|node|python|python3|java|gcc|clang|docker --version`. It needs all three conditions:
     - `which` resolves the program to a PE `.exe`, never a `.cmd`/`.bat` shim. cmd.exe searches the cwd first, so a scratch `runs/x/pyenv.bat` would run from a pyenv-win `python.bat`.
     - That `.exe` is not in a version-manager shim folder (asdf, mise, volta, pyenv).
     - The line has no other command.
   - The rule keeps Delete for:
     - rustup proxies (`rust-toolchain.toml` `path`);
     - go (go.mod `toolchain`);
     - yarn, pnpm and npm (yarnPath, corepack `packageManager`);
     - dotnet (global.json `paths`);
     - mvn (`.mvn/jvm.config`);
     - gradle (`gradle.properties` `org.gradle.java.home`);
     - pytest (`addopts -p`).
2. **CODE_DIRS lacks `.mvn`, `.yarn` and `.volta`** (`paths.rs:184`).
   - The probe shows `echo x > runs/x/.mvn/jvm.config` and `runs/x/.yarn/releases/y.js` are Read.
   - Non-dot project files are also scratch in runs/ and Fill elsewhere: `mise.toml`, `rust-toolchain.toml`, `global.json`, `pytest.ini`, `package.json`.
   - Not Blocking: every program that reads these is Delete now. These files matter only if item 1 (or anything else) makes a consumer Read again, so add the folders before any such change.
3. **create_dirs off Windows** (`safeio.rs:459`).
   - It makes each level by path, then checks it. A folder made through a link swapped in during that syscall is refused but left in place outside.
   - The doc on `create_dirs` (`safeio.rs:144`) says a swap "cannot make a folder outside". That holds only on Windows, so say so, or use `mkdirat` when a libc dependency is allowed.
4. **`create_dirs` asks for FILE_ADD_SUBDIRECTORY on the existing parent even when nothing is missing** (`safeio.rs:296`, called by every fs.write, fs.move and csv.clean out). So fs.write into a folder where the user may add files but not subfolders now fails. Use `check_entry` when `missing` is empty.
5. **Carried from round 1, deferred by the coder to T-14c:**
   - should-fix 3: the submodule walk cap ends silently instead of raising.
   - should-fix 4, rest: non-absorbed submodule `.git` folders, and `.git` files outside `modules/`. Both need a dotfile write, which is Delete, so the remaining risk is low.
   - should-fix 7: two git spawns per sealed call.
   - should-fix 8: global `filter.lfs` is dropped, and `git config --global` shows the seal's empty file.

## Notes

- **NtCreateFile FFI (`safeio.rs:306`) is sound.**
  - The layouts match the SDK: UNICODE_STRING, OBJECT_ATTRIBUTES (48 bytes on x64) and IO_STATUS_BLOCK (usize, usize).
  - `wide` and `uname` outlive the call. Length is in bytes with no terminator, and at most 255 chars, so at most 510 bytes, which fits u16.
  - NUL, `\` and `/` in a name are refused. `..`, `.`, trailing dots and spaces cannot reach it, because `paths::resolve` normalises them first.
  - The handle is owned by `File::from_raw_handle` only after a success. The previous level's `File` drops in the loop. No leak on any path.
  - The error mapping (`RtlNtStatusToDosError`) is correct.
  - `FILE_OPEN_IF` on a name swapped to a junction follows it, and `verify` refuses. It is not removed because `Information != FILE_CREATED`. A dangling junction is created at its target, then removed by handle.
- **`open_existing`'s rights match std:** FILE_GENERIC_WRITE 0x120116, and append without FILE_WRITE_DATA. FILE_READ_ATTRIBUTES is added for the link count.
- **No other Read program I tried reads cwd config that names a program.**
  - node/.npmrc and pip/pip.conf: both programs are Delete.
  - `rg` (ignore files only), `gh pr list` (metadata git) and jq/dig/whois (HOME-only config).
  - In the cmd fallback, every line except plain argv is refused. Plain argv runs through std `Command`, which does not search the cwd.
- **Hard rules.** No keys, `.env`, sleeps, em dashes, captcha handling, raw HTML or vault paths were added. `unwrap` appears only in tests. The commit's ids (D-550..D-554, T-14b) match the code. There are no `interfaces.yaml` entries for srv-files.

## Verdict

Every round-1 Blocking item and failing acceptance row (4b, 6b, 7b, 7c) is fixed and tested. One Blocking item remains, same class as 7c: a bare repository planted with Fill writes outside runs/ makes a Read git run its diff driver with no ask (`find_repo` misses it, and the seal leaves `safe.bareRepository` at `all`). The fix is one seal key plus a test.

Verdict: request changes
