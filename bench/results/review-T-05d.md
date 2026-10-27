# Review T-05d round 1 (D-77, B-151, H-2)

Reviewed `git diff main...HEAD` at 682f7f5 on swarm/core-T05d: `crates/core/src/vault.rs`
(spellings, base64_into, push_base64_spellings, MIN_BASE64_CHARS, 6 unit tests),
`crates/core/tests/vault_accept_t05d.rs` (tester rows 1-4, 6), Cargo.toml/lock, handoff.
Uncommitted tester work in the worktree (row 5, a mutation run) was not reviewed.

Run: `cargo test -p harness-core --lib vault` 38 passed; `--test vault_accept_t05d` 12 passed,
1 ignored (child helper); clippy `-D warnings` clean; rustfmt clean on the HEAD files.
Release micro-bench against a HEAD export: build 0.8 ms (1 entry), 6.9 ms (50 alphanumeric
entries), 71 ms (50 entries with `@!#%&/"`), 207 ms (200 such entries); scrub of 1.2 MB of
base64 noise plus prose takes 9 to 17 ms whatever the entry count. Scrub cost is fine.

Checked and fine:
- Algorithm (vault.rs:1225): `lead = ceil(8k/6)`, `core_end = floor(8(k+n)/6)` and
  `tail_end = ceil(8(k+n)/6)` match the characters that carry only value bits at each of
  the 3 alignments. Std and url alphabets, core, unpadded tail and padded tail forms. Basic
  (`user:` at any length), JWT payload (raw and JSON-escaped), data URL and arbitrary
  offsets with any tail are scrubbed, and only edge characters (at most 4 value bits each)
  survive.
- Zeroizing: `blob` and `enc` are `Zeroizing` and sized up front, so they never reallocate
  (`k + n`, `tail_end + pad`). The slices are copied into `Zeroizing<String>`. No new error,
  Debug, log or panic path: `ValueInLiteral` carries only the name.
- Panics: `chunks(3)` never yields an empty chunk, `idx <= 63`, and the slice bounds are
  guarded by the floor checks. n = 0 cannot panic. The input is vault values, not external
  text.
- Floor: MIN_BASE64_CHARS = 8 applies to every form, with padding not counted. Values of 5
  bytes or less get no base64 spelling. Over-scrub odds for one 8-character pattern matched
  ignoring ASCII case are about 2^-40 per position. That is negligible, even over MB-size
  base64.
- Tests: the r1, r2 and r3 acceptance tests go red if base64 is dropped, if only k = 0 is
  kept, if only one alphabet is kept, or if the JSON-form base64 is dropped. r4 pins the
  floor in both directions. r6 runs end to end through dispatch, serve stdout and stderr,
  the ledger, jsonl and runs/. No assertion message formats a value.

## Blocking

- none

## Should-fix

1. **base64 of a percent-encoded value is not scrubbed** (vault.rs:1126-1134). Only the raw
   and JSON forms of each value get base64 spellings. CDP returns request and response bodies
   base64-encoded (`Network.Request.postDataEntries[].bytes`, `Fetch`/`Network.getResponseBody`
   with `base64Encoded`). A login form POST is `application/x-www-form-urlencoded`, so a `cdp`
   or network reader tool echoing it would carry base64 of `password=P%40ss%21...`.
   I checked this with a probe on the HEAD export: `user=bob&password=<canary>%21%40%23`,
   base64 STANDARD, is **not** scrubbed. Plain `user:<canary>!@#` is. No such consumer exists
   in the code yet, which is why this is not Blocking. Before the browser `cdp` or network
   tools ship (step 3), either add base64 of the form-urlencoded spelling (uppercase hex, `+`
   for space; base64 is case-sensitive over the hex) when it differs from `t`, or record the
   gap in B-152 with that deadline.
2. **Other consumer base64 forms are missing, and the "Not covered" list (vault.rs:1106)
   does not name them.** I confirmed each with the probe:
   - MIME and PEM base64 wrapped at 76 or 64 columns: a value that straddles a CRLF leaks
     whole, because neither half is long enough to match.
   - UTF-16LE base64 (PowerShell `-EncodedCommand`).
   - base64 of a base64 blob.
   - Latin-1 base64 of non-ASCII values. JS `btoa(user + ':' + pass)` encodes code points up
     to U+00FF as Latin-1, not UTF-8, so a non-ASCII password sent through `btoa` (eval or
     forged tools) is not covered. This one is cheap: add a Latin-1 base for non-ASCII values
     whose characters are all U+00FF or below.

   Either fix these or list them as known limits with an owner and deadline (B-152 or a new
   blocker), so T-17's zero-leak claim is scoped honestly.

## Nits

- vault.rs:1181: "from 8 characters on every alignment is covered" is wrong. From 7 bytes
  every alignment has a core (k = 1 gives exactly 8 characters), as the handoff and
  `base64_spellings_respect_the_floor` say. Say bytes, not characters.
- vault.rs:1127 and 1131: `json_escape(t, false)` and `json_escape(t, true)` are computed
  again after `push_spellings` already built them (vault.rs:1142). This is build-time cost
  only. Return or reuse them.
- vault.rs:1191: the stack temporaries `b` and `n` in `base64_into` hold value bytes and are
  not zeroized. This is the same class as the documented library-internal residue, so a
  one-line note is enough.
- vault.rs:18: the reflowed module doc line runs past the file's width.
- Acceptance `assert_edges` (tests/vault_accept_t05d.rs:150) accepts a surviving tail
  character and padding. Only the unit test `scrub_basic_auth_header` pins the tail and padded
  forms exactly. That is acceptable, because the tail character holds at most 4 value bits,
  but a row 3 case that pins exact output for a value ending the blob would make the tail
  form's loss visible in the black-box suite.
- tasks.yaml still has T-05d `status: doing`, and `bench/results/accept-T-05d.md` is not
  committed yet. The gate needs both.

## Verdict

D-77 scope (both alphabets, padded and unpadded, 3 alignments, trimmed edges, 8-character
floor, Basic and JWT tests) is implemented and tested. No rule break. The two should-fix
items are base64 spellings outside D-77's list that consumers can produce. Close them, or
record them with a deadline, before step 3.

Verdict: approve
