# Review T-10 (round 7, D-229/D-230): password value joined across link forms

Range: `git diff 8ca6ec8..HEAD -- crates/sb-sheet`. Commits:
- c550253: the code change, D-229 as D-230 clarifies it.
- 9f74cf2: test only, the new acceptance_t10_d227.rs.
- a402967: merge of main. `git diff a402967^1 a402967 -- crates/sb-sheet agent-memory/interfaces.yaml`
  is empty.

Coder handoff: agent-memory/handoffs/T-10-rust-coder-b32-5.yaml. B-203's default is what D-230
records: a contributing link span becomes `***` whole, and a plain span hides only the characters
it contributes.

Earlier rounds:
- Round 1 asked for changes.
- Rounds 2, 3, 4 and 6 approved.
- Round 5 asked for changes. Its Blocking item was fixed in round 6.

What I ran at a402967:
- `cargo fmt --all -- --check` is clean.
- `cargo clippy -p sb-sheet --all-targets -- -D warnings` is clean.
- `cargo test -p sb-sheet` is green: lib 87, acceptance_t10 20, acceptance_t10_b32 16,
  acceptance_t10_d226 9, acceptance_t10_d227 8, contract_paths 1, find_register 8,
  read_article 11, skeleton_sheet 8. Nothing is ignored.
- Black-box probes ran through the public API (`read`, `skeleton`, `sheet`) in a throwaway copy of
  the crate under the session scratchpad, release build. I ran the same probes against a copy of
  8ca6ec8 to check that they catch the old leaks. No repo code was touched.
- Leak check: a value counts as leaked if any of these readings of an output string contains it:
  - the raw string;
  - each `[x](#id)` read as `x`;
  - each `[alias "label"](#id)` read as `label`;
  - each of the above with whitespace collapsed, and with `\|` read as `|`.

  I tested the value as is, collapsed, and in its `[`->`(` and `"`->`'` spellings. I checked every
  read of a cursor chain at chunk_tokens/max_tokens (0,-), (30,-), (8,-), (1,-), (3,40), (5,15),
  (2,6), (40,3) and (0,1), and also the `remaining` entries, the skeleton and the sheet.

## Blocking

1. **A value that spans a table cell boundary and also contains an in-cell `|` still shows in a
   table row (rule 2/5, H-2, W-5, D-229).**
   - Probe:
     - Password `x | y|z`, cells `x` and `y|z`, gives the body `| x | y\|z |`.
     - The same value with cell 1 a link gives `| [x](#99) | y\|z |`.
     - `p | L|m | q` with the middle cell a link gives `| p | [L\|m](#99) | q |`.
     - `Kq|b | Zc` gives `| Kq\|b | [Zc](#99) |`.
   - Each of these shows the whole value once `\|` is read as `|`, which is how a renderer shows
     it. It fails at every chunk/max setting above.
   - D-229 already treats the ` | ` separator as part of a joined value: the coder's
     `value_joined_across_a_table_cell_link_is_hidden` hides `Kx7 | Zq9w`. So the same reading must
     also unescape the cell's own `|`.
   - Cause:
     - reader.rs:928 (`table_row_tagged`) copies the `\` it inserts with the `|`'s tag and
       `Shows::Always`, so every reading in `joined_hits` (reader.rs:1274-1276) keeps the
       backslash.
     - `escaped_spellings` (reader.rs:339, :350) escapes every `|` in the value or none of them. It
       never produces the mixed spelling that a cross-cell value renders as: separators bare,
       in-cell pipes escaped.
   - This was already there at 8ca6ec8 (the plain-cell case slips past the D-218 literal scrub). It
     is not a regression of c550253. But it is the cross-cell case that this round was asked to
     clear, so it is a hard-rule break now.
   - Fix: tag the inserted escape `\` so that at least one `joined_hits` reading drops it. For
     example, add a flag that a third pass filters out, or treat it as `Shows::FormOnly` and check
     the `alias "` + unescaped combination separately. Add a lib test and an acceptance case for
     plain|plain, link|plain and plain|link|plain rows holding such a value.

## Should fix

1. **(Round 6, still open, T-10c.) A `remaining:` label that was cut cannot be used as `region`.**
   heading_section (reader.rs:419) matches exactly or by prefix, and the trailing `...` that
   `cap_label` adds never matches. Strip a trailing `...` from `region` before the prefix try.
2. **(Round 6, still open, T-10c.) An indented over-long code line gives an empty fenced chunk.**
   `cut_point` takes a word break inside the leading indent. Ignore breaks at or before the end of
   the indent.
3. **(Still open, T-10c.) B-202.** A table header separator wider than chunk_tokens cannot be split
   when there is no max_tokens.
4. **A password now makes each read about 6 to 8 times slower.** Each read still stays under
   100 ms, so this is not a cliff. Release timings on a 40 KB page with 379 blocks, two links per
   block and a password row. Each was run twice, and the machine was noisy:

   | Case | 8ca6ec8 | a402967 |
   |---|---|---|
   | First read, no password | 9 ms | 9 ms |
   | First read, one password | 10 ms | 56 ms |
   | First read, 20 passwords with escaped chars | 27-46 ms | 161-265 ms |
   | Chain at chunk_tokens 200, no password | 57 reads, 0.37 s | 57 reads, 0.71 s |
   | Chain at chunk_tokens 200, one password | 57 reads, 0.33-0.51 s | 49 reads, 1.5-3.0 s |
   | One 6,000-span block | 67 ms | 86 ms |

   The no-password chain costing twice as much means the fast path builds tagged chars before it
   checks `secrets.is_empty()` (reader.rs:1239-1242).

   Cheap wins:
   - Check `secrets.is_empty()` before building tagged chars, and use the old string path when it
     is empty.
   - reader.rs:641 still runs `inline` (and so `scrub_joined`) on every cell just for the emptiness
     test and the column count. `span_form` emptiness would do.
   - `joined_hits` renders four strings and scans every escaped spelling on each pass, even when no
     secret can match. A first-char prefilter, or one pass for all secrets, would cut that.
5. **No test pins the skeleton's h1/h2 line, or an acceptance-level table row, against a joined
   value.** `block_line` goes through `inline`, and my probes show that both hold. But only lib
   tests cover tables, and nothing covers the skeleton. Add both to the tester's binary.

## Notes

- **D-229/D-230 work as written for every case that does not involve pipes.**
  - Hidden, with 8ca6ec8 leaking each one:
    - plain+link (`Kx7(`), link+plain (`.`) and plain+link+plain;
    - a button's `alias "label"` and an empty-text link or checkbox falling back to the row name;
    - list, quote and code blocks;
    - `Kx7 | Zq9w` across cells;
    - a heading that feeds `remaining:` and the skeleton;
    - a value with `[`, `"` or a backtick run in it;
    - multibyte and emoji values;
    - two overlapping values (`Kx7(Zq9w` and `Zq9w.Ab3` give `x *** *** *** y`);
    - a value at the end of a longer link text.
  - Every chunk_tokens/max_tokens setting above keeps the value hidden, because the hiding happens
    per unit before any cut.
- **Fuzz.** 9,000 random cases over 3 seeds, plus 4,000 with `*` allowed in values. The cases mixed
  block kinds, values split over plain, link, button and checkbox spans, and random chunk/max
  settings, with cursors followed to the end.
  - No panic, and every chain ended.
  - Every leak flagged by my checker was a false positive or a degenerate value:
    - a 3-char value made of fence markup;
    - one-char values after collapse (`1`, `"`, `a`) that the skeleton's counts or the sheet
      syntax happen to contain.
- **The hit loop terminates.** Each pass must set a new `Hide.all` or insert a new char index. Both
  are finite sets, so the loop ends (reader.rs:1257, :1264). A value containing `***` still ends:
  `***` chars carry no plain `at`.
- **A space inserted by the renderer is out of scope.** It is outside D-229 ("where no space was
  put"):
  - `[Kx7](#99) [Zq9w](#98)` for `Kx7Zq9w`;
  - `Kx7 ([Zq9w](#99)` for `Kx7 ( Zq9w`.

  These still show the value with one extra space. If supreme wants them hidden, record a decision.
  A squeezed reading in `joined_hits` would do it.
- **A value made only of table syntax and spaces (`| |`) cannot be hidden.** Every character of it
  comes from the renderer. This is degenerate, so I note it only.
- **Pre-existing, not in this diff: region matching is a yes/no oracle.** heading_section matches
  `region` against `plain_text` (reader.rs:429). `plain_text` scrubs each span alone and joins the
  spans with a space, so `region` can probe a value split across spans one guess at a time. This is
  low risk.
- **No regressions of D-218..D-228.** Every earlier test passes unchanged. The only change to a
  tester file is the removed `#[ignore]` on `w5b_body_never_shows_a_value_joined_across_a_link_form`.
  With no password the output is byte-identical (fast path at reader.rs:1241).
- **Round-6 should-fix 3 is closed.** `d227_outline_cap_entries_labels_and_more_entry` is in the
  tester's binary.
- **Hard rules.**
  - The diff has no keys, `.env`, I/O, sleeps or em dashes.
  - Every new `unwrap` is in a test.
  - New indexing in the library goes through `get`.
  - The lib-test assert messages print each value part as `<pN>`.
- **Interfaces.** lib.rs, index.rs, text.rs and interfaces.yaml are unchanged in this range. I-5/I-6
  are untouched, and contract_paths.rs is still green.
- **Commit messages.** c550253 cites W-5 H-2 D-229 and its text matches the code: inline, whole
  table rows, code and the skeleton block line. 9f74cf2 is test-only, as its message says.

## Verdict

D-229/D-230 close the round-6 W-5b leak for link forms in every block kind, the outline and the
skeleton. Gates are green and the loop terminates. But one cross-cell case still shows a password
value: a value that holds both ` | ` and an in-cell `|`. That is a rule-2 break in the exact area
this round covers, so it blocks. Should-fix 1 to 3 stay in T-10c. Should-fix 4 and 5 are new.

Verdict: changes
