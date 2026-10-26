# review: T-14 srv-files

History:
- Round 1 (81b6bf6): request changes. B1-B9 (shell classifier failed open), S1-S10.
- Round 2 (f9a9079, D-523 allowlist classifier): request changes. B1-B9 closed. Blocking:
  `~/.config/git/config` and `~/.config/git/attributes` writes were Fill.
- Closing round (c6e28e3, D-524): request changes. Blocking: (1) fs.move destination
  unchecked; (2) `cp -r SRC/. DST` / `-T` classed as DST/<basename>; (3) an archive or copy
  plants `.git/config` in runs/ for a later Read-class git; (4) PATH check used the harness
  PATH, not Git Bash's; (5) AppData over-match broke acceptance under the default %TEMP%.
- Confirm round (this file, head fd8f827, D-525).

Method: probes call the committed `srv_files::shell::classify::classify` and the fs, csv and
docs `ToolHandler::classify` (through `ToolCtx`) from a throwaway crate in the git-ignored
`runs/review-t14-c/probe` of the servers checkout, with its own target dir. Classification
only; nothing runs against real files. Bases: `F:\sb-swarm\tmp-servers` (not below home) and
`C:\Users\Admin\AppData\Local\Temp`. `cargo clean -p srv-files`, then
`cargo test -p srv-files -j 2` (shared `F:/sb-swarm/target-servers`): 56 unit,
accept_t14 22 pass / 4 ignored (21 committed plus the tester's uncommitted
`r2_ladder_rung9_write_then_delete_outside_runs` in the worktree), history 1, contract 2, 0 failed, with `TEMP=TMP=F:\sb-swarm\tmp-servers`
and again with `TEMP=TMP=%USERPROFILE%\AppData\Local\Temp`. `cargo clippy -p srv-files
--all-targets -- -D warnings` and `cargo fmt --check` clean.

## Blocking

- none

## Should fix

- **The PATH probe fails open for `$HOME/bin`.** Git Bash's `bin/bash.exe` puts `$HOME/bin`
  on the PATH, not `%USERPROFILE%\bin`. Checked: with `HOME=F:/.../fakehome` bash reports
  `/mingw64/bin:/usr/bin:/f/.../fakehome/bin:...`. `paths::path_dirs` (`paths.rs:260-262`)
  always adds only `dirs::home_dir()/bin`. When the probe cannot spawn bash or passes the
  10 s cap (`shell/mod.rs:564-566`), it keeps just the guessed Git folders, and the
  `OnceLock` caches that result for the life of the process. If HOME differs from the
  profile folder, `echo x > $HOME_DIR/bin/jq` is then Fill. Fix: add `<h>/bin` for every
  entry of `paths::homes()`, or set `relative = true` (every write is Delete) when the
  runner is Bash and the probe fails. The test at `shell/mod.rs:736-744` also assumes
  HOME equals USERPROFILE.
- **Git's own system config is not a code location.** A write to `<msys_root>/etc/gitconfig`
  is Fill. Probe: `echo x > '%LOCALAPPDATA%/Programs/Git/etc/gitconfig'` gives Fill, and so
  do fs.write, fs.move, fs.mkdir and csv.clean `out` there. The next Read-class `git status`
  reads that file (`git config --system --show-origin` on this machine:
  `C:/Program Files/Git/etc/gitconfig`). On this machine the folder needs admin rights, so
  the write fails. With a per-user Git install ("Only for me", `%LOCALAPPDATA%\Programs\Git`)
  or an elevated harness, the write succeeds and this is the round-2 hole again.
  `shell::msys_root` already knows the folder: add `<msys_root>/etc` (and
  `<msys_root>/mingw64/etc`) to `runs_code_on_use`. I also checked `C:\ProgramData\Git\config`:
  users can create it here, but Git 2.50.1 does not read it (the string is not in git.exe),
  so it is not a finding.
- Carried from the closing round, unchanged (fail-closed, costs an ask each time):
  `echo x > runs/x/.env` and any dotfile or `.github`/`.vscode` file inside runs/ is Delete.

## Notes

- Blocking 1 closed. fs.move, fs.write, fs.mkdir and csv.clean `out` to
  `~/.config/git/config`, `~/.config/git/attributes`, `~/.bashrc`, `~/bin/jq`,
  `somerepo/.git/hooks/pre-commit`, `runs/x/repo/.git/hooks/pre-commit` and
  `AppData/Roaming/GitHub CLI/config.yml` are all Delete. Also Delete: `fs.move runs/x/.gitconfig
  -> ~` and a folder moved into `~` or into an existing folder outside runs/. A file moved to
  `newname.txt` stays Fill. docs `out` goes through `write_verdict` (`docs.rs:381`).
  Test: `fs::tests::move_and_write_into_config_startup_and_path_are_delete`.
- Blocking 2 closed. Every `cp -r|-a|-R SRC/. ~`, `cp -rT`, `cp -r -T`,
  `--no-target-directory`, `SRC/./`, plain `cp -r src ~`, `cp -r payload/. somerepo`,
  `cp -r ... existingdir|newdir` and `cp -rL` is Delete. So are `mv dir ~|existingdir|newdir`
  and `mkdir -p runs/x/new && mv runs/x/new existingdir`. `cp --parents`, `install`, `ditto`,
  `rsync`, `robocopy`, `xcopy` and `ln` are Delete (not allowlisted). Inside runs/,
  `cp -r`, `/.` and `-T` stay Read. Test: `recursive_copies_and_extracts_outside_runs_are_delete`.
- Blocking 3 closed. These are Delete: `tar -x ... && git -C runs/x/r status`,
  `unzip ... && git -C runs/x/r diff`, `git -C runs/x/repo status`, `.../sub log`,
  `./runs/../runs/x/repo`, `RUNS/x/repo`, `Runs/X/Repo`, `runs./x/repo`, `runs/x/repo/.git`,
  `git -C runs status`, `cd runs/x/repo && git status` (also with `;`, a subshell, `pushd`),
  `cd "$(...)" && git status`, and a `.git` file outside runs/ whose relative or absolute
  gitdir points into runs/. `--git-dir`, `--work-tree`, `GIT_DIR=` as a prefix, `export
  GIT_DIR` and `env GIT_DIR=` are Delete (pre-existing). `cd runs/x/repo && cd ../../.. &&
  git status` is Read, which is correct. `cd runs/x/repo && gh pr list|repo view` is Delete;
  `gh search` is Read. `git add -A`, `git add .` and `git add runs/x/repo` are Delete
  without a `runs/` ignore line and Fill with one; `git add -f runs/x/repo` is Delete. Test:
  `git_in_runs_is_delete_one_line_and_two_calls`, `git_add_reaching_runs_is_delete`.
- The exemptions checked out on real git, in a scratch repo: `git help status` there runs
  `browser.<x>.cmd` from the repo config. The classifier never treats `git help` as Read
  (`git help` and `git status --help` are Delete), and bare `git -C <repo> --help` only
  prints usage. `git init` and `git clone` run from a repo whose config sets
  `init.templateDir` do not copy that template, so the clone/init exemption holds.
  `--template`, `-P`/`--absolute-names` on tar, and `gh ... -- <git flags>` are Delete.
- Blocking 4 closed on this machine. `path_dirs` holds `c:/users/admin/bin`, the Git
  `mingw64/bin`, `usr/bin`, `usr/local/bin`, `bin` and `cmd` folders, and the full PATH that
  bash reports (80 entries, `relative=false`). `echo x > ~/bin/jq`, `~/bin/gh.exe` and the
  Git bin folders are Delete. The sanitized env strips `XDG_CONFIG_HOME`, `GIT_CONFIG*`,
  `GIT_DIR`/`WORK_TREE`/`EXEC_PATH`, `GIT_SSH*`, editors, pagers, `CURL_HOME`, `WGETRC`
  and `NETRC`, and drops relative or empty PATH entries before bash starts. Not stripped:
  `GH_CONFIG_DIR`, which is harmless because gh aliases and extensions are Delete in the
  classifier, and `GIT_TRACE*`/`GIT_TRACE2_*`, which only write trace files when the harness
  already has them set.
- Blocking 5 closed. With the base under `AppData\Local\Temp`, `echo x > runs/x/a.txt`, `cp`
  inside runs/, `mkdir`, `rm -rf`, `tar -x` and `unzip` into runs/ are Read, and
  `echo x > notes.txt` is Fill. Acceptance is green with that TEMP (see Method). Test:
  `paths::tests::home_rules_are_narrow_and_skip_the_base`.
- Closing-round Should-fix items closed: shell D-524 tests now use absolute home paths
  (`tests.rs` `home()`), the reason text is generic (`util::code_reason`), PATH entries are
  canonicalised with the drive kept, and the config env vars are stripped.
- The PATH probe blocks the first write check of a process for up to 10 s (about 0.2 s
  here). It waits with a cap, not a fixed sleep (rule 6 holds).
- Hard rules 1, 2, 3, 5, 6, 7: nothing in fd8f827 touches them. Reasons carry paths only;
  no new `unwrap` outside tests. The D-525 entry in decisions.yaml has the same keys as its
  neighbours. Commit ids F-1, F-2, F-5 and D-525 match the diff.
- Shared-target gotcha confirmed: the first run after `cargo clean -p srv-files` reported
  "test failed" for the `--bin srv-files` target while another build held the lock. A rerun
  passed, and all later runs were clean.

## Verdict

Approve. All five closing-round Blocking items are closed by code and covered by tests
that assert the class. The new git-in-runs rule and `tree_verdict` hold against every
bypass I tried. Two Should-fix items remain: `$HOME/bin` when the PATH probe fails, and
Git's `etc/gitconfig` on a per-user install. Both depend on the setup and neither applies
to this machine's default configuration.

Verdict: approve
