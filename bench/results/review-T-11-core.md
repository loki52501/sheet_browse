# Review T-11 core (commit 955ce52, swarm/store)

Scope: crates/toolstore/src/{lib,header,domain,loader,fingerprint,lifecycle,invariants,archive,tests}.rs, Cargo.toml, tools/ashbyhq.com/list_jobs.js, tools/_generic/read_table.js. runner.rs is out of scope (see review-T-11-runner.md).

Checks run: `cargo test -p toolstore` exit 0 (68 passed). `cargo clippy -p toolstore --all-targets -- -D warnings` exit 0. `cargo fmt -p toolstore --check` exit 0. I also ran a scratch probe (outside the repo) against the crate to confirm the inputs below.

Rules that pass: no keys, no .env, no sleeps, no vault values (only key names via `vault_literal`), no captcha code, no HTML reaches the model. Layering is toolstore -> core, sb-sheet, sb-act (D-38). I-1 `MemoryStore`/`ToolHealth` and I-5 `IndexRow` are used unchanged. The I-7/I-12 signatures match the stubs, and the new surface is recorded as IC-300/IC-301. There is no `unwrap` on external input outside tests.

## Blocking

- **archive.rs:97-118 (`under_root`), :65 (`git_commit`): git pathspec magic lets a commit escape the tools root.** `under_root` accepts any relative path made of `Normal` components, and `:/`, `:(top)x` and `:(glob)**` are all `Normal`. Probe: in a repo with `tools/` as root and an untracked `outside.txt` next to it, `ts.git_commit(&[PathBuf::from(":/")], "x (T-1)")` returns Ok and HEAD contains `outside.txt` and `tools/luma.com/a.js`. To fix, reject any component that starts with `:`, or pass `--literal-pathspecs` (or set `GIT_LITERAL_PATHSPECS=1`) on both `git add` and `git commit`. Add a test for `:/`.
- **archive.rs:16, :21, :31; lifecycle.rs:288, :338, :104, :221: path traversal from `domain`/`name`.** `tool_path`/`archive_path` join raw strings, and `record`, `reforge`, `register_forged`, `archive` and `retire` all build on them. `@name` is checked to be an identifier, but `ToolRunResult.name` and the `reforge(domain, name, ..)` arguments are not. Probes:
  - `tool_path("luma.com", "../../evil")` gives `<root>\luma.com\../../evil.js`.
  - `tool_path("..\\..", "evil")` gives a path outside the root.
  - `folder_for("\\windows")` returns `"\\windows"` (domain.rs:145-173: on Windows `root.join("\\windows").is_dir()` is true), and `tool_path(that, "x")` is `C:\windows\x.js`.
  - `folder_for("..\\..\\x")` returns `".\\x"`, and `folder_for("\\\\server\\share")` passes through unchanged.

  `reforge` writes `new_src` to that path, and `retire`/`archive` rename files there. Only `scan` guards (loader.rs:127). To fix, validate in one place: `name` must match `[A-Za-z0-9_]+`, and `domain` must be `_generic` or a normalized host with no `/`, `\`, `:`, `..`, leading `.`/`_`. Return `StoreError` otherwise, and make `folder_for` never return a non-folder string. Add tests for `../`, `..\`, absolute paths, drive letters, UNC and `_archive`.
- **invariants.rs:29-46, :103-109, :142-159: the G-4/D-29 invariant "cannot add a vault key" is a textual check, and it can be bypassed.** The new body `return \`t#1=@vau\` + \`lt(bank_pin); c#${..}; ?s\`` passes `check_repair(.., Reforge)` (probe: `Ok(())`), yet at run time the program contains `@vault(bank_pin)`. `planner_act_class` never compares the vault refs in the program with the tool's source, so nothing catches it at run time either. To fix, in `planner_act_class` reject a program whose `@vault(k)` refs are not all in `vault_keys(&tool.source)`, and add a test with a concatenated key.
- **lifecycle.rs:287-303, :272-285, :349-360: `ask` (T-5, "until the user confirms") can be left without `confirm()`.** Three paths:
  - (a) `reforge` a second time with the same raised `@gate`. `og` is now the raised gate, so `ng > og` is false, the status becomes `Repaired`, and the user never confirmed.
  - (b) `record(Relocated{..})` on an `Ask` row sets `Repaired` (`if !retired`).
  - (c) `record(FingerprintMiss)` on `Ask` gives `NeedsRepair`, then `record(Ok)` gives `Repaired`.

  User confirmation is permission, and G-4 says learning never changes permission. To fix, keep `Ask` through every outcome and through re-forge until `confirm`, and test each path.

## Should fix

- **invariants.rs:49-51 (`stop_count`): the `?s` count is a substring count over the whole body, including comments and strings.** Moving the stop into a comment passes (`...\`; // ?s` gives Ok), and `'?sort=1'` counts as a stop (old=1, new=1, Ok). The run-time `SbAct` D-50 check is the real backstop, so this is not a hard break. Count only `?s` act tokens inside template or quoted program text, or document that check_repair is advisory and that callers must use `planner_class`. D-307 also says a re-forge may not "fail sb-act's ?s rule", but check_repair never calls `PlannerCheck`.
- **lifecycle.rs:444 (sweep idle clock): `since = last_ok.or(last_fail)` should be the later of the two.** With `last_ok = now-20d` and `last_fail = now-1d` (a `Failed` outcome keeps the row ephemeral), the tool is retired as "unused 14 days" although it was used yesterday.
- **header.rs:128-140 (`match_violation`): backslashes and percent-encoded dots/slashes are not rejected.** `/\evil.com/*` and `/%2e%2e/%2f%2fevil.com` pass check_repair (probe: Ok). This is harmless while `@match` is only a path filter, but under WHATWG `/\host` is protocol-relative. Reject `\` and `%2e`/`%2f` (case-insensitive) to match D-307's "no host, `//`, `..`".
- **domain.rs:106, :147: `folder_for` quirks.**
  - `to_ascii_lowercase` leaves non-ASCII uppercase (`BÜCHER.de` gives `bÜcher.de`), and there is no IDNA/punycode, so one site can map to two folders.
  - `_anything` and `.git` are returned as folders (`folder_for(".git")` gives `.git`, `folder_for("_archive")` gives `_archive`). scan guards only `_archive`, so `scan(".git", "/")` reads `*.js` from `.git/`.

  Return only `_generic` or a real site folder.
- **Commit message claims T-1..T-9, but T-6 (growth), T-8 (promotion) and T-5's auto `@expects` are not implemented** (the handoff assigns them to reflect/toolforge). G-5's hand-edit reset is also not done. Narrow the claimed ids in tasks.yaml/handoff to what shipped.
- **tools/_generic/read_table.js:65-71: output is capped at 50 rows, but cell length and column count are not capped.** Rule 5 says readers return short text, and a wide table with long cells can return many KB. Cap the characters per cell and the total output, and report truncation. list_jobs (60 jobs x 4 fields) is acceptable.
- **lifecycle.rs:221-236 with :323: `retire` renames the file and appends the ledger event before `tool_health_set`.** If the row write fails, the file is archived but the row still says needs_repair/ephemeral. Persist the row first or make the order recoverable. Same for `reforge` at :351-367.
- **Missing tests** for the four Blocking items above, for `folder_for` with private suffixes (`user.github.io`, which does work) at the ToolStore level, and for `Ask` versus `record`.

## Notes

- G-2 static denylist (`document`, `window`, `fetch`, `eval`, `import`, `require`) is not in core. The runner prunes globals (D-305), so enforcement exists at run time. toolforge must still do the static step, or check_repair could reuse it.
- Relocate synonyms (fingerprint.rs:49-51) put "submit" with "send", "confirm" and "save", so `button"Submit"` relocates to "Confirm purchase" (score 1.0). The class still comes from run-time classification, so this is not a gate issue. Consider narrower groups.
- `record(Relocated)` on a `Retired` row bumps the version before `fs::read_to_string` fails. It is harmless (the Io error returns first), but `Relocated` on a retired row should be refused explicitly.
- list_jobs.js:13: `decodeURIComponent` throws URIError on a malformed `%`. `@match /*` applies on every ashbyhq.com host, so `www.ashbyhq.com/pricing` gives org `pricing` and an API 404. The API endpoint and fields (`jobs[].title/location/address.postalAddress/department/team/jobUrl/applyUrl/isListed/isRemote`) are correct, `org` is URL-encoded, and the output is short JSON.
- `invariants::vault_keys` duplicates `harness_core::vault::vault_refs` (still `todo!()` in core). Swap it when core lands, as the handoff says.
- D-302 counts plain `Failed` toward the 5 fails that retire a needs_repair tool (4 failures, then 1 miss, retires). This matches D-302's wording but not T-7's "5 misses". Flag it to supreme if that is unintended.
- Acceptance coverage is good. All three docs/03 items have asserting tests (`seed_tools_parse_and_load`, `acceptance_planted_mismatch_then_relocate_to_version_2`, `sweep_retires_stale_ephemeral_then_deletes_after_90_days` with ledger event checks). T-9 has tests for gate lowered/changed, `?s` removed, loader skips with why, and planner class = max (fake PlannerCheck per D-301).
- An untracked `crates/toolstore/tests/` appeared in the worktree during review. It is not part of 955ce52 and was not reviewed.

## Verdict

Four blocking items: a git commit can escape the tools root, domain/name path traversal, the vault-key invariant can be bypassed, and `ask` can be left without user confirmation. The rest is solid, and the tests are meaningful.

Verdict: request changes
