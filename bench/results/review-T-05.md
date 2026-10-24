# Review T-05 round 2: core::vault (49cce85, fix round b19c2cc)

Scope: `crates/core/src/vault.rs` at HEAD, `crates/core/tests/vault_scrub_t05_fix.rs`, `crates/core/tests/vault_accept_t05.rs`.
Spec: docs/01 H-2, I-4, D-1, D-101, D-102, D-112, CLAUDE.md rule 2. Handoff: `agent-memory/handoffs/T-05-rust-coder-2.yaml`.

Checks run:
- Tests, with a fresh `SB_VAULT_CANARY`: `cargo test -p harness-core --lib vault::` passed 16/16. `--test vault_scrub_t05_fix` passed 9/9 and `--test vault_accept_t05` passed 9/9. The canary had 0 hits in the combined output.
- Clippy: `cargo clippy -p harness-core --lib --test vault_scrub_t05_fix --test vault_accept_t05 -- -D warnings` is clean. `--all-targets --keep-going` fails only in the tester's untracked, in-progress `tests/vault_accept_t05_r2.rs:766` (`unwrap()` on an `Err` value), which is not part of this review.
- `rustfmt --check` is clean on all three files.
- I also probed the scrubber with a scratch crate outside the repo. Results are quoted below.

## Blocking

- none

## Should fix

- **Stale MSRV allows.** `vault.rs:910-912` and `tests/vault_scrub_t05_fix.rs:397` carry `#[allow(clippy::incompatible_msrv)]`. D-112 made rust-version 1.94, and `File::lock` (1.89) and `io::pipe` (1.87) are both older than that, so the allows are now unnecessary. The comment at `vault.rs:910-911` ("workspace rust-version (1.85). A bump is proposed") is now false. Remove both allows and the comment.
- **The fail-closed scrub fails silently.**
  - When `Scrubber::build` (`vault.rs:503-510`) gets `Matcher::Failed`, `open` (:179) and `set` (:249) still return `Ok`.
  - From then on every global scrub, ledger line and panic message becomes `SCRUB_WITHHELD`, and nothing reports why.
  - Both `open` and `set` already return `VaultError`. Return an error there and keep the marker only as the backstop (`in_memory` can keep the marker).
  - The marker `@vault(?)` (:386) is itself a malformed reference: `vault_refs` and `resolve_into` on text that contains it give `BadReference`. That is acceptable because it is fail-closed, but document it.
- **Short values corrupt scrub output and the literal.** The automaton is ASCII-case-insensitive (:505) with no minimum length (:484-493).
  - Probe: a personal entry `x = "a"` turns `@vault(name)` into `@v@vault(x)ult(n@vault(x)me)`.
  - `st = "ca"` turns `Call the cache in CA` into `@vault(st)ll the @vault(st)che in @vault(st)`.
  - So scrub is not idempotent, and its output can carry malformed refs that G-4's `vault_refs` rejects. Short personal entries such as a middle initial or a state code are realistic.
  - This does not break rule 2, but it costs efficiency because reader output is destroyed. Never match inside an existing `@vault(name)` literal, and use a minimum length or word boundary for short personal entries. The handoff's not_done (d) names the over-scrub but not the literal damage.
- **D-102 text is still stale** (carried over from round 1). `decisions.yaml:144-146` still says "leftmost-longest" and "scrubs the panic message before the previous hook prints it". The code differs in four ways:
  - It uses a Standard overlapping search with span union (:444-478, :503-506).
  - Matching is ASCII-case-insensitive and adds Unicode case bases (:611-620).
  - A failed build fails closed (:386).
  - The hook prints dirty messages itself (:771-805).
  
  Record the amendment, or write a blocker for supreme. `decisions_made: []` in the handoff.
- **No interface_changes entry for the new public items** (carried over from round 1). `default_path` (:87) and `default_key` (:97) are public in an I-4 CONTRACT module, but `blockers.yaml` `interface_changes:` still lists only B-101. Add an entry.
- **Known limits have no tracked owner or deadline.** They are listed only in the handoff and in the `spellings` doc (:607-608, which names only masks and base64). My judgement on each is below. None has to be fixed for T-05, because step 1 has no browser reader and no HTTP sink yet. Each needs a `blockers.yaml` entry with the step it must close before:
  - **Before step 2** (srv-web `api` and the header-sink acceptance) and the Phase 1 baseline "zero vault leaks":
    - **base64/base64url:** fix then. Probe: `Basic dXNlcjpjYW5hcnktN2YzYQ==` survives for `user:canary-7f3a`. The 3 alignments with trimmed edges are enough.
    - **HTML-safe JSON escapes:** a new gap, not in the handoff. Go `encoding/json`, Rails and System.Text.Json write `& < > ' + "`. Probe: `{"a":"canary-7f3a A&B <x>"}` survives. This is a JSON spelling inside D-102's intent and costs one extra spelling per value.
  - **Before step 3** (browser readers):
    - **Input masks:** fix then, with digit-normalised matching for personal entries.
    - **DOM-split, ellipsis-truncated values:** fix then, by scrubbing the reader's joined text. Probe: `canary-7f3…` survives.
    - **Mixed Unicode case:** a new gap. Probe: CSS `text-transform: capitalize` turns `émile ærø` into `Émile Ærø`, which survives because only the all-lower and all-upper bases exist.
  - **Acceptable for Phase 1:**
    - **HTML entities:** readers return decoded text (rule 5), so entities reach scrub only if raw markup is leaked, and rule 5 already forbids that.
    - **Quoted-printable:** mail goes through the browser UI, which shows decoded text.
    - **Punycode:** only IDN hosts are affected.
    - **Unzeroed internals:** memory hygiene, not an output path. They are documented at :22-29.

## Notes

- Round-1 items, re-checked at HEAD:
  - **WHATWG URL sets and encodeURI:** OK. `PCT_SETS` at :561-585 matches the spec sets, which the test writes out independently at `vault_scrub_t05_fix.rs:62-88`.
  - **Case handling:** OK. ASCII-CI is at :505 and the Unicode bases at :613-619.
  - **%25 double encoding:** OK, :590-596 and :649-656.
  - **Debug forms:** OK. `debug_inner`, `escape_debug` and `escape_default` are at :638-640, and a unit test checks `debug_inner` against real `{:?}`.
  - **Span union:** OK. The stack merge plus the `late` fallback is correct for any order (:444-478). Char-boundary safety holds because the case folding is ASCII-only.
  - **Locked set with re-read:** OK.
    - The lock is taken before the identity load (:239-240).
    - An existing file with a missing identity is an error, never a fresh key (:942-944).
    - The race test runs 4 threads × 6 rounds.
  - **Panic hook write:** OK. It uses `let _ = writeln!(stderr().lock())` (:803). The broken-stderr child test asserts exit 101, and `vault_accept_t05.rs:511-559` greps child stdout and stderr for the canary and for `@vault(test)`, so round 1's vacuous panic test is now covered.
  - **Zeroized decrypt buffer:** OK. The buffer has ciphertext length and refuses to grow (:826-839).
- **TOML serializer** (:857-896): correct, with no breakout.
  - Names and values are both written as quoted basic strings.
  - Every character TOML 1.0 forbids raw is escaped: `"`, `\`, U+0000-U+001F and U+007F.
  - A value or name cannot close its string, so it cannot inject or drop an entry or change `kind` (fixed `as_str`).
  - Dotted names are quoted.
  - `set` validates names, and names loaded from the file are escaped on rewrite.
  - Capacity: the fixed text is 39 bytes, under 48, and escapes are at most 6×, so the buffer never reallocates.
  - The round-trip test `vault_scrub_t05_fix.rs:432-462` covers quotes, backslash, all the short escapes, U+0001, DEL, `'''`/`"""` and astral characters.
- **Adjacent spans of two different entries merge under the left name** (:455; the test at `vault_scrub_t05_fix.rs:291-292` pins this). That over-scrubs, which is safe for rule 2. But if the model reuses the scrubbed text as a template, only the left value gets filled. Consider emitting one literal per entry when spans only touch.
- **The global scrubber does not see `set` from another process** (a CLI `vault set` while harnessd runs) until the vault is reopened. That is not a leak path, because the daemon cannot resolve the new name either. T-07 bootstrap should reopen or watch the file.
- **Panic hook cosmetics:** it prints a leading `\n` (:796) and omits the default hook's `RUST_BACKTRACE` note. Harmless. If the hook ran on a thread that holds the `GLOBAL` write guard, it would deadlock; only `Arc` drop runs under that guard, so this is practically unreachable.
- **I-4 fidelity:** every public signature is byte-identical to the wave-0 stub. The only additions are `default_path` and `default_key` (see Should fix).
- **Spec ids:** b19c2cc claims H-2 and D-102, and both are implemented. The locked set is D-101/H-2 behaviour, and D-101 is not named. The fail-closed marker and case-insensitive matching go beyond D-102's text (see Should fix). The round-1 note on 49cce85 carrying T-03/T-04 dependencies still stands for the PR description.
- **Hard rules 1, 3-7:**
  - No keys, `.env` or key-shaped material. `AGE-SECRET-KEY-` appears only as a prefix plus the canary in `vault_accept_t05.rs:381`.
  - There are no sleeps; `set` blocks on the OS lock, not on a timer.
  - There is no challenge, gate or HTML surface here.

## Verdict

Approve. I found no rule break and no missing acceptance test. Every round-1 item is fixed and tested, the TOML writer is injection-safe, and the scrub now fails closed. Before step 2's acceptance:
- Remove the stale MSRV allows.
- Surface the failed scrubber build as an error.
- Protect `@vault(...)` literals from short-value over-scrubbing.
- Record the D-102 amendment and the interface change.
- Open blockers, with deadlines, for base64 and HTML-safe JSON (step 2) and for masks, DOM-split values and mixed Unicode case (step 3).

Verdict: approve
