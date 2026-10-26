# Review T-12 round 1 (srv-web, docs/04 W-1..W-7)

Commit dbacdbb on swarm/servers-T12. `cargo fmt --check` clean, `cargo clippy -p srv-web --all-targets -D warnings` clean, committed tests green (lib 11, accept_w 6 + 1 ignored, web_tools 13, contract_paths 1, live 3 ignored). The untracked `tests/accept_t12.rs` (the tester's, not part of this commit) had 3 failures when I ran it; those were not reviewed here.

## Blocking

- Rule 5 fails open on HTML sent with the wrong content type. `fetch.rs:151-152` sends any `text/plain` body to `html::plain_text`, which keeps the tags. The HTML sniff in `fetch.rs:191-201` only runs when there is no MIME type at all. In `api.rs:324-347`, a non-JSON body with an empty MIME type, `text/*` or `*xml*` is returned verbatim (up to 20 k chars) through `escape::text`, which does not touch tags. So a server that labels HTML as `text/plain`, or sends no Content-Type, puts raw HTML in the model's context. raw.githubusercontent.com serves `.html` files as `text/plain`, so this happens with ordinary sites. Fix: sniff `<!doctype html` / `<html` (after BOM and whitespace) whatever the declared MIME type is. In fetch, send a match to the HTML walk. In api, refuse it the way `api.rs:318` refuses `text/html`. Add a test for each tool.

## Should fix

- Vault headers can be sent in cleartext. The api redirect policy (`http.rs:47-56`) only compares host strings, so a same-host `https -> http` downgrade or a port change still carries the resolved header. reqwest strips `Authorization` only on a host or port change, never on a scheme change, and it never strips custom headers like `X-Api-Key`. Also, `api.rs:225-240` resolves `@vault(k)` for a plain `http://` URL. Require the same scheme and port on redirects. Refuse `@vault` headers on non-https URLs unless the host is loopback.
- Classify and call resolve state separately, and serve_io runs calls concurrently (`core/src/mcp.rs:965`).
  - `fetch(url="#12")` resolves `cache.last()` in classify (`fetch.rs:227`) and again in call (`fetch.rs:238`). If another fetch lands in between, the call reads a URL whose domain the gate never checked (rule 4, Read class).
  - Site tools have the same gap: `site.rs:124` vs `site.rs:148-151` (`for_folder`, and for `_generic` the last page), so page.api's domain can differ from the gated domain.
  - Fix: resolve once, e.g. remember the resolved target per call in classify, or pin `#id` to a page URL.
- B-501 (cross-domain fetch redirects). I do not treat it as blocking. It is a Read GET, Read is allow by default, the browser behaves the same on navigation, the final URL is in `consequences.url`, and the protocol was followed (a blocker with a default). Site tools registered for the final domain (`fetch.rs:175`) are gated with that domain when they are called (`site.rs:123-143`), so no side effect skips the gate.
  - Two issues remain. A `(domain, Read, stop)` rule on the redirect target is not applied. A public page can also redirect `fetch` and `feed` (`http.rs:42-43`, `feed.rs:93`) into loopback or private addresses (127.0.0.1:5433, 169.254.169.254), whose content then reaches the model.
  - Cheap local fix now: a custom page `Policy` that refuses a hop from a public host into loopback, private or link-local ranges.
  - Supreme should decide on a non-interactive gate check seam (e.g. `ToolCtx::check(domain, class)`) so each hop can be re-checked.
- Rule 3 is reported only by fetch. `feed.rs:95-118` and `api.rs:281-284` ignore `cf-mitigated: challenge` and challenge interstitials, so they return "did not answer with RSS..." or a JSON parse failure instead of a Challenge stop that hands back to the user. Reuse the fetch check: the `cf-mitigated` header plus `html::challenge_url` on the final URL.
- T-4 deviations have no decision id.
  - `site.rs:161-168`: a `partial` fingerprint runs with a note instead of relocating.
  - `site.rs:41-63`: tools are registered without looking at `tools.health`, so a `needs_repair` or retired tool is announced and runnable on the next fetch. T-4 says a miss disables the tool "for this run".
  - Record both in decisions.yaml or blockers.yaml, or filter on health before registering.
- The W-2 caps have no committed tests: the 5 MB truncation (`http.rs:115-147`, the `truncated` skeleton note in `fetch.rs:307-309`), the 15 s timeout, and `SITE_OUTPUT_MAX_CHARS` (`site.rs:207-213`). The tester's draft found that a stalled body comes back as "request to X failed", not "did not answer in time" (`http.rs:86-97`, `read_capped` at `http.rs:135`). Check that mapping.

## Notes

- Rule 2 checks pass. `@vault` is resolved only in the sink into a sensitive `HeaderValue` (`api.rs:229-234`). Errors never use `reqwest::Error` text and name only the host (`http.rs:62-97`). Classify puts the template, not the value, into card values (`api.rs:193-198`). It is refused in the URL and body. Tests cover the ledger, the returned text, an echoed header and an unreachable host. The base64 echo test is ignored until T-05d. Under D-88 that is not a merge hold, but T-05d must land before the T-17 zero-leak baseline (D-77).
- Rule 5 otherwise checks out. `mode=raw` is refused before any request (B-500, test `raw_mode_is_refused_before_any_request`). `text/html` is refused by api. Password and `cc-` values are only `empty` or `filled` (`html.rs:695-731`). Site tool output is JSON capped at 20 k chars.
- D-503 escaping holds against the vectors I tried. Text spans are merged before escaping, so a split word like `<b>remain</b>ing:` is still quoted. Permalink and hidden splits are covered. Link forms never render empty. Prose is collapsed to one line per block. Fences are closed per unit.
  - Residual 1: a prose block starting with `# ` or `- ` can still forge a heading or outline line. That is cosmetic.
  - Residual 2: api and page.api JSON strings can carry `[x](#3)` inside quotes. These are not escaped, but they appear inside JSON strings.
- Site tool classes are `max(@gate, Read)` and the registry never lowers a class (D-127). The test asserts `strict_read` -> Submit. Tools are filtered with `try_tool_path` (D-72). The runner runs in `spawn_blocking`. page.api allows only the page's registrable domain, GET only, with same-host redirects, and is wrapped in `timeout(remaining)` (D-64, D-501).
- `site::register` scans the store with blocking file I/O on the async handler (`site.rs:46-58`). It is small, but `spawn_blocking` would be cleaner.
- `search` classifies with no domain (`search.rs:110-117`). That is fine for a localhost backend.
- The fixtures under `crates/srv-web/tests/fixtures` are clean. The only long hex is the docs.rs static CSS cache-buster (`cf3269...`, a public commit hash). The GitHub JSON is public org data. The emails are `@example.org` UIDs and `git@github.com`. There is no key-shaped string. The canary is the planted `SB_VAULT_CANARY` default.
- `config/searxng/secret.env` is git-ignored (`.gitignore:15`) and not tracked. The compose change is limited to the `searxng` service on profile `search` and its own network. The image is pinned by digest. Without the secret, SearXNG's default `ultrasecretkey` stops it from starting, so a missing secret fails closed.
- Interface fidelity: the code uses I-2 (`ToolCtx::registry`, `resolve_into`), I-6 and I-12 (`run_reader`, `ApiFetch::api(url, remaining)`, `SheetPage`) as declared. `toolstore::GENERIC` is used but is not in the I-7 list.
- No fixed sleeps. No `unwrap` on external input. The slicing in `feed.rs:405-425`, `http.rs:128,191` and `escape.rs:59` stays on char boundaries.

## Verdict

One rule-5 fail-open (HTML sent under a non-HTML or missing content type) must be fixed with tests. Everything else is should-fix.

Verdict: request changes
