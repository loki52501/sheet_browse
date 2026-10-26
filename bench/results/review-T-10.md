# Review T-10 (round 6, D-227/D-228): long first word char-cut, outline capped

Range: `git diff 182f11c..HEAD -- crates/sb-sheet`. Commits: d2cf21b (D-227, with the threshold
D-228 confirms), 185ad00 (test only, d226_2 sweep), 8ca6ec8 (test only, contract_paths.rs) and
bb2ab68 (merge of main). Coder handoff: agent-memory/handoffs/T-10-rust-coder-b32-4.yaml. I judged
the word threshold against D-228 (this read's budget, min(chunk cap, max_tokens remainder)), not
against my round-5 suggestion.

Earlier rounds:
- Round 1 asked for changes.
- Rounds 2, 3 and 4 approved.
- Round 5 (182f11c, D-226) asked for changes. One Blocking item: a fresh read under max_tokens
  could return an empty body.

What I ran at 8ca6ec8:
- `cargo fmt --all -- --check` is clean.
- `cargo clippy -p sb-sheet --all-targets -- -D warnings` is clean.
- `cargo test -p sb-sheet` is green: lib 83, acceptance_t10 20, acceptance_t10_b32 16,
  acceptance_t10_d226 9 (6.3 s in debug), contract_paths 1, find_register 8, read_article 11,
  skeleton_sheet 8. Nothing is ignored.
- I ran probes and mutations in a throwaway copy of the crate under the session scratchpad (its own
  `[workspace]`, release build). No repo code was touched.
- bb2ab68 touches no crates/sb-sheet file. `git diff bb2ab68^1 bb2ab68 -- crates/sb-sheet` is empty.

## Blocking

- none

## Should fix

1. **A `remaining:` label that was cut cannot be used as `region`.**
   - Code: reader.rs:693 `cap_label` appends `...`. heading_section (reader.rs:437-441) matches the
     region text exactly or as a prefix of the heading's plain text, so the trailing `...` never
     matches.
   - Probe: a 104-char heading gives the label `## Release notes x00 ... x14...` (79 chars).
     `region` set to that label returns `region "..." not found, reading the page`. The same label
     without the `...` finds the section.
   - Effect: copying a long label from the outline costs one wasted turn. The note is visible, so
     the model can recover.
   - Fix: strip a trailing `...` (or `…`) from `region` before the prefix try, plus a test that
     reads a capped label back as a region.
2. **An indented over-long code line gives an empty fenced chunk.** This was there before
   D-227. D-227 now also reaches it under max_tokens.
   - Code: the first line is `    zzzz…` (indent plus one long token). `code_cut_point` falls back
     to `cut_point` (reader.rs:1287). That returns the last whitespace before the limit
     (reader.rs:1322), which is inside the indent, so the piece holds only spaces.
   - Probe: `    ` + 20,000 `z`, then `    next();`. At max_tokens 100, at 1000 and with no
     max_tokens, chunk 1 of 3 is "```\n\n```". Only the next read starts on the text. The budget
     stays within max_tokens and the chain goes on.
   - Before d2cf21b, the max_tokens case stopped with an empty body (the round-5 Blocking). It is
     now one empty read and then the text, so the round-5 failure (text that can never be reached)
     is gone. The coder's `code line` fixture (`let x = "…"`) has a real word break, so it does not
     cover this case.
   - Fix: in `cut_point`/`code_cut_point`, ignore a word break at or before the end of the leading
     whitespace and fall back to the char cut.
3. **The fold entry is only checked in a lib test.** `remaining_outline_is_capped_in_entries_and_label_length`
   (reader.rs:2752) is good, but the tester's acceptance binaries have no D-227 outline case. File it
   with the tester's restamp.

## Notes

- **The round-5 Blocking item is fixed as D-227/D-228 say.**
  - Code: reader.rs:1063 `word_cap = cap` under max_tokens, so reader.rs:1221 char-cuts any first
    word that does not fit this read's budget.
  - Test: `long_first_word_is_cut_by_characters_under_max_tokens` (reader.rs:2700). It covers a
    6000-char token at max 100, 500, 1000 and 1499 in three places: first in a paragraph, after an
    intro paragraph, and in a code line. It asserts that the first read is non-empty, that the
    char-cut piece fills exactly what is left, and that the cursor chain is a prefix of the page
    with no gap and no repeat.
  - Mutation: putting `word_cap` back to `chunk_cap` turns this test red. It also turns
    `tiny_remainder_stops_the_read_instead_of_overshooting` and acceptance
    `d226_2_a_read_under_max_tokens_never_takes_less_than_a_word` red.
- **Should-fix 1 from round 5 (outline cap) is done.**
  - Cap: at most 20 sections, then one entry, `+N more sections` (`+1 more section` when N is 1).
    Its `words` is the sum over the folded sections (reader.rs:1360). Each label is at most 80
    chars, `...` included.
  - Not spoofable: the fold entry is rendered as `, W words` only at index 20 (reader.rs:201).
    Page labels always start with `#` (or are `(top)`) and `collapse` makes them one line. A page
    whose headings read `+99 more sections, 1 words` renders as `- ## +99 more sections, 1 words
    (7 words)`, which looks different.
  - Scrub before cap: the label is cut after `ctx.scrub` (reader.rs:685-686). The unit body is
    also scrubbed before the label is built. A password placed at offsets 60 to 90 across the
    80-char cut never shows a prefix (`hunt`, `pass`). Reordering cap and scrub turns no test red,
    but it also cannot leak, because the body is scrubbed first.
  - Char boundaries: `cap_label` cuts with `chars().take` and then `trim_end`. The test uses
    multibyte `ééé` headings.
  - Mutation: the fold entry rendered like a normal entry turns the outline test red.
- **No regressions of D-219..D-226.**
  - Tests: every earlier test still passes. Two tests changed, and both changes follow from
    D-227/D-228:
    - `tiny_remainder…`: its stop fixture is now a code fence that cannot fit, and a 1-token
      remainder before `## Cccc` now reads `## C`.
    - acceptance `d226_2_a_read…`: a partial word is allowed only as the last token of the chain's
      last read, filling exactly what was left, and the test asserts that this case happens.
  - The termination test samples max_tokens, still asserts that at least one stopped read happens,
    and still checks the chain bound.
  - 185ad00 keeps max_tokens 1 to 12 plus a spread up to 60 at chunk_tokens 1 and 3, and asserts
    the case count. The overhead edges (prefix, depth-4 list, 10-backtick fence, D-225 backslash)
    are all at 12 or below.
- **Fuzz probe.** 400 random pages mixed headings, code, prose, 900-char tokens, multibyte and
  emoji text, backtick runs up to 30, `remaining:` text, `[a](#3)` and leading indents. Each ran
  with a random max_tokens (1-400 or none) and chunk_tokens, following cursors to the end (4803
  reads).
  - No overshoot, no repeated cursor, no panic and no line with 11 or more backticks.
  - Every outline had at most 21 entries, and every label was at most 80 chars.
  - 37 reads had a cursor but no content: D-226 overhead stops, plus should-fix 2.
- **B-202 (wide table header separator) is deferred by D-227.** It is still open: a header row with
  thousands of empty cells makes a separator-only chunk larger than chunk_tokens when there is no
  max_tokens. max_tokens itself holds.
- **Hard rules.** The diff has no keys, `.env`, I/O in the library, sleeps or em dashes.
  - New `unwrap`/`expect` calls are only in tests. The canary in acceptance_t10_d226.rs is built at
    runtime or comes from SB_VAULT_CANARY, never a literal.
  - New slicing in the library is char-based. `OUTLINE_LABEL_MAX - 3` is a constant expression.
- **Interfaces.** lib.rs, index.rs, tokens.rs and data.rs are unchanged, so I-5/I-6 are untouched.
  The interfaces.yaml diff in this range comes from the main merge (I-3, I-4, H-5 entries) and does
  not touch I-5/I-6. contract_paths.rs pins every I-5 and I-6 path and the I-6 fn signatures. It
  matches interfaces.yaml lines 43 and 51.
- **Commit messages.** d2cf21b cites W-5 B-201 D-227 and matches the code. 185ad00 and 8ca6ec8
  are test-only, as their messages say. No behaviour falls outside those ids.
- **Coder edited tester files** (acceptance_t10_d226.rs, two tests). Both edits follow
  D-227/D-228 and neither weakens a D-226 check. The tester should still re-run and restamp
  bench/results/accept-T-10.md.

## Verdict

The round-5 Blocking item is fixed under D-228, and its test fails when the fix is reverted.
Round-5 should-fix 1 (outline cap) is done: it is bounded, cannot be spoofed, scrubs before
cutting and cuts on char boundaries. Gates are green, D-219..D-226 hold, there are no hard-rule
breaks, and I-5/I-6 are unchanged and now pinned by a test. Should-fix 1 to 3 and B-202 go to
follow-ups (T-10b).

Verdict: approve
