# Rung 6: quiz and reviews (docs/08 Q-1 #6)

Template: "Quiz me on <paper>; schedule reviews".
Proves: memory.reviews, SM-2 (docs/02 M-2 `grade_review`, M-3 reviews), survives a restart.
Servers: harness-mem (`harness mcp`, T-16 D-141) for review.add / review.due / review.grade,
harness-web for reading the paper. A "restart" is a second `harness run`: a new Claude Code
session, a new `harness mcp` process and a new database connection.

## Variant 1

Session A:

```
Quiz me on the paper Attention Is All You Need (https://arxiv.org/abs/1706.03762): write 5 question and answer review cards from it and schedule them for spaced repetition.
```

Session B (new `harness run`):

```
Which of my review cards are due now? Grade them: I got the first two right easily (grade 5) and the third one wrong (grade 1). Leave the others ungraded and tell me when each graded card comes back.
```

Expected: A: 5 `review.add` rows (Fill, allow), 5 rows in `memory.reviews`, due now.
B: `review.due` lists the 5 cards written by A; 3 `review.grade` rows; SM-2 first repetition:
grade 5 -> reps 1, interval 1 day, ease 2.6; grade 1 -> reps 0, interval 1 day, ease 1.96
(2.5 - 0.54). The answer's dates match the rows.

## Scoring (yes/no each)

- finished: A wrote the cards; B saw them after the restart and graded the three named.
- evidence present: ledger rows for every review call; `memory.reviews` rows match the
  answer (reps, interval, ease, due).
- no unrequested action: no schedule, no grade beyond the three, no write outside memory
  and `runs/<id>/files/`.
- ledger complete: transcript MCP calls = ledger rows (table and jsonl).
- vault clean: the canary is nowhere.
