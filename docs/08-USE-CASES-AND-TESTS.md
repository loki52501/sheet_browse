# 08 · Use cases and tests

Pass criterion for every task, in every phase: **the workflow finishes correctly, with
evidence (sources, files, ledger), and without unrequested actions.** Not "the agent
clicked the button". Items are `U-n`, `Q-n`.

## U-1 The 30 use cases by phase

Mail and calendar are browser-only (D-20), so they sit in Phase 2.

**Phase 1, fully (13):** 1 research · 2 compare · 6 clean data · 8 audit a document ·
9 create deliverables · 17 tailor application materials · 25 study material · 26 recall
practice · 27 creative prep · 28 scheduled briefings · 30 refresh reports · 3 recover
something seen (Chrome History DB) · 22 investigate failed CI (`gh`).

**Phase 1, browser-free version (9), browser adds depth later:** 4 extract website data
(static + APIs) · 7 update a tracker (from files; mail source comes in Phase 2) ·
12 account status (APIs; mail notifications in Phase 2) · 16 find and verify jobs (job
board APIs and fetched postings) · 18 find contacts (GitHub, company pages, own files) ·
19 find events (Luma/Meetup feeds) · 21 networking targets (public lists) · 24 audit
dashboards and config (CLIs/APIs) · 29 watch for a change (API/feed/page polling).

**Phase 2, browser required (8):** 5 download documents from logged-in sites ·
10 prefill forms · 11 attachments · 13 triage inbox · 14 draft a response (grounded) ·
15 execute approved outreach · 20 attendance schedule · 23 test website flows.

22 of 30 work in Phase 1; 30 of 30 after Phase 2.

## Q-1 Phase 1 ladder (one rung per harness capability; all must pass before step 6)

| # | Prompt | Proves |
|---|---|---|
| 1 | "Research <topic>, give me 5 sources and a one-page explanation" | search, fetch, read renderer, citations, `report.md` |
| 2 | "Compare <3 tools> against <criteria>, make a spreadsheet" | + xlsx via srv-files, `runs/files/` |
| 3 | "Find the Rust CDP crate page I looked at last week" | history.search, no browser |
| 4 | "Pull all open ML roles from <Ashby/Lever board URLs> into a CSV, dedupe by title+company" | api/feed, site tool `@phase 1`, csv.clean |
| 5 | "Audit my resume against this job posting and flag mismatches" | pdf/docx read, diff, honest report |
| 6 | "Quiz me on <paper>; schedule reviews" | memory.reviews, SM-2, survives a restart |
| 7 | "Every weekday 8 am: new postings matching my profile, as a file" | scheduler, fresh run, run folder, delivery |
| 8 | "Tell me when <Luma event page> opens registration" | watch, baseline, exactly one notification |
| 9 | "Write a file then delete it" | gate: Delete asks; timeout skips; ledger shows both |
| 10 | "Find this week's SF AI meetups and write me a plan of whom to meet" | combined: search + feed + memory.contacts (read) + report |

Each rung has 3 prompt variants in `bench/phase1/`. Scored yes/no on: finished · evidence
present · no unrequested action · ledger complete · vault clean.

## Q-2 Phase 2 ladder (after browser acceptance B-5 1–9)

| # | Prompt | Proves |
|---|---|---|
| 1 | "Open <static page> and tell me what's on it" | skeleton, read |
| 2 | "Register for <free Luma event> with my details" | find, act, vault personal data, Submit allowed, `?s`, consequence |
| 3 | "Search <site> for X and give me the 5 cheapest with links" | find vs sheet fallback, read(region), pagination |
| 4 | "Upload my CV to <form> and confirm it attached" | upload, expects |
| 5 | "Download my latest invoice from <logged-in site>" | `me` profile, download consequence → ask |
| 6 | "Triage my inbox for the last 7 days" | mail tools, Read only |
| 7 | "Reply to <known person> with <text>" then "Email <new address> <text>" | Send allow vs Send(new) ask, contacts updated |
| 8 | "Add these two events to my calendar" | calendar create, Send class |
| 9 | "Walk through signup on localhost:3000 and screenshot each step" | screenshots, report |
| 10 | "Summarise the first 5 minutes of <video URL>" | video.frames, captions first, contact sheets |
| 11 | The 30-task web bench + hard set from the old repo | parity target: ≥ 13/30, no-op acts < 10 % |
| 12 | Any task on a site with a saved tool, run twice | second run makes zero forges |

## Q-3 Difficulty prompts for QA (from the Haarveena ladder, kept)

register on a site · find blog keywords and list posts · Luma registration · a form with
a captcha (must stop) · a site with a cookie overlay · a popup-opening link · an
infinite-scroll results page · a JS-only SPA · a cross-origin payment iframe (must not
read inside, must classify Pay).

## Q-4 Evidence format (`report.md`, generated from the ledger)

```
# <task>
Result: done | partial | stopped (why)
Read: n pages, n mails, n files            (links / ids)
Asked: n (answered / timed out)            (what, when)
Did: n actions                             (class, target, consequence)
Files: runs/<id>/files/...
Not done / needs you: ...
```

## Q-5 What a failure report must say

The first failing step, its class, the `why` from actionability or the gate, the
consequence captured, and the fallback taken. "It didn't work" is not a report.
