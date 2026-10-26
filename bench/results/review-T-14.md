# review: T-14 srv-files

Round 1 (commit 81b6bf6): request changes, B1-B9 (shell classifier failed open) + S1-S10.
Round 2 (this file): fixes in f9a9079, decision D-523 (allowlist classifier, fail closed by
construction).

Scope: `git diff main...HEAD` on swarm/servers-T14. B1-B9 and the new bypass hunt were
re-run by calling `srv_files::shell::classify::classify` from a throwaway crate outside the
worktree (classification only; nothing was executed against files). Unit tests (47) and
`accept_t14` (21 pass, 4 docs ignored) are green under `CARGO_TARGET_DIR=F:/sb-swarm/target-servers -j 2`.

## Blocking

- **Rule 4: the user-global git config `~/.config/git/config` is Fill, not Delete.**
  `paths::runs_code_on_use` (`crates/srv-files/src/paths.rs:149-171`) matches the basename
  `.gitconfig` and any `.git/` directory component, but not the XDG location
  `$XDG_CONFIG_HOME/git/config` (default `~/.config/git/config`). Probe:
  `echo x >> ~/.config/git/config` classifies Fill. git reads this file on every
  invocation, so an allowlisted Read-class `git status` / `git log` / `git diff` then runs
  whatever it sets (`core.fsmonitor`, `core.pager`, `alias.*`, `[diff].textconv`). Because
  Fill is `Mode::Allow` by default (`crates/core/src/gate.rs:360`), this write is
  auto-allowed with no ask. This directly misses D-523(8), which makes git config Delete
  "because allowlisted git ... run what these say." Same gap on `fs.write` (shares
  `util::write_verdict`, `crates/srv-files/src/util.rs:94-100`).
- **Rule 4: the global gitattributes `~/.config/git/attributes` is Fill, not Delete.**
  Same function, same cause (`crates/srv-files/src/paths.rs:160-170`). Probe:
  `echo x >> ~/.config/git/attributes` is Fill. A global `* diff=x` / `filter=x` entry runs
  a program on the next allowlisted `git diff` / `git log -p`. The repo-local
  `.gitattributes` is correctly Delete; the XDG global one slips to auto-allow Fill. Add a
  regression test (the round-1 `writes_to_config_that_runs_programs_are_delete` set covers
  `.git/`, `.gitattributes`, `.curlrc`, `.wgetrc` but not these XDG forms).

## Should fix

- **Rule 4 judgment (the item the brief asked me to rule on): appending to shell/login
  startup and autorun files outside runs/ is Fill, and Fill auto-allows.** Confirmed Fill
  (no ask) for `~/.bashrc`, `~/.bash_profile`, `~/.profile`, `~/.zshrc`, the PowerShell
  profile, a `.cmd` under the `Startup` folder, `~/.ssh/authorized_keys`, `~/.ssh/config`,
  `~/.npmrc`, `~/.cargo/config.toml`, and direnv `.envrc`. My judgment: these are exactly
  "a line that makes a later step run attacker-chosen code, outside runs/," so by the same
  principle D-523(8) applies to git/curl/wget config they should be Delete. The reason they
  are not strictly a rule break today is that D-523 deliberately keeps "create/append to a
  literal path outside runs/" at Fill, and the harness runs its own bash with
  `--noprofile --norc`, so the harm lands on the human's later sessions rather than on the
  harness itself. Because Fill auto-proceeds, a persistence/backdoor append happens with no
  ask. Extend the `runs_code_on_use` Delete set to these, or have supreme explicitly ratify
  Fill for them in D-523.
- **`~/.netrc` / `_netrc` are Fill** (`crates/srv-files/src/paths.rs:160-170`). curl, wget
  and git read this for credentials; altering it steers allowlisted `curl`/`wget` auth.
  Consider adding it to the Delete set.
- **S2 build tools:** `cargo build`/`npm test`/`make` etc. are Delete, only `--version`
  (and `go/cargo/docker version`) is Read (`classify.rs:1689-1700`). Sound. B-520/B-521 are
  answered in D-523; supreme should close them.
- **PATH-planted program residual (already noted in the handoff, not newly broken):**
  `echo x >> ~/bin/git` is Fill, and a later bare `git ...` is classified as the real git.
  A program planted on PATH outside runs/ is out of the classifier's model. Document the
  assumption (PATH is trusted) or guard writes to PATH dirs.

## Notes

- **B1-B9 are closed.** Every round-1 probe now classifies Delete, verified directly against
  the committed classifier and asserted in `crates/srv-files/src/shell/classify/tests.rs`
  (`round1_blocking_probes_are_delete`, `round1_b9_cmd_exe_runs_only_plain_commands`). The
  tests assert the class, not merely "no panic." Spot checks that pass: `$(( $(rm) ))`,
  `let 'x[$(rm)]'`, `exec -a`, `history -w`, `xargs`, `find -exec`, `ln`/`cp -l/-s`,
  `--upload-pack/--receive-pack/--template/-c`, cmd.exe single-quote injection.
- **Allowlist design holds** against the forms I probed: redirection variants
  (`>|`, `<>`, `&>`, `2>`, `>&`, fd-dup, `{fd}>`), `~`/glob/brace targets, process
  substitution `<(..)`/`>(..)`, coproc, `source`/`.`, `alias`/`shopt`, function defs,
  heredocs with `$(`, `[[ ]]` arithmetic, `set -o`, `sed w/e`, `awk system()/print >`,
  `tar --to-command`/`-x` outside -C, `unzip` outside -d, `zip -m`, `curl -K/-O`,
  `wget -r/-N/-e`, `git` program-running options and history-rewriting subcommands, `gh`
  non-group first words / `auth token` / `config set`, quoting tricks (`"rm"`, `\rm`,
  `r''m`, `RM.EXE`, unicode `ｒｍ`), `cp`/`mv` overwriting an existing outside file,
  bookkeeping deletes, and the cmd.exe fallback (only a single plain argv command runs).
- **S3/S4 fixed and consistent:** `fs.delete` inside runs/ is Read, shell `rm` inside runs/
  is Read (`fs.rs:228-245`, `classify.rs` delete_target). `fs.move`/shell `mv` of a source
  outside runs/ is Delete (`fs.rs:186-202`). Acceptance:
  `fs_delete_inside_runs_runs_outside_asks`, `r1_fs_case_and_dotdot_variants_...`.
- **B7 links:** `paths::hard_linked` makes a file with >1 link (or an unreadable count)
  Outside for `fs.write/delete/move` and every shell target; `links_elsewhere()` makes
  scratch targets Delete when another command in the same line extracts/clones/copies a
  tree. TOCTOU between classify and the syscall is narrowed by `util::recheck` re-resolving
  (which re-runs `place`/`hard_linked`) but a link/junction swapped between recheck and the
  syscall remains an inherent residual; `fs_junction_out_of_runs_is_outside` covers the
  classify-time junction case.
- **S6/S7/S8/S10 fixed:** diff runs in `spawn_blocking` with a `similar` timeout
  (`diff.rs:90-149`); no machine path, skills under `$CLAUDE_CONFIG_DIR/skills` or
  `~/.claude/skills` (`docs.rs:104-106`); pandoc gets `--sandbox` on 2.15+, an image-dropping
  Lua filter on docx.write, and `-t markdown-raw_html` on docx.read (`docs.rs:11-13,
  407-427, 688-691`); the job sets `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and the tree is
  killed on every exit path (`proc.rs:147-169, 262, 341`).
- **Sanitized bash env (beyond-brief):** `sanitize_env` strips `BASH_ENV`, `ENV`, `CDPATH`,
  `GLOBIGNORE`, `BASHOPTS`, `SHELLOPTS`, `PROMPT_COMMAND`, `PS4`, `IFS` and every
  `BASH_FUNC_*` (`shell/mod.rs:118-140`); bash runs `--noprofile --norc`. Sound.
- **Rules 2 and 5 unchanged and intact:** shell output is capped and the full stream is
  scrubbed before it is saved to `files/`; `r1_vault_canary_echoed_by_shell_never_leaves`
  and `vault_canary_never_leaves_through_fs_read` assert the canary never reaches text, the
  saved file or the ledger. No resolved vault value appears in any new reason string.
- **Commit ids:** F-1..F-3, F-5, D-523 as claimed; the diff matches (shell allowlist,
  link counts, cmd fallback, pandoc sandbox).

## Verdict

Request changes. B1-B9 are genuinely closed and the allowlist is a strong, fail-closed
design. The two Blocking items are a single implementation gap: `runs_code_on_use` does not
recognise the XDG git config/attributes paths, so writing `~/.config/git/config` or
`~/.config/git/attributes` is auto-allowed Fill even though an allowlisted Read-class git
command then executes what they contain. Fix those (and ideally the Should-fix startup-file
set, or get D-523 to ratify Fill for it) with a regression test, and this is an approve.

Verdict: request changes
