# Review T-10 (round 8, D-231): in-cell pipe across table cells, fast path, prefilter

Range: `git diff c3b9c4f..HEAD -- crates/sb-sheet`. Commits:
- 465e9b8: the code change (D-231) and lib tests. Its only change to a tester file removes the
  `#[ignore]` on `d231_value_with_an_in_cell_pipe_across_cells_never_shows`.
- b9e1a9d: tester only. It adds acceptance_t10_d229.rs.
- c8bd2e4 and 7eb3c10: agent-memory only (D-231, B-204).

Coder handoff: agent-memory/handoffs/T-10-rust-coder-b32-6.yaml.

Earlier rounds:
- Rounds 1, 5 and 7 asked for changes.
- Rounds 2, 3, 4 and 6 approved.

What I ran:
- I ran the gates on a clean `git archive HEAD` export. The shared worktree had a tester mutation
  in reader.rs (`could_show`) at the time, so I did not run them in place.
  - `cargo fmt --check` is clean.
  - `cargo clippy -p sb-sheet --all-targets -D warnings` is clean.
  - `cargo test -p sb-sheet` is green: lib 93, acceptance_t10 20, b32 16, d226 9, d227 8,
    d229 7, contract_paths 1, find_register 8, read_article 11, skeleton_sheet 8. Nothing is
    ignored.
- I ran black-box probes through `read`, `skeleton` and `sheet` in a release build, on a
  throwaway copy of HEAD under target/tmp. I made one change to the copy: a thread-local switch
  that turns on the crate's `joined_forced()`. That runs the full check on every line with every
  value, with no fast path and no prefilter.
- Leak readings are the same as in round 7:
  - the raw output;
  - each `[x](#id)` read as `x`;
  - each `alias "label"` read as `label`;
  - each of those collapsed, and with `\|` read as `|`.

  I followed every cursor chain to its end. I checked `remaining:`, the skeleton and the sheet at
  chunk/max (0,-), (30,-), (8,-), (1,-), (3,40), (5,15), (2,6), (40,3) and (0,1).

## Blocking

- none

## Should fix

1. **The soundness note on `could_show` (reader.rs:1368-1372) claims more than holds.**
   - `cut_runs_tagged` runs after hiding (reader.rs:918, :1039). So hiding one char of a backtick
     run that was cut can uncover backticks that the unhidden `line` dropped.
     - Example: a plain span ````` ```` ````` renders as ``` `` ```. Hiding char 1 gives
       ``` `***`` ```, which has three backticks.
     - So a later build is not always a subsequence of `line` plus `*`.
   - This matters only for a password spelled with `***` between backticks.
   - `render_block`'s final `ctx.scrub` (reader.rs:684) still catches the literal match. My probe
     (`` ` b `` and ``` `***`` ``` over ```` ```` ```` + link `b`) gives the same `*** ***` with
     the prefilter on and off.
   - This is degenerate, so it does not block. Fix: skip `` ` `` in `needed` the way `*` is
     skipped (reader.rs:1378). Or say in the comment that the final scrub covers this case.
2. **(Still open, T-10c.)** Not refiled here:
   - the region with a `...` label;
   - the empty fenced chunk from an indented long code line;
   - B-202;
   - B-204, the cursor cycle on a duplicate block order.

## Notes

- **The round-7 blocker is fixed.**
  - What changed:
    - `table_row_tagged` tags the backslash it inserts with `esc` (reader.rs:952-957).
    - `joined_hits` adds readings that drop it (reader.rs:1404-1414).
    - D-230 rules apply: a link cell becomes `***` whole, and a plain cell hides only the
      characters it contributes.
  - The round-7 probes all hold now. `x | y|z` (plain|plain and link|plain), `a|b | c`,
    `Kq|b | Zc` and `p | L|m | q` render as `| *** | *** |` and the like.
  - New probes that also hold, each at every chunk/max above and in remaining, the skeleton and
    the sheet:
    - two in-cell pipes over three cells;
    - a button cell whose `alias "L\|m"` label holds a pipe;
    - an empty checkbox that falls back to the name `N|o`;
    - a pipe-only middle cell;
    - a value `a\|b | c`;
    - multibyte and emoji across cells;
    - NBSP in a value;
    - `[` and `"` values in table link and button cells;
    - `*` in a joined value;
    - code and heading lines.
- **The prefilter and the fast path agree with the full check.**
  - Differential fuzz: 27,000 cases over 7 seeds. The alphabet had `*`, `\`, `|`, backticks,
    brackets, quotes, NBSP, tab, `é` and emoji. Pages mixed every block kind, header rows, a
    checkbox whose name holds `|` and `"`, unknown elements and 0 to 2 passwords. Chunk/max were
    random, and every cursor was followed.
  - The normal output was byte-identical to the forced output in every case, across chain reads,
    `remaining:`, cursors, skeleton and sheet. Every case with no password was identical too.
  - The argument holds by reading the code as well.
    - The ASCII bitmap uses only chars under 128, guarded by `is_ascii`, so the shift cannot
      overflow.
    - The subsequence test is per char, so multibyte is fine. It skips whitespace and `*`, which
      only makes it pass more values, never fewer.
    - The readings only drop chars (link syntax, `alias "`, `esc`), collapse whitespace or insert
      `***`/spaces. The one exception is the backtick case in Should fix 1.
    - Escaped spellings are separate entries in `secrets`, each tested against the line, which
      already holds the `(`, `'`, `\|` and `)(` replacements.
  - `inline_str`, `code_str` and the table row string mirror the tagged builders line for line,
    including the needs_space test on the last char and on the first char of the form.
  - Pinned by `no_password_value_renders_the_same_as_the_full_check` and
    `prefilter_never_skips_a_value_that_shows`. The coder's mutation runs turn both red.
- **No panic, and every chain ends.**
  - Fuzz covered 27,000 differential cases plus 6,000 cases of the round-7 fuzz.
  - The hit loop is unchanged: each pass grows a finite `Hide` set.
  - New library code indexes through `get`. Every new `unwrap` is in a test.
- **Leak checker hits were false positives or degenerate values.**
  - Values with 1 to 2 chars after collapse (`\tc`, ` ,`, `a  `) matched skeleton counts or sheet
    syntax.
  - One 3+ char hit (`9 Y.(#'`) took its `9` from the header `chunk 4 of 29`. At chunk 2, a cut
    inside a link form left `[Y.(#'](`, and my collapsed reading paired it with `(top)` from the
    outline. That is my checker's artifact, not a value on the page.
- **Performance: round-7 should-fix 4 is closed.** Release build, same 40 KB / 379-block page:

  | Case | Round 7 | Round 8 |
  |---|---|---|
  | No password, first read | 9 ms | 5.7 ms |
  | Both values on every line, first read | 56 ms | 34 ms |
  | Both values on every line, chain at chunk_tokens 200 | 1.5-3.0 s | 49 reads, 1.48 s |
  | One 6,000-span block | 86 ms | 90 ms |

  The worst case gains little because every line passes the prefilter there. That is expected.
- **Round-7 should-fix 5 is closed.**
  - Lib test `value_joined_in_the_skeleton_heading_line_is_hidden`.
  - The tester's `d229_joined_value_in_a_heading_never_reaches_remaining_or_skeleton` and
    `d229_value_across_a_table_cell_separator_never_shows`.
- **Acceptance.**
  - `d231_value_with_an_in_cell_pipe_across_cells_never_shows` covers plain|plain, link|plain,
    plain|link|plain and a pipe in cell 1 with a link after. It asserts on the renderer reading
    with `\|` read as `|`, and it is green at HEAD.
  - The lib tests assert exact rows (`| keep *** | *** left |` and so on).
- **D-231 wording.** It says that "D-218's escapes" are also dropped. D-218's escapes replace
  chars and insert no backslash, so `esc` is set only on the table `\|`. That is right, as the
  coder's handoff explains. Only the decision text is loose.
- **Hard rules.**
  - The diff has no keys, `.env`, I/O, sleeps or em dashes.
  - `FORCE_JOINED` exists only under `cfg(test)`. The non-test `joined_forced` is the constant
    `false`.
  - Lib-test assert messages redact the value parts as `<pN>`, including the `\|` spelling.
- **Interfaces.** Only reader.rs changed under src. lib.rs and interfaces.yaml are unchanged, and
  contract_paths is green.
- **Commit message.** 465e9b8 cites T-10, W-5, H-2 and D-231. Its text matches the code: the esc
  reading, inline_str/code_str, the fast path, the prefilter and the listed tests.

## Verdict

D-231 hides a password value that crosses table cells with in-cell pipes, for every cell mix I
tried, in reads, `remaining:`, skeleton and sheet. With no password, the fast path gives the same
bytes as the full check. The prefilter matched the forced check in 27,000 differential cases, and
its soundness argument holds apart from one degenerate backtick case, which the final scrub
backstops (Should fix 1). Gates are green, with no panics and no runaway chains.

Verdict: approve
