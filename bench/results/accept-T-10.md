# Acceptance T-10 (tester, round 3): docs/05 B-6 and docs/04 W-5, crate sb-sheet

Branch `swarm/sheet` at c8e892a (implementation: 4220e1b, review/acceptance fixes round 2, with
`main` merged). Tests: `crates/sb-sheet/tests/acceptance_t10.rs`. The fixtures are built by hand,
independently of the coders' tests and `examples/register.index.json`. The first is
bench/pages/register.html walked by hand in walk order. It has the hidden promo rows marked
`visible:false` and one CHALLENGE+OPAQUE row for .g-recaptcha, per D-55. The second is an
invented article page with nav/header/aside/footer chrome, a list, a table, and a block much
larger than a chunk. Round 3 re-ran all 16 tests from rounds 1 and 2. It added 4 `r3_*` tests
for the review-round-2 fixes: D-217 synonym groups, D-218 escaped password spellings, spans into
sealed frames, sealed regions in sheet, and the skeleton's frame-0 main.

Canary for W5-b: `SB_VAULT_CANARY` was set for the whole run to a fresh unique string,
`sbcanary-T10r3-<ns>-<pid>`. It was planted as `FieldValue::Text` on password rows
(`input_type=password` and `autocomplete=current-password`). It was also planted as span text
linked to password rows, inside a long paragraph split over chunks, and (round 3) inside values
holding `|`, `[ ]` and `"`. Those values were placed in table cells, paragraphs, list items,
quotes, code blocks, and in the text of a span linked to a non-password row.

| item | command | result | note |
|---|---|---|---|
| A6 find on register.html, k=1 (docs/05 acc. 6, D-54) | `cargo test -p sb-sheet --test acceptance_t10 a6_find_register_four_plus_challenge_and_overlay_in_document_order` | pass | Hits are `#6 #7 #10 #11 #12 #17`, in document order. #6, #7, #10 and #12 are `Match{part 0..3}` and not weak. #11 is `Challenge` and #17 is `Overlay`. find_lines gives the same order. The result is the same with and without text blocks. |
| A6 hidden promo never surfaces, newsletter ranks below signup (k=3, D-202) | `cargo test -p sb-sheet --test acceptance_t10 a6_never_hidden_promo_and_newsletter_ranks_below_signup` | pass | Promo rows #4 and #5 are absent. Output is in document order. |
| A6 document order is index (walk) order, not id order | `cargo test -p sb-sheet --test acceptance_t10 a6_document_order_follows_index_order_not_id` | pass | With the password re-rendered as #99, output is `#6 #99 #10 #11 #12 #17`. |
| A6 respelled query keeps the four (D-217) | `cargo test -p sb-sheet --test acceptance_t10 r3_a6_respelled_query_keeps_the_four_and_document_order` | pass | `email`, `e-mail` and `E-mail, Password, agree, Register` at k=1, with and without blocks, all give `#6 #7 #10 #11 #12 #17`. The four are strong `Match{part}`, and the form does not flip. |
| A6/rule 3: a hidden-flagged CHALLENGE is still reported (D-215) | `cargo test -p sb-sheet --test acceptance_t10 r2_hidden_challenge_still_reported_by_find_and_skeleton` | pass | With #11 set to `visible:false`, k=1 still gives `#6 #7 #10 #11 #12 #17` and #11 is `Challenge`. #11 is also appended to `pricing`, and the skeleton still has `! CHALLENGE #11`. |
| A7-pure article markdown without nav (docs/05 acc. 7) | `cargo test -p sb-sheet --test acceptance_t10 a7_article_markdown_without_nav` | pass | 1 chunk at 4000. Every main marker appears once. Nav, header, aside and footer text and their `(#id)` links are absent. Links, buttons, the list and the table are present. |
| A7-pure chunk cursors: no repeats, no gaps | `cargo test -p sb-sheet --test acceptance_t10 a7_chunk_cursors_no_repeats_no_gaps` | pass | At chunk_tokens 60, 120 and 333, the chunk count matches the cursor chain and `chunk i of N` is in order. Every marker and heading appears once, and the 400 split words each appear once and in order. |
| A7-pure D-211/D-216: no chunk is only a heading or ends on one | `cargo test -p sb-sheet --test acceptance_t10 a7_no_chunk_ends_on_a_heading_when_content_follows` | pass | Checked at chunk_tokens 60, 120 and 333. |
| W5-a first heading is the title, no nav links, `remaining:` lists sections, cursor continues (docs/04 acc., pure half) | `cargo test -p sb-sheet --test acceptance_t10 w5a_first_heading_title_remaining_lists_sections_cursor_continues` | pass | The first heading is `# Tokio Runtime Guide`. `remaining:` lists Configuration, Big Section and FAQ with word counts. Chunk 2 repeats no body line of chunk 1. The last chunk has no cursor and an empty remaining list. |
| W5-b password value never emitted by read, sheet, sheet_line, find_lines, skeleton (canary) | `SB_VAULT_CANARY=<unique> cargo test -p sb-sheet --no-fail-fast -- --nocapture > log`, then `grep -c -F "$SB_VAULT_CANARY" log`, `grep -rlF` on `runs/` and the worktree, `git grep -lF` | pass | `w5b_password_value_never_emitted` and `r2_w5b_autocomplete_password_span_and_scrub_before_chunking` pass. The canary is absent from every read chunk, outline and cursor, across regions and chunk sizes 4000/40/30/25, also with chunks concatenated so a split value would show. It is absent from sheet, sheet_line, find_lines and skeleton. Grep hits: test stdout/stderr 0, `runs/` absent (0), worktree 0, tracked tree 0, and there is no `.claude/state/leaks.log`. The ledger table and jsonl mirror do not apply, because sb-sheet is pure and writes neither. |
| W5-b escaped spellings of password values (D-218) | `cargo test -p sb-sheet --test acceptance_t10 r3_w5b_escaped_password_spellings_never_emitted` | pass | Two password values contain a pipe character, square brackets, a double quote and spaced pipes. They are planted in table cells, a paragraph, the text of a span linked to a non-password row, a list item, a quote and a code block. The canary is absent from all read outputs (3 regions x chunk 4000/40/20, also concatenated), sheet, sheet_line, find_lines and skeleton. Every block that held a value was still read. |
| W5-c CHALLENGE frame content never emitted by read | `cargo test -p sb-sheet --test acceptance_t10 w5c_challenge_frame_content_never_emitted_by_read` | pass | Rows and blocks of frame 1 (hosted by #11) and of nested frame 2 are absent from read. Checked for the whole page, `main`, the heading and the `## heading` region, at chunk 4000 and 25. Main text is still read. |
| W5-c spans into the challenge frame, sealed regions (D-218) | `cargo test -p sb-sheet --test acceptance_t10 r3_w5c_spans_into_challenge_frame_and_sealed_regions` | pass | A top-frame paragraph has spans linking `#f1.1` and `#f2.1`. Read never emits their text or any `#f1.`/`#f2.` ref, nor the frame heading or form names. sheet(`Chalheading`/`Chalform`/`dialog`) answers `no region` and its region list does not name them. No other sheet output names them. |
| W5-c extra: challenge frame content not in sheet, find, skeleton | `cargo test -p sb-sheet --test acceptance_t10 w5c_extra_challenge_frame_content_not_in_sheet_find_skeleton` | pass | find has no hit outside frame 0. No marker appears in sheet, find_lines or skeleton. |
| W5-c extra: OPAQUE-hosted frame content never emitted (B-5, D-216) | `cargo test -p sb-sheet --test acceptance_t10 r2_w5c_opaque_frame_content_never_emitted` | pass | Frame 3 is hosted by a non-challenge OPAQUE iframe. Its rows and block are absent from read (3 regions x 2 chunk sizes), sheet, find, find_lines and skeleton. |
| B61 skeleton of register: 80-150 tokens (D-200), title, URL, both forms with field count and submit, CHALLENGE, OVERLAY, site_tools | `cargo test -p sb-sheet --test acceptance_t10 b61_skeleton_register_budget_and_content` | pass | `tokens::estimate` equals an independent ceil(chars/4) and is within 80..=150, with and without site_tools. The skeleton has `5 fields, submit "Register"`, `1 field, submit "Subscribe"`, `! CHALLENGE #11` and `! OVERLAY #17`. The site_tools line appears verbatim when given. There is no em dash. |
| B61 content hint uses only a frame-0 main (D-218) | `cargo test -p sb-sheet --test acceptance_t10 r3_b61_skeleton_content_hint_ignores_main_inside_iframe` | pass | A `main` inside an iframe does not produce a `main:` hint. The `page: article` count includes the 300 top-frame words. |
| B63 sheet(region) by landmark, form id, heading; document order | `cargo test -p sb-sheet --test acceptance_t10 b63_sheet_region_landmark_form_heading` | pass | nav gives `#1-#3`, footer `#13-#16`, region `#17-#19`, f1 `#6-#12` and f2 `#13 #14`. The heading and `## heading` regions work. Hidden promo rows are marked `hidden`. The whole page lists all 19 rows once. |
| B63 no cap, document order with non-monotonic ids | `cargo test -p sb-sheet --test acceptance_t10 b63_sheet_no_cap_and_document_order` | pass | sheet(main) lists all 600 rows in index order. The whole page lists 602. |
| CLI skeleton example | `cargo run -p sb-sheet --example skeleton -- crates/sb-sheet/examples/register.index.json` | pass | Exit 0. Prints the 10-line skeleton: title, URL, landmarks, f1 and f2 with field count and submit, headings, counts, `! CHALLENGE #11 iframe "reCAPTCHA"`, `! OVERLAY #17 region "Cookie consent"` and `main: form`. |
| clippy | `cargo clippy -p sb-sheet --all-targets -- -D warnings` | pass | Exit 0, no warnings. This includes acceptance_t10.rs. |
| fmt | `cargo fmt -p sb-sheet --check` | pass | Exit 0. |
| cargo test -p sb-sheet all pass | `cargo test -p sb-sheet --no-fail-fast` | pass | Unit 57/57, acceptance_t10 20/20, find_register 8/8, read_article 11/11, skeleton_sheet 8/8, doctests 0. 0 ignored. |

## Failures

None.

## Out of scope for T-10 (need sb-driver or a live browser; not rows of the table)

- docs/05 acceptance 1 (identity across walks, `gone`), 2 (actionability, covered/disabled, 5 s cap),
  3 (consequences: new tab, dialog, download, no-op), 4 (frame registry), 5 (settle on a late SPA),
  8 (media frames and contact sheet), 9 (bench: 13/30 web, no-op acts, vault leaks, captcha attempts).
- The live-index halves of docs/05 acceptance 6 and 7 (find and read on the index produced by
  `__sb.index()` / `__sb.text()` in a real browser).
- The live half of docs/04 acceptance (`fetch("https://docs.rs/tokio")` through srv-web) and the other
  docs/04 items (`find("search")` on a fetched page, `api`, `shell`, `history.search`, `gh`).

Verdict: green
