# Acceptance T-10 (tester, round 1): docs/05 B-6 and docs/04 W-5, crate sb-sheet

Branch `swarm/sheet`, implementation at 98e2f3d. Tests: `crates/sb-sheet/tests/acceptance_t10.rs`
(hand-built fixtures: register.html walked by hand in walk order, with the hidden promo rows
`visible:false` and one CHALLENGE+OPAQUE row for .g-recaptcha per D-55; an invented article page with
nav/header/aside/footer chrome, a list, a table and a block much larger than a chunk).
Canary for W5-b: `SB_VAULT_CANARY` set to a fresh unique string (`sbcanary-T10-<ns>-<pid>`)
for the whole run, planted as `FieldValue::Text` on two password rows (`input_type=password`
and `autocomplete=current-password`) and as span text linked to the password row.

| item | command | result | note |
|---|---|---|---|
| A6 find on register.html, k=1 (docs/05 acc. 6, D-54) | `cargo test -p sb-sheet --test acceptance_t10 a6_find_register_four_plus_challenge_and_overlay_in_document_order` | pass | hits `#6 #7 #10 #11 #12 #17` in document order; #6/#7/#10/#12 are `Match{part 0..3}`, not weak; #11 `Challenge`, #17 `Overlay`; find_lines prints the same order with CHALLENGE/OVERLAY flags; identical with and without text blocks |
| A6 hidden promo never surfaces, newsletter ranks below signup (k=3, D-202) | `cargo test -p sb-sheet --test acceptance_t10 a6_never_hidden_promo_and_newsletter_ranks_below_signup` | pass | promo #4/#5 absent; output still in document order |
| A6 document order is index (walk) order, not id order | `cargo test -p sb-sheet --test acceptance_t10 a6_document_order_follows_index_order_not_id` | pass | password re-rendered as #99: output `#6 #99 #10 #11 #12 #17` |
| A7-pure article markdown without nav (docs/05 acc. 7) | `cargo test -p sb-sheet --test acceptance_t10 a7_article_markdown_without_nav` | pass | 1 chunk at 4000; all 15 main markers once; nav/header/aside/footer text and their `(#id)` links absent; `[tokio spawn](#5)`, `[button "Copy snippet"](#6)`, `- ` list and `\|` table present |
| A7-pure chunk cursors: no repeats, no gaps | `cargo test -p sb-sheet --test acceptance_t10 a7_chunk_cursors_no_repeats_no_gaps` | pass | chunk_tokens 60/120/333: cursor chain length == chunks, `chunk i of N` in order, every marker and every heading once, 400 words of the split block each once and in order, chunks within budget |
| A7-pure D-211: no chunk is only a heading / ends on a heading when content follows | `cargo test -p sb-sheet --test acceptance_t10 a7_no_chunk_ends_on_a_heading_when_content_follows` | fail | `chunk_tokens 60: chunk 5 of 20 has no content, only ["## Big Section"]`; at 120 chunk 3 of 11 is only `## Big Section`. See failure 1 below |
| W5-a first heading is the title, no nav links, `remaining:` lists sections, cursor continues without repeats (docs/04 acc., pure half) | `cargo test -p sb-sheet --test acceptance_t10 w5a_first_heading_title_remaining_lists_sections_cursor_continues` | pass | first heading `# Tokio Runtime Guide`; remaining on chunk 1 = Configuration 48, Big Section 400, FAQ 50 words; chunk 2 repeats no body line of chunk 1; last chunk: cursor None, remaining empty |
| W5-b password value never emitted by read, sheet, sheet_line, find_lines, skeleton (canary) | `SB_VAULT_CANARY=<unique> cargo test -p sb-sheet --no-fail-fast -- --nocapture` then `grep -c -F "$SB_VAULT_CANARY"` on the captured output, `runs/`, `git grep` | pass | test `w5b_password_value_never_emitted` ok: canary in none of read markdown/outline/cursor (3 regions x 2 chunk sizes, all chunks), sheet (4 regions), sheet_line (every row), find_lines (4 queries incl. the canary itself x k 1/3/50), skeleton. Grep: 0 hits in test stdout/stderr, `runs/` does not exist (0), 0 in tracked tree, no `.claude/state/leaks.log`. Ledger table and jsonl mirror: not applicable, sb-sheet is pure and writes neither. Password rows print `filled` |
| W5-c CHALLENGE frame content never emitted by read | `cargo test -p sb-sheet --test acceptance_t10 w5c_challenge_frame_content_never_emitted_by_read` | pass | frame 1 (child of #11) and nested frame 2 rows and text blocks planted inside `main`; absent from read for page, `main`, heading and `## heading` regions, chunk 4000 and 25; surrounding main text still read |
| W5-c extra: challenge frame content not in sheet, find, skeleton | `cargo test -p sb-sheet --test acceptance_t10 w5c_extra_challenge_frame_content_not_in_sheet_find_skeleton` | pass | no find hit outside frame 0; no marker in sheet (page/main/f1), find_lines, skeleton |
| B61 skeleton of register: 80-150 tokens (D-200), title, URL, both forms with field count and submit, CHALLENGE, OVERLAY, site_tools | `cargo test -p sb-sheet --test acceptance_t10 b61_skeleton_register_budget_and_content` | pass | `tokens::estimate` equals independent ceil(chars/4), within 80..=150 with and without site_tools; `form f1 ... 5 fields, submit "Register"`, `form f2 ... 1 field, submit "Subscribe"`, `! CHALLENGE #11`, `! OVERLAY #17`; site_tools line verbatim when given, absent otherwise; hidden promo not named; no em dash |
| B63 sheet(region) by landmark, form id, heading; document order | `cargo test -p sb-sheet --test acceptance_t10 b63_sheet_region_landmark_form_heading` | pass | nav `#1-#3`, footer `#13-#16`, region `#17-#19`, f1 `#6-#12`, f2 `#13 #14`, `Stay in the loop`, `## Create your account`; hidden promo rows listed with `hidden`; whole page lists all 19 once, lines equal `sheet_line` |
| B63 no cap, document order with non-monotonic ids | `cargo test -p sb-sheet --test acceptance_t10 b63_sheet_no_cap_and_document_order` | pass | sheet(main) lists all 600 rows in index order; whole page 602 |
| CLI skeleton example | `cargo run -p sb-sheet --example skeleton -- crates/sb-sheet/examples/register.index.json` | pass | exit 0, prints 10-line skeleton (title, URL, landmarks, f1/f2, headings, counts, CHALLENGE #11, OVERLAY #17, `main: form`); missing file exits 1 with a message |
| clippy | `cargo clippy -p sb-sheet --all-targets -- -D warnings` | pass | exit 0, no warnings (includes acceptance_t10.rs) |
| fmt | `cargo fmt -p sb-sheet --check` | pass | exit 0 |
| cargo test -p sb-sheet all pass | `cargo test -p sb-sheet --no-fail-fast` | fail | unit 37/37, find_register 8/8, read_article 11/11, skeleton_sheet 8/8, acceptance_t10 12/13: `a7_no_chunk_ends_on_a_heading_when_content_follows` FAILED. 0 ignored |

## Failures

1. **D-211 heading-only chunk** (`cargo test -p sb-sheet --test acceptance_t10 a7_no_chunk_ends_on_a_heading_when_content_follows`).
   When a heading is the first piece of a chunk and the next unit (a block bigger than the room
   left) does not fit, `next_chunk` in `crates/sb-sheet/src/reader.rs` breaks at line 760
   (`if !pieces.is_empty() { break; }`). The heading-pop guard at line 802 needs
   `pieces.len() > 1`, so a lone heading becomes a whole chunk with no content (chunk 3 of 11
   at chunk_tokens 120 is just `## Big Section`). That breaks D-211 ("A heading never ends a chunk
   when content follows") and costs the model a wasted turn. What's missing: when the pieces so
   far are only headings and the next unit is too big, split that unit into the room left in the
   current chunk instead of breaking. In other words, handle the "too big" case for a chunk that
   holds only headings, not only for an empty chunk. The cursor offset and `chunk i of N` plan
   would then follow from the same `next_chunk`.

## Out of scope for T-10 (need sb-driver / a live browser; not rows of the table)

- docs/05 acceptance 1 (identity across walks, `gone`), 2 (actionability, covered/disabled, 5 s cap),
  3 (consequences: new tab, dialog, download, no-op), 4 (frame registry), 5 (settle on a late SPA),
  8 (media frames and contact sheet), 9 (bench: 13/30 web, no-op acts, vault leaks, captcha attempts).
- The live-index halves of docs/05 acceptance 6 and 7 (find and read on the index produced by
  `__sb.index()` / `__sb.text()` in a real browser).
- docs/04 acceptance live half (`fetch("https://docs.rs/tokio")` through srv-web) and the other
  docs/04 items (`find("search")` on a fetched page, `api`, `shell`, `history.search`, `gh`).

Verdict: red
