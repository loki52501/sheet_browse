# 01 · Harness core (crate `core`)

One crate. Every tool server registers through it. Items are `H-n`.

## H-1 Gate

Every tool call is classified into exactly one class before it runs. The class comes
from the tool's declared class, raised by the classifier if the arguments show more:
`act("c#19")` where `#19` is a SUBMIT button in a form containing a card-number field is
Pay, not Submit.

| Class | Covers |
|---|---|
| Read | any read: fetch, skeleton, find, read, sheet, mail read, file read, `gh run view` |
| Fill | type, select, check, upload a file into an input |
| Navigate | open, click a link, new tab, scroll |
| Submit | a form submit that is not Pay/Account/Delete: register for a free event, post a comment, save a filter |
| Send | mail send, calendar create, message send |
| Pay | checkout, add card, confirm order, any form with a card/IBAN field |
| Account | create account, change password or email, enable 2FA, write to production config (DNS, flags, repo settings) |
| Delete | rm outside `runs/`, trash mail, cancel a booking, unsubscribe, delete a record |
| Challenge | captcha, Turnstile, hCaptcha, "verify you're human", phone/SMS verification |

**H-1.1 Default modes (lenient).**

| Class | Mode |
|---|---|
| Read, Fill, Navigate, Submit | allow |
| Send to a recipient in `memory.contacts` | allow |
| Send to a recipient never contacted | ask |
| Pay, Account, Delete | ask |
| Challenge | stop |

**H-1.2 Overrides.** Table `gate.rules(domain text, class text, mode text, note text)`.
Lookup order: `(domain, class)` → `('*', class)` → default. `domain` is eTLD+1. Examples:
`('bank.com','Submit','ask')`, `('luma.com','Account','allow')`. The user's standing
rule "job submissions and signatures are mine" is a rule row:
`('*','Submit','ask','job applications')` scoped by the classifier's `job_application`
hint, not a hardcode.

**H-1.3 Ask cards.** An ask produces a card: what will happen, on which site, with which
values (vault values shown as `@vault(name)`), and a 60-second timer. Timeout behaviour:
Send(new) and Account → proceed; Pay and Delete → skip. The user can set `strict` on a
run or a rule to make every timeout skip. The answer and the timeout are ledger events.

**H-1.4 Stop.** A Challenge stops the current act program at that step, returns the
partial result with `stopped: challenge` and the element, and marks the tab. Nothing
in the program after that step runs. The user resumes by acting in the browser
themselves and telling the model to continue.

**H-1.5 Floors.** A site tool declares `@gate <class>`; the gate treats a call as at
least that class. A repair or forge may raise a floor, never lower it.

## H-2 Vault

- `vault.toml.age` (age-encrypted) or OS keychain; key never in the repo.
- Entries: `name = { value, kind }` where kind ∈ `secret | personal`. Personal entries
  are your form data (full name, address, phone, work email).
- `@vault(name)` is a literal in act programs, HTTP headers and tool args. Resolution
  happens inside `core::vault::resolve_into(sink)` at the sink: the `insertText` call,
  the header write. The resolved string is never returned to any caller.
- `core::scrub(s: &str) -> String` replaces every vault value found in `s` with
  `@vault(name)`. Every string that leaves the process (tool results, ledger lines, logs,
  panics) passes through `scrub`. Tested by a fixture that plants a value and greps all
  outputs.
- Passwords are never read from the DOM; `injected.js` returns only `empty | filled`.

## H-3 Ledger

- Table `ledger.events(id bigserial, run_id, ts, server, tool, class, mode, args_scrubbed
  jsonb, result_summary text, consequences jsonb, duration_ms, ok bool, why text)`.
  Append-only: a trigger raises on UPDATE and DELETE.
- Mirror: `runs/<run>/ledger.jsonl`, one line per event, written after the DB insert.
- Gate decisions, ask answers, timeouts, challenge stops, tool repairs and forges are
  events too (`server = core`).
- `report.md` at the end of a run is generated from the ledger: what was read, what was
  asked, what was done, what files came out. This is the evidence in "finished, with
  evidence, without unrequested actions".

## H-4 Run folder

```
runs/<yyyy-mm-ddThh-mm-ss>-<slug>/
  prompt.md        the task as given
  ledger.jsonl
  report.md
  files/           deliverables and downloads
  frames/          video frames, screenshots
  handoff/         what one agent leaves for another in multi-agent runs
```

Table `runs(id, started, ended, prompt, status, folder, parent_run)`. A scheduled firing
creates a run with `parent_run` pointing at the schedule's run.

## H-5 Scheduler

- Table `schedules(id, cron, prompt, kind, baseline_id, enabled, last_run)`.
  `kind ∈ cron | watch | reflect`.
- A firing starts a fresh Claude Code session with the stored prompt and a run folder.
- **watch:** after the run, hash the `watch_target` the prompt names; compare to
  `memory.baselines`; on change, notify once (file + message) and store the new baseline.
  No change → `report.md` says so and nothing is sent.
- **reflect:** nightly. For every domain with saved tools: open (Phase 2) or fetch
  (Phase 1), check fingerprints, run **readers only**, compare output shape, update
  `tools.health`. Propose forges and `_generic/` merges into `agent-memory/blockers.yaml`
  under `proposals`. Never runs a planner.
- Phase 1 implementation: a `core::sched` loop in a long-running `harnessd` process,
  plus a `harness schedule add` CLI. OS cron is not required.

## H-6 MCP wrapper

`core::mcp::serve(server_name, tools)` is how a tool server exposes tools. The wrapper:
classifies, consults the gate, resolves vault at the sink, scrubs the result, writes the
ledger, measures duration, and attaches the run id. Tool servers never write the ledger
or read the vault directly.

## H-7 CLI

```
harness run "prompt"            start a run interactively
harness schedule add|list|rm
harness vault set <name> [--personal]
harness gate set <domain> <class> <mode>
harness tools list|health|pin|unpin|retire <domain>/<name>
harness ledger <run>            print the ledger as a table
harness db migrate|status
```

## Acceptance for this doc (step 1)

- A fake tool `echo.send(to, body)` registered through H-6: with `to` in contacts → runs,
  ledger has class=Send mode=allow; with unknown `to` → ask card, timeout → proceed and
  ledger shows `timeout_proceed`; with class forced to Pay → timeout → skip.
- Vault fixture: `@vault(test)` resolves inside the fake tool's sink; the value appears
  nowhere in stdout, ledger, DB, or `runs/`. A test greps all of them.
- `UPDATE ledger.events` fails. `runs/<id>/ledger.jsonl` matches the table row-for-row.
- `harness schedule add "*/1 * * * *" "say hi"` fires, creates a run folder with
  `report.md`, and `runs` has a row with `parent_run` set.
