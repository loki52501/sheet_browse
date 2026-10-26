# Acceptance T-10 (tester, round 4): docs/05 B-6 and docs/04 W-5, crate sb-sheet, after the B-32 follow-up

Branch `swarm/sheet` at HEAD 0e0207b. The code under test is the code of 6357f07: the two later
commits (d5a44ed review round 4, 0e0207b D-226) change only `bench/results/review-T-10.md`, a
reviewer handoff and `agent-memory/decisions.yaml` (`git diff --stat 6357f07 HEAD` lists no
`crates/` file). This round covers the B-32 follow-up commits 91f9e46, 0994380 and 6357f07.

Tests:
- `crates/sb-sheet/tests/acceptance_t10.rs`: the 20 tests from rounds 1 to 3, unchanged, all re-run.
- `crates/sb-sheet/tests/acceptance_t10_b32.rs`: new in this round. It holds 16 black-box tests
  through the public API, with fixtures built by hand and independent of the coders' unit tests.
  There is one test for each of B-32 (1) to (7), for D-223, D-224, D-225 and B-201 P5, and a canary
  test over the new escapes. A new W-5b test covers a leak found in this round. One test is
  `#[ignore]`d. It documents the B-201 P4 edge that supreme deferred, and it is not a row of this
  table (see Known open edges).

Canary: `SB_VAULT_CANARY` was set for each full run to a fresh unique string (`sbcanary-T10r4-<ns>-<pid>`,
then `sbcanary-T10r4b-<ns>-<pid>` for the final run). It was planted as `FieldValue::Text` on password
rows (`input_type=password` and `autocomplete=current-password`). It also appeared inside values
holding `](`, three backticks and a leading `remaining:`. It was placed in code blocks (one value split
over two spans), paragraphs, list items, quotes, h1/h2 headings, link labels and a heading split over
two spans.

| item | command | result | note |
|---|---|---|---|
| A6 find on register.html, k=1 (docs/05 acc. 6, D-54) | `cargo test -p sb-sheet --test acceptance_t10 a6_find_register_four_plus_challenge_and_overlay_in_document_order` | pass | Hits are `#6 #7 #10 #11 #12 #17` in document order. #6, #7, #10 and #12 are `Match{part 0..3}` and not weak, #11 is `Challenge` and #17 is `Overlay`. find_lines gives the same order. The result is the same with and without blocks. |
| A6 hidden promo never surfaces, newsletter ranks below signup (k=3, D-202) | `cargo test -p sb-sheet --test acceptance_t10 a6_never_hidden_promo_and_newsletter_ranks_below_signup` | pass | #4 and #5 are absent. Output is in document order. |
| A6 document order is index (walk) order, not id order | `cargo test -p sb-sheet --test acceptance_t10 a6_document_order_follows_index_order_not_id` | pass | Output is `#6 #99 #10 #11 #12 #17`. |
| A6 respelled query keeps the four (D-217) | `cargo test -p sb-sheet --test acceptance_t10 r3_a6_respelled_query_keeps_the_four_and_document_order` | pass | `email`, `e-mail` and the full four-part query at k=1 all give `#6 #7 #10 #11 #12 #17`. |
| A6/rule 3: a hidden-flagged CHALLENGE is still reported (D-215) | `cargo test -p sb-sheet --test acceptance_t10 r2_hidden_challenge_still_reported_by_find_and_skeleton` | pass | #11 is still `Challenge` in find. The skeleton still has `! CHALLENGE #11`. |
| A7-pure article markdown without nav (docs/05 acc. 7) | `cargo test -p sb-sheet --test acceptance_t10 a7_article_markdown_without_nav` | pass | 1 chunk. Main markers appear once each. No chrome text or chrome `(#id)` links. Links, buttons, the list and the table are present. |
| A7-pure chunk cursors: no repeats, no gaps | `cargo test -p sb-sheet --test acceptance_t10 a7_chunk_cursors_no_repeats_no_gaps` | pass | Checked at chunk_tokens 60, 120 and 333. The cursor chain matches `chunk i of N`. The 400 split words each appear once and in order. |
| A7-pure D-211/D-216: no chunk is only a heading or ends on one | `cargo test -p sb-sheet --test acceptance_t10 a7_no_chunk_ends_on_a_heading_when_content_follows` | pass | Checked at chunk_tokens 60, 120 and 333. |
| W5-a first heading is the title, no nav links, `remaining:` lists sections, cursor continues (docs/04 acc., pure half) | `cargo test -p sb-sheet --test acceptance_t10 w5a_first_heading_title_remaining_lists_sections_cursor_continues` | pass | The first heading is `# Tokio Runtime Guide`. `remaining:` has word counts. No body line repeats. The last chunk has no cursor. |
| W5-b password value never emitted by read, sheet, sheet_line, find_lines, skeleton (canary) | `SB_VAULT_CANARY=<unique> cargo test -p sb-sheet --no-fail-fast -- --nocapture > log`, then `grep -c -F "$SB_VAULT_CANARY"` on the logs, `grep -rlF` on `runs/`, the worktree and target/debug/deps, and `git grep -lF` | pass | `w5b_password_value_never_emitted` and `r2_w5b_autocomplete_password_span_and_scrub_before_chunking` pass. Grep hits for the contiguous canary: sb-sheet log 0, workspace log 0, clippy/fmt/example output 0, `runs/` absent, worktree 0, target/debug/deps 0, tracked tree 0. There is no `.claude/state/leaks.log`. The ledger table and the jsonl mirror do not apply, because sb-sheet is pure and writes neither. A split-value leak in the outline is the separate row below. |
| W5-b escaped spellings of password values (D-218) | `cargo test -p sb-sheet --test acceptance_t10 r3_w5b_escaped_password_spellings_never_emitted` | pass | Values with a pipe, brackets and quotes are absent from every output. |
| W5-b canary through the B-32 escapes (D-219, D-223, D-225) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_canary_never_leaks_through_the_new_escapes` | pass | Three password values (`<c>](q`, `<c>` + three backticks + `z`, `remaining: <c>`) are planted in code (also split over spans as `<c>]` + `(q`), headings, paragraphs, a list item, a quote and a link label. The canary is absent from read (3 regions x chunk 4000/30/10, plus all chunks concatenated, outline and cursor), skeleton, sheet, sheet_line and find_lines. |
| W5-b outline never shows a password value split across heading spans (found this round; reviewer D-226 (3)) | `cargo test -p sb-sheet --test acceptance_t10_b32 w5b_outline_never_shows_a_password_split_across_heading_spans` | fail | `panicked at acceptance_t10_b32.rs:1118: chunk 10: password value (space inserted) in outline entry`. The body renders the heading as `## Key *** end` (scrubbed). The `remaining:` outline label comes from reader.rs `plain_text`, which joins spans with a space. So a value split over two heading spans shows as `## Key <first half> <second half> end`. The scrub never matches it, and the model gets the value with one space inserted. A contiguous canary grep cannot see this. Missing: the outline label must be built with the body's span joining (`inline`) and scrub, which is D-226 (3) and not yet implemented. |
| W5-c CHALLENGE frame content never emitted by read | `cargo test -p sb-sheet --test acceptance_t10 w5c_challenge_frame_content_never_emitted_by_read` | pass | Frames 1 and 2 are absent across regions and chunk sizes. Main text is still read. |
| W5-c spans into the challenge frame, sealed regions (D-218) | `cargo test -p sb-sheet --test acceptance_t10 r3_w5c_spans_into_challenge_frame_and_sealed_regions` | pass | No sealed-frame text or `#f1.`/`#f2.` ref appears. Sealed regions answer `no region`. |
| W5-c extra: challenge frame content not in sheet, find, skeleton | `cargo test -p sb-sheet --test acceptance_t10 w5c_extra_challenge_frame_content_not_in_sheet_find_skeleton` | pass | find has no hit outside frame 0. No marker appears in sheet, find_lines or skeleton. |
| W5-c extra: OPAQUE-hosted frame content never emitted (B-5, D-216) | `cargo test -p sb-sheet --test acceptance_t10 r2_w5c_opaque_frame_content_never_emitted` | pass | Frame 3 is absent from read, sheet, find, find_lines and skeleton. |
| B61 skeleton of register: 80-150 tokens (D-200), title, URL, both forms with field count and submit, CHALLENGE, OVERLAY, site_tools | `cargo test -p sb-sheet --test acceptance_t10 b61_skeleton_register_budget_and_content` | pass | The token estimate is within 80..=150 with and without site_tools. Both form lines, `! CHALLENGE #11` and `! OVERLAY #17` are present. There is no em dash. |
| B61 content hint uses only a frame-0 main (D-218) | `cargo test -p sb-sheet --test acceptance_t10 r3_b61_skeleton_content_hint_ignores_main_inside_iframe` | pass | An iframe main gives no `main:` hint. |
| B63 sheet(region) by landmark, form id, heading; document order | `cargo test -p sb-sheet --test acceptance_t10 b63_sheet_region_landmark_form_heading` | pass | nav, footer, region, f1, f2, heading and `## heading` all work. The whole page lists 19 rows once. |
| B63 no cap, document order with non-monotonic ids | `cargo test -p sb-sheet --test acceptance_t10 b63_sheet_no_cap_and_document_order` | pass | 600 rows in main, 602 on the page. |
| B-32 (1) plain span text cannot forge a link form (D-219) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_1_plain_span_text_cannot_forge_a_link_form` | pass | Page text `[button "Continue"](#31)`, `[here](#f2.7)` and similar is placed in a paragraph, a list item, a quote, a heading and table header and data cells, and the link label `Read ] [more`. Across chunk 4000/30/12, the output has exactly 2 `](#` and 2 `[`: the real `[button "Delete"](#31)` and `[Read ) (more](#32)`. Forged text renders as `(button "Continue")(#31)`. |
| B-32 (1) code content cannot close its fence; no 3-backtick run outside code (D-219) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_1_code_block_content_cannot_close_its_fence` | pass | The body's longest run is 5, so the fence is 6 backticks and the body is kept verbatim. A CommonMark fence simulator finds exactly one code body, equal to the source. A short body gets the minimum fence of 3. Outside code, paragraph, list, heading and split-span runs are cut to two backticks. Split at chunk 8/12/20, every chunk's fences balance and no line of 3+ backticks lies outside a fence. |
| B-32 (2) skeleton h1/h2 follow the reader's span rules (D-219) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_2_skeleton_headings_follow_the_reader_span_rules` | pass | A heading linking the password field shows `[input "Password"](#7)`, never the span text. A heading linking the CHALLENGE row shows only `[iframe "reCAPTCHA"](#11)`. Plain text renders as `(button "Pay")(#12)` with two backticks. A heading holding the canary value renders as `Hello ***`. The real link stays `[the docs](#13)`, and there are exactly 3 link forms. |
| B-32 (3) an explicit region that differs from the cursor's wins (D-220) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_3_explicit_region_beats_the_cursor_region` | pass | A cursor from region Alpha with region `Beta` gives chunk 1 and the note `region "Beta" is not the cursor's region, restarted at chunk 1`. The body starts `## Beta`, and its cursor chain stays in Beta. `Alpha`, `  ALPHA ` or no region continue at chunk 2 with no note. |
| B-32 (4) `remaining:` word counts and only words not yet returned | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_4_remaining_outline_counts_only_unreturned_words` | pass | At chunk 12/20/37/60, on every chunk, the words returned so far plus the outline sum equal 115, and the trailer prints exactly the outline. The exact start outline is `(top)` 9 minus returned, One 45, Two 40 (two paragraphs) and Three 21. |
| B-32 (5) FindOpts default k 3; k 0 means the default (D-221) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_5_find_k_default_is_three_and_zero_means_default` | pass | `FindOpts::default()` is `k 3` with an empty query. On register, k=0, k=3 and `..Default::default()` give identical hits and find_lines for three queries, and k=0 keeps its Match rows. |
| B-32 (6) a headings-only chunk fill stays within max_tokens (D-222) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_6_headings_only_fill_stays_within_max_tokens` | pass | Two pages (four headings, and one 60-char h1), with max_tokens 1..=80 and chunk_tokens 0/40/400. The first read stays within max_tokens. A headings-only first chunk is filled from the paragraph once max_tokens can hold it (12 and 18). Every continuation read stays within what is left, and the sequence ends `truncated`. One case is exempt: a remainder of 4 chars or less, which is B-201 P4 (see Known open edges). |
| B-32 (7) a page change keeps the max_tokens budget (D-220) | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_7_page_change_keeps_the_max_tokens_budget` | pass | With max_tokens 100 and chunk 40, the first read spends 21..40. After the page changes, the read notes `page changed since the cursor` and starts at `b000`. The whole sequence spends at most 100 and ends `truncated: max_tokens 100 reached`. |
| D-219 one escaping rule for reader and skeleton (span link_text, backtick cut, longer fence) | `cargo test -p sb-sheet --test acceptance_t10_b32 -- b32_1 b32_2` | pass | Covered by the three B-32 (1)/(2) tests above (3 passed). |
| D-220 explicit region wins over the cursor; the budget survives a page change | `cargo test -p sb-sheet --test acceptance_t10_b32 -- b32_3 b32_7` | pass | Covered by the B-32 (3) and (7) tests above (2 passed). |
| D-221 FindOpts Default k 3, k 0 as 3 | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_5` | pass | Same test as B-32 (5). |
| D-222 the headings-only fill respects max_tokens | `cargo test -p sb-sheet --test acceptance_t10_b32 b32_6` | pass | Same test as B-32 (6). |
| D-223 code keeps `[` `]`, only `](` becomes `)(` | `cargo test -p sb-sheet --test acceptance_t10_b32 d223_code_keeps_brackets_and_breaks_only_the_link_shape` | pass | `#[tokio::main]` and `a[i] + b[0]` are verbatim. `// [click here)(#3)` is broken. Two spans `[y]` + `(#8)` render as `[y)(#8)`. The real link `[docs](#3)` in code is the only `](#`. Outside code, `a(i)` and `(z)(#8)` get the full swap. |
| D-224 a region switch keeps the cursor's used | `cargo test -p sb-sheet --test acceptance_t10_b32 d224_region_switch_keeps_the_max_tokens_budget` | pass | With max_tokens 40, Alpha spends 11..20. Switching to Beta restarts at `## Beta`. The whole sequence spends at most 40 and ends `truncated: max_tokens 40 reached`. |
| D-225 body lines cannot pose as reader lines | `cargo test -p sb-sheet --test acceptance_t10_b32 d225_body_lines_cannot_pose_as_reader_lines` | pass | Nine posers get a leading backslash: `remaining:`, `Truncated:`, `chunk 2 of 9`, `NOTE:`, page changed, cursor not understood, `region "`, `(no readable text)` and `(nothing left to read)`. `chunky bacon` and `chunk of text` are untouched. At chunk 4000/9/15/23/40, which includes continuation pieces of a paragraph whose sentences start like reader lines, no body line poses as a reader line. |
| D-225 skeleton title, form headings, submit and warning names, sheet lines get link_text | `cargo test -p sb-sheet --test acceptance_t10_b32 d225_skeleton_and_sheet_names_get_link_text` | pass | The skeleton has no `[` and no `](`. Expected lines are present: title `Pay (button 'x')(#3)`, `form f1 "Sign (link 'y')(#4) up"`, `submit "Go (button 'w')(#6)"`, `! CHALLENGE #7 iframe "(iframe 'c')(#7)"` and `! OVERLAY #8 ...`. sheet, every sheet_line (names, text values, options) and find_lines have no `](`. |
| B-201 P5 title and URL lines cannot pose as skeleton lines (6357f07) | `cargo test -p sb-sheet --test acceptance_t10_b32 b201_p5_title_and_url_cannot_pose_as_skeleton_lines` | pass | Eight titles get a leading backslash, and no `! ` line appears: `! CHALLENGE`, `landmarks:`, `12 elements:`, `h1`, `form f9`, `+3 more forms`, `main:` and an already escaped title. An ordinary title is left alone. A URL with `[a](#3)` has no `](`. A URL starting `! OVERLAY` makes no `! ` line. |
| Mutation spot-check: the B-32 tests go red when the code path is broken | Edit the source, run `cargo test -p sb-sheet --test acceptance_t10_b32 <test> -- --exact`, then `git checkout -- <file>` | pass | 16 mutations, each turning its test red: B-32 (1) unescaped plain span, fixed 3-backtick fence, and no backtick cut outside code; (2) raw heading spans in skeleton; (3) cursor overrides region (also reddens D-224); (5) k=0 not defaulted, and Default k=1; (6) floor not capped by max_tokens; (7) page change refills used; D-224 region switch refills used; D-223 code without break_link_shape, and code with full link_text; D-225 no body backslash, and clean_name without link_text; B-201 P5 escape_lead disabled. One first try missed (only the trailing code push) and was redone on both pushes. After each mutation, `git status` showed only the new test file: no break committed. |
| CLI skeleton example | `cargo run -p sb-sheet --example skeleton -- crates/sb-sheet/examples/register.index.json` | pass | Exit 0. Prints 10 lines: title, URL, landmarks, f1 and f2, headings, `17 elements: 5 links, 4 buttons, 6 fields`, `! CHALLENGE #11 iframe "reCAPTCHA"`, `! OVERLAY #17 region "Cookie consent"` and `main: form`. |
| clippy | `cargo clippy -p sb-sheet --all-targets -- -D warnings` | pass | Exit 0, no warnings. This includes both acceptance files. |
| fmt | `cargo fmt -p sb-sheet --check` | pass | Exit 0. |
| cargo test -p sb-sheet all pass | `cargo test -p sb-sheet --no-fail-fast -- --nocapture` | fail | Exit 101. Unit 74/74, acceptance_t10 20/20, acceptance_t10_b32 14 passed, 1 failed (`w5b_outline_never_shows_a_password_split_across_heading_spans`), 1 ignored (B-201 P4, not a row). find_register 8/8, read_article 11/11, skeleton_sheet 8/8, doctests 0. |
| cargo test --workspace | `cargo test --workspace --no-fail-fast` | fail | Exit 101. 37 test binaries, 268 passed, 1 failed (the same W5-b outline test), 1 ignored (B-201 P4). No other crate fails (toolstore lib 95, accept_t11 29 and the rest are ok). |

## Failures

- `w5b_outline_never_shows_a_password_split_across_heading_spans`
  (crates/sb-sheet/tests/acceptance_t10_b32.rs): this is a W-5b / hard rule 2 leak. A password
  value split over two heading spans is scrubbed in the body (`## Key *** end`). It is not scrubbed
  in the `remaining:` outline, which shows it with one space inserted. The cause is reader.rs
  `render_block`: the label comes from `plain_text`, which joins spans with `' '`, while the body
  uses `inline`, which joins them without one. The scrub matches only the unspaced spellings. The
  fix needs an implementation change: build the label from the same joined, escaped and scrubbed
  text as the heading body. The reviewer filed this as D-226 (3), which is not implemented yet.
  Before this round the canary grep could not see the leak, because the canary never appears as one
  contiguous string.
- `cargo test -p sb-sheet` and `cargo test --workspace` fail only because of that test.

## Known open edges (not rows of the table)

- B-201 P4 / D-226 (1)(2): when max_tokens leaves fewer chars than a unit's fixed overhead, the read
  overshoots the remainder. Examples are a `### ` prefix with a 1-token remainder, and code fences,
  whose length is set by the page under D-219. `b201_p4_tiny_remainder_never_overshoots_max_tokens`
  (`#[ignore]`) shows it: with max_tokens 5 the sequence spends 6. Run it with
  `cargo test -p sb-sheet --test acceptance_t10_b32 -- --ignored`. Supreme deferred P4 to before
  step 3, and D-226 now schedules the fix. Un-ignore the test when it lands.
- Observed, not a defect against any T-10 item: under a small max_tokens, continuation reads can
  repeat `chunk 2 of 2`. Chunk numbering follows the canonical chunk_tokens plan, not the
  max_tokens-capped reads.

## Out of scope for T-10 (need sb-driver or a live browser; not rows of the table)

- docs/05 acceptance 1 (identity across walks, `gone`), 2 (actionability, covered/disabled, 5 s cap),
  3 (consequences: new tab, dialog, download, no-op), 4 (frame registry), 5 (settle on a late SPA),
  8 (media frames and contact sheet), 9 (bench: 13/30 web, no-op acts, vault leaks, captcha attempts).
- The live-index halves of docs/05 acceptance 6 and 7 (find and read on the index produced by
  `__sb.index()` / `__sb.text()` in a real browser).
- The live half of docs/04 acceptance (`fetch("https://docs.rs/tokio")` through srv-web) and the other
  docs/04 items (`find("search")` on a fetched page, `api`, `shell`, `history.search`, `gh`).
- D-226 (review round 4 should-fix, recorded after the code under test) except its item (3), which
  is the W5-b failure above.

Verdict: red
