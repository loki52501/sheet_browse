# Review T-10 (round 5, D-226): bounded fence, overhead in budget, outline scrub, skeleton tools line

Range: `git show 182f11c` (D-226), checked against my round 4 should-fix 1 to 4. I also re-checked
the whole follow-up `git diff 147b9c8..HEAD -- crates/sb-sheet` for regressions of D-219..D-225.
Coder handoff: agent-memory/handoffs/T-10-rust-coder-b32-3.yaml.

Earlier rounds:
- Round 1 asked for changes.
- Rounds 2 and 3 approved.
- Round 4 (6357f07, B-32 follow-up) approved with four should-fix items. They were: (1) a
  page-controlled fence overhead under max_tokens; (2) the table header suffix overshooting
  max_tokens; (3) the outline label spelling a password the body hides; (4) a title posing as the
  `tools for this site:` line.

What I ran at 182f11c (main is merged, nothing behind):
- `cargo fmt --all -- --check` is clean.
- `cargo clippy -p sb-sheet --all-targets -- -D warnings` is clean.
- `cargo test -p sb-sheet` is green: lib 81, acceptance_t10 20, acceptance_t10_b32 16 (0 ignored),
  find_register 8, read_article 11, skeleton_sheet 8.
- Black-box probes ran through the public API in a throwaway copy under `target/review5`, release
  build. The copy is deleted and no repo code was touched.
- One mutation: the suffix was dropped from `fit_piece`. `table_header_separator_counts_against_the_budget`
  then went red.

## Blocking

- **A fresh read under max_tokens can return nothing at all.** This is a D-204 regression that
  D-226 introduced.
  - Code: reader.rs:1036 sets `word_cap` to `chunk_cap`. reader.rs:1194-1198 needs the whole
    first word unless `base + word > chunk_cap`. reader.rs:1096-1098 then stops the read.
  - Effect: a word longer than the max_tokens remainder but shorter than a whole chunk is never
    char-cut. The read stops with an empty body, `truncated`, and a cursor that marks the budget
    as spent.
  - Probe: a paragraph that starts with a 6000-char token (a long URL, base64, minified text),
    default chunk_tokens.
    - `max_tokens` 100, 500, 1000 and 1499: the first read has an empty body and
      `truncated: max_tokens N reached`. The follow-up read ends with no cursor.
    - Same with an intro paragraph first: 4 tokens, then a stop. Same with a code line
      `let x = "ZZZ…";`.
    - Before 182f11c this char-cut to about max_tokens (D-204: sentence, else word, else char).
  - Why it matters: the text cannot be reached with that max_tokens. The model sees a truncated
    empty read and has to guess a bigger budget, which is a wasted turn (CLAUDE.md priorities).
    D-226 meant "at least one word" for a tiny remainder, not for a full budget that only a long
    word overflows.
  - Fix: decide "over-long" against the whole budget, not only against chunk_tokens. For example,
    `word_cap = chunk_cap.min(max_tokens * CHARS_PER_TOKEN)` (pass the full max_tokens into
    `next_chunk`). Then a word that no read under this max_tokens could hold is char-cut, and a
    word that a fresh budget could hold still stops.
  - Add a test: a 6000-char first word with `max_tokens: 1000` returns a non-empty piece of at
    most 1000 tokens, and a cursor follow-up continues with no gap.

## Should-fix

1. **The `remaining:` outline is unbounded and outside max_tokens.** This was already there before
   this change. Page text controls it, and it blows the budget by far the most.
   - Code: reader.rs:194-199 prints every entry. reader.rs:1332 `outline_from` has no cap.
     reader.rs:675 keeps the full heading as the label.
   - Probe: 2000 sections, each with a 190-char heading, read with `max_tokens: 50`. The body is
     50 tokens and the markdown is 103,788 tokens. The same trailer comes back on every read of
     the sequence.
   - D-210 counts only body tokens, so this matches the decision as written. But docs/04 W-5 says
     "`max_tokens` caps a whole read".
   - Fix: cut each label like the skeleton does (about 60-80 chars, never inside a form), and cap
     the entries (for example 30, plus `+N more sections`). Supreme should amend D-210 before
     T-12 feeds real docs pages through read().
2. **A wide header row's separator cannot be split, so chunk_tokens is page-controlled.**
   - Code: reader.rs:638 adds 4 chars per column. Without max_tokens, the empty-chunk path
     (reader.rs:1102-1117) pushes the whole last piece with its suffix.
   - Probe: a header with 1 text cell and 3000 empty cells. Chunk 2 is 3002 tokens and holds only
     the separator. About 4000 columns or more go past the default 4000-token chunk.
   - max_tokens itself holds: at max 300 the read stops, with no overshoot.
   - Low impact. Cap the column count (or the separator) when fixing should-fix 1.

## Notes

- **Round-4 should-fix 1 to 4 are fixed as D-226 says, each with a test that can fail.**
  - (1) `cut_code_backticks` runs after joining spans (reader.rs:871), so the fence is at most 10
    (reader.rs:649). `escaped_spellings` gains the cut spelling (64 masks). Probe at max 50 with a
    2000-backtick run: 3 reads, 48 tokens in total, no overshoot. 40 KB with a 9000-backtick run:
    3 chunks, 27 ms per read, at most 3995 tokens per chunk. Tests:
    `code_backtick_runs_are_cut_to_nine_so_the_fence_is_at_most_ten` (exact text),
    `long_backtick_run_stays_within_max_tokens` and `huge_block_with_a_long_run_is_a_few_chunks`.
  - (2) The suffix counts in `fit_piece` (reader.rs:1182-1188), and a cut piece never carries it.
    The 30-column probe at max_tokens 10, 30, 45, 60, 80 and 120 never overshoots. The test went
    red under my mutation. B-201 P4 `b201_p4_tiny_remainder_never_overshoots_max_tokens` is
    un-ignored, and the b32_6 4-char exemption is gone.
  - (3) The label is `scrub(outline_text(body))` (reader.rs:670-675). Probe headings: split plain
    spans give `## Your *** here`; `[hunter2` + `pass]` gives `## q (***) z`; a value split around
    a password link form or a button label never spells the value. Test:
    `outline_label_is_the_scrubbed_body_heading` plus the tester's
    `w5b_outline_never_shows_a_password_split_across_heading_spans`.
  - (4) skeleton.rs:100 `FIXED_LINE_STARTS` is matched case-insensitively with `get` (char-safe).
    The test has both colon and no-colon cases and a negative (`tools for everyone`). My grep of
    the skeleton's `format!`/`push` lines found no other multi-word fixed start.
- **Fuzz probe.** 600 random pages mixed headings, code, tables, lists of depth up to 9, backtick
  runs up to 30, 300-char words, multibyte text, `remaining:` and `chunk 3 of 9` text, and `[a](#3)`.
  Each ran with a random max_tokens (1-120 or none) and chunk_tokens (0-59), following cursors to
  the end (up to 1926 reads).
  - No read overshot what max_tokens left.
  - No cursor repeated and no follower looped.
  - No line had 11 or more backticks, and nothing panicked.
  - The only `s3cret`/`value` adjacency was across table cells, which are separate cells by design
    (not a scrubbable spelling).
- **Rulings on the coder's judgment calls** (handoff T-10-rust-coder-b32-3):
  - Stopped read returns a cursor with `used = max(used_after, max_tokens)` (reader.rs:155-159).
    Accepted. The same max_tokens then ends at once with no cursor (`left == Some(0)` path), so a
    cursor follower cannot loop, and charging the full budget fails closed.
  - "At least one word" = leading whitespace plus the first word, and an over-long word is char-cut.
    Accepted, except for the threshold: see Blocking.
  - Without max_tokens, the headings-only and empty-chunk paths keep the one-char minimum.
    Accepted: chunk_tokens is a soft cap and progress is needed (should-fix 2 is the only page-sized
    case).
  - The fence stays at longest run + 1, not a clamp. Accepted: it is bounded at 10 by the code cut,
    and stays minimal for ordinary code.
- **D-219..D-225 did not regress.** 182f11c removes or weakens no existing test. All earlier B-32,
  D-223, D-224 and D-225 tests still pass unchanged. The D-225 backslash is counted in
  `piece_overhead` from the whole rest (reader.rs:1162-1169). That can only over-count, because a
  piece poses as a harness line only if its rest does.
- **Coder edited the tester's acceptance_t10_b32.rs.** It un-ignored P4 and removed the exemption.
  Both changes only make it stricter. The tester should still re-run and restamp
  bench/results/accept-T-10.md, since a coder does not accept its own work.
- **Caller-chosen tiny chunks are slow.** A 40 KB code block at `chunk_tokens: 1` plans 35,009 chunks
  (0.6 s per read in release, `plan_starts` twice per read). chunk_tokens is a caller option, not
  page-controlled, so this is not a finding.
- **Hard rules.** The diff has no keys, `.env`, I/O, sleeps or em dashes. All new `unwrap`/`expect`
  calls are in `#[cfg(test)]`. New slicing uses `get`, `checked_sub` and `char_indices`, and
  `form_label`'s `len() - 1` is guarded by `ends_with('"')`.
- **Interfaces.** 182f11c does not touch lib.rs, index.rs or text.rs, so I-5/I-6 are unchanged.
- **Commit message.** It cites W-5 B-6 H-2 B-201 D-226 and matches the code. No behaviour outside
  those ids, apart from the threshold in Blocking.

## Verdict

Round-4 should-fix 1 to 4 are fixed with tests that fail when broken. Gates are green, there are
no hard-rule breaks, and the interfaces are unchanged. One regression blocks: under max_tokens, a
word longer than the remainder but shorter than a chunk is never char-cut, so a fresh read can
return an empty body (reader.rs:1036, 1194). Should-fix 1 (the unbounded `remaining:` outline)
needs a supreme ruling on D-210 before T-12.

Verdict: changes
