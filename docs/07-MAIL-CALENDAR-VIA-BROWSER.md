# 07 · Mail and calendar through the browser

Decision D-20: Gmail and Google Calendar are used in the browser (`me` profile), not via
API or connectors. They are **site tools** in the store, hand-written first, maintained
by the same lifecycle as any other site. Items are `E-n`.

## E-1 Why this works and what it costs

Works: no OAuth setup, no API quotas, the same gate/vault/ledger path as everything else,
and the model sees mail as the same short text it sees everywhere. Costs: Phase 2 only
(needs the browser); Google's web UI changes and the tools will need repair (that is
what T-4/T-5 are for); a logged-in persistent profile is required; headless is not used.

## E-2 Profile and login

`sheetbrowse profile login me` opens headed Chromium on `accounts.google.com`; the user
logs in by hand once; cookies persist in the profile dir. The harness never types a
Google password. If Google shows a challenge, it is a Challenge stop like any other.
Scheduled mail runs use `me` headed in a hidden window, not headless.

## E-3 Mail tools (`tools/mail.google.com/`)

| Tool | Kind | Gate | Does |
|---|---|---|---|
| `inbox(query?, limit=25)` | reader | Read | opens `#search/<query>` or inbox, `read()` the list region, returns `[ {thread_id, from, subject, snippet, date, unread} ]` |
| `thread(thread_id)` | reader | Read | opens the thread, expands all, `read()`; returns messages with from/to/date/body text; attachments listed by name |
| `triage(since="7d")` | reader | Read | `inbox` + per-thread heuristics (unanswered, question marks, deadlines) → needs-action list |
| `draft(to, subject, body, reply_to?)` | planner | Fill | compose or reply, fill fields, **stops at `?s`**, never sends; draft stays in Gmail Drafts |
| `send(draft_or_fields)` | planner | Send | `c#<send button>`; Send(new) if `to` not in `memory.contacts`; on success `touch_contact` |
| `attach(path)` | planner | Fill | `u#<file input>=path`, then verify the chip appears (expects) |
| `label(thread_id, label)` | planner | Fill | |
| `archive(thread_id)` | planner | Fill | |
| `trash(thread_id)` | planner | Delete | ask |
| `download_attachment(thread_id, name)` | planner | Read | download consequence → saved to `runs/files/`, asks first (H-1) |

Bodies the model writes for `draft` go through the no-em-dash check (CLAUDE.md rule 7).

## E-4 Calendar tools (`tools/calendar.google.com/`)

| Tool | Kind | Gate | Does |
|---|---|---|---|
| `events(from, to)` | reader | Read | agenda view, `read()`, returns `[ {id, title, start, end, location, attendees} ]` |
| `find_free(from, to, minutes)` | reader | Read | derived from `events` |
| `create(title, start, end, location?, description?, invite?)` | planner | Send | fills the event form, `?s`, then save; Send(new) per unknown invitee |
| `respond(event_id, yes\|no\|maybe)` | planner | Submit | |
| `delete(event_id)` | planner | Delete | ask |

## E-5 Fingerprints and expectations for Google UIs

Google's DOM is obfuscated; names come from aria-labels, which are stable. Fingerprints
use role + aria name only (`button "Send"`, `textbox "To"`, `textbox "Subject"`,
`textbox "Message Body"`), never class names. `@expects` for `send`:
`text"Message sent"`. For `create`: `text"Event saved"` or `navigated:#agenda`.

## E-6 Use cases this unlocks (from docs/08)

13 triage inbox · 14 draft response (grounded in real threads) · 15 approved outreach
with contact history · 20 attendance schedule · 5 download documents from logged-in
mail · 12 account status from notification mail.

## Acceptance (step 6, after browser acceptance 1–7)

- `inbox("from:luma")` returns rows; the ledger has one Read event and no vault use.
- `draft(...)` leaves a draft visible in Gmail Drafts and the act result ends at `?s`.
- `send` to a known contact runs without an ask; to a new address shows an ask card;
  after success `memory.contacts` has the row.
- Changing the Send button's aria-label in a fixture copy of the compose DOM flips the
  tool to `needs_repair`; the real label flips it back.
- `trash` always asks. `download_attachment` always asks before the file lands.
