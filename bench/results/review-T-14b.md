# Review T-14b, round 1 (D-89)

Scope: `git diff origin/main...HEAD` in `.wt/T14b`. That is code commit 3a9cd80 (D-543..D-549), handoff 6f36203, and the suborch progress handoff 0fe3918. Claims are from `agent-memory/handoffs/T-14b-rust-coder-1.yaml` and decisions D-543..D-549.

What I ran (CARGO_TARGET_DIR=F:/sb-swarm/target-servers, -j 2, HEAD 0fe3918):
- `cargo test -p srv-files` on the committed targets: all green.
  - lib 73, accept_t13c 6, accept_t14 22 (+4 ignored), accept_t14_history 1, accept_t14b_git 1, contract_paths 2.
  - The safeio junction tests really ran; they were not skipped.
- The worktree also holds three untracked test files from the concurrent tester: `tests/accept_t14b.rs`, `accept_t14b_home.rs` and `accept_t14b_seal.rs`. They are not part of this commit. `accept_t14b.rs` has 4 failing tests. Two of them back the findings below:
  - `cd runs/x && yarn --version` is Read.
  - `git commit -m x --pathspec-from-file=list.txt` is Fill.
- `cargo clippy -p srv-files --all-targets -- -D warnings` fails only in the tester's untracked `accept_t14b.rs:920` (`if` with identical blocks). The same check on the lib, the bins and every committed test target is clean.
- `cargo fmt -p srv-files --check`: clean.

## Blocking

- **D-545 lowers a class (rule 4).** `paths.rs:248` (`code_path`, `in_runs` branch) and `RUNS_STRICT_NAMES` (`paths.rs:274`) make every other dotfile in runs/ scratch. D-545 rests on "no program this server starts reads startup files from a run folder", and that is false.
  - `runs_code` (`classify.rs:1690`) keeps `<prog> --version` Read for every RUNS_CODE program. Yarn reads `.yarnrc` (`yarn-path`) and `.yarnrc.yml` (`yarnPath`) from the cwd and its parents. It then runs that JS file with node before it parses any argument, `--version` included.
  - So this line is all Read and runs code: `cd runs/x && echo 'yarn-path "e.js"' > .yarnrc && echo '...' > e.js && yarn --version`. Before this commit the `> .yarnrc` write was Delete. The tester's untracked `accept_t14b.rs` confirms both halves:
    - `cp runs/x/a.txt runs/x/.yarnrc.yml` is Read.
    - `cd runs/x && yarn --version` is Read.
  - The same applies to `.npmrc` (pnpm `--version` with `packageManager` and `registry=` can switch to a downloaded pnpm) and to `.pytest.ini` (pytest `--version` honours `addopts = -p mod`).
  - Fix, either way: keep dotfiles strict in runs/ for any line that also runs a RUNS_CODE program there, or drop the `--version` exception when the line's cwd is in runs/. Add the yarn chain as a test that must be Delete.

## Should fix

1. **`git commit --pathspec-from-file` stays Fill** (`classify.rs:5012`). The D-546 rule only runs when `-a` is given or there are ops. `--pathspec-from-file` has neither, so `tree_reaches_runs` is never called. It already treats that flag as reaching runs/ (the git add rule), so commit is the inconsistent one. The tester's untracked test fails on exactly this line.
2. **The unsealable scan fails open on read errors** (`classify.rs:4304`). `read_to_string` skips a config file that is not valid UTF-8, and git reads such files fine. A UTF-8 BOM before the first header is not stripped either, so the following keys get an empty section and are missed; git skips the BOM. Read the bytes lossily and strip a leading BOM.
3. **The submodule walk stops silently at 400 files or depth 6** (`classify.rs:4171`). Hitting the cap should raise to Delete, not end the scan.
4. **Repositories that `find_repo` (`classify.rs:3995`) cannot see are never scanned.**
   - It finds only a `.git` entry. A cwd inside a bare repository (`x.git/` with `config`, `HEAD`, `objects`) is a repository to git but not to `find_repo`. Its `filter.*`/`diff.*.textconv` drivers then run under a Read `git log -p`.
   - A submodule with a non-absorbed `.git` folder, or a `.git` file pointing outside `modules/`, is not scanned either. `git status --ignore-submodules=none` is an accepted flag, and it makes status recurse into submodules despite the seal's `diff.ignoreSubmodules=all`.
5. **`fs.write`'s hard-link recheck fails open** (`fs.rs:623`). When the link count cannot be read (`is_none_or`) the write goes ahead. `paths::hard_linked` fails closed in the same case. `csv.clean` out (`csv.rs:239`) can overwrite an existing file and has no link recheck at all.
6. **`safeio::check_parent` skips the check when the parent does not exist yet** (`safeio.rs:100`). In that case `fs.mkdir` runs `create_dir_all` with no check, so `mkdir runs/x/new/a` through a swapped `runs/x` makes folders outside.
   - `fs.write` and `fs.move` run `create_dir_all(parent)` before their handle check, which has the same effect.
   - Fix: check the nearest existing ancestor.
7. **A seal costs two git spawns per Read-git call** (`shell/mod.rs:493`, `:691`). Each `git config --system/--global --list` has a 5 s cap, and this runs on every `git status` line, which is latency on the most common call. Cache the kept entries per process, keyed on the config files' mtimes.
8. **The sealed output is wrong for the repo's own data in two cases**, which costs a wasted turn:
   - In an LFS repository set up by a global `git lfs install`, the global `filter.lfs` is dropped. Status and diff then compare pointers with content.
   - `git config --global ...` reads and `git config --list` show the seal's empty global file and temp paths.
   - Document both in the tool description, or fall back to Delete for LFS-attributed trees.
9. **Pre-existing, outside this diff, but the same class of hole as the Blocking item.** The RUNS_CODE `--version` exception runs project files that are not dotfiles and were scratch before this commit:
   - `pytest.ini` addopts with pytest;
   - `rust-toolchain.toml` `path =` with the rustup `cargo` proxy;
   - `.mvn/jvm.config` (`-javaagent`) with mvn;
   - `global.json` sdk paths with dotnet.
   - These are Fill writes anywhere outside dot names, and Read writes in runs/. File a task.

## Notes

- **Seal (D-547).**
  - `-c`, `--config-env`, `-p`/`--paginate`, `--exec-path` and `--git-dir` are Delete (`classify.rs` git global options).
  - Inline `GIT_*=` assignments, `export`, `env` and `unset` are Delete. Unknown subcommands (aliases) are Delete.
  - So nothing from the model can override the command-scope keys or unset the seal.
  - `sanitize_env` drops every inherited `GIT_CONFIG*`, `GIT_SSH*`, `GIT_PAGER`, `GIT_EXTERNAL_DIFF` and `GIT_ASKPASS` before `apply`.
  - None of `KEPT_KEYS`/`KEPT_PREFIXES` names a program: no pager, editor, sshCommand, alias, include or external diff.
  - The empty `credential.helper` in command scope clears the URL-scoped helpers that git read earlier.
  - `GIT_CONFIG_NOSYSTEM` also covers Git for Windows' `%ProgramData%\Git\config`.
  - A seal that cannot be built refuses the call (fail closed). A timed-out config read only drops the kept data keys, which is also safe.
  - `accept_t14b_git` is a good test: it shows every plant live under plain bash and dead through the tool.
- **`gh` gets the seal but no `unsealable_config` scan.** Read gh commands run only metadata git commands (`remote`, `config`, `rev-parse`), so I found no driver path. Worth a line in D-547.
- **`git ls-remote ext::...` stays Read.** That is safe only because of the seal's `protocol.ext.allow=never`. In a line that also has a Fill git, the seal is off. Planting `protocol.ext.allow` needs `git config`, which is Account or above, so I did not raise it.
- **Item 7, other names.** I found no other Read program that reads a cwd dotfile: jq, dig, whois and curl read HOME or env paths, and bash is non-interactive with `BASH_ENV` stripped. The Blocking item comes only from RUNS_CODE `--version`. `.gitignore` in runs/ cannot re-include files below an ignored `runs/`.
- **safeio on Windows.**
  - Paths are compared after `GetFinalPathNameByHandleW` (normalized, so 8.3 names are expanded) and ASCII case folding.
  - Alternate data streams are refused earlier, at resolve.
  - With `OPEN_REPARSE_POINT` the delete removes the link, not its target.
  - When the final path cannot be read, the call is refused (fail closed). On RAM disks or filesystems that cannot answer, every write will refuse, and a non-ASCII case mismatch refuses too. Both are functional, not safety, issues.
  - The one-syscall windows (folder delete, rename, mkdir) and the uncovered docs and shell writers are recorded in D-549.
- **Items 1-3.**
  - **D-543.** HOME (msys-converted) and the profile folder both add `<h>/bin`. It is tested with a failed probe, and the test also proves the check can fail.
  - **D-544.** `git_root` does not handle a scoop-style `shims/git.exe`, so that install's `etc` is missed. It is low risk: Read git no longer reads system config.
  - **D-546 (1).** Tested.
- **Hard rules.** No keys, `.env` files, sleeps, em dashes, captcha handling or raw HTML were added. `unwrap` appears only in tests. The new parsers (`config_keys`, `header_end`, `kept_entries`) slice only at ASCII boundaries and cannot panic on file input.
- **Spec ids and interfaces.** The commit's ids match the code. `interfaces.yaml` has no srv-files entries, and the new public items are crate API only.

## Verdict

The seal, the D-543/D-544/D-546 class changes and safeio are sound and tested. D-545 lowers a class: a dotfile planted in runs/ by a Read shell write is executed by a Read `yarn --version` in that folder.

Verdict: request changes
