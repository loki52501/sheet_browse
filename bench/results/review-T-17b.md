# Review T-17b round 1 (T-17b, T-17c; D-145, D-146, B-206)

Branch `swarm/core-T17b`, `git diff main...swarm/core-T17b` (b2cbcf4, f739e05, a549823, 2d532ae).
Read against D-145, D-146, B-206, WAVE3 core task 1, phase1-2026-10-27 Failures 1, 2, 5, 7.
Code and git only; no cargo build, test or clippy run (host RAM, per brief).

## Blocking

- none

## Should fix

1. **Not built.** a549823 says harness was never compiled or tested, and clippy was never run on f739e05.
   Before merge run, one at a time: `cargo clippy -j 1 -p harness -p srv-files --all-targets -- -D warnings`,
   `cargo test -j 1 -p harness --lib launcher`, `cargo test -j 1 -p srv-files`, then the workspace (D-87).
   From reading, it should compile: `split([char; 2])` is fine, and so are the `&String == &str` compares and
   `DISALLOWED_TOOLS.last().copied()`.
2. **The joined preamble is not scrubbed again.** crates/harness/src/launcher.rs:390-397. `run_preamble` scrubs
   the text with newlines in it (crates/core/src/sched.rs:271). `preamble_line` then joins the lines with
   spaces, which makes new strings across each line break that the scrubber never checked as a whole. Matching
   a vault value there is very unlikely. Rule 2 still says every outgoing string is scrubbed, and the fix is one
   call: wrap the result in `harness_core::vault::scrub(..)`.
3. **No test goes through the `claude.cmd` shim.** launcher.rs:553-607 builds the argv for program `claude` and
   never spawns it. So nothing checks the batch-file path (`cmd_shim`, launcher.rs spawn fallback): std's .cmd
   escaping refuses `\r`/`\n` and quotes spaces and `%`. The no-newline assert covers the refusal. Nothing covers
   a runs root with spaces (`F:/aside_mcp for claude/...`) or with `%`. Add a `#[cfg(windows)]` test:
   - a temp folder `dir with space%x`;
   - in it a `claude.cmd` that writes `%*` to a file;
   - spawn it through the fallback;
   - assert the spawn succeeds, the preamble arrives as one quoted token holding the files/ path, and
     `--disallowedTools` is the last flag.
4. **The link-out refusal test can pass without testing anything.** crates/srv-files/src/paths.rs:1026: the
   assert sits inside `if make_link(..)`, so the test passes if `mklink /J` fails. Junctions need no privilege,
   so assert `make_link` succeeded under `cfg(windows)`. Also add one dispatch case that pins rule 4 by name:
   `fs.write` with `../ledger.jsonl` and `../asks/1.answer` is refused with Args (accept_t13c.rs:235 covers only
   `../x.txt`).
5. **`fs.move` accepts destinations that `resolve_out` refuses.** crates/srv-files/src/fs.rs:190-193. For a
   scratch source, if `resolve_out` errors and `in_base` is a folder, the `_ if paths::is_dir(&in_base)` arm
   still moves there. So `to: ".."`, `"../.."` or a drive-relative `"C:x"` that names an existing folder moves
   the file out of the base (or into the server's per-drive cwd). The class is Fill: dst Outside only raises to
   Fill. This is the same as on main, so no gate class is lowered. It does contradict D-146 ("a relative output
   that leaves files/ is refused") and B-206 (2) ("drive-relative refused for outputs"). Limit that arm to
   `!is_drive_relative(raw) && in_base.starts_with(&roots.base)`. Add `"to": ".."` to the refused case at
   fs.rs tests (plan_move `../../nowhere.txt`).

## Nits

- agent-memory/tasks.yaml:434: T-17c's `branch` is `swarm/core-T17c`, but the work is on `swarm/core-T17b`.
- D-146 (agent-memory/decisions.yaml:763) lists D-570 and D-571 as amended. `move_dest` now puts a files/ folder
  ahead of a base folder of the same name (fs.rs:190-191, B-206), which changes D-573. Add D-573 to that list.
- paths.rs:54 with paths.rs:92: when the runs root has no parent (a drive root), `base == runs`. Then every
  relative base resolution `starts_with(runs)`, and nothing is redirected to files/. This is an edge case;
  add a comment or a `base != runs` guard.
- bench/fixtures/ladder_session.py:65-68: a trailing `--append-system-prompt` with no value is dropped without a
  word. That is acceptable for a fixture.
- launcher.rs:593 compares the result with `run_preamble().lines().join(" ")`, which is close to the
  implementation. The test still means something because of the asserts before it (id, absolute files/ path,
  the cite-only text, no newline, position, the last element).

## Notes

- **Hard rules.**
  - No keys, `.env` or model SDKs.
  - Captcha handling is untouched.
  - No raw HTML or pixels reach the model.
  - No sleeps.
  - The tool descriptions contain no em dashes or `--` separators.
  - Rule 2: see should-fix 2. The preamble holds only the run id and path, and is scrubbed.
- **Rule 4 checks on paths.**
  - Every class is still computed by `place` on the resolved path (fs.rs `plan`, `plan_move`,
    `delete_verdict`), so redirecting a path cannot pick a class for a different file.
  - Relative outputs now land in files/ (scratch, Fill), and relative deletes fall back to files/ (Read).
    Both are what D-146 and D-571 intend; neither lowers the class of an unchanged effect.
  - `in_files` (paths.rs:100-110) checks `starts_with(files)` after `resolve`, which runs Win32 normalisation
    (`..`, trailing dots and spaces) and canonicalises every existing component (links, junctions, 8.3 names).
    So `../x`, `a/../../x`, `.. /x`, `x./`, and a link or junction in files/ that points out all fail closed.
  - `\\?\` and `\\.\` (either slash) are still refused in `resolve`. UNC paths, `//host` and rooted `\x` are
    not relative (paths.rs:157-165), so they resolve as before. `C:x` is refused (paths.rs:86-90). ADS is still
    refused.
- **Bookkeeping.** asks/, ledger.jsonl, report.md and prompt.md are at the run folder's top level. Nothing in
  files/ can reach them as scratch. The B-206 (1) path through the base (`runs/<id>/ledger.jsonl`) is classed
  Bookkeeping by `place` (paths.rs:212-219), as before D-146.
- **Argv.** In interactive mode the order is prompt (unchanged, with the leading-space guard), then
  `--append-system-prompt <one line>`, then `--disallowedTools` with its three entries, which come last.
  There is no `--allowedTools` in interactive mode. The `-p` argv and stdin are unchanged
  (launcher.rs:536-539 still asserts the full `-p` argv; `session_prompt` is untouched).
  The ladder shim cuts its own args at `--disallowedTools` before it looks for `-p` and for the new flag
  (ladder_session.py:59-68). It forwards the flag before `--allowedTools` (:90).
- **The tests can fail.**
  - If the `--append-system-prompt` arg is dropped, launcher.rs:543 and :571 go red.
  - If `resolve_out` goes back to bare-names-only, accept_t13c.rs:190 goes red (`files/here.txt`).
  - Without the `starts_with(files)` guard, accept_t13c.rs:235 and paths.rs:1015 go red.
  - Without the runs/ exception, accept_t13c.rs:190 goes red (`deep/x.txt` would nest).
  - If the read fallback left files/, paths.rs:1083 goes red (the last assert sets up `runs/<id>/x.txt` for it).
- **Failures 2, 5 and 7 used absolute paths into the session cwd.** D-146 leaves absolute paths alone, so those
  failures are fixed only if the model follows the preamble's "Save deliverables in this folder" line. Failure 1
  likewise depends on the cite-only line. No offline test can show either; the T-29 ladder rerun is the check.
- **Pre-existing, outside this diff.** An interactive `harness run` with a multi-line prompt still cannot spawn
  through `claude.cmd`, because std refuses `\r`/`\n` in batch-file arguments. The new test's prompt
  `"find jobs\nand save them"` would hit this if it spawned. This is worth a follow-up task.
- **Merge gate.** merge_gate.py reads `review-<task>.md` for each gated task. If T-17c is gated on its own, it
  needs its own review file.

## Verdict

Approve, provided the builds and tests in should-fix 1 are green before merge. Should-fix 2 to 5 are small and
can be done in round 2 or as follow-ups.

Verdict: approve
