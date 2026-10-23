# 03 · Site-scoped tool store (crate `toolstore`)

Tools forged for a site are saved under that site and announced on the next visit, so
the agent uses them instead of forging again. Items are `T-n`.

## T-1 Layout

```
tools/
  luma.com/
    _site.yaml               aliases, notes, pinned list
    list_events.js           reader
    register.js              planner
    extract_attendees.js     reader
  mail.google.com/           see docs/07
  calendar.google.com/
  ashbyhq.com/
    list_jobs.js             a web.api reader: works in Phase 1
  _generic/                  site-agnostic, hand-written or promoted
    read_table.js  paginate.js  dismiss_banner.js
  _archive/<domain>/<name>.<version>.js
```

Folder name = registrable domain (eTLD+1, public suffix list). `lu.ma`, `luma.com`,
`app.luma.com` resolve to one folder via `_site.yaml: aliases`. Health rows live in
Postgres `tools.health`; code lives here, in git. Every change to `tools/` is a commit
whose message carries the task id.

## T-2 Tool file format

One JS file, ES2020, header comments the loader parses:

```js
// @name register
// @kind planner                 reader | planner
// @match /*                     path pattern within the site (glob)
// @args name:string email:string plan?:string
// @gate Submit                  class floor
// @fingerprint form#f2 button"Register" input"Email"
// @expects navigated:/thanks|/confirm OR text"You're registered"
// @forged 2026-10-22 task=T-31 by=toolforge score=0.92
// @phase 2                      1 = works on fetched HTML, 2 = needs the browser
export default function (page, args) {
  const email = page.find('input "Email"');          // returns an #id, throws if weak
  const name  = page.find('input "Full name"');
  const btn   = page.find('button "Register" SUBMIT');
  return `t#${name}=${args.name}; t#${email}=${args.email}; c#${btn}; ?s`;
}
```

- **reader**: receives `page` (index + text helpers) and returns JSON. Never calls act.
- **planner**: returns an act program string. Never gets hands; `act()` runs it through
  the gate. `?s` before a Submit is required and a repair cannot remove it.
- `page` API available to tools: `find(q)`, `findAll(q)`, `text(regionId)`, `table(id)`,
  `url`, `title`. No DOM access, no network, no `eval`.
- Phase 1 readers run against `srv-web`'s fetched index; the same file runs against the
  live index in Phase 2 if `@phase 1`.

## T-3 Loading on open/fetch

1. domain → folder (aliases considered).
2. Load tools whose `@match` fits the path; register them as MCP tools for this tab,
   namespaced `luma.register`, `luma.list_events`.
3. Append one line to the skeleton: `tools for this site: luma.register(name,email) · luma.list_events(city)`.
4. Load `_generic/` after, lower precedence on name clash.

## T-4 Fingerprint check (before running)

`injected.js fingerprint(spec)` (or the fetched-index equivalent) returns
`match | partial(missing: [...]) | miss`.

- match → run; `runs += 1` on success.
- partial → **relocate** (≤200 ms): re-find each missing item by role + name synonyms;
  success → patch `@fingerprint`, `version += 1`, status `repaired`.
- miss → status `needs_repair`, tool disabled for this run, fall through to normal
  find/act. The reason is written to `tools.health.why`.

## T-5 Expectation check (after running a planner)

Consequence capture is compared with `@expects`. Miss → `needs_repair` with why. If the
task then succeeds by hand, reflect re-forges from the successful trace under the same
name, `version += 1`, old file to `_archive/`. If the new flow has a new gate class
(e.g. a payment step appeared) the floor is raised and the tool's status is `ask` until
the user confirms. After 3 clean runs the observed consequences become `@expects`
automatically for tools that lack one.

## T-6 Growth

Reflect looks at runs on domains that have a folder. Manual `find/act` sequences of ≥ 2
acts on one form, that succeeded, are candidates. Overlap ≥ 70 % with an existing tool →
extension (optional arg or step). Otherwise forge a new tool, status `ephemeral`.

## T-7 Lifecycle

| Status | Enter | Exit |
|---|---|---|
| ephemeral | forged | saved after 3 clean runs; deleted if unused 14 days or 10 site visits |
| saved | 3 clean runs | repaired / needs_repair |
| repaired | fingerprint patched | saved after 3 clean runs |
| needs_repair | miss or expects miss | repaired by relocate or re-forge; retired after 5 misses |
| retired | 5 misses, superseded, merged | moved to `_archive/`; deleted after 90 days |
| pinned | user | never auto-retired or deleted; repairs that change gate class ask first |

Deletion is always: retire → archive → delete, with time between. `harness tools health`
shows every status with its why.

## T-8 Promotion to `_generic/`

Near-identical tools (same arg shape, same role sequence) under ≥ 3 domains → reflect
writes a proposal; user approves; the merged tool takes site-specific bits as arguments.

## T-9 Invariants (tests)

- A planner's output is parsed by `sb-act` and classified; the resulting class is
  `max(declared floor, classified)`.
- `page` passed to tools has no `document`, `window`, `fetch`.
- A repair that would change `@gate` or remove `?s` is rejected and logged.
- Loader ignores files without a valid header and logs them.

## Acceptance (step 2 for Phase 1, step 6 for Phase 2)

- `tools/ashbyhq.com/list_jobs.js` (`@phase 1`, reader using `api`) loads when
  `fetch("https://jobs.ashbyhq.com/...")` runs and appears in the skeleton line.
- A planted fingerprint mismatch flips the row to `needs_repair`; fixing the page flips
  it back via relocate with `version = 2`.
- An `ephemeral` tool with `last_ok` 15 days old is deleted by reflect, with a ledger event.
