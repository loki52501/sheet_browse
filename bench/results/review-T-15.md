# Review T-15 round 1 (srv-cli, docs/04 C-1..C-4; commits 1221877, 33aa903)

Checked: CLAUDE.md hard rules, D-540/D-541/B-540, I-2/I-4/I-13, pr-ready section 5.
Ran: `cargo test -p srv-cli` (46 passed, 1 ignored), `cargo clippy -p srv-cli --all-targets -- -D warnings` (clean),
`cargo fmt -p srv-cli --check` (clean). Classifier probed from a scratch crate calling `srv_cli::gh::classify_args`.

## Blocking

- crates/srv-cli/src/gh/mod.rs:129-146 (`api_class`): gh's flag parser (pflag) accepts clustered short flags, but the
  scan only finds `-X`/`-f`/`-F` when they come first in a token. Probe results: `api -iXPOST repos/o/r/issues` gives
  Read, `api -iX POST repos/o/r` gives Read, `api repos/o/r/issues -iftitle=x` gives Read. (`-i` is gh api's boolean
  `--include`.) gh runs these as POST, so a write runs as Read with no ask. That breaks rule 4 and fails open, against
  D-540's "every token is scanned". Fix: for any single-dash token that is not `--...`, walk its letters. Treat an `X`
  as a method flag whose value is the rest of the token, or the next token when nothing follows. Treat any `f` or `F`
  as a field flag. Simpler still, treat any single-dash cluster that contains X, f or F as Account. Add these cases to
  `classification_table`. The same bug is at :111-113: `pr view 5 -cw` (`--comments` plus `--web`) opens a browser but
  is classified Read, not Navigate.
- crates/srv-cli/src/gh/mod.rs:101 and :212: gh api accepts an absolute URL as its endpoint (`gh api https://any.host/page`
  sends a GET to that host). Today that is Read, does not ask, and is labelled with the gate domain `github.com`. Two
  results: (a) the raw HTML body of any site reaches the model through the 4 MiB / 12k-char text path (rule 5: only
  the srv-web readers may return page content, as short text); (b) gate rules for the real host never apply, because
  the domain is wrong (rule 4). Fix: refuse an api endpoint that contains `://`, or allow it only for GitHub hosts and
  set the domain to that host. Add a test.

## Should fix

- crates/srv-cli/src/gh/mod.rs:91: only `gh auth` is refused. `gh config get oauth_token -h github.com` (and
  `gh config list` on a hosts.yml-backed install) prints the same credential, which the vault cannot scrub. It
  classifies as Account, so it asks, but D-540's reason for refusing `auth` applies equally. Refuse `config get` of a
  token key, or all of `gh config`.
- crates/srv-cli/src/cli.rs:149-160 and config/cli-allow.toml:40: `deny_flags` matches only `-x`, `-x=v` and `-xvalue`,
  not short flags clustered behind another letter. dig accepts `dig -4f names.txt` (BIND's dash_option takes the
  single-letter flags 4/6/d/h/i/m/n/u/v in a cluster before f/k/y), so the shipped `-f`/`-k`/`-y` denials can be
  bypassed. Either match a denied short letter anywhere in a single-dash cluster, or document the limit and add the
  clustered forms to the shipped file. Add a test.
- crates/srv-cli/src/flags.rs:91 (accepts `http`) and crates/srv-cli/src/config.rs:430 (`SB_CLOUDFLARE_API` is not
  checked): the vault-resolved auth header can be sent in cleartext over plain http, or through an http proxy. Require
  https for any base that gets a `@vault` header, and allow plain http only for loopback (the tests use 127.0.0.1).
- crates/srv-cli/src/http.rs:177-198 (`reply_text`): the content type is never checked. A WAF or proxy HTML error page,
  or a misconfigured base, returns raw HTML to the model (up to 1,500 chars in errors, 12k chars on 2xx). That is
  rule 5 adjacent. When the response is not JSON or text/plain, return only the status and content type.

## Notes

- crates/srv-cli/src/http.rs:86: the base check compares path prefixes as strings, and `url` normalises `%2e%2e`
  segments. So `/%2e%2e/v4x` under `/client/v4` resolves to `/client/v4x` and passes. The origin is still the same,
  so the token never leaves the host. Comparing at a segment boundary (prefix + `/`) would close it.
- Read subcommands of git (`status`, `diff`, `log`) can still run programs set in the repository's own config or
  attributes (`core.fsmonitor`, `diff.external`, textconv drivers). `-c` is denied, so only the repository can set
  these, and cli always runs in the server's cwd. That is acceptable while the cwd is trusted; worth a line in
  cli-allow.toml.
- When the class rises between classify and call, gh, cli, cloudflare and flags return `ToolError::Failed` rather
  than `ToolOutput::stopped` (the mcp.rs:149 guidance). That is fail-closed and fine for single-step tools.
- `save_log` relies on the global scrubber. `core_pg::bootstrap` installs it (core-pg/src/lib.rs:990), and the tests
  install it too. OK.
- Tests are real: the docs/04 acceptance (`gh run view 123 --log` returns the first failing step, the full log is
  saved and scrubbed) asserts the content, the class, the mode and that no card was shown. The vault canary is checked
  in the text, errors, ledger and saved files, and the header test proves the resolved value reached the wire. Not
  asserted anywhere: the `MAX_FAILING` = 20 and `MAX_CONTEXT` = 10 caps in gh/log.rs.
- `.cmd` shims killed on timeout may leave grandchildren (handoff not_done 5); acceptable for now.
- No hard-rule hits for keys or .env files, captcha handling, fixed sleeps, em dashes, or `unwrap` on external input.
  Spec ids match the code. Interfaces I-2, I-4 and I-13 are used at their contract paths
  (tests/contract_paths.rs).

## Verdict

Two blocking gate holes in gh classification: clustered short flags run writes as Read, and `api <absolute URL>` is
an ungated, mislabelled raw-HTML fetch. Fix both with tests, then re-review.

Verdict: request changes
