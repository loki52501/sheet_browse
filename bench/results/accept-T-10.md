# Acceptance T-10 (tester, round 6): docs/05 B-6 and docs/04 W-5, crate sb-sheet, after D-227/D-228

Branch `swarm/sheet` at HEAD 8ca6ec81165755c989f7a0a30000916386705e27 (8ca6ec8), plus the round-6
test file listed below. The code under test is the D-227 commit d2cf21b: a long first word is cut by
characters under max_tokens, and the outline is capped. D-228 clarifies D-227: the word threshold is
the read's budget. Commits since round 5 (6dc452b):
- d2cf21b: D-227 in reader.rs. The coder also amended my `d226_2_a_read_under_max_tokens_never_takes_less_than_a_word`
  and shrank the termination test.
- 185ad00: the d226_2 overhead sweep is smaller at chunk_tokens 1 and 3. Chunk 0/20/400 keep 1..=60.
- 8ca6ec8: `tests/contract_paths.rs`.
- bb2ab68: merge of main. In `crates/`, it brings only other crates.

Tests:
- `crates/sb-sheet/tests/acceptance_t10.rs` (20 tests) and `acceptance_t10_b32.rs` (16 tests): unchanged,
  all re-run.
- `crates/sb-sheet/tests/acceptance_t10_d226.rs` (9 tests): re-run as amended by d2cf21b and 185ad00.
  The amended tests were checked by mutation (rows below).
- `crates/sb-sheet/tests/acceptance_t10_d227.rs`: new this round. It holds 8 black-box tests through the
  public API, with fixtures built here. 7 pass. One is a red-first W-5b finding, committed `#[ignore]`d
  and run here with `--ignored`. No test in the file takes more than 3 s in debug (the whole binary runs in 3.4 s).

Canary: `SB_VAULT_CANARY` was set to a fresh unique string (`sbcanary-T10r6-<ns>-<pid>`), the same
one for every run: sb-sheet, `--ignored`, mutations, clippy, fmt, example and workspace. It was planted
as a `FieldValue::Text` value on password rows. New placements this round:
- straddling the 80-char label cut: whole, split over two plain spans, and split over three plain spans;
- as the text of a link to the password field at the cut;
- in a heading folded into `+N more`;
- split across two adjacent headings;
- joined to a link form with no space, before or after the link.

| item | command | result | note |
|---|---|---|---|
| A6 find on register.html, k=1 (docs/05 acc. 6, D-54) | `cargo test -p sb-sheet --test acceptance_t10 a6_find_register_four_plus_challenge_and_overlay_in_document_order` | pass | Hits are `#6 #7 #10 #11 #12 #17` in document order. #11 is `Challenge` and #17 is `Overlay`. |
| A6 hidden promo never surfaces, newsletter ranks below signup (k=3, D-202) | `cargo test -p sb-sheet --test acceptance_t10 a6_never_hidden_promo_and_newsletter_ranks_below_signup` | pass | #4 and #5 are absent. Output is in document order. |
| A6 document order is index (walk) order, not id order | `cargo test -p sb-sheet --test acceptance_t10 a6_document_order_follows_index_order_not_id` | pass | Output is `#6 #99 #10 #11 #12 #17`. |
| A6 respelled query keeps the four (D-217) | `cargo test -p sb-sheet --test acceptance_t10 r3_a6_respelled_query_keeps_the_four_and_document_order` | pass | `email`, `e-mail` and the full four-part query at k=1 give the same hits. |
| A6/rule 3: a hidden-flagged CHALLENGE is still reported (D-215) | `cargo test -p sb-sheet --test acceptance_t10 r2_hidden_challenge_still_reported_by_find_and_skeleton` | pass | #11 is `Challenge` in find. The skeleton has `! CHALLENGE #11`. |
| A7-pure article markdown without nav (docs/05 acc. 7) | `cargo test -p sb-sheet --test acceptance_t10 a7_article_markdown_without_nav` | pass | 1 chunk, with no chrome text and no chrome links. Links, buttons, the list and the table are there. |
| A7-pure chunk cursors: no repeats, no gaps | `cargo test -p sb-sheet --test acceptance_t10 a7_chunk_cursors_no_repeats_no_gaps` | pass | Checked at chunk_tokens 60, 120 and 333. Each of the 400 split words appears once and in order. |
| A7-pure D-211/D-216: no chunk is only a heading or ends on one | `cargo test -p sb-sheet --test acceptance_t10 a7_no_chunk_ends_on_a_heading_when_content_follows` | pass | Checked at chunk_tokens 60, 120 and 333. |
| W5-a first heading is the title, no nav links, `remaining:` lists sections, cursor continues (docs/04 acc., pure half) | `cargo test -p sb-sheet --test acceptance_t10 w5a_first_heading_title_remaining_lists_sections_cursor_continues` | pass | The first heading is `# Tokio Runtime Guide`. `remaining:` has word counts. The last chunk has no cursor. |
| W5-b password value never emitted by read, sheet, sheet_line, find_lines, skeleton (canary) | `SB_VAULT_CANARY=<unique> cargo test -p sb-sheet --no-fail-fast -j 2 -- --nocapture > log` (also the `--ignored`, mutation, clippy, fmt, example and workspace logs), then `grep -c -F "$SB_VAULT_CANARY"` on each log (also with all whitespace removed), `grep -rlF` on `runs/`, the worktree and target/debug/deps, and `git grep -lF` | pass | `w5b_password_value_never_emitted` and `r2_w5b_autocomplete_password_span_and_scrub_before_chunking` pass. Contiguous hits: 0 in every log (sb-sheet, ignored, mutations, clippy, both fmt, example, workspace). With whitespace removed: also 0 in every log. `runs/` is absent. The worktree outside target/ has 0 hits, as do target/debug/deps and the tracked tree. There is no `.claude/state/leaks.log`. The ledger table and jsonl mirror do not apply: sb-sheet is pure and writes neither. The one W-5b fail (a value joined across a link form, row below) is a rendered-text spelling. The canary string itself never reached any output. |
| W5-b escaped spellings of password values (D-218) | `cargo test -p sb-sheet --test acceptance_t10 r3_w5b_escaped_password_spellings_never_emitted` | pass | Values holding a pipe, brackets and quotes are absent from every output. |
| W5-b canary through the B-32 escapes (D-219, D-223, D-225) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_canary_never_leaks_through_the_new_escapes` | pass | Absent from read (3 regions x chunk 4000/30/10, outline, cursor), skeleton, sheet, sheet_line and find_lines. |
| W5-b outline never shows a password split across heading spans (D-226 (3)) | `cargo test -p sb-sheet --test acceptance_t10_b32 w5b_outline_never_shows_a_password_split_across_heading_spans` | pass | The body renders `## Key *** end`. No outline entry or read at chunk 10/30/60 holds the value, even with whitespace removed. |
| W5-b round-6 finding: the read body never shows a value joined across a link form as rendered text (H-2, D-218 spirit) | `cargo test -p sb-sheet --test acceptance_t10_d227 -- --ignored` (`w5b_body_never_shows_a_value_joined_across_a_link_form`) | fail | Red-first. It is committed `#[ignore]`d so the suite stays green. Two values are tested. In the first, plain text ends in `(` and the tail is an unknown-element link's text. In the second, the link text is the head and the plain text after it starts with `.`. needs_space() puts no space around the link form in either case, so the body holds `<head>([<tail>](#99)` or `[<head>](#98).tail`. With each `[text](#id)` reduced to `text` (what a markdown renderer shows), both values appear. This holds in a paragraph and in a heading's body line, at chunk 0 and 30: 5 hits (`chunk 0 read 0: split before/after a link, in the body`, `chunk 30 read 1/2/3`). The outline is clean, because outline_text reduces the link forms and then scrubs. Missing: the body scrub must also run on the link-reduced spelling, and a hit must replace the spans involved with `***`. Another fix would always put a space between plain text and a link form. |
| W5-c CHALLENGE frame content never emitted by read | `cargo test -p sb-sheet --test acceptance_t10 w5c_challenge_frame_content_never_emitted_by_read` | pass | Frames 1 and 2 are absent across regions and chunk sizes. Main text is still read. |
| W5-c spans into the challenge frame, sealed regions (D-218) | `cargo test -p sb-sheet --test acceptance_t10 r3_w5c_spans_into_challenge_frame_and_sealed_regions` | pass | No sealed-frame text and no `#f1.`/`#f2.` ref. Sealed regions answer `no region`. |
| W5-c extra: challenge frame content not in sheet, find, skeleton | `cargo test -p sb-sheet --test acceptance_t10 w5c_extra_challenge_frame_content_not_in_sheet_find_skeleton` | pass | No hit outside frame 0, and no marker in sheet, find_lines or skeleton. |
| W5-c extra: OPAQUE-hosted frame content never emitted (B-5, D-216) | `cargo test -p sb-sheet --test acceptance_t10 r2_w5c_opaque_frame_content_never_emitted` | pass | Frame 3 is absent from read, sheet, find, find_lines and skeleton. |
| B61 skeleton of register: 80-150 tokens (D-200), title, URL, both forms with field count and submit, CHALLENGE, OVERLAY, site_tools | `cargo test -p sb-sheet --test acceptance_t10 b61_skeleton_register_budget_and_content` | pass | Within 80..=150 tokens with and without site_tools. Both forms, `! CHALLENGE #11` and `! OVERLAY #17` are there. No em dash. |
| B61 content hint uses only a frame-0 main (D-218) | `cargo test -p sb-sheet --test acceptance_t10 r3_b61_skeleton_content_hint_ignores_main_inside_iframe` | pass | An iframe main gives no `main:` hint. |
| B63 sheet(region) by landmark, form id, heading; document order | `cargo test -p sb-sheet --test acceptance_t10 b63_sheet_region_landmark_form_heading` | pass | nav, footer, region, f1, f2 and both heading spellings work. The whole page lists 19 rows, each once. |
| B63 no cap, document order with non-monotonic ids | `cargo test -p sb-sheet --test acceptance_t10 b63_sheet_no_cap_and_document_order` | pass | 600 rows in main and 602 on the page. |
| B-32 (1) plain span text cannot forge a link form (D-219) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_1_plain_span_text_cannot_forge_a_link_form` | pass | Exactly the 2 real link forms survive at chunk 4000/30/12. |
| B-32 (1) code content cannot close its fence; no 3-backtick run outside code (D-219) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_1_code_block_content_cannot_close_its_fence` | pass | A run of 5 gets a fence of 6. Fences balance at chunk 8/12/20. |
| B-32 (2) skeleton h1/h2 follow the reader's span rules (D-219) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_2_skeleton_headings_follow_the_reader_span_rules` | pass | The password and CHALLENGE links show only their element form. The canary heading renders as `Hello ***`. |
| B-32 (3) an explicit region that differs from the cursor's wins (D-220) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_3_explicit_region_beats_the_cursor_region` | pass | Region `Beta` with an Alpha cursor restarts at chunk 1 with the note. |
| B-32 (4) `remaining:` word counts and only words not yet returned | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_4_remaining_outline_counts_only_unreturned_words` | pass | Returned plus outline equals 115 on every chunk at chunk 12/20/37/60. |
| B-32 (5) FindOpts default k 3; k 0 means the default (D-221) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_5_find_k_default_is_three_and_zero_means_default` | pass | k=0, k=3 and `..Default::default()` give the same hits. |
| B-32 (6) a headings-only chunk fill stays within max_tokens (D-222) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_6_headings_only_fill_stays_within_max_tokens` | pass | max_tokens 1..=80 at chunk_tokens 0/40/400. Every read stays within what is left. |
| B-32 (7) a page change keeps the max_tokens budget (D-220) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_7_page_change_keeps_the_max_tokens_budget` | pass | The sequence spends at most 100 and ends `truncated: max_tokens 100 reached`. |
| D-219 one escaping rule for reader and skeleton | `cargo test -p sb-sheet --test acceptance_t10_b32 -- b32_1 b32_2` | pass | Covered by the three B-32 (1)/(2) tests above. |
| D-220 explicit region wins over the cursor; the budget survives a page change | `cargo test -p sb-sheet --test acceptance_t10_b32 -- b32_3 b32_7` | pass | Covered by B-32 (3) and (7). |
| D-221 FindOpts Default k 3, k 0 as 3 | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_5` | pass | Same test as B-32 (5). |
| D-222 the headings-only fill respects max_tokens | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_6` | pass | Same test as B-32 (6). |
| D-223 code keeps `[` `]`, only `](` becomes `)(` | `cargo test -p sb-sheet --test acceptance_t10_b32 d223_code_keeps_brackets_and_breaks_only_the_link_shape` | pass | `#[tokio::main]` and `a[i] + b[0]` stay verbatim. |
| D-224 a region switch keeps the cursor's used | `cargo test -p sb-sheet --test acceptance_t10_b32 d224_region_switch_keeps_the_max_tokens_budget` | pass | The sequence spends at most 40. |
| D-225 body lines cannot pose as reader lines | `cargo test -p sb-sheet --test acceptance_t10_b32 d225_body_lines_cannot_pose_as_reader_lines` | pass | Nine posers get a leading backslash. |
| D-225 skeleton and sheet names get link_text | `cargo test -p sb-sheet --test acceptance_t10_b32 d225_skeleton_and_sheet_names_get_link_text` | pass | No `[` or `](` in the skeleton, and no `](` in sheet or find_lines. |
| B-201 P5 title and URL lines cannot pose as skeleton lines | `cargo test -p sb-sheet --test acceptance_t10_b32 b201_p5_title_and_url_cannot_pose_as_skeleton_lines` | pass | Eight posing titles get a backslash. |
| B-201 P4 a tiny max_tokens remainder never overshoots | `cargo test -p sb-sheet --test acceptance_t10_b32 b201_p4_tiny_remainder_never_overshoots_max_tokens` | pass | The sequence spends at most 5. |
| D-226 (1) the code fence is at most 10 backticks whatever the page holds | `cargo test -p sb-sheet --test acceptance_t10_d226 d226_1_code_fence_is_at_most_ten_backticks` | pass | A 2000 run, a split 12 run and a 9 run give fences of at most 10, balanced at chunk 4000/40/12/5. A 40 KB block reads in 3 chunks or fewer. |
| D-226 (1) W-5b a password value holding a 12-backtick run never shows in code | `cargo test -p sb-sheet --test acceptance_t10_d226 d226_1_password_value_with_a_long_backtick_run_never_shows_in_code` | pass | Neither the value nor its cut-to-9 spelling appears, even with whitespace removed. |
| D-226 (1)+(2) fixed overhead never pushes a read past max_tokens (sweep per 185ad00) | `cargo test -p sb-sheet --test acceptance_t10_d226 d226_2_overhead_never_pushes_a_read_past_max_tokens` | pass | 8 overhead pages. max_tokens 1..=60 at chunk 0/20/400, and 18 values (1..=12, 16, 23, 31, 42, 59, 60) at chunk 1/3: the test asserts exactly 8 x (180 + 36) cases. Every read stays within what is left. Every sequence ends truncated or reads the whole page, as a gap-free prefix. |
| D-226 (2) a stopped read is truncated with a cursor; the same max_tokens ends, a larger one continues | `cargo test -p sb-sheet --test acceptance_t10_d226 d226_2_stopped_read_has_a_cursor_and_a_larger_budget_continues` | pass | Some case stops (asserted > 0). The continuation is gap-free. |
| D-226 (2) as amended by D-227: under max_tokens a piece holds a whole word when it fits (coder-amended) | `cargo test -p sb-sheet --test acceptance_t10_d226 d226_2_a_read_under_max_tokens_never_takes_less_than_a_word` | pass | Passes and asserts partials > 0. In the mutation check it goes red when D-227 is reverted (M1). It stays green when every word may be cut (M2, word_cap 0), because the amendment allows any partial word in the chain's last read that fills what is left. That gap is closed by the D-228 row below. |
| D-226 (2) without max_tokens an empty chunk still takes one piece | `cargo test -p sb-sheet --test acceptance_t10_d226 d226_2_no_max_tokens_an_empty_chunk_still_takes_a_piece` | pass | 8 pages at chunk_tokens 1..=6. Every read is non-empty and the chain covers the page. |
| Termination: following cursors with a fixed max_tokens always ends (shrunk by d2cf21b) | `cargo test -p sb-sheet --test acceptance_t10_d226 d226_cursor_chain_with_fixed_max_tokens_always_terminates` | pass | 384 chains: 8 pages x chunk 0/1/5/400 x max_tokens 1..=8, 11, 19, 33, 60. Each ends within m+2 reads with no cursor on the last, and stopped reads occur (asserted > 0). It still goes red when the stop cursor's used is not raised to max_tokens (M12: `cursor chain did not end within 4 reads`). |
| D-226 (3) the outline label is the scrubbed body heading with link forms reduced to labels | `cargo test -p sb-sheet --test acceptance_t10_d226 d226_3_outline_label_is_the_scrubbed_body_heading_without_link_syntax` | pass | The six expected labels, with no `](` and no value. |
| D-226 (4) a title or URL cannot pose as the `tools for this site` line | `cargo test -p sb-sheet --test acceptance_t10_d226 d226_4_title_and_url_cannot_pose_as_the_site_tools_line` | pass | Posers get exactly one backslash. The real line is the only one. |
| D-227 a long first word under max_tokens: non-empty reads, budget spent, gap-free prefix | `cargo test -p sb-sheet --test acceptance_t10_d227 d227_long_first_word_under_max_tokens_reads_non_empty_and_chain_is_a_gap_free_prefix` | pass | 7 pages with a 6000-char first word: first, after an intro, after a heading, in a list item, in a quote, in a code line, and multibyte (2/3-byte chars). Sweeps: chunk 0 at max_tokens least, least+1, 7, 100, 500, 1000, 1499, and chunk 50 at least, least+1, 7, 100, 333. Every read of every chain is non-empty, each stays within what is left, and the sequence spends max_tokens to within a token and ends `truncated`. The read stream is a prefix of the page. At max 100000 with chunk 400, the chain over char cuts equals the page. Char cuts occur (asserted > 0). |
| D-227 boundary: the word is whole at its size, cut one token below, and the review-round-5 cursor case | `cargo test -p sb-sheet --test acceptance_t10_d227 d227_the_word_is_whole_at_its_size_and_cut_one_token_below` | pass | max 1500 returns the 6000-char word whole. max 1499 returns exactly its first 5996 chars and `truncated`. Cursor case at chunk 1600 and max 2000: read 1 is the 1060-word intro, and read 2 (410 tokens left, less than the word) is a non-empty char cut. Together they spend exactly 2000. Before D-227 (M1), read 2 was empty. |
| D-228 a word that fits the read's budget is never cut, not even in the last read | `cargo test -p sb-sheet --test acceptance_t10_d227 d228_a_word_that_fits_the_read_budget_is_never_cut` | pass | Chunk 0/10/40 at max_tokens 1..=60. Every partial token is a prefix of a page word w with chars(w) + 8 > min(chunk, what is left) x 4. Partials occur (asserted > 0). This row closes the M2 gap: it goes red with word_cap 0. |
| D-227 outline cap: 20 entries, labels at most 80 chars, one `+N more sections, W words` entry | `cargo test -p sb-sheet --test acceptance_t10_d227 d227_outline_cap_entries_labels_and_more_entry` | pass | 60 sections plus `(top)`. An 80-char label stays whole. An 81-char label is cut to 80 ending `...`, as a prefix of the original, and multibyte and 200-char labels are cut on char boundaries. Entry 20 is `+41 more sections` with 205 words, and the markdown line is `- +41 more sections, 205 words` right before `truncated:`. Each markdown line matches its entry. On every read of a chunk-40 chain: at most 21 entries, returned plus listed equals 340, and the fold has the right form. Boundaries: 19 sections give no fold, 20 give `+1 more section`, 21 give `+2 more sections`. 3000 sections with 300-char headings: the outline is at most 520 tokens, the whole read at most 610, and the fold is `+2981 more sections`. |
| D-227 a page heading cannot pose as the `+N more` entry | `cargo test -p sb-sheet --test acceptance_t10_d227 d227_a_page_heading_cannot_pose_as_the_more_entry` | pass | Ten posing headings, among them `+5 more sections, 10 words`, `- +3 more ...`, a newline followed by a fake entry, `remaining:`, `truncated: ...`, `(top)`, and levels 0, 9 and none. A `remaining:` body paragraph is also on the page. With 10 or 40 sections, at chunk 0/10 and max 400: there is at most one `remaining:` header line. Every outline line before index 20 starts `- #` or `- (top) (`, and no label holds a newline. The only `- +` line is the real fold, as the last line, and there is none without a fold. |
| D-227 W-5b a password value never appears in `remaining:` | `cargo test -p sb-sheet --test acceptance_t10_d227 d227_password_value_never_appears_in_remaining` | pass | The value is placed across the 80-char cut (whole, over two spans, over three spans, as link text of the password field), in a folded section, and across two adjacent headings. At chunk 10/30/4000, with and without max 60: neither the value nor the squeezed canary is in the `remaining:` block, an entry, the read or the cursor. No prefix of 8 or more chars of the value shows in a capped label. |
| D-227 the label is scrubbed before it is cut | `cargo test -p sb-sheet --test acceptance_t10_d227 d227_label_is_scrubbed_before_the_cut` | pass | The value straddles the cut and is joined only by outline_text (plain text ending `(` plus a link's text). The label shows `***` and no prefix of the value. This is the only test that reddens M7. |
| D-227 mutation check: each D-227 path goes red when broken, then restored | `python target/scratch/mutate_t10r6.py <log> [ids]`: each mutation is applied to reader.rs, then `cargo test -q -p sb-sheet --test <bin> <filter>`, then reader.rs is restored. Afterwards `git status --short` and `git diff --stat` were checked | pass | 12 mutations, each red on at least one test. M1 word_cap back to chunk_cap: long-first, the-word and d226_2_a_read red. M2 word_cap 0: d228 red (d226_2_a_read green, gap noted). M3 no entry cap: outline and poser red. M4 OUTLINE_MAX 21: outline red. M5 no label cap: outline red. M6 OUTLINE_LABEL_MAX 81: outline red. M7 cut before the scrub: label red (d227_password green, because the body is scrubbed before the label is built, so plain-text values cannot reach a label). M8 fold line rendered like an entry: outline red. M9 fold words 0: outline red. M10 fold always plural: outline red. M11 label without hashes: poser red. M12 stop cursor used not raised: termination red. After the runs, `git status --short` shows only the new test file and `git diff --stat` is empty. |
| I-5/I-6 contract paths and signatures (8ca6ec8) | `cargo test -p sb-sheet --test contract_paths` | pass | 1 passed (`i6_signatures_hold`). |
| CLI skeleton example | `cargo run -p sb-sheet --example skeleton -- crates/sb-sheet/examples/register.index.json` | pass | Exit 0. Output includes `17 elements: 5 links, 4 buttons, 6 fields`, `! CHALLENGE #11 iframe "reCAPTCHA"`, `! OVERLAY #17 region "Cookie consent"` and `main: form`. |
| clippy | `cargo clippy -p sb-sheet --all-targets -j 2 -- -D warnings` | pass | Exit 0, no warnings, the four acceptance files included. |
| fmt | `cargo fmt -p sb-sheet --check` and `cargo fmt --all -- --check` | pass | Both exit 0. |
| cargo test -p sb-sheet all pass; the only ignored test is the red-first W-5b finding | `cargo test -p sb-sheet --no-fail-fast -j 2 -- --nocapture` | pass | Exit 0: 163 passed, 0 failed, 1 ignored (`w5b_body_never_shows_a_value_joined_across_a_link_form`, recorded as fail above). Per binary: unit 83, acceptance_t10 20, acceptance_t10_b32 16, acceptance_t10_d226 9 (6.6 s), acceptance_t10_d227 7+1 ignored (3.4 s), contract_paths 1, find_register 8, read_article 11, skeleton_sheet 8. |
| cargo test --workspace | `cargo test --workspace --no-fail-fast -j 2` | pass | Exit 0. 75 result lines (61 test binaries and 14 doctest runs): 763 passed, 0 failed, 6 ignored. The ignored ones are this round's red-first W-5b test, three red-first T-08 round-3 harness tests (`d124_1_*`) and two child-process helpers (`d119_7_diag_child`, `file_asker_stderr_child`). The first workspace attempt at the default -j ran rustc out of memory while a reviewer built in the same worktree. The second hit LNK1285 on two corrupt PDBs left behind, which were deleted. The third run, recorded here, is clean. |

## Failures

- W5-b round-6 finding (`w5b_body_never_shows_a_value_joined_across_a_link_form`, run with `--ignored`).
  The body scrub runs on rendered units. Link forms keep `[` `](#id)` around their text, and
  needs_space() puts no space between a link form and a neighbouring `(`, `"`, `'` before it or
  `.,;:!?)'"` after it. So a value with punctuation at the split point, displayed partly as plain text
  and partly as a link's text, reaches the model as a markdown spelling that renders the whole value.
  The outline is already safe (outline_text plus ctx.scrub, D-226 (3)). What is missing: the same check
  in the body, that is, scrub the link-reduced spelling of each unit and replace the spans involved with
  `***` on a hit, or always separate a link form from plain text. The test needs no change once the
  body is fixed. Remove its `#[ignore]` then.

## Known open edges (not rows of the table)

- The coder-amended `d226_2_a_read_under_max_tokens_never_takes_less_than_a_word` is looser than D-228.
  It accepts any partial word in the last read that fills the remainder, and does not check that the
  word was longer than the read's budget. It passes, and the new D-228 test covers the gap.
  Tightening it is optional.
- The round-5 D-226 mutation set (10 mutations) was not repeated in full. M12 (stop cursor not spent)
  and M1 against d226_2 were re-run. All D-226 tests still pass.
- Seen, not a defect against any T-10 item: under a small max_tokens, continuation reads can repeat
  `chunk 2 of 2`. Chunk numbering follows the canonical chunk_tokens plan.
- B-202 (table header separator wider than the chunk) is deferred by D-227. It is not a row.

## Out of scope for T-10 (need sb-driver or a live browser; not rows of the table)

- docs/05 acceptance 1 (identity across walks, `gone`), 2 (actionability, covered/disabled, 5 s cap),
  3 (consequences: new tab, dialog, download, no-op), 4 (frame registry), 5 (settle on a late SPA),
  8 (media frames and contact sheet), 9 (bench: 13/30 web, no-op acts, vault leaks, captcha attempts).
- The live-index halves of docs/05 acceptance 6 and 7 (find and read on the index produced by
  `__sb.index()` / `__sb.text()` in a real browser).
- The live half of docs/04 acceptance (`fetch("https://docs.rs/tokio")` through srv-web) and the other
  docs/04 items (`find("search")` on a fetched page, `api`, `shell`, `history.search`, `gh`).

Verdict: red
