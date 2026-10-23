# 04 · Phase 1 tool servers (crates `srv-web`, `srv-files`, `srv-cli`)

Each is an MCP server registered through `core::mcp::serve`. No browser anywhere in
this doc. Items are `W-n`, `F-n`, `C-n`.

## srv-web

**W-1 `search(q, k=8)`** → `[ {title, url, snippet} ]`. Backend: a local SearXNG
instance (Docker) or Brave Search's free tier via a vault token. Choose in
`decisions.yaml`. No scraping of Google result pages.

**W-2 `fetch(url, mode="read")`** → HTTP GET with a normal browser UA string (no
spoofing beyond that), follows redirects, 15 s timeout, 5 MB cap. Then:
- builds an **index** from the HTML with the same row shape the browser uses
  (`id role name nearby heading state flags`), via a Rust HTML walk (`scraper` crate).
  Visibility is approximated: `hidden`, `display:none` in inline style, `aria-hidden`.
- `mode="read"` → `read()` renderer output (W-5).
- `mode="sheet"` → numbered element lines.
- `mode="raw"` → cleaned HTML of `<main>` or the densest block, for tools that need
  structure. Capped at 60 k chars.
- The index is cached per URL for the run so `find` and site tools can use it.

**W-3 `api(url, method="GET", body?, headers?)`** → JSON. Headers may contain
`@vault(k)`, resolved at the sink. Response capped and pretty-printed.

**W-4 `feed(url)`** → normalized items from RSS, Atom, iCal, JSON Feed.

**W-5 `read()` renderer** (lives in `sb-sheet::reader`, used here and by the browser)
- Input: index rows + ordered text blocks for a region.
- Boilerplate scoring per block: text density, link density, landmark role
  (`nav footer aside` are penalised), repeated-across-pages heuristics (same text in
  header and footer). Blocks below threshold are dropped from `main`-less pages;
  pages with a `main` landmark use it directly.
- Output: light markdown. `#`/`##` headings, `-` lists, `| a | b |` tables, paragraphs,
  `[text](#id)` for links and `[button "Add to cart"](#id)` for actionable things so the
  model can still act after reading.
- Chunking: default 4 000 tokens per chunk. Returns `chunk 1 of N`, a `remaining:`
  outline (heading + word count per section), and a `cursor`. `read(cursor=…)` continues;
  `read(region="## FAQ")` jumps. `max_tokens` caps a whole read.
- Never emits a password field's value, never emits content of a CHALLENGE frame.

**W-6 `find(q)` on a fetched page** uses the same scorer as the browser (B-20) over the
fetched index, so Phase 1 tools and tests exercise the real ranking code.

**W-7 Site tools with `@phase 1`** are loaded on `fetch` and announced in the
skeleton exactly as the browser does (T-3).

## srv-files

**F-1 `fs`**: read, write, glob, move, mkdir. Delete is class Delete outside `runs/`.
**F-2 `shell(cmd, cwd, timeout)`**: class derived from the command (`rm`, `git push`,
`curl -X POST` → not Read). Output capped at 20 k chars, saved whole in `runs/files/`.
**F-3 `docs`**: `pdf.read`, `pdf.write`, `xlsx.read/write`, `docx.read/write`,
`pptx.write`, `md.to_pdf`. Thin wrappers over the existing skills' scripts; the server
shells out, it does not reimplement.
**F-4 `csv.clean(path, rules)`**: dedupe, normalise fields, flag missing → new file +
report.
**F-5 `diff(a, b, mode=text|table)`** for document audits.
**F-6 `history.search(q, since)`**: reads a copy of Chrome's `History` SQLite (copy
first; the live file is locked), returns `[ {url, title, visited} ]`. Use case 3 without
a browser.

## srv-cli

**C-1 `gh(args)`**: `run list`, `run view --log`, `api`, `pr view`. Read by default;
`gh api -X POST|PUT|DELETE`, `gh pr merge`, `gh release` → Account.
**C-2 `dns(name, type)`** via `dig`/`hickory`; `cloudflare(...)` via `api` with vault.
**C-3 `flags(provider, env)`** read; writes are Account.
**C-4 Generic `cli(cmd)`** behind an allowlist in `config/cli-allow.toml`.

## Acceptance (step 2 and 3)

- `fetch("https://docs.rs/tokio")` with `mode=read` returns markdown whose first heading
  is the crate name and contains no nav links; `remaining:` lists sections; `cursor`
  continues without repeating text.
- `find("search")` on a fetched docs page returns the search input first.
- `api("https://api.github.com/repos/tokio-rs/tokio")` returns JSON; a header with
  `@vault(gh_token)` never appears in the ledger.
- `shell("rm -rf runs/x")` is Read-class-safe (inside runs); `shell("rm file.txt")` is
  Delete → ask.
- `history.search("rust cdp")` returns rows from a copied History file.
- `gh("run view 123 --log")` returns the first failing step when given a fixture log.
