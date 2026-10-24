# Review T-10 (round 3): sb-sheet skeleton, find, sheet, read

Range: `git diff main...HEAD -- crates/sb-sheet agent-memory bench` (e4c8a8a..c8e892a). Round 3 focuses on the round-2 fix commit 4220e1b (D-217, D-218), the tester commit 8a54518 and the merge of main (495b371). I re-checked the whole range.
What I ran locally:
- `cargo fmt -p sb-sheet --check` is clean.
- `cargo clippy -p sb-sheet --all-targets -- -D warnings` is clean.
- `cargo test -p sb-sheet` is green at HEAD: lib 57, acceptance_t10 16/16, find_register 8, read_article 11, skeleton_sheet 8.
- `cargo run -p sb-sheet --example skeleton -- crates/sb-sheet/examples/register.index.json` exits 0. It prints the 10-line skeleton with `! CHALLENGE #11`, `! OVERLAY #17` and `main: form`.
- decisions.yaml, blockers.yaml and tasks.yaml parse.
- `.claude/state/leaks.log` is absent.

## Blocking

- none

## Should fix

- bench/results/accept-T-10.md:3: the report is pinned to dd00fd6. Its last row says "unit 48/48", but HEAD has 57 lib tests. 4220e1b changed find scoring (D-217, synonym groups and respelling weight 1.0), and it also changed reader escaping, sealed-span and code-split behaviour (D-218), all after the tester ran. I reran the suite at HEAD and acceptance_t10 is 16/16. Still, the tester must rerun the ladder on 4220e1b or later and restamp the report so the merge gate reads a current one. Also, agent-memory/handoffs/T-10-rust-coder-fixer-2.yaml:6 says the report "still says red". That is wrong: it says green.
- Branch: main has moved 2 commits past the merge (7ff5d06, 45cd284). They touch only agent-memory/handoffs/supreme-wave1.yaml, so nothing conflicts. Run `git merge main` again before the PR.

## Notes

- Round-2 should-fix items are resolved:
  - The skeleton content hint now treats only a frame-0 `main` as the page's main (skeleton.rs:101-134, D-218). This matches the reader's D-216 rule. Test: `iframe_main_does_not_drive_the_hint` (skeleton.rs:363) covers blocks, rows and the top-frame case.
  - The main merge (495b371) is in, and I-6 in agent-memory/interfaces.yaml:50 matches what the crate exports.
- New round-2 fixes check out:
  - Sealed spans. `Ctx::linked` (reader.rs:254-270) walks up to the outermost host outside every sealed frame. It bounds the walk against cycles and ignores self-hosting frames (reader.rs:416-420). `span_form`/`plain_text` render the host's link form, or nothing when the host is not in the index. Test: `span_into_a_challenge_frame_shows_only_the_host` covers CHALLENGE, OPAQUE and a nested frame, plus the outline headings.
  - Escaped password spellings. `escaped_spellings` (reader.rs:503-521) produces all 8 combinations of the link, quote and pipe escapes. Span text is now scrubbed before escaping (reader.rs:558, 565, 596). Test: `escaped_password_values_are_scrubbed` covers table, link, label and heading forms at 2 chunk sizes.
  - sheet(region). It skips sealed frames both when matching a region and when listing known regions (sheet.rs:154-166, 208-245). Test: `unknown_region_lists_nothing_from_sealed_frames`.
  - Code splitting. `code_cut_point`/`skip_lead` (reader.rs:917-957) cut code at line breaks and keep indentation. Test: `split_code_keeps_indentation`.
  - Chunk size. A huge `chunk_tokens` saturates (reader.rs:805). Test: `huge_chunk_tokens_do_not_overflow` with `usize::MAX`.
  - Find speed. Each IDF is computed once per term per part and field (find.rs:82, 242-247). The test runs 5000 rows in debug, well under a second.
- D-217 behaviour vs B-6.2:
  - Max-per-synonym-group and the respelling weight of 1.0 are not literally in B-6.2's formula. B-6.2 only says synonyms "expand the query", so the weighting is the decision's to make. Both choices are recorded in D-217 as amendments to D-205/D-213, with a why.
  - The typed words keep their own groups, so the 3*name + 1.5*nearby + 0.5*heading shape, the role prior, visibility and the group bonus are unchanged.
  - Tests `exact_typed_name_beats_synonym_spelling` and `synonym_group_is_alternatives_not_a_sum` assert the ranking and the max-not-sum. They do not only check that the code runs.
  - Acceptance a6 (register, k=1) still returns `#6 #7 #10 #11 #12 #17`.
- Hard rules:
  - Keys and secrets: the library has no keys and no `.env`.
  - I/O and time: no `std::fs`, `std::env`, network, time or sleep.
  - Panics: every `unwrap`/`expect`/`panic!` in `src/` sits in a `#[cfg(test)]` module. New slicing in `skip_lead`/`code_cut_point` uses char-boundary offsets taken from `get(off..)`/`char_indices`.
  - Dashes: crate sources and data files have no em or en dashes. The one ` -- ` is in examples/skeleton.rs:4, a cargo command line in a doc comment, not output.
  - Canary: the vault canary is only read at runtime in tests.
- Interfaces:
  - I-5: index.rs is untouched.
  - interfaces.yaml is untouched by this branch.
  - I-6: lib.rs signatures are unchanged and the bodies only delegate. The additions match B-200 and D-59: `sheet_line`, `find_lines`, `data::{aria_roles, alias_for, role_prior, synonyms}` (data.rs:135-172) and `tokens::estimate` (tokens.rs:6).
- Spec ids:
  - The commit 4220e1b ids (B-6.1, B-6.2, B-6.3, W-5, H-2, D-5, D-55, D-205, D-206, D-212, D-213, D-216, D-217, D-218) match the changes.
  - B-6.4's infinite-scroll half stays driver-side, as the coder-B handoff records.
- Carried minor items, unchanged:
  - find with `k = 0` returns only the appended rows.
  - A cursor region silently overrides `opts.region`.
  - Scrubbing a very short password value can mangle unrelated text (the safe direction).
  - decisions.yaml has D-210..D-212 listed before D-205..D-208.
  - In sheet.rs:153 the doc comment cites D-206 for the sealed-frame rule. D-218 is the decision that adds it.

## Verdict

I found no hard-rule breaks, and the interfaces match I-5, I-6 and B-200. The acceptance items for B-6 (6, the pure half of 7, B-6.1 and B-6.3) and for W-5 (a, b and c, pure half) have tests that assert behaviour, and all are green at HEAD. Every round-2 finding is fixed and has a targeted test. Two things remain: the tester should restamp the acceptance report against 4220e1b, and the branch needs one more merge with main.

Verdict: approve
