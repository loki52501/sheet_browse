# review: T-14 srv-files

History:
- Round 1 (81b6bf6): request changes. B1-B9 (shell classifier failed open), S1-S10.
- Round 2 (f9a9079, D-523 allowlist classifier): request changes. B1-B9 closed. Blocking:
  `~/.config/git/config` and `~/.config/git/attributes` writes were Fill (auto-allowed),
  then a Read-class git runs their programs. Should fix: startup/rc files, .netrc, PATH.
- Closing round (this file, head c6e28e3, D-524): `paths::runs_code_on_use` widened to
  dotfiles, CODE_DIRS, home dot folders / AppData / PowerShell / Start Menu, PATH folders.

Method: probes call the committed `srv_files::shell::classify::classify`,
`paths::runs_code_on_use` and the fs/docs/csv `ToolHandler::classify` from a throwaway crate
in the session scratchpad (classification only, nothing executed against real files).
`cargo test -p srv-files` is green with this machine's `TEMP=F:\sb-swarm\tmp-servers`
(48 unit, accept_t14 21 pass / 4 ignored); clippy `-D warnings` and fmt clean.

## Blocking

- **Rule 4: `fs.move` never calls `runs_code_on_use` on its destination.**
  `crates/srv-files/src/fs.rs:171-220` (`plan_move`). Probes, all Fill (auto-allowed):
  `fs.move runs/x/a.txt -> C:/Users/Admin/.config/git/config`, `-> ~/.config/git/attributes`,
  `-> ~/.bashrc`, `runs/x/.gitconfig -> ~` (lands as `~/.gitconfig`),
  `-> somerepo/.git/hooks/pre-commit`, `-> runs/x/repo/.git/hooks/pre-commit`,
  `-> C:/Users/Admin/bin/jq`. This is the round-2 Blocking item on a call site the fix
  missed (D-523 (8) already required it for .git). Route the final `to` through
  `util::write_verdict`/`runs_code_on_use` and add the probes above as a test.
- **Rule 4: `cp -r SRC/. DST` and `cp -T` are classed as if they wrote `DST/<basename>`, but
  they write SRC's contents into DST.** `crates/srv-files/src/shell/classify.rs:2591-2630`
  (`fin = dst_path.join(src.file_name())`; `runs/x/home/.` resolves to `runs/x/home`, and
  `-T` is accepted but ignored). Probes, all Fill: `cp -r runs/x/home/. C:/Users/Admin`
  (writes `~/.config/git/config`, `~/.bashrc`), `cp -a ...`, `cp -R ...`,
  `cp -rT runs/x/home C:/Users/Admin`, `cp -r runs/x/payload/. somerepo` (writes
  `somerepo/.git/config`; the next `git -C somerepo status` is Read and runs its
  `core.fsmonitor`). The same hole also turns "replaces an existing file outside runs/"
  (Delete, D-520) into Fill for every file inside the copied tree. A recursive copy whose
  destination is an existing folder outside runs/ (or any `-T`) should be Delete unless
  every file of the source tree is checked; `cp -r` into runs/ is unaffected.
- **Rule 4: an archive extracted inside runs/ can plant `.git/config` and the next Read-class
  git runs it, with no ask at any step.** `tar -xf runs/x/a.tar -C runs/x/r && git -C
  runs/x/r status` classifies Read as one line; `unzip [-o] runs/x/a.zip -d runs/x/r` is Read;
  the download into runs/ is Read. An archive entry `.git/config` with `core.fsmonitor`
  (or `.gitattributes` + `.git/config` driver) runs on `git status`/`git diff`. D-523 (8)
  states writes inside a `.git` folder are Delete "in or out of runs/"; extraction writes
  them at Read. Pre-dates this commit (round 2 missed it); T-14b lists "hooks/drivers
  already present in a repo" but this is planted by the agent itself through Read steps.
  Fix options: list the archive at classify time and raise to Delete if any entry hits
  `runs_code_on_use` (fail closed when it cannot be listed), or make the bash runner pass
  git a neutralising config for runs/ repos (e.g. `GIT_CONFIG_COUNT` with
  `core.fsmonitor=false`, `core.hooksPath` to an empty folder, `diff.external=`,
  `core.pager=cat`), or have supreme ratify the residual explicitly in decisions.yaml.
- **Rule 4: the PATH check reads the harness's own PATH, not the PATH bash runs with.**
  `crates/srv-files/src/paths.rs:212-221`. `Runner::discover` picks
  `Program Files/Git/bin/bash.exe`, a wrapper that prepends `/mingw64/bin:/usr/bin:$USERPROFILE/bin`
  to the child PATH (verified: launched with `PATH=C:\WINDOWS\system32;C:\WINDOWS`, bash
  sees `/mingw64/bin:/usr/bin:/c/Users/Admin/bin:...`). The registry PATH on this machine
  has no `C:\Users\Admin\bin`, so a harness started from PowerShell / Claude Code does
  not see it: under that PATH `echo x > C:/Users/Admin/bin/jq` (also `gh`, `zip`) is Fill.
  `jq`, `gh`, `zip`, and the System32 tools in FREE (`nslookup`, `ping`, `where`, ...) are
  not in Git's folders, so `~/bin` wins and the next Read-class `jq . file` runs the
  planted file. Take the PATH from the runner (ask bash once at discover, cache it) or add
  `<home>/bin` and the Git folders when the runner is Git Bash; test with a PATH that
  lacks `~/bin`.
- **Acceptance is red on a stock Windows machine.** `CODE_HOME_DIRS` holds `appdata`
  (`paths.rs:163`) and the home rule matches every ancestor (`paths.rs:187-197`), so every
  write anywhere under `%USERPROFILE%\AppData` (default `%TEMP%`) is Delete, runs/
  included. With `TEMP=TMP=C:\Users\Admin\AppData\Local\Temp`: 7 unit tests fail
  (`fs::tests::hard_linked_targets_are_delete_b7`,
  `writing_config_that_runs_programs_is_delete`, `shell::classify::tests::network_classes`,
  `redirections`, `edits_moves_copies`, `read_only_controls_stay_below_delete`,
  `writes_into_a_path_folder_are_delete`) and 4 of accept_t14 fail
  (`fs_read_write_glob_move_mkdir`, `csv_clean_dedupes_normalises_and_flags`,
  `docs_missing_helper_is_a_clear_error`, `docs_xlsx_round_trip_or_names_what_is_missing`).
  Probe with the base under AppData Temp: `echo x > runs/x/a.txt`, `cp runs/x/a.txt
  runs/x/b.txt` and `echo x > notes.txt` are all Delete. The same happens for a repo below
  any home dot folder (`~/.wt/...`, `/home/u/.local/src/...`): every runs/ write asks. Apply
  the ancestor rules only to the part of the path below the runs root (for scratch) and
  match the specific config locations (`~/.config/git/*`, AppData Startup, PowerShell
  profile) rather than any ancestor named AppData.

## Should fix

- Shell regression tests for D-524 that use `~/...` are vacuous: the shell classifier makes
  any `~` word Unknown, hence Delete (`classify.rs:1959-1961`), so
  `writes_to_config_that_runs_programs_are_delete` (`shell/classify/tests.rs:475-506`) would
  pass without the fix. Use absolute home paths (`dirs::home_dir().join(".config/git/config")`)
  and add `fs.move`/`fs.write` cases.
- `config_write` reason still says "which git, curl or wget read as configuration"
  (`classify.rs:2137-2145`) for PATH programs, startup files and `.env` in runs/; reuse the
  `util.rs:99` wording.
- `on_path` compares components only: PATH entries spelled with 8.3 names
  (`C:\PROGRA~1\...`) or that are junctions/symlinks never match the canonical target;
  relative PATH entries are ignored; the drive prefix is dropped (`lower_names`), which only
  over-matches (`D:/Users/Admin/AppData/x` counts as home).
- `XDG_CONFIG_HOME`, `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_SYSTEM`, `CURL_HOME`, `WGETRC`,
  `NETRC` are not set here and cannot be set from a line (prefix assignment and `export` of
  them are Delete, verified), but if the harness inherits one, writes to the file it names
  are Fill (`echo x >> D:/cfg/git/config` is Fill). Either strip them in `sanitize_env`
  or add their targets to `runs_code_on_use`.
- Over-match noted in D-524 is real: `echo x > runs/x/.env` and any `.github`/`.vscode`
  file inside runs/ are Delete. Acceptable fail-closed, but it costs an ask per write.

## Notes

- Round-2 Blocking on the direct shell and `fs.write` paths is closed: `echo x >>`
  (absolute, `/c/...`, mixed case, `.config.`, trailing dot, trailing space),
  `tee -a`, `sed -i`, `touch`, `cp` file to `~/.gitconfig` (direct, into `~`, `~/`,
  `-t ~`), `mv runs/x/.bashrc ~`, `mv runs/x/home/.config ~` are Delete; `fs.write` to
  `~/.config/git/config`, `~/.config/git/attributes`, `~/bin/jq` is Delete. `$HOME/...`
  and `~/...` in shell are Delete (run-time word). `git config ...`, `tar -x -C ~`,
  `unzip -d ~` are Delete. Junctions resolve to their target before the check.
- Regressions with the base on F: (this worktree under `.wt/`, not below home): none.
  `echo x > runs/x/a.txt`, `rm -rf runs/x`, `cp runs/x/a.txt runs/x/b.txt`,
  `mkdir -p runs/x/out`, `git status`, `jq . runs/x/a.txt` are Read; `echo x > notes.txt`
  and `fs.write notes.txt` are Fill. `.wt` only matters if the tree is below home (see
  Blocking).
- D-524 entry in `agent-memory/decisions.yaml` (appended outside the write hook): valid
  YAML, keys `id/text/why` like its neighbours, ids unique (146), ASCII only, no secrets or
  paths; harmless.
- Hard rules 1, 2, 3, 5, 6, 7: nothing new in c6e28e3 touches them. Reason strings carry
  paths only, no resolved values.
- Commit ids F-1, F-2, D-524 match the diff.

## Verdict

Request changes. The widened `runs_code_on_use` closes the round-2 cases on the shell
redirection and `fs.write` paths, but `fs.move` skips it entirely, `cp -r SRC/.`/`cp -T`
misplace the destination, extraction inside runs/ still plants git config at Read, the PATH
check uses the wrong PATH for Git Bash, and the AppData ancestor match makes acceptance fail
on a default Windows `%TEMP%`.

Verdict: request changes
