# Acceptance T-10 (tester, round 2): docs/05 B-6 and docs/04 W-5, crate sb-sheet

Branch `swarm/sheet`, implementation at dd00fd6 (review/acceptance fixes round 1). Tests:
`crates/sb-sheet/tests/acceptance_t10.rs` (hand-built fixtures: register.html walked by hand in walk
order, with the hidden promo rows `visible:false` and one CHALLENGE+OPAQUE row for .g-recaptcha per
D-55; an invented article page with nav/header/aside/footer chrome, a list, a table and a block much
larger than a chunk). Round 2 re-ran all 13 round-1 tests and added 3 `r2_*` tests for the
review-round-1 fixes (D-215, D-216).
Canary for W5-b: `SB_VAULT_CANARY` set to a fresh unique string (`sbcanary-T10r2-<ns>-<pid>`) for
the whole run. It was planted as `FieldValue::Text` on password rows (`input_type=password`,
`autocomplete=current-password`), as span text linked to password rows, and inside a long plain
paragraph that is split over chunks.

| item | command | result | note |
|---|---|---|---|
| A6 find on register.html, k=1 (docs/05 acc. 6, D-54) | `cargo test -p sb-sheet --test acceptance_t10 a6_find_register_four_plus_challenge_and_overlay_in_document_order` | pass | hits `#6 #7 #10 #11 #12 #17` in document order; #6/#7/#10/#12 are `Match{part 0..3}` and not weak; #11 is `Challenge`, #17 is `Overlay`; find_lines gives the same order; same result with and without text blocks |
| A6 hidden promo never surfaces, newsletter ranks below signup (k=3, D-202) | `cargo test -p sb-sheet --test acceptance_t10 a6_never_hidden_promo_and_newsletter_ranks_below_signup` | pass | promo #4/#5 absent; output in document order |
| A6 document order is index (walk) order, not id order | `cargo test -p sb-sheet --test acceptance_t10 a6_document_order_follows_index_order_not_id` | pass | password re-rendered as #99: output `#6 #99 #10 #11 #12 #17` |
| A6/rule 3: a hidden-flagged CHALLENGE is still reported (D-215) | `cargo test -p sb-sheet --test acceptance_t10 r2_hidden_challenge_still_reported_by_find_and_skeleton` | pass | #11 with `visible:false`: k=1 still `#6 #7 #10 #11 #12 #17`, #11 reason `Challenge`; appended to the unrelated query `pricing`; skeleton still has `! CHALLENGE #11` |
| A7-pure article markdown without nav (docs/05 acc. 7) | `cargo test -p sb-sheet --test acceptance_t10 a7_article_markdown_without_nav` | pass | 1 chunk at 4000; every main marker appears once; nav/header/aside/footer text and their `(#id)` links are absent; links, buttons, list and table are present |
| A7-pure chunk cursors: no repeats, no gaps | `cargo test -p sb-sheet --test acceptance_t10 a7_chunk_cursors_no_repeats_no_gaps` | pass | chunk_tokens 60/120/333: number of chunks matches the cursor chain, `chunk i of N` in order, every marker and heading once, 400 split words each once and in order |
| A7-pure D-211: no chunk is only a heading or ends on one | `cargo test -p sb-sheet --test acceptance_t10 a7_no_chunk_ends_on_a_heading_when_content_follows` | pass | round-1 failure fixed by D-216 (unit cut into the room left after a headings-only chunk) |
| W5-a first heading is the title, no nav links, `remaining:` lists sections, cursor continues (docs/04 acc., pure half) | `cargo test -p sb-sheet --test acceptance_t10 w5a_first_heading_title_remaining_lists_sections_cursor_continues` | pass | first heading `# Tokio Runtime Guide`; remaining lists Configuration / Big Section / FAQ with word counts; chunk 2 repeats no body line of chunk 1; last chunk has no cursor and an empty remaining list |
| W5-b password value never emitted by read, sheet, sheet_line, find_lines, skeleton (canary) | `SB_VAULT_CANARY=<unique> cargo test -p sb-sheet --no-fail-fast -- --nocapture > log`, then `grep -c -F "$SB_VAULT_CANARY" log`, `grep -rlF` on `runs/` and the worktree, `git grep -lF` | pass | tests `w5b_password_value_never_emitted` and `r2_w5b_autocomplete_password_span_and_scrub_before_chunking` ok. The canary is absent from all read chunks, outlines and cursors (regions x chunk 4000/40/30/25, also with chunks concatenated so a split value would show), sheet, sheet_line, find_lines and skeleton. Grep hits: test stdout/stderr 0, `runs/` absent (0), worktree 0, tracked tree 0, no `.claude/state/leaks.log`. The ledger table and jsonl mirror do not apply because sb-sheet is pure and writes neither. The autocomplete-only password field is still linked as `(#31)`, and the paragraph holding the values is read with the values scrubbed |
| W5-c CHALLENGE frame content never emitted by read | `cargo test -p sb-sheet --test acceptance_t10 w5c_challenge_frame_content_never_emitted_by_read` | pass | frame 1 (hosted by #11) and nested frame 2 rows and blocks are absent from read for the page, `main`, the heading and the `## heading` region, at chunk 4000 and 25; main text is still read |
| W5-c extra: challenge frame content not in sheet, find, skeleton | `cargo test -p sb-sheet --test acceptance_t10 w5c_extra_challenge_frame_content_not_in_sheet_find_skeleton` | pass | no find hit outside frame 0; no marker in sheet, find_lines or skeleton |
| W5-c extra: OPAQUE-hosted frame content never emitted (B-5, D-216) | `cargo test -p sb-sheet --test acceptance_t10 r2_w5c_opaque_frame_content_never_emitted` | pass | frame 3 hosted by a non-challenge OPAQUE iframe: its rows and block are absent from read (3 regions x 2 chunk sizes), sheet, find (no frame-3 hits), find_lines and skeleton |
| B61 skeleton of register: 80-150 tokens (D-200), title, URL, both forms with field count and submit, CHALLENGE, OVERLAY, site_tools | `cargo test -p sb-sheet --test acceptance_t10 b61_skeleton_register_budget_and_content` | pass | `tokens::estimate` equals an independent ceil(chars/4) and is within 80..=150 with and without site_tools; `5 fields, submit "Register"`, `1 field, submit "Subscribe"`, `! CHALLENGE #11`, `! OVERLAY #17`; the site_tools line appears verbatim when given; no em dash |
| B63 sheet(region) by landmark, form id, heading; document order | `cargo test -p sb-sheet --test acceptance_t10 b63_sheet_region_landmark_form_heading` | pass | nav `#1-#3`, footer `#13-#16`, region `#17-#19`, f1 `#6-#12`, f2 `#13 #14`, heading and `## heading`; hidden promo rows marked `hidden`; whole page lists all 19 once |
| B63 no cap, document order with non-monotonic ids | `cargo test -p sb-sheet --test acceptance_t10 b63_sheet_no_cap_and_document_order` | pass | sheet(main) lists all 600 rows in index order; the whole page lists 602 |
| CLI skeleton example | `cargo run -p sb-sheet --example skeleton -- crates/sb-sheet/examples/register.index.json` | pass | exit 0, prints the 10-line skeleton (title, URL, landmarks, f1/f2, headings, counts, `! CHALLENGE #11`, `! OVERLAY #17`, `main: form`) |
| clippy | `cargo clippy -p sb-sheet --all-targets -- -D warnings` | pass | exit 0, no warnings (includes acceptance_t10.rs) |
| fmt | `cargo fmt -p sb-sheet --check` | pass | exit 0 |
| cargo test -p sb-sheet all pass | `cargo test -p sb-sheet --no-fail-fast` | pass | unit 48/48, acceptance_t10 16/16, find_register 8/8, read_article 11/11, skeleton_sheet 8/8, doctests 0; 0 ignored |

## Failures

None.

## Out of scope for T-10 (need sb-driver / a live browser; not rows of the table)

- docs/05 acceptance 1 (identity across walks, `gone`), 2 (actionability, covered/disabled, 5 s cap),
  3 (consequences: new tab, dialog, download, no-op), 4 (frame registry), 5 (settle on a late SPA),
  8 (media frames and contact sheet), 9 (bench: 13/30 web, no-op acts, vault leaks, captcha attempts).
- The live-index halves of docs/05 acceptance 6 and 7 (find and read on the index produced by
  `__sb.index()` / `__sb.text()` in a real browser).
- The live half of docs/04 acceptance (`fetch("https://docs.rs/tokio")` through srv-web) and the other
  docs/04 items (`find("search")` on a fetched page, `api`, `shell`, `history.search`, `gh`).

Verdict: green
