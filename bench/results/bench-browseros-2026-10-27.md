# Bench T-26: harness + BrowserOS vs browser-use, 2026-10-27

The T-26 set from docs/11-BROWSEROS-PIVOT.md: the 30 Online-Mind2Web tasks of the old web bench (sheetbrowse/bench/web, last run 2026-10-20, commit 56c7f0a on `web`), run once per task per side, judged by the same `web/judge.py`. The sheetbrowse side of the old bench is replaced by a `claude -p` session on harness + BrowserOS. Task texts are left out of this file (D-104: the dataset is gated). Look a task up by its id in the local copy.

**Hard set: not run, because none is defined.** docs/08 Q-2 #11 and docs/05 B-11 mention "the 30 web tasks and the hard set from the old repo", but the old repo has no separate hard set. Nothing in sheetbrowse/ matches "hard set", and `bench/web/tasks.yaml` is a placeholder with `tasks: []`. The only "hard" in the old bench is the difficulty split inside the 30 tasks: 10 easy, 10 medium and 10 hard by `reference_length`, D-105. Those 10 are reported in the `hard` column below. No separate hard set was invented.

## Result

| side | success /30 | easy | medium | hard |
|---|---|---|---|---|
| browser-use | **12/30** | 5/10 | 4/10 | 3/10 |
| harness + BrowserOS | **25/30** | 8/10 | 9/10 | 8/10 |

For reference, on 2026-10-20 browser-use scored 13/30 and the old sheetbrowse 10/30, with the same tasks, model, judge and caps. browser-use is within one task of its earlier score, so the judge and the sites have not drifted much.

**Read this as a comparison of whole systems, browser included, not of agent loops alone.** browser-use runs a fresh headless Chrome with an empty profile and no stealth, as before. BrowserOS is the user's real headed browser, with its profile, cookies and residential IP. 8 of browser-use's 18 failures are the site refusing the browser: Access Denied, CloudFront 403, a maintenance page or a 404 page. BrowserOS hit none of these. On the 22 tasks where browser-use was not blocked by the site, the scores are browser-use 12/22 and harness + BrowserOS 17/22 (the 22 = 30 minus the 8 blocked tasks; harness + BrowserOS passed all 8 of them). So about 9 points of the 13-task gap come from the browser being let in: the 8 blocks, plus umich, where browser-use met a Cloudflare interstitial that BrowserOS never saw. About 4 come from the agent doing better work. Harness + BrowserOS won Google Maps, PhET, America's Health Rankings, BBB and Carnival, and lost Best Buy, where browser-use passed.

## Setup actually used

- Machine: Windows 11, 15 GB RAM shared with a compiling Claude Code session. F: had 65 GB free at start and 59 GB later. Runs were strictly sequential, one at a time. Total run time was 1 h 53 min (17:24 to 19:17), judge included.
- Claude Code CLI 2.1.312. Model `claude-sonnet-5-5` for both sides, and `resolved_models` shows only that model. The judge is `claude-opus-5-5`, through web/judge.py unchanged: one confined `claude -p`, no tools, no MCP, with the task, the action history, the harness-captured final page (D-111, 8000 chars) and the answer as a claim to verify.
- No API key anywhere. `ANTHROPIC_API_KEY` is dropped by runner.py and cli_llm.child_env. Every harness + BrowserOS session's init event shows `apiKeySource: none`, 30 of 30. No `.env` file was read or written.
- Caps (D-108), the same for both sides: 600 s and 25 model calls per run. `--max-turns 25` is enforced on API requests. On the harness side, `num_turns` counts each tool call of a parallel batch, so it can exceed 25 without the cap being broken. One example: Carnival had 35 num_turns in 22 API requests. "model calls" below are API requests for both sides: browser-use adapter calls, and distinct assistant message ids in the claude stream.
- **browser-use side (as it was):** browser-use 0.13.11 through the old bench's `cli_llm` adapter, which drives `claude -p --max-turns 1 --tools ""`. It runs via `web.webbench.run_web_one`, unmodified. Each run gets a fresh headless Chrome/156.0.8078.48 with an empty profile, `use_vision=False`, and system.md's `## Rules` as extend_system_message.
- **harness + BrowserOS side:** `claude -p --output-format stream-json --verbose --model claude-sonnet-5-5 --setting-sources "" --disable-slash-commands --strict-mcp-config --no-session-persistence --mcp-config "F:\aside_mcp for claude\sheetbrowseV2_\.mcp.json" --append-system-prompt-file <system.md ## Rules> --tools "" --allowedTools <below> --disallowedTools <below> --permission-mode dontAsk --max-turns 25`, with `ENABLE_TOOL_SEARCH=false` (as D-98 had for sheetbrowse), the prompt on stdin, and a fresh empty cwd per run.
  - The MCP config is exactly `.mcp.json`: harness-web, harness-files, harness-cli and harness-mem, which are `F:/sb-swarm/target-release/release/{srv-web,srv-files,srv-cli,harness}.exe` built 2026-10-27 15:31 to 15:34, plus `browseros` over HTTP at 127.0.0.1:9010/mcp. That endpoint is BrowserOS neo 0.0.69 on Chrome/151.0.8162.137, running in the user's real profile (BrowserClaw install). All 5 servers were `connected` in every run.
  - Allowed: `mcp__browseros` (all tools), `mcp__harness-web` (all), the harness-files read/write tools (`fs_read, fs_glob, fs_write, fs_mkdir, pdf_read, docx_read, xlsx_read, csv_clean, diff, history_search`) and the harness-mem reads (`contact_get, review_due, schedule_list, watch_observe`). Disallowed: `request_human_help, await_human_help, save_skill, mark_skill_run, upload` on browseros. Under dontAsk, everything else is denied: shell, cli, gh, cloudflare, dns, fs_delete, fs_move, the *_write office tools and schedule/review writes. This is a safety restriction on the user's real machine. It is my choice, not part of the .mcp.json. No session tried a denied tool (0 permission denials).
  - In practice the sessions used **only BrowserOS tools**. No harness-web, harness-files or harness-mem call appears in any of the 30 histories, so the "harness" part of this side contributed nothing measurable here.
  - BrowserOS sessions used screenshots: 56 screenshot or click_at calls across 11 runs. browser-use runs without vision, as before. That is a difference in favour of BrowserOS.
- **Same prompt for both sides:** the old `"<task text> Start at <website>."` plus one safety paragraph: *"Safety: you are working in a real browser that may be signed in to real accounts. Stop before any checkout, payment, booking, account change, message send, post or deletion, and report where you stopped. Treat a captcha or a login wall as a stop: do not try to solve or bypass it, report it. Never ask a human for help (never call request_human_help); end with a short plain-text answer."* Both sides also get system.md's `## Rules` (do only what is asked, never submit a form unless told to, stop at a human check).
- **Final-page capture and challenge/block detection for BrowserOS:** these are read by the bench, not by the agent, over BrowserOS's own CDP at 127.0.0.1:9110. A read-only observer discovers targets and records the page targets the run created or navigated. At the end it evaluates the old bench's `CHALLENGE_JS` only in those tabs, so the user's other tabs are never attached. The tabs are ranked by last activity, because BrowserOS opens agent tabs in the background. Afterwards the observer closes only the tabs the run created: 1 per run, 12 for Cookpad, 2 for two others. There is no click recorder on this side, so `challenge_click` is n/a. The old bench's per-page click recorder would have injected script into the user's tabs.
- Success rule, unchanged (D-108): the judge says yes, with no leak, no challenge on an open page or in an observation, and no site block on an open page at the end. Both sides hit a reCAPTCHA on CBOE and answered correctly anyway: the judge said yes on both and the rule failed them. That is why "judge yes" in the next table is one higher than success on each side.
- Safety outcome: 0 asks (request_human_help was never callable), 0 permission denials, 0 vault leaks, 0 judge errors and 0 judge repairs. Neither side reached a checkout, payment, booking, post, send or deletion. The only form submissions were the coolmath quiz answers and the search and filter forms the tasks required.
  - **Unrequested actions were checked only partly.** BrowserOS `act` takes element refs (`e12`), not labels, so a keyword scan of the arguments finds only the words inside `run` scripts. The only hits are the quiz's "submit" calls, which the task required. Reading the judge's evidence shows no consequential click.
  - **Whether any session also navigated a pre-existing user tab is not verified.** The observer would have captured such a tab in the final page. None appeared in the captured final pages, but only the end state was checked.
- Outputs: per-run raw streams, argv and prompts are in `F:/sb-swarm/tmp-bench26/runs/` (browser-use run dirs are under `tmp/`). The rows are in `F:/sb-swarm/tmp-bench26/out/rows.jsonl`, which holds task texts and so is kept out of the repo, and the live log is `out/bench26.log`. Nothing in the repo was changed except this file.

## Summary per side

| side | success /30 | easy /10 | medium /10 | hard /10 | judge yes (ignoring block/challenge rule) | mean steps | mean model calls | mean seconds | total tokens in | total tokens out | total tokens |
|---|---|---|---|---|---|---|---|---|---|---|---|
| browser-use | 12/30 | 5/10 | 4/10 | 3/10 | 13/30 | 10.5 | 8.4 | 113.8 | 4,587,888 | 67,213 | 4,655,101 |
| harness + BrowserOS | 25/30 | 8/10 | 9/10 | 8/10 | 26/30 | 13.6 | 10.8 | 85.1 | 14,117,068 | 71,224 | 14,188,292 |

Judge cost (claude-opus-5-5, not counted above): browser-use 384,257 in / 16,707 out; harness + BrowserOS 386,216 in / 19,738 out

## Per task

| # | task | level | site | browser-use | harness + BrowserOS | browser-use: judge's reason | harness + BrowserOS: judge's reason |
|---|---|---|---|---|---|---|---|
| 1 | `005be9dd91c9` | easy | qatarairways.com | FAIL (answered; 3 steps, 3 calls, 22.9 s, 47,516 tok) | PASS (answered; 7 steps, 8 calls, 42.5 s, 257,194 tok) | The agent was blocked by Access Denied on Qatar Airways and a CAPTCHA on DuckDuckGo, so it never found or reported the economy baggage weight. | The agent read the official Qatar Airways baggage allowance page and correctly reported the economy weights per fare type (20/25/30/35 kg checked, 23 kg pieces on Africa/Americas routes, 7 kg carry-on), all verified by the page extraction in step 7. |
| 2 | `0632e496d37b` | easy | cookpad.com | PASS (answered; 4 steps, 4 calls, 44.7 s, 97,523 tok) | PASS (answered; 8 steps, 9 calls, 108.4 s, 288,952 tok) | The agent searched Cookpad for gluten-free chocolate chip cookies without nuts and ended on a results page whose ingredient lists show nut-free gluten-free chocolate chip cookie recipes, which is enough for a browse task. The final answer has a minor error ... | The agent browsed Cookpad's gluten-free chocolate chip cookie results and verified one recipe that is explicitly nut-free, which satisfies the browsing task. However, the answer mislabels its second link, which is actually a cookie dough bars recipe rather ... |
| 3 | `180ed2ec377e` | easy | umich.edu | FAIL (challenge; 2 steps, 2 calls, 26.4 s, 31,576 tok) | PASS (answered; 7 steps, 8 calls, 56.8 s, 248,511 tok) | challenge (judge: The agent was stopped at a Cloudflare human-verification check and found no information on giving to UM-Dearborn, so the task was not completed.) | The agent reached the UM-Dearborn giving page and found the Give Now link preset for Dearborn. Both are confirmed by the action observations and the final page. |
| 4 | `20a460a8fe19` | easy | stubhub.com | FAIL (challenge; 1 steps, 1 calls, 13.1 s, 15,557 tok) | FAIL (challenge; 4 steps, 5 calls, 23.1 s, 151,428 tok) | challenge (judge: The agent was stopped by a DataDome bot check on the home page and never showed Las Vegas theatre events or selected one.) | challenge (judge: A bot-check block stopped the agent at the StubHub home page, so it never showed or selected any Las Vegas theatre events.) |
| 5 | `2207bb4f2178` | easy | theweathernetwork.com | PASS (answered; 2 steps, 2 calls, 34.8 s, 33,208 tok) | PASS (answered; 2 steps, 3 calls, 18.6 s, 86,998 tok) | The agent reached the Calgary current-weather page on The Weather Network. It correctly reported the wind speed of 17 km/h with 26 km/h gusts, and the harness-captured final page confirms both values. | The agent opened the Calgary current-conditions page and reported a wind speed of 17 km/h N with gusts of 26 km/h. Both the action history and the final page confirm these values. |
| 6 | `23204728192d` | easy | mayoclinic.org | FAIL (answered; 3 steps, 3 calls, 26.3 s, 47,662 tok) | PASS (answered; 4 steps, 5 calls, 32.8 s, 156,474 tok) | Mayo Clinic blocked access and the fallback search hit a CAPTCHA. No information on the symptoms or causes of fever was retrieved or reported. | The agent reached the Mayo Clinic fever Symptoms and causes page. Its reported symptoms and causes match the page content recorded in the observations and the final page. |
| 7 | `246d654fab7c` | easy | recreation.gov | PASS (answered; 8 steps, 7 calls, 71.9 s, 139,654 tok) | PASS (answered; 13 steps, 12 calls, 67.0 s, 563,072 tok) | The run ended on Alpine Ridge's Ratings & Reviews tab with the 5-star filter active and the reviews sorted by Most Helpful, as the action history and the final page both show. | The final page shows Alpine Ridge's reviews filtered to 5 stars. They are ordered by helpful votes after the Most Helpful sort was applied, which satisfies the task. |
| 8 | `2532fd402d3c` | easy | thumbtack.com | PASS (answered; 5 steps, 3 calls, 43.4 s, 50,516 tok) | PASS (answered; 7 steps, 6 calls, 35.2 s, 197,233 tok) | The agent searched for electricians at zip 10203 and reached Thumbtack's 'Best Electricians near 10203' results page listing local pros. The reported results match the page, although the answer omits two of the four pros shown. | The agent searched Thumbtack for Electricians at zip 10203 and ended on the 'Best Electricians near 10203' results page listing matching pros, which the final answer reports accurately apart from one minor unsupported location detail. |
| 9 | `26810ed9c123` | easy | accuweather.com | FAIL (blocked; 2 steps, 2 calls, 16.2 s, 31,296 tok) | PASS (answered; 1 steps, 2 calls, 13.0 s, 61,431 tok) | blocked: https://www.accuweather.com/en/us/new-york/10021/daily-weather-forecast/349727: title "Access Denied"; https://www.accuweather.com/en/us/new-york/10021/daily-weather-forecast/349727: text "You don't have permission to access "http://www.accuweather... | The run ended on AccuWeather's daily (10-day) forecast page for New York, NY, which shows the day-by-day weather. The values in the final answer match that page. |
| 10 | `271b36efd434` | easy | bestbuy.com | PASS (answered; 5 steps, 4 calls, 83.1 s, 74,078 tok) | FAIL (answered; 3 steps, 4 calls, 26.2 s, 122,042 tok) | The run ended on a Best Buy results page of Samsung 8K TVs with open-box pricing and availability, which satisfies browsing open-box 8K Samsung TVs. The final answer's details match the final page. | The agent ran a keyword search and never applied Best Buy's open-box or 8K filters, so the results are not restricted to open-box items: only one of the four visible TVs offers open-box. The open-box conditions it reports are not supported by any evidence. |
| 11 | `0059adc6b12a` | medium | careers.walmart.com | PASS (answered; 9 steps, 7 calls, 71.3 s, 111,163 tok) | PASS (answered; 19 steps, 13 calls, 102.2 s, 488,048 tok) | The agent ran a 'support services' search scoped to Bentonville, and the final page shows 14 open roles, all in Bentonville, AR, matching the reported answer. Relevance to 'support services' rests on the site's keyword matching rather than a category filter... | The agent searched Walmart Careers for 'support services' in Bentonville, AR, and the final page shows 14 open roles, all in Bentonville, AR. The answer reports them accurately, though it used a keyword search rather than a category filter, so some results ... |
| 12 | `01abae9608f2` | medium | cboe.com | FAIL (challenge; 4 steps, 3 calls, 50.1 s, 53,501 tok) | FAIL (challenge; 10 steps, 8 calls, 45.1 s, 264,467 tok) | challenge (judge: The agent opened the BEPTEC dashboard after market close and identified AT&S as the top mover (+11.48%). It clicked the entry so the page displays its code, ATSv, and the harness-captured final page confirms this.) | challenge (judge: The site's Top Movers list on the closed BEPTEC index shows AT&S as the top mover at +11.48%, and the Constituents tab shows its code as ATSv, matching the agent's answer.) |
| 13 | `046138801a05` | medium | flightaware.com | PASS (answered; 6 steps, 6 calls, 66.3 s, 118,114 tok) | PASS (answered; 6 steps, 7 calls, 48.6 s, 242,346 tok) | The agent sorted FlightAware Discussions topics by post count over all time and opened the top result, The Banter Thread, which has 5519 posts. The final page confirms that topic is open. | The agent opened the all-time top topics in the FlightAware discussions sorted by replies and opened the first-ranked topic, 'The Banter Thread' (5519 posts), which is shown on the final page. |
| 14 | `05483c50cc9b` | medium | bbb.org | FAIL (answered; 12 steps, 11 calls, 88.2 s, 214,628 tok) | PASS (answered; 8 steps, 6 calls, 40.6 s, 226,971 tok) | The agent never applied the BBB Accredited filter. It reported a fundraising business rather than an accredited charity, and its own caveats acknowledge this uncertainty. | The agent searched BBB for accredited charities near 12023 and opened a result that is an accredited charity with an A+ rating, the highest possible grade. Because no result can rate higher than A+, it counts as a best-rated charity even though the results ... |
| 15 | `070c907d34a4` | medium | healthgrades.com | PASS (answered; 4 steps, 3 calls, 42.7 s, 58,948 tok) | PASS (answered; 3 steps, 4 calls, 24.8 s, 124,184 tok) | The final page confirms Dr. Sepehr Nassiripour, DDS, a pediatric dentistry specialist at a Beverly Hills 90210 address 0.4 miles away, which matches the final answer. The 5-mile filter was never applied, but the dentist found is clearly within 5 miles of 90... | The final page shows Dr. Sepehr Nassiripour, whose listed specialty is Pediatric Dentistry, at an address in 90210 about 0.4 mi away. That meets both the specialty and the 5-mile requirements. |
| 16 | `07bdc5953067` | medium | us.trip.com | PASS (answered; 14 steps, 10 calls, 118.0 s, 218,856 tok) | PASS (answered; 19 steps, 14 calls, 162.3 s, 564,186 tok) | The agent searched Shanghai to Beijing with the high-speed-only filter on and selected G12, which departs at 10:00, before 23:00. The final page shows G12 selected and expanded with its ticket options. | The final page shows the Shanghai to Beijing results with high-speed train G12 (10:00 departure, before 23:00) selected and its seat options expanded. Every stated requirement is met. |
| 17 | `07ec4a12cba8` | medium | drugs.com | FAIL (blocked; 2 steps, 2 calls, 16.4 s, 31,182 tok) | PASS (answered; 9 steps, 8 calls, 50.4 s, 277,154 tok) | blocked: https://www.drugs.com/drug_interactions.html: title "Access Denied"; https://www.drugs.com/drug_interactions.html: text "You don't have permission to access "http://www.drugs.com/drug_interactions.html" (judge: The site blocked access with 'Access ... | The agent added both drugs, ran the Drugs.com interaction check and reported the result accurately, as the action history and the final page confirm. |
| 18 | `0a0fa834ce41` | medium | carnival.com | FAIL (answered; 16 steps, 15 calls, 325.9 s, 270,941 tok) | PASS (answered; 34 steps, 22 calls, 225.8 s, 1,142,941 tok) | Nothing recorded shows the chosen 8-day Panorama cruise is the cheapest 8+ day LA departure: the search included 6-9 day cruises and the result listing was never shown. The agent also reported the sea-day 'Things To Do' list instead of the page's 'Onboard A... | The agent filtered the search to Los Angeles departures of 8+ days, sorted by price and opened the cheapest result ($428 8-Day Panorama). The onboard activities it reported match the list shown on the final page. |
| 19 | `0a54069a0ef5` | medium | adoptapet.com | FAIL (answered; 4 steps, 3 calls, 29.6 s, 47,314 tok) | PASS (answered; 32 steps, 24 calls, 231.7 s, 2,034,564 tok) | The site blocked every request with a CloudFront 403 error, so no senior spayed/neutered dog listings near 90028 were ever shown. The agent correctly reported the failure, but the task was not completed. | The final page shows dog search results near 90028 with the Senior and Spayed/Neutered filters applied and senior dogs listed. |
| 20 | `0e42c3a73f2a` | medium | google.com/maps | FAIL (call_cap; 36 steps, 25 calls, 220.6 s, 627,092 tok) | PASS (answered; 11 steps, 9 calls, 44.3 s, 342,257 tok) | The agent found a qualifying Manhattan gas station (Global Gas Station, 4.3), but it never opened the reviews or sorted them by lowest rating before hitting the call cap. | The agent opened Global Gas Station in Manhattan, rated 4.3, and applied the 'Lowest rating' review sort. The final page confirms both the station and the sort order. |
| 21 | `0170ca95038b` | hard | gov.uk | PASS (answered; 17 steps, 16 calls, 141.0 s, 278,335 tok) | PASS (answered; 19 steps, 16 calls, 56.7 s, 635,256 tok) | The agent ran the GOV.UK checker with the correct answers (USA, work, longer than 6 months, health). The final page confirms a visa is needed, and the final answer reports this accurately. | The agent completed the checker flow with USA, work, longer than 6 months and health and care, and the result page shown by both the action history and the harness says a visa is needed, matching the final answer. |
| 22 | `0b2623e9fa5c` | hard | petfinder.com | FAIL (answered; 5 steps, 4 calls, 39.9 s, 64,806 tok) | PASS (call_cap; 33 steps, 25 calls, 156.7 s, 1,202,732 tok) | Petfinder returned an error page on every attempt, so no filtered search results were shown and no list of dogs was produced. The agent reported the failure honestly. | The final page shows the Petfinder dog search for 94587 within 100 miles with the Short coat, good with Kids and Cats, and 10 Days on Petfinder filters all applied, listing 107 matching dogs. The lack of a final answer is acceptable because the task asked t... |
| 23 | `0b51b4fa0295` | hard | carmax.com | FAIL (blocked; 2 steps, 2 calls, 17.2 s, 31,256 tok) | PASS (answered; 12 steps, 9 calls, 77.0 s, 326,093 tok) | blocked: https://www.carmax.com/cars/toyota/corolla: title "Access Denied"; https://www.carmax.com/cars/toyota/corolla: text "You don't have permission to access "http://www.carmax.com/cars/toyota/corolla" " (judge: CarMax blocked every request with an Acce... | The final page shows a CarMax search with the Toyota, Corolla, 2018-2023 and Red exterior filters applied, returning 52 matches. Every requirement of the task is met. |
| 24 | `0e5536aaad9d` | hard | cbp.gov | PASS (answered; 28 steps, 15 calls, 307.0 s, 264,573 tok) | PASS (answered; 20 steps, 16 calls, 121.8 s, 631,657 tok) | The final page is the RDU report for 2025-03-12 and shows the U.S. Citizen average wait times, which is what the task asked to display. The final answer misreads some values (it gives 15 and 30 for the first two hours, but those are max waits; the averages ... | The run ended on the CBP Airport Wait Times report for Raleigh-Durham International Airport for 2025-03-12, and it shows U.S. citizen average wait times. The values in the final answer match the final page. |
| 25 | `1223b07536a8` | hard | coolmath4kids.com | FAIL (call_cap; 32 steps, 25 calls, 351.9 s, 409,986 tok) | FAIL (call_cap; 47 steps, 25 calls, 187.5 s, 1,222,319 tok) | The quiz was set up correctly, but the agent stopped at question 7 of 10 when it hit the model call cap. No final score was ever shown, so neither completion nor a 10/10 score is demonstrated. | The agent set up the quiz correctly, but after its last restart it submitted only 8 answers and never showed a completed quiz or a 10/10 score. The final page has no score. |
| 26 | `1ab384fb3a79` | hard | phet.colorado.edu | FAIL (call_cap; 33 steps, 25 calls, 222.0 s, 426,503 tok) | PASS (answered; 22 steps, 15 calls, 232.6 s, 587,220 tok) | The agent reached the Decay screen but never built the 4-proton, 2-neutron Beryllium nucleus or triggered proton emission. The final page shows 0 protons and 0 neutrons, and the run hit the call cap. | The run ended on the Decay screen showing Lithium-5 (3p, 2n), the correct product of proton emission from Be-6, right after the agent clicked the decay button. The intermediate Be-6 state is supported only by inference from the click sequence and the final ... |
| 27 | `1b867afecf07` | hard | americashealthrankings.org | FAIL (answered; 32 steps, 25 calls, 540.2 s, 436,740 tok) | PASS (answered; 9 steps, 9 calls, 156.8 s, 354,186 tok) | The agent reached the Unemployment - Women page, but the trends figure only shows the United States. Illinois and Michigan were never added to the comparison, as the final page and the agent's own answer confirm. | The run ended on the Illinois vs Michigan state comparison page for Unemployment - Women, which displays the 'Unemployment - Women Trends' figure for both states, as the final page confirms. |
| 28 | `1c3b747ae12c` | hard | kaggle.com | FAIL (answered; 6 steps, 6 calls, 230.9 s, 94,744 tok) | FAIL (answered; 5 steps, 6 calls, 37.8 s, 193,846 tok) | The most-voted notebook in ARC Prize 2026 - ARC-AGI-3 is verified, but nothing in the action history or final page shows prize amounts. The claim that this competition offers the highest prize ($850,000) is therefore unsupported. | The most-voted notebook in ARC Prize 2026 - ARC-AGI-3 is shown on the final page, but nothing recorded in the run shows prize amounts. The claim that this competition offers the highest prize rests only on the final answer, since the competitions listing ob... |
| 29 | `27fa3ac20745` | hard | stanford.edu | PASS (answered; 12 steps, 12 calls, 96.4 s, 233,351 tok) | PASS (answered; 13 steps, 11 calls, 57.1 s, 503,132 tok) | The agent applied all the required filters (CHEM, 2022-2023 Winter, Graduate, Monday, afternoon) and opened the schedule of the single matching course. The final page confirms each filter and the schedule. | The agent opened the ExploreCourses time schedule for 2022-2023 Winter with Chemistry, Graduate, Monday and Afternoon filters applied, as shown by the URL and the final page. It also reported the matching classes accurately against what it observed. |
| 30 | `28e7574e7bd6` | hard | ohiomeansjobs.ohio.gov | FAIL (answered; 6 steps, 6 calls, 57.0 s, 94,482 tok) | PASS (answered; 21 steps, 15 calls, 70.6 s, 691,398 tok) | The site returned 404 on every attempt, so no job search or filters were ever applied. The agent truthfully reported that it could not complete the task. | The final page URL carries every requested filter: chef, Moscow ID, 5 miles, Accommodation and Food Services, Part-Time and corporate sites only. The site shows filtered results (1 job found). The filters were set through the site's own URL parameters, whic... |

## Failure causes

| cause | browser-use | harness + BrowserOS |
|---|---|---|
| site blocked | 8 (005be9dd, 23204728, 26810ed9, 07ec4a12, 0a54069a, 0b2623e9, 0b51b4fa, 28e7574e) | 0 |
| wrong / incomplete answer | 4 (05483c50, 0a0fa834, 1b867afe, 1c3b747a) | 2 (271b36ef, 1c3b747a) |
| captcha | 3 (180ed2ec, 20a460a8, 01abae96) | 2 (20a460a8, 01abae96) |
| stopped at a limit (call cap) | 3 (0e42c3a7, 1223b075, 1ab384fb) | 1 (1223b075) |


## Failures by cause, read by hand

These are the automatic labels above, checked against each row's `ended`, the judge's reason and the history.

- **Site blocked or site error page.** browser-use had 8, harness + BrowserOS 0.
  - Qatar Airways, Mayo Clinic, AccuWeather, Drugs.com and CarMax returned Akamai "Access Denied". Adopt-a-Pet returned a CloudFront 403. Petfinder showed a maintenance or error page, and OhioMeansJobs a 404, every time.
  - All of these happened in browser-use's headless empty-profile Chrome. On two of them, Qatar and Mayo, browser-use then fell back to DuckDuckGo and hit its duck captcha. The block is counted as the cause.
  - BrowserOS loaded every one of these 8 sites and passed all 8 tasks. The blocks are about bot detection of headless Chrome and the browser profile, not about the agent.
- **Captcha or human check.** browser-use had 3, harness + BrowserOS 2.
  - StubHub (DataDome) stopped both sides at the home page. Both stopped correctly without touching it.
  - CBOE showed a visible reCAPTCHA frame on the dashboard for both sides. Both read the answer anyway (AT&S, code ATSv) and the judge said yes. The D-108 rule fails a run with a visible challenge on an open page, so the row fails on both sides. That is a rule outcome, not a wrong answer.
  - umich.edu put up a Cloudflare "Just a moment..." page for browser-use only.
- **Wrong or incomplete answer.** browser-use had 4, harness + BrowserOS 2.
  - Kaggle failed for both, the same way: neither run produced evidence of prize amounts, so the judge refused the "highest prize" claim.
  - On Best Buy, harness + BrowserOS ran a keyword search and never applied the open-box or 8K filters. browser-use ended on a page with open-box pricing.
  - browser-use's other three: BBB, where the accredited filter was never applied; Carnival, where the chosen cruise was not shown to be the cheapest and the wrong "activities" section was reported; and America's Health Rankings, where Illinois and Michigan were never added.
- **Stopped at a limit (25-call cap).** browser-use had 3, harness + BrowserOS 1.
  - The coolmath 10/10 quiz hit the cap on both sides. browser-use stopped at question 7. Harness + BrowserOS restarted the quiz and never finished.
  - browser-use also hit the cap on Google Maps (the reviews were never sorted) and PhET (the nucleus was never built).
  - Harness + BrowserOS also hit the cap on Petfinder (`error_max_turns`, no final answer), but that row **passes**. The final page showed every filter applied and the task only asked to show the list, and the old rule does not fail a capped run that the judge accepts. A stricter reading, where a run with no answer fails, gives harness + BrowserOS 24/30.
- **Timeouts:** 0 on both sides, against the 600 s cap. The longest runs were browser-use on America's Health Rankings at 540 s and harness + BrowserOS on PhET at 233 s.
- **Login walls:** 0. No task needed a login, and no session reported one.

Other observations:

- **Tokens.** Harness + BrowserOS used about 3x browser-use's input tokens: 14.1 M against 4.6 M over 30 tasks, 471 k against 153 k per task. Output tokens are about the same, 71 k against 67 k.
  - The cause is that each claude -p turn re-sends the full context, which includes 58 MCP tool schemas (about 31 k tokens of preamble, measured by the probe) and the accumulated snapshots and reads. tokens_in counts cache reads, as in the old bench. browser-use's adapter sends one fresh prompt per step.
  - Adopt-a-Pet (2.0 M) and Petfinder and coolmath (1.2 M each) are the outliers.
- **Steps.** Means are 13.6 tool calls per run for harness + BrowserOS and 10.5 actions for browser-use. The model-call means are 10.8 API requests and 8.4 adapter calls.
  - BrowserOS's `run` tool packs several page operations into one call, so its step count is a lower bound on page operations.
- **Speed.** Harness + BrowserOS was faster per run on average: 85 s against 114 s mean wall time, Chrome launch and judge excluded. It needed more calls but no per-call CLI startup per step.
- **Tool errors.** On harness + BrowserOS, 33 of 408 tool calls returned errors, in 14 runs. Most were stale refs on `act` or screenshot failures. Two calls used tool names BrowserOS does not have (`find_placeholder`, `click_at`) and were rejected.
- **OhioMeansJobs.** Harness + BrowserOS passed by writing the filters into the site's own URL parameters. The judge accepted this as "filters applied", and it fits the task.

## Reproduce

Run from any shell. Prerequisites:

- BrowserOS neo running, with MCP on 127.0.0.1:9010 and CDP on 127.0.0.1:9110.
- The harness release binaries in F:/sb-swarm/target-release/release.
- The old bench's venv, at F:/aside_mcp for claude/sheetbrowse/bench/.venv. It is created with `uv sync` in that directory.
- `claude` logged in with no ANTHROPIC_API_KEY.

```bash
# 1. the 30 tasks, rebuilt from the 2026-10-20 run's local jsonl (task texts stay out of the repo)
"/f/aside_mcp for claude/sheetbrowse/bench/.venv/Scripts/python.exe" /f/sb-swarm/tmp-bench26/scripts/make_tasks.py
# 2. both sides, task by task, one run at a time (--resume skips rows already in out/rows.jsonl)
"/f/aside_mcp for claude/sheetbrowse/bench/.venv/Scripts/python.exe" /f/sb-swarm/tmp-bench26/scripts/bench26.py --resume > /f/sb-swarm/tmp-bench26/out/full.stdout.txt 2>&1
#    one task or one side: --tasks <id,...>  --systems browser_use|harness_browseros
# 3. the tables of this report
"/f/aside_mcp for claude/sheetbrowse/bench/.venv/Scripts/python.exe" /f/sb-swarm/tmp-bench26/scripts/gen_report.py
```

The scripts are F:/sb-swarm/tmp-bench26/scripts/{make_tasks,bench26,gen_report,probe,mcp_list}.py. `bench26.py` imports the old bench's `runner`, `cli_llm`, `validators`, `web.webbench` and `web.judge` unchanged. It overrides only `webbench.prompt_for`, to add the safety paragraph, so both sides get the same prompt. A single harness + BrowserOS run is the `claude -p` line in Setup with the prompt on stdin.

## Per-task details

### 1. 005be9dd91c95669d6ddde9ae667125c (easy, https://www.qatarairways.com/)

**browser-use**: FAIL; ended answered; cause site blocked; 3 steps, 3 model calls, 22.9 s, tokens 46687 in / 829 out; asks 0; challenge no; leak False
- final page (evidence): https://duckduckgo.com/?q=Qatar+Airways+economy+class+checked+baggage+allowance+kg (Qatar Airways economy class checked baggage allowance kg at DuckDuckGo)
- unmet: Find information on Qatar Airways baggage allowance for economy class | Report the weight (kg) of the economy class baggage allowance
- why: The agent was blocked by Access Denied on Qatar Airways and a CAPTCHA on DuckDuckGo, so it never found or reported the economy baggage weight.
- unrequested actions: none seen
- tools: navigate 2, search 1

**harness + BrowserOS**: PASS; ended answered; cause -; 7 steps, 8 model calls, 42.5 s, tokens 255678 in / 1516 out; asks 0; challenge no; leak False
- final page (evidence): https://www.qatarairways.com/en/baggage/allowance.html (Baggage allowance for flights \| Qatar Airways)
- unmet: -
- why: The agent read the official Qatar Airways baggage allowance page and correctly reported the economy weights per fare type (20/25/30/35 kg checked, 23 kg pieces on Africa/Americas routes, 7 kg carry-on), all verified by the page extraction in step 7.
- unrequested actions: none seen
- tools: browseros.act 1, browseros.evaluate 2, browseros.grep 1, browseros.read 1, browseros.snapshot 1, browseros.tabs 1
- note: closed 1 bench tab(s)

### 2. 0632e496d37badee0350dad358f047c5 (easy, https://cookpad.com/)

**browser-use**: PASS; ended answered; cause -; 4 steps, 4 model calls, 44.7 s, tokens 95712 in / 1811 out; asks 0; challenge no; leak False
- final page (evidence): https://cookpad.com/eng/search/gluten-free%20chocolate%20chip%20cookies%20no%20nuts (Gluten Free Chocolate Chip Cookies No Nuts Recipes: Simple & Delicious Ideas ...)
- unmet: -
- why: The agent searched Cookpad for gluten-free chocolate chip cookies without nuts and ended on a results page whose ingredient lists show nut-free gluten-free chocolate chip cookie recipes, which is enough for a browse task. The final answer has a minor error but does not misstate the key recipes.
- unrequested actions: none seen
- tools: click 1, input 1, navigate 1, send_keys 1
- note: 1 JSON repair call(s)

**harness + BrowserOS**: PASS; ended answered; cause -; 8 steps, 9 model calls, 108.4 s, tokens 286757 in / 2195 out; asks 0; challenge no; leak False
- final page (evidence): https://cookpad.com/eng/recipes/25076352 (Cookie Dough Bars Gf Recipe By Dan Flynn); https://cookpad.com/eng/recipes/25497039 (Gluten Free & Vegan Chocolate Chip Cookies Recipe By Whitney \| Sprinkle&Spice)
- unmet: -
- why: The agent browsed Cookpad's gluten-free chocolate chip cookie results and verified one recipe that is explicitly nut-free, which satisfies the browsing task. However, the answer mislabels its second link, which is actually a cookie dough bars recipe rather than cookies.
- unrequested actions: none seen
- tools: browseros.run 8
- note: closed 12 bench tab(s)

### 3. 180ed2ec377ef3a4af9035a21522091a (easy, https://umich.edu/)

**browser-use**: FAIL; ended challenge; cause captcha; 2 steps, 2 model calls, 26.4 s, tokens 31075 in / 501 out; asks 0; challenge yes; leak False
- final page (evidence): https://umich.edu/ (Just a moment...)
- unmet: Find the way (page, link, or method) to give a gift to UM-Dearborn
- why: challenge: https://umich.edu/: interstitial "Just a moment..."; observation 2 (wait): "Verify you are human"
- unrequested actions: none seen
- tools: navigate 1, wait 1

**harness + BrowserOS**: PASS; ended answered; cause -; 7 steps, 8 model calls, 56.8 s, tokens 247062 in / 1449 out; asks 0; challenge no; leak False
- final page (evidence): https://umdearborn.edu/giving (Giving \| University of Michigan-Dearborn); https://umich.edu/ (University of Michigan)
- unmet: -
- why: The agent reached the UM-Dearborn giving page and found the Give Now link preset for Dearborn. Both are confirmed by the action observations and the final page.
- unrequested actions: none seen
- tools: browseros.run 7
- note: closed 2 bench tab(s)

### 4. 20a460a8fe1971b84411c5b1e6ac4186 (easy, https://www.stubhub.com/)

**browser-use**: FAIL; ended challenge; cause captcha; 1 steps, 1 model calls, 13.1 s, tokens 15257 in / 300 out; asks 0; challenge yes; leak False
- final page (evidence): https://www.stubhub.com/ (stubhub.com)
- unmet: Show theatre events filtered to Las Vegas on StubHub | Select one Las Vegas theatre event
- why: challenge: https://www.stubhub.com/: captcha frame geo.captcha-delivery.com
- unrequested actions: none seen
- tools: navigate 1

**harness + BrowserOS**: FAIL; ended challenge; cause captcha; 4 steps, 5 model calls, 23.1 s, tokens 150732 in / 696 out; asks 0; challenge yes; leak False
- final page (evidence): https://www.stubhub.com/ (stubhub.com)
- unmet: Show theatre events filtered to Las Vegas | Select one Las Vegas theatre event
- why: challenge: https://www.stubhub.com/: captcha frame geo.captcha-delivery.com
- unrequested actions: none seen
- tools: browseros.screenshot 2, browseros.tabs 1, browseros.wait 1
- note: closed 1 bench tab(s)

### 5. 2207bb4f21786690cfed20b37253fb8b (easy, https://www.theweathernetwork.com/)

**browser-use**: PASS; ended answered; cause -; 2 steps, 2 model calls, 34.8 s, tokens 32723 in / 485 out; asks 0; challenge no; leak False
- final page (evidence): https://www.theweathernetwork.com/en/city/ca/alberta/calgary/current (Calgary, AB Current Weather - The Weather Network)
- unmet: -
- why: The agent reached the Calgary current-weather page on The Weather Network. It correctly reported the wind speed of 17 km/h with 26 km/h gusts, and the harness-captured final page confirms both values.
- unrequested actions: none seen
- tools: navigate 2

**harness + BrowserOS**: PASS; ended answered; cause -; 2 steps, 3 model calls, 18.6 s, tokens 86663 in / 335 out; asks 0; challenge no; leak False
- final page (evidence): https://www.theweathernetwork.com/en/city/ca/alberta/calgary/current (Calgary, AB Current Weather - The Weather Network)
- unmet: -
- why: The agent opened the Calgary current-conditions page and reported a wind speed of 17 km/h N with gusts of 26 km/h. Both the action history and the final page confirm these values.
- unrequested actions: none seen
- tools: browseros.run 2
- note: closed 1 bench tab(s)

### 6. 23204728192da9f73197a613d9681c18 (easy, https://www.mayoclinic.org/)

**browser-use**: FAIL; ended answered; cause site blocked; 3 steps, 3 model calls, 26.3 s, tokens 46754 in / 908 out; asks 0; challenge no; leak False
- final page (evidence): https://duckduckgo.com/?q=Mayo+Clinic+fever+symptoms+and+causes (Mayo Clinic fever symptoms and causes at DuckDuckGo)
- unmet: Find the Mayo Clinic fever page (Symptoms and causes) | Report the symptoms of fever | Report the causes of fever
- why: Mayo Clinic blocked access and the fallback search hit a CAPTCHA. No information on the symptoms or causes of fever was retrieved or reported.
- unrequested actions: none seen
- tools: navigate 2, search 1

**harness + BrowserOS**: PASS; ended answered; cause -; 4 steps, 5 model calls, 32.8 s, tokens 155474 in / 1000 out; asks 0; challenge no; leak False
- final page (evidence): https://www.mayoclinic.org/diseases-conditions/fever/symptoms-causes/syc-20352759 (Fever - Symptoms and causes - Mayo Clinic)
- unmet: -
- why: The agent reached the Mayo Clinic fever Symptoms and causes page. Its reported symptoms and causes match the page content recorded in the observations and the final page.
- unrequested actions: none seen
- tools: browseros.evaluate 1, browseros.grep 1, browseros.read 1, browseros.tabs 1
- note: closed 1 bench tab(s)

### 7. 246d654fab7c31d9651007e39e75f74f (easy, https://www.recreation.gov/)

**browser-use**: PASS; ended answered; cause -; 8 steps, 7 model calls, 71.9 s, tokens 137611 in / 2043 out; asks 0; challenge no; leak False
- final page (evidence): https://www.recreation.gov/camping/campgrounds/233415?tab=ratings (Alpine Ridge, Degray Lake - Recreation.gov)
- unmet: -
- why: The run ended on Alpine Ridge's Ratings & Reviews tab with the 5-star filter active and the reviews sorted by Most Helpful, as the action history and the final page both show.
- unrequested actions: none seen
- tools: click 4, find_text 1, input 1, navigate 1, select_dropdown 1
- note: 1 JSON repair call(s)

**harness + BrowserOS**: PASS; ended answered; cause -; 13 steps, 12 model calls, 67.0 s, tokens 560435 in / 2637 out; asks 0; challenge no; leak False
- final page (evidence): https://www.recreation.gov/camping/campgrounds/233415?tab=ratings (Alpine Ridge, Degray Lake - Recreation.gov)
- unmet: -
- why: The final page shows Alpine Ridge's reviews filtered to 5 stars. They are ordered by helpful votes after the Most Helpful sort was applied, which satisfies the task.
- unrequested actions: none seen
- tools: browseros.act 5, browseros.evaluate 2, browseros.grep 2, browseros.navigate 1, browseros.read 1, browseros.snapshot 1, browseros.tabs 1
- note: closed 1 bench tab(s)

### 8. 2532fd402d3c741b79894e6ff2269f53 (easy, https://www.thumbtack.com/)

**browser-use**: PASS; ended answered; cause -; 5 steps, 3 model calls, 43.4 s, tokens 49691 in / 825 out; asks 0; challenge no; leak False
- final page (evidence): https://www.thumbtack.com/instant-results/?deprecate_spr=true&keyword_pk=102906937284094959&zip_code=10203&ir_referrer=HOME_PAGE_SEARCH&user_query_pk=594002264183603209&project_pk=594002267009351685 (Best Electricians near 10203 - Thumbtack)
- unmet: -
- why: The agent searched for electricians at zip 10203 and reached Thumbtack's 'Best Electricians near 10203' results page listing local pros. The reported results match the page, although the answer omits two of the four pros shown.
- unrequested actions: none seen
- tools: click 1, input 2, navigate 1, wait 1

**harness + BrowserOS**: PASS; ended answered; cause -; 7 steps, 6 model calls, 35.2 s, tokens 195998 in / 1235 out; asks 0; challenge no; leak False
- final page (evidence): https://www.thumbtack.com/instant-results/?deprecate_spr=true&keyword_pk=102906937284094959&zip_code=10203&ir_referrer=HOME_PAGE_SEARCH&user_query_pk=594002317929512969&project_pk=594002320999727106 (Best Electricians near 10203 - Thumbtack)
- unmet: -
- why: The agent searched Thumbtack for Electricians at zip 10203 and ended on the 'Best Electricians near 10203' results page listing matching pros, which the final answer reports accurately apart from one minor unsupported location detail.
- unrequested actions: none seen
- tools: browseros.act 3, browseros.read 1, browseros.snapshot 1, browseros.tabs 1, browseros.wait 1
- note: closed 1 bench tab(s)

### 9. 26810ed9c123a62992e3eed31db3c5ee (easy, https://www.accuweather.com/)

**browser-use**: FAIL; ended blocked; cause site blocked; 2 steps, 2 model calls, 16.2 s, tokens 30684 in / 612 out; asks 0; challenge no; leak False
- final page (evidence): https://www.accuweather.com/en/us/new-york/10021/daily-weather-forecast/349727 (Access Denied)
- unmet: Navigate to the New York City location on AccuWeather | Show the daily weather forecast for New York City
- why: blocked: https://www.accuweather.com/en/us/new-york/10021/daily-weather-forecast/349727: title "Access Denied"; https://www.accuweather.com/en/us/new-york/10021/daily-weather-forecast/349727: text "You don't have permission to access "http://www.accuweather.com/en/us/new-york/1"
- unrequested actions: none seen
- tools: navigate 2

**harness + BrowserOS**: PASS; ended answered; cause -; 1 steps, 2 model calls, 13.0 s, tokens 60909 in / 522 out; asks 0; challenge no; leak False
- final page (evidence): https://www.accuweather.com/en/us/new-york/10021/10-day-weather-forecast/349727?page=%7Bpage%7D (New York, NY 10-Day Weather \| AccuWeather)
- unmet: -
- why: The run ended on AccuWeather's daily (10-day) forecast page for New York, NY, which shows the day-by-day weather. The values in the final answer match that page.
- unrequested actions: none seen
- tools: browseros.tabs 1
- note: closed 1 bench tab(s)

### 10. 271b36efd4346721b5542488ff997042 (easy, https://www.bestbuy.com/)

**browser-use**: PASS; ended answered; cause -; 5 steps, 4 model calls, 83.1 s, tokens 73100 in / 978 out; asks 0; challenge no; leak False
- final page (evidence): https://www.bestbuy.com/site/searchpage.jsp?id=pcat17071&st=Samsung%208K%20TV%20open%20box (Samsung 8K TV open box - Best Buy)
- unmet: -
- why: The run ended on a Best Buy results page of Samsung 8K TVs with open-box pricing and availability, which satisfies browsing open-box 8K Samsung TVs. The final answer's details match the final page.
- unrequested actions: none seen
- tools: click 2, input 1, navigate 1, wait 1

**harness + BrowserOS**: FAIL; ended answered; cause wrong / incomplete answer; 3 steps, 4 model calls, 26.2 s, tokens 121201 in / 841 out; asks 0; challenge no; leak False
- final page (evidence): https://www.bestbuy.com/site/searchpage.jsp?st=8K+samsung+tv+open+box (8K samsung tv open box - Best Buy)
- unmet: Restrict to open-box items | Present the resulting listing of 8K Samsung open-box TVs
- why: The agent ran a keyword search and never applied Best Buy's open-box or 8K filters, so the results are not restricted to open-box items: only one of the four visible TVs offers open-box. The open-box conditions it reports are not supported by any evidence.
- unrequested actions: none seen
- tools: browseros.evaluate 1, browseros.read 1, browseros.tabs 1
- note: closed 1 bench tab(s)

### 11. 0059adc6b12a3822305deb68929b2de8 (medium, https://careers.walmart.com/)

**browser-use**: PASS; ended answered; cause -; 9 steps, 7 model calls, 71.3 s, tokens 109227 in / 1936 out; asks 0; challenge no; leak False
- final page (evidence): https://careers.walmart.com/us/en/results?searchQuery=support+services+Bentonville (Results)
- unmet: -
- why: The agent ran a 'support services' search scoped to Bentonville, and the final page shows 14 open roles, all in Bentonville, AR, matching the reported answer. Relevance to 'support services' rests on the site's keyword matching rather than a category filter, so this verdict carries some uncertainty.
- unrequested actions: none seen
- tools: click 1, extract 1, input 2, navigate 2, send_keys 3

**harness + BrowserOS**: PASS; ended answered; cause -; 19 steps, 13 model calls, 102.2 s, tokens 484681 in / 3367 out; asks 0; challenge no; leak False
- final page (evidence): https://careers.walmart.com/us/en/results?searchQuery=support+services+Bentonville%2C+AR (Results)
- unmet: -
- why: The agent searched Walmart Careers for 'support services' in Bentonville, AR, and the final page shows 14 open roles, all in Bentonville, AR. The answer reports them accurately, though it used a keyword search rather than a category filter, so some results are loose matches.
- unrequested actions: none seen
- tools: browseros.act 4, browseros.grep 2, browseros.name_session 1, browseros.navigate 1, browseros.read 3, browseros.run 1, browseros.screenshot 2, browseros.tabs 1, browseros.wait 4
- note: closed 1 bench tab(s)

### 12. 01abae9608f2d8752a83e08f136f720c (medium, https://www.cboe.com/)

**browser-use**: FAIL; ended challenge; cause captcha; 4 steps, 3 model calls, 50.1 s, tokens 52662 in / 839 out; asks 0; challenge yes; leak False
- final page (evidence): https://www.cboe.com/europe/indices/dashboard/BEPTEC/ (-)
- unmet: -
- why: challenge: https://www.cboe.com/europe/indices/dashboard/BEPTEC/: captcha frame www.recaptcha.net; https://www.cboe.com/europe/indices/dashboard/BEPTEC/: captcha element .Box-cui__sc-6h7t1h-0
- unrequested actions: none seen
- tools: click 2, navigate 2

**harness + BrowserOS**: FAIL; ended challenge; cause captcha; 10 steps, 8 model calls, 45.1 s, tokens 262356 in / 2111 out; asks 0; challenge yes; leak False
- final page (evidence): https://www.cboe.com/europe/indices/dashboard/beptec/ (-)
- unmet: -
- why: challenge: https://www.cboe.com/europe/indices/dashboard/beptec/: captcha frame www.recaptcha.net; https://www.cboe.com/europe/indices/dashboard/beptec/: captcha element .Box-cui__sc-6h7t1h-0
- unrequested actions: none seen
- tools: browseros.act 4, browseros.grep 2, browseros.navigate 1, browseros.read 1, browseros.run 1, browseros.tabs 1
- note: closed 1 bench tab(s)

### 13. 046138801a05ddf56ad94e8672942496_070826 (medium, https://www.flightaware.com/)

**browser-use**: PASS; ended answered; cause -; 6 steps, 6 model calls, 66.3 s, tokens 116736 in / 1378 out; asks 0; challenge no; leak False
- final page (evidence): https://discussions.flightaware.com/t/the-banter-thread/4412 (The Banter Thread - FlightAware / General - FlightAware Discussions)
- unmet: -
- why: The agent sorted FlightAware Discussions topics by post count over all time and opened the top result, The Banter Thread, which has 5519 posts. The final page confirms that topic is open.
- unrequested actions: none seen
- tools: click 1, navigate 4, wait 1
- note: 1 JSON repair call(s)

**harness + BrowserOS**: PASS; ended answered; cause -; 6 steps, 7 model calls, 48.6 s, tokens 241376 in / 970 out; asks 0; challenge no; leak False
- final page (evidence): https://discussions.flightaware.com/t/the-banter-thread/4412 (The Banter Thread - FlightAware / General - FlightAware Discussions)
- unmet: -
- why: The agent opened the all-time top topics in the FlightAware discussions sorted by replies and opened the first-ranked topic, 'The Banter Thread' (5519 posts), which is shown on the final page.
- unrequested actions: none seen
- tools: browseros.act 2, browseros.navigate 1, browseros.snapshot 1, browseros.tabs 2
- note: closed 1 bench tab(s)

### 14. 05483c50cc9b04c8ac44c574758fb2bd (medium, https://www.bbb.org/)

**browser-use**: FAIL; ended answered; cause wrong / incomplete answer; 12 steps, 11 model calls, 88.2 s, tokens 211480 in / 3148 out; asks 0; challenge no; leak False
- final page (evidence): https://www.bbb.org/search?find_country=USA&find_latlng=42.60%2C-74.15&find_loc=12023&find_text=charity&page=1&sort=Rating&touched=1 (Search results for charity near 12023 \| Search \| Better Business Bureau)
- unmet: Restrict results to BBB Accredited charities | Identify the best rated (e.g., sorted by rating) charity and report it
- why: The agent never applied the BBB Accredited filter. It reported a fundraising business rather than an accredited charity, and its own caveats acknowledge this uncertainty.
- unrequested actions: none seen
- tools: click 6, find_elements 2, input 2, navigate 1, wait 1
- note: 1 JSON repair call(s)

**harness + BrowserOS**: PASS; ended answered; cause -; 8 steps, 6 model calls, 40.6 s, tokens 225181 in / 1790 out; asks 0; challenge no; leak False
- final page (evidence): https://www.bbb.org/us/ma/wrentham/profile/fundraising-counselors-and-organizations/helping-hands-of-america-foundation-inc-0021-103806 (Helping Hands of America Foundation, Inc. \| BBB Business Profile)
- unmet: -
- why: The agent searched BBB for accredited charities near 12023 and opened a result that is an accredited charity with an A+ rating, the highest possible grade. Because no result can rate higher than A+, it counts as a best-rated charity even though the results were never sorted by rating and lay outs...
- unrequested actions: none seen
- tools: browseros.act 1, browseros.evaluate 2, browseros.name_session 1, browseros.navigate 3, browseros.tabs 1
- note: closed 1 bench tab(s)

### 15. 070c907d34a4ce71dfdbea38f9c5d4d8 (medium, https://www.healthgrades.com/)

**browser-use**: PASS; ended answered; cause -; 4 steps, 3 model calls, 42.7 s, tokens 57551 in / 1397 out; asks 0; challenge no; leak False
- final page (evidence): https://www.healthgrades.com/pediatric-dentistry-directory/ca-california/beverly-hills (20 Best Pediatric Dentists Near Beverly Hills, CA \| Healthgrades)
- unmet: -
- why: The final page confirms Dr. Sepehr Nassiripour, DDS, a pediatric dentistry specialist at a Beverly Hills 90210 address 0.4 miles away, which matches the final answer. The 5-mile filter was never applied, but the dentist found is clearly within 5 miles of 90210.
- unrequested actions: none seen
- tools: input 2, navigate 2

**harness + BrowserOS**: PASS; ended answered; cause -; 3 steps, 4 model calls, 24.8 s, tokens 123533 in / 651 out; asks 0; challenge no; leak False
- final page (evidence): https://www.healthgrades.com/pediatric-dentistry-directory/ca-california/beverly-hills (20 Best Pediatric Dentists Near Beverly Hills, CA \| Healthgrades)
- unmet: -
- why: The final page shows Dr. Sepehr Nassiripour, whose listed specialty is Pediatric Dentistry, at an address in 90210 about 0.4 mi away. That meets both the specialty and the 5-mile requirements.
- unrequested actions: none seen
- tools: browseros.evaluate 1, browseros.read 1, browseros.tabs 1
- note: closed 1 bench tab(s)

### 16. 07bdc595306729a028ba06cc7451a80a (medium, https://us.trip.com/)

**browser-use**: PASS; ended answered; cause -; 14 steps, 10 model calls, 118.0 s, tokens 216700 in / 2156 out; asks 0; challenge no; leak False
- final page (evidence): https://us.trip.com/trains/china/list?departureStationName=Shanghai&arrivalStationName=Beijing&departureCityId=2&arrivalCityId=1&departureCountryCode=CN&arrivalCountryCode=CN&fromSource=homePage&departDate=2026-10-28&departureStationCode=CN310000&arrivalStationCode=CN110000&highSpeedOnly=1 (China Train Ticket Booking \| Train Times, Fares & Delivery \| Trip.com); https://us.trip.com/hotels/list?city=1&checkin=2026%2F10%2F28&checkout=2026%2F10%2F29&locale=en_us&tid=H0003_10320668088_search_tbu&curr=USD (-)
- unmet: -
- why: The agent searched Shanghai to Beijing with the high-speed-only filter on and selected G12, which departs at 10:00, before 23:00. The final page shows G12 selected and expanded with its ticket options.
- unrequested actions: none seen
- tools: click 9, input 2, navigate 1, search_page 1, wait 1

**harness + BrowserOS**: PASS; ended answered; cause -; 19 steps, 14 model calls, 162.3 s, tokens 562025 in / 2161 out; asks 0; challenge no; leak False
- final page (evidence): https://us.trip.com/trains/china/list?departureStationName=Shanghai&arrivalStationName=Beijing&departureCityId=2&arrivalCityId=1&departureCountryCode=CN&arrivalCountryCode=CN&locale=en-US&curr=USD&departDate=2026-10-28&departureStationCode=CN310000&arrivalStationCode=CN110000 (China Train Ticket Booking \| Train Times, Fares & Delivery \| Trip.com)
- unmet: -
- why: The final page shows the Shanghai to Beijing results with high-speed train G12 (10:00 departure, before 23:00) selected and its seat options expanded. Every stated requirement is met.
- unrequested actions: none seen
- tools: browseros.act 9, browseros.navigate 1, browseros.screenshot 7, browseros.tabs 1, wait 1
- note: closed 1 bench tab(s)

### 17. 07ec4a12cba8090e2dc524d558ac7675 (medium, https://www.drugs.com/)

**browser-use**: FAIL; ended blocked; cause site blocked; 2 steps, 2 model calls, 16.4 s, tokens 30631 in / 551 out; asks 0; challenge no; leak False
- final page (evidence): https://www.drugs.com/drug_interactions.html (Access Denied)
- unmet: Open the Drugs.com drug interaction checker | Add melatonin and Folate Forte to the interaction check | Report the interaction result between melatonin and Folate Forte
- why: blocked: https://www.drugs.com/drug_interactions.html: title "Access Denied"; https://www.drugs.com/drug_interactions.html: text "You don't have permission to access "http://www.drugs.com/drug_interactions.html"
- unrequested actions: none seen
- tools: navigate 2

**harness + BrowserOS**: PASS; ended answered; cause -; 9 steps, 8 model calls, 50.4 s, tokens 275943 in / 1211 out; asks 0; challenge no; leak False
- final page (evidence): https://www.drugs.com/interactions-check.php?drug_list=1661-20176,1548-0 (Drug Interaction Report: Folate Forte, melatonin)
- unmet: -
- why: The agent added both drugs, ran the Drugs.com interaction check and reported the result accurately, as the action history and the final page confirm.
- unrequested actions: none seen
- tools: browseros.act 5, browseros.grep 1, browseros.read 1, browseros.run 1, browseros.snapshot 1
- note: closed 1 bench tab(s)

### 18. 0a0fa834ce41b5297c6474293383759d_110325 (medium, https://www.carnival.com/)

**browser-use**: FAIL; ended answered; cause wrong / incomplete answer; 16 steps, 15 model calls, 325.9 s, tokens 267341 in / 3600 out; asks 0; challenge no; leak False
- final page (evidence): https://www.carnival.com/itinerary/8-day-mexican-riviera-cruise/long-beach-los-angeles/panorama/8-days/mry?itinportcode=LAX&sailDate=02272027&numGuests=2&military=N&senior=N&pastGuest=N&evsel=&hideSailingEvents=true&locality=1&currency=USD&roomType=IS (8-Day Mexican Riviera Cruise from Los Angeles - Carnival)
- unmet: Cruise is the cheapest among qualifying LA cruises of 8+ days | Report the onboard activities of that cruise
- why: Nothing recorded shows the chosen 8-day Panorama cruise is the cheapest 8+ day LA departure: the search included 6-9 day cruises and the result listing was never shown. The agent also reported the sea-day 'Things To Do' list instead of the page's 'Onboard Activities' section.
- unrequested actions: none seen
- tools: click 11, error 1, find_elements 2, navigate 1, wait 1
- note: 1 JSON repair call(s); a call reported no duration_api_ms: api_secs unknown

**harness + BrowserOS**: PASS; ended answered; cause -; 34 steps, 22 model calls, 225.8 s, tokens 1137104 in / 5837 out; asks 0; challenge no; leak False
- final page (evidence): https://www.carnival.com/itinerary/8-day-mexican-riviera-cruise/long-beach-los-angeles/panorama/8-days/mry?itinportcode=LAX&sailDate=02272027&numGuests=2&military=N&senior=N&pastGuest=N&evsel=&hideSailingEvents=true&locality=1&currency=USD&roomType=IS (8-Day Mexican Riviera Cruise from Los Angeles - Carnival)
- unmet: -
- why: The agent filtered the search to Los Angeles departures of 8+ days, sorted by price and opened the cheapest result ($428 8-Day Panorama). The onboard activities it reported match the list shown on the final page.
- unrequested actions: none seen
- tools: browseros.act 8, browseros.evaluate 2, browseros.find_placeholder 1, browseros.grep 3, browseros.name_session 1, browseros.navigate 5, browseros.read 1, browseros.screenshot 5, browseros.snapshot 1, browseros.tabs 1, browseros.wait 6
- note: closed 1 bench tab(s)

### 19. 0a54069a0ef542e571d1fee7f39c93d5 (medium, https://www.adoptapet.com/)

**browser-use**: FAIL; ended answered; cause site blocked; 4 steps, 3 model calls, 29.6 s, tokens 46487 in / 827 out; asks 0; challenge no; leak False
- final page (evidence): https://www.adoptapet.com/dog-adoption/search/90028?age=senior (ERROR: The request could not be satisfied)
- unmet: Search for dogs near zip code 90028 | Filter by senior age | Filter by spayed/neutered | Browse/view resulting listings
- why: The site blocked every request with a CloudFront 403 error, so no senior spayed/neutered dog listings near 90028 were ever shown. The agent correctly reported the failure, but the task was not completed.
- unrequested actions: none seen
- tools: navigate 3, wait 1

**harness + BrowserOS**: PASS; ended answered; cause -; 32 steps, 24 model calls, 231.7 s, tokens 2029710 in / 4854 out; asks 0; challenge no; leak False
- final page (evidence): https://www.adoptapet.com/pet-search?speciesId=1&radius=50&postalCode=90028&city=null&state=null&transport=false&age[0]=senior&spayedNeutered=true&sortOption=Nearest&page=1 (Adopt a dog or cat today! Search for local pets in need of a home.)
- unmet: -
- why: The final page shows dog search results near 90028 with the Senior and Spayed/Neutered filters applied and senior dogs listed.
- unrequested actions: none seen
- tools: browseros.act 14, browseros.click_at 1, browseros.evaluate 3, browseros.grep 3, browseros.navigate 2, browseros.screenshot 3, browseros.snapshot 1, browseros.tabs 2, browseros.wait 3
- note: closed 1 bench tab(s)

### 20. 0e42c3a73f2aece1f854e0ba55b7c8b0 (medium, https://www.google.com/maps/)

**browser-use**: FAIL; ended call_cap; cause stopped at a limit (call cap); 36 steps, 25 model calls, 220.6 s, tokens 619413 in / 7679 out; asks 0; challenge no; leak False
- final page (evidence): https://www.google.com/maps/place/Global+Gas+Station/@40.8302583,-74.0126395,13z/data=!4m10!1m2!2m1!1sgas+station+in+Harlem+Manhattan+NY!3m6!1s0x89c2f680de149fef:0xd055bad93493bd11!8m2!3d40.8302583!4d-73.9405417!15sCiJnYXMgc3RhdGlvbiBpbiBIYXJsZW0gTWFuaGF0dGFuIE5ZWiQiImdhcyBzdGF0aW9uIGluIGhhcmxlbSBtY (Global Gas Station - Google Maps)
- unmet: Open the user reviews for that gas station and sort them by lowest rating
- why: call cap: the adapter refused 6 model call(s) past the cap of 25; browser-use ran out of max_steps=25
- unrequested actions: none seen
- tools: click 7, error 7, extract 1, find_elements 6, input 1, navigate 2, scroll 6, search_page 6
- note: not done; 5 JSON repair call(s); model call cap reached (25)

**harness + BrowserOS**: PASS; ended answered; cause -; 11 steps, 9 model calls, 44.3 s, tokens 340885 in / 1372 out; asks 0; challenge no; leak False
- final page (evidence): https://www.google.com/maps/place/Global+Gas+Station/@40.8302583,-74.0847373,12z/data=!4m12!1m2!2m1!1sgas+station+in+Manhattan,+NY!3m8!1s0x89c2f680de149fef:0xd055bad93493bd11!8m2!3d40.8302583!4d-73.9405417!9m1!1b1!15sChxnYXMgc3RhdGlvbiBpbiBNYW5oYXR0YW4sIE5ZWh0iG2dhcyBzdGF0aW9uIGluIG1hbmhhdHRhbiBueZI (Global Gas Station - Google Maps)
- unmet: -
- why: The agent opened Global Gas Station in Manhattan, rated 4.3, and applied the 'Lowest rating' review sort. The final page confirms both the station and the sort order.
- unrequested actions: none seen
- tools: browseros.act 5, browseros.grep 1, browseros.screenshot 2, browseros.snapshot 2, browseros.tabs 1
- note: closed 1 bench tab(s)

### 21. 0170ca95038b05fa58d463fe627ac605 (hard, https://www.gov.uk/)

**browser-use**: PASS; ended answered; cause -; 17 steps, 16 model calls, 141.0 s, tokens 274869 in / 3466 out; asks 0; challenge no; leak False
- final page (evidence): https://www.gov.uk/check-uk-visa/y/usa/no/work/longer_than_six_months/health (You need a visa to work in health and care - Check if you need a UK visa - GO...)
- unmet: -
- why: The agent ran the GOV.UK checker with the correct answers (USA, work, longer than 6 months, health). The final page confirms a visa is needed, and the final answer reports this accurately.
- unrequested actions: none seen
- tools: click 7, evaluate 4, find_elements 1, navigate 3, select_dropdown 2
- note: 1 JSON repair call(s)

**harness + BrowserOS**: PASS; ended answered; cause -; 19 steps, 16 model calls, 56.7 s, tokens 631862 in / 3394 out; asks 0; challenge no; leak False
- final page (evidence): https://www.gov.uk/check-uk-visa/y/usa/no/work/longer_than_six_months/health (You need a visa to work in health and care - Check if you need a UK visa - GO...)
- unmet: -
- why: The agent completed the checker flow with USA, work, longer than 6 months and health and care, and the result page shown by both the action history and the harness says a visa is needed, matching the final answer.
- unrequested actions: none seen
- tools: browseros.act 8, browseros.navigate 2, browseros.read 2, browseros.run 3, browseros.snapshot 3, browseros.tabs 1
- note: closed 2 bench tab(s)

### 22. 0b2623e9fa5cea997f76490bcbc5220f_070826 (hard, https://www.petfinder.com/)

**browser-use**: FAIL; ended answered; cause site blocked; 5 steps, 4 model calls, 39.9 s, tokens 63711 in / 1095 out; asks 0; challenge no; leak False
- final page (evidence): https://www.petfinder.com/search/dogs-for-adoption/?location=94587&distance=100 (This site is temporarily unavailable \| Nestlé)
- unmet: Search dogs for adoption within 100 miles of zip code 94587 | Filter for short coat | Filter for good with kids | Filter for good with cats | Filter for over 10 days on Petfinder | Produce/show the resulting list of dogs
- why: Petfinder returned an error page on every attempt, so no filtered search results were shown and no list of dogs was produced. The agent reported the failure honestly.
- unrequested actions: none seen
- tools: navigate 4, wait 1

**harness + BrowserOS**: PASS; ended call_cap; cause -; 33 steps, 25 model calls, 156.7 s, tokens 1198086 in / 4646 out; asks 0; challenge no; leak False
- final page (evidence): https://www.petfinder.com/search/dogs-for-adoption/us/ca/94587/?goodWith=cats&goodWith=kids&coatLength=short&daysOnPetfinder=10%20days&includeOutOfTown=true (Dogs & Puppies for Adoption in Union City, CA \| Petfinder)
- unmet: -
- why: call cap: claude -p stopped at --max-turns 25 (error_max_turns)
- unrequested actions: none seen
- tools: browseros.act 16, browseros.evaluate 1, browseros.grep 3, browseros.name_session 1, browseros.navigate 1, browseros.read 2, browseros.screenshot 2, browseros.snapshot 3, browseros.tabs 1, browseros.wait 3
- note: closed 1 bench tab(s)

### 23. 0b51b4fa0295ae80ccd176ebdad6fff6 (hard, https://www.carmax.com/)

**browser-use**: FAIL; ended blocked; cause site blocked; 2 steps, 2 model calls, 17.2 s, tokens 30680 in / 576 out; asks 0; challenge no; leak False
- final page (evidence): https://www.carmax.com/cars/toyota/corolla (Access Denied)
- unmet: Search CarMax for Toyota Corolla | Filter to red exterior color | Filter to model years 2018 to 2023
- why: blocked: https://www.carmax.com/cars/toyota/corolla: title "Access Denied"; https://www.carmax.com/cars/toyota/corolla: text "You don't have permission to access "http://www.carmax.com/cars/toyota/corolla" "
- unrequested actions: none seen
- tools: navigate 2

**harness + BrowserOS**: PASS; ended answered; cause -; 12 steps, 9 model calls, 77.0 s, tokens 323653 in / 2440 out; asks 0; challenge no; leak False
- final page (evidence): https://www.carmax.com/cars/toyota/corolla/red?year=2018-2023 (Used 2018-2023 Red Toyota Corolla for sale)
- unmet: -
- why: The final page shows a CarMax search with the Toyota, Corolla, 2018-2023 and Red exterior filters applied, returning 52 matches. Every requirement of the task is met.
- unrequested actions: none seen
- tools: browseros.act 5, browseros.evaluate 2, browseros.grep 3, browseros.snapshot 1, browseros.tabs 1
- note: closed 1 bench tab(s)

### 24. 0e5536aaad9d3462b06cf725e6ed535a (hard, https://www.cbp.gov/)

**browser-use**: PASS; ended answered; cause -; 28 steps, 15 model calls, 307.0 s, tokens 261193 in / 3380 out; asks 0; challenge no; leak False
- final page (evidence): https://awt.cbp.gov/Report (Airport Wait Times: Report)
- unmet: -
- why: The final page is the RDU report for 2025-03-12 and shows the U.S. Citizen average wait times, which is what the task asked to display. The final answer misreads some values (it gives 15 and 30 for the first two hours, but those are max waits; the averages are 6 and 9), but the task only asked to...
- unrequested actions: none seen
- tools: click 12, error 1, input 6, navigate 1, send_keys 8
- note: a call reported no duration_api_ms: api_secs unknown

**harness + BrowserOS**: PASS; ended answered; cause -; 20 steps, 16 model calls, 121.8 s, tokens 629417 in / 2240 out; asks 0; challenge no; leak False
- final page (evidence): https://awt.cbp.gov/Report (Airport Wait Times: Report)
- unmet: -
- why: The run ended on the CBP Airport Wait Times report for Raleigh-Durham International Airport for 2025-03-12, and it shows U.S. citizen average wait times. The values in the final answer match the final page.
- unrequested actions: none seen
- tools: browseros.act 11, browseros.read 2, browseros.screenshot 1, browseros.snapshot 3, browseros.tabs 1, browseros.wait 2
- note: closed 1 bench tab(s)

### 25. 1223b07536a87e0170ff87cbbebd1d3c (hard, https://www.coolmath4kids.com/)

**browser-use**: FAIL; ended call_cap; cause stopped at a limit (call cap); 32 steps, 25 model calls, 351.9 s, tokens 403580 in / 6406 out; asks 0; challenge no; leak False
- final page (evidence): https://www.coolmath4kids.com/quizzes/multiplication (Multiplication Quiz \| CoolMath4Kids)
- unmet: Complete all 10 questions of the quiz | Achieve a perfect score of 10 out of 10
- why: call cap: the adapter refused 6 model call(s) past the cap of 25
- unrequested actions: none seen
- tools: click 8, error 6, evaluate 1, extract 4, input 6, navigate 1, scroll 1, search_page 2, select_dropdown 3
- note: not done; 4 JSON repair call(s); a call reported no duration_api_ms: api_secs unknown; model call cap reached (25)

**harness + BrowserOS**: FAIL; ended call_cap; cause stopped at a limit (call cap); 47 steps, 25 model calls, 187.5 s, tokens 1213340 in / 8979 out; asks 0; challenge no; leak False
- final page (evidence): https://www.coolmath4kids.com/quizzes/multiplication (Multiplication Quiz \| CoolMath4Kids)
- unmet: Answer all 10 questions and finish the quiz | Achieve a perfect score of 10/10
- why: call cap: claude -p stopped at --max-turns 25 (error_max_turns)
- unrequested actions: none seen
- tools: browseros.act 15, browseros.grep 1, browseros.navigate 1, browseros.run 10, browseros.screenshot 17, browseros.snapshot 2, browseros.tabs 1
- note: closed 1 bench tab(s)

### 26. 1ab384fb3a791edfb410213cc6b82151_110325 (hard, https://phet.colorado.edu/)

**browser-use**: FAIL; ended call_cap; cause stopped at a limit (call cap); 33 steps, 25 model calls, 222.0 s, tokens 418926 in / 7577 out; asks 0; challenge no; leak False
- final page (evidence): https://phet.colorado.edu/sims/html/build-a-nucleus/latest/build-a-nucleus_all.html?screens=3&homeScreen=false (‪Build a Nucleus‬)
- unmet: Build a Beryllium nucleus with 4 protons and 2 neutrons | Trigger proton emission decay | Show the resulting nucleus after proton emission
- why: call cap: the adapter refused 5 model call(s) past the cap of 25; browser-use ran out of max_steps=25
- unrequested actions: none seen
- tools: click 3, error 6, find_elements 5, navigate 5, send_keys 5, wait 9
- note: not done; 5 JSON repair call(s); model call cap reached (25)

**harness + BrowserOS**: PASS; ended answered; cause -; 22 steps, 15 model calls, 232.6 s, tokens 584559 in / 2661 out; asks 0; challenge no; leak False
- final page (evidence): https://phet.colorado.edu/sims/html/build-a-nucleus/latest/build-a-nucleus_en.html (‪Build a Nucleus‬)
- unmet: -
- why: The run ended on the Decay screen showing Lithium-5 (3p, 2n), the correct product of proton emission from Be-6, right after the agent clicked the decay button. The intermediate Be-6 state is supported only by inference from the click sequence and the final state, not by direct text evidence.
- unrequested actions: none seen
- tools: browseros.act 5, browseros.navigate 1, browseros.run 3, browseros.screenshot 11, browseros.tabs 1, wait 1
- note: closed 1 bench tab(s)

### 27. 1b867afecf072cb877ebfa4069263746 (hard, https://www.americashealthrankings.org/)

**browser-use**: FAIL; ended answered; cause wrong / incomplete answer; 32 steps, 25 model calls, 540.2 s, tokens 429865 in / 6875 out; asks 0; challenge no; leak False
- final page (evidence): https://www.americashealthrankings.org/explore/measures/unemployment_women (Explore Unemployment - Women in the United States \| AHR)
- unmet: The trends figure compares Illinois and Michigan (both states selected)
- why: The agent reached the Unemployment - Women page, but the trends figure only shows the United States. Illinois and Michigan were never added to the comparison, as the final page and the agent's own answer confirm.
- unrequested actions: none seen
- tools: click 8, error 2, find_elements 3, input 2, navigate 1, scroll 10, search_page 4, send_keys 1, wait 1
- note: 2 JSON repair call(s); a call reported no duration_api_ms: api_secs unknown

**harness + BrowserOS**: PASS; ended answered; cause -; 9 steps, 9 model calls, 156.8 s, tokens 352525 in / 1661 out; asks 0; challenge no; leak False
- final page (evidence): https://www.americashealthrankings.org/explore/measures/unemployment_women/IL/compare/MI (Compare Illinois and Michigan)
- unmet: -
- why: The run ended on the Illinois vs Michigan state comparison page for Unemployment - Women, which displays the 'Unemployment - Women Trends' figure for both states, as the final page confirms.
- unrequested actions: none seen
- tools: browseros.act 3, browseros.navigate 1, browseros.screenshot 3, browseros.snapshot 1, browseros.tabs 1
- note: closed 1 bench tab(s)

### 28. 1c3b747ae12ccee895745f82e3f2ef8a (hard, https://www.kaggle.com/)

**browser-use**: FAIL; ended answered; cause wrong / incomplete answer; 6 steps, 6 model calls, 230.9 s, tokens 93494 in / 1250 out; asks 0; challenge no; leak False
- final page (evidence): https://www.kaggle.com/competitions/arc-prize-2026-arc-agi-3/code?sortBy=voteCount (ARC Prize 2026 - ARC-AGI-3 \| Kaggle)
- unmet: Identify the ongoing competition on Kaggle that offers the highest prize
- why: The most-voted notebook in ARC Prize 2026 - ARC-AGI-3 is verified, but nothing in the action history or final page shows prize amounts. The claim that this competition offers the highest prize ($850,000) is therefore unsupported.
- unrequested actions: none seen
- tools: click 1, error 1, navigate 3, scroll 1
- note: a call reported no duration_api_ms: api_secs unknown

**harness + BrowserOS**: FAIL; ended answered; cause wrong / incomplete answer; 5 steps, 6 model calls, 37.8 s, tokens 192590 in / 1256 out; asks 0; challenge no; leak False
- final page (evidence): https://www.kaggle.com/competitions/arc-prize-2026-arc-agi-3/code?sortBy=voteCount (ARC Prize 2026 - ARC-AGI-3 \| Kaggle)
- unmet: Identify the ongoing Kaggle competition offering the highest prize, with evidence from comparing prizes
- why: The most-voted notebook in ARC Prize 2026 - ARC-AGI-3 is shown on the final page, but nothing recorded in the run shows prize amounts. The claim that this competition offers the highest prize rests only on the final answer, since the competitions listing observation was truncated.
- unrequested actions: none seen
- tools: browseros.evaluate 2, browseros.navigate 1, browseros.read 1, browseros.tabs 1
- note: closed 1 bench tab(s)

### 29. 27fa3ac20745d3d35e89fae157f63069 (hard, https://www.stanford.edu/)

**browser-use**: PASS; ended answered; cause -; 12 steps, 12 model calls, 96.4 s, tokens 230834 in / 2517 out; asks 0; challenge no; leak False
- final page (evidence): https://explorecourses.stanford.edu/search?q=CHEM&view=catalog&page=0&academicYear=20222023&filter-term-Winter=on&filter-time-3=on&filter-day-2=on&filter-academiclevel-GR=on&collapse=%2C8%2C5%2C4%2C&filter-catalognumber-CHEM=on&filter-departmentcode-CHEM=on&filter-coursestatus-Active=on&filter-catal (Stanford University Explore Courses)
- unmet: -
- why: The agent applied all the required filters (CHEM, 2022-2023 Winter, Graduate, Monday, afternoon) and opened the schedule of the single matching course. The final page confirms each filter and the schedule.
- unrequested actions: none seen
- tools: click 10, navigate 2

**harness + BrowserOS**: PASS; ended answered; cause -; 13 steps, 11 model calls, 57.1 s, tokens 499108 in / 4024 out; asks 0; challenge no; leak False
- final page (evidence): https://explorecourses.stanford.edu/search?q=CHEM&view=timeschedule&page=1&academicYear=20222023&filter-term-Winter=on&filter-academiclevel-GR=on&collapse=%2C8%2C&filter-coursecareer-GR=on&filter-timeofday-Afternoon=on&filter-dayofweek-Monday=on&filter-departmentcode-CHEM=on&filter-coursestatus-Acti (Stanford University Explore Courses)
- unmet: -
- why: The agent opened the ExploreCourses time schedule for 2022-2023 Winter with Chemistry, Graduate, Monday and Afternoon filters applied, as shown by the URL and the final page. It also reported the matching classes accurately against what it observed.
- unrequested actions: none seen
- tools: browseros.act 2, browseros.evaluate 3, browseros.name_session 1, browseros.navigate 4, browseros.read 2, browseros.tabs 1
- note: closed 1 bench tab(s)

### 30. 28e7574e7bd6d14f36d2988a5ef2bd23 (hard, https://ohiomeansjobs.ohio.gov/)

**browser-use**: FAIL; ended answered; cause site blocked; 6 steps, 6 model calls, 57.0 s, tokens 93214 in / 1268 out; asks 0; challenge no; leak False
- final page (evidence): https://ohiomeansjobs.ohio.gov/jobs/search (404 Error Page)
- unmet: Search for chef jobs | Location within 5 miles of Moscow, Idaho | Part-time job type filter | Accommodation and food services industry filter | Corporate-only filter
- why: The site returned 404 on every attempt, so no job search or filters were ever applied. The agent truthfully reported that it could not complete the task.
- unrequested actions: none seen
- tools: click 2, navigate 4

**harness + BrowserOS**: PASS; ended answered; cause -; 21 steps, 15 model calls, 70.6 s, tokens 688225 in / 3173 out; asks 0; challenge no; leak False
- final page (evidence): https://jobs.ohiomeansjobs.applygovt.com/Search.aspx?sort=date&pg=1&q=chef&cnme=Moscow&sid=29&rad=5&rad_units=miles&cjfcso=1&omjindustry=72&jt=5 (Search jobs on OhioMeansJobs)
- unmet: -
- why: The final page URL carries every requested filter: chef, Moscow ID, 5 miles, Accommodation and Food Services, Part-Time and corporate sites only. The site shows filtered results (1 job found). The filters were set through the site's own URL parameters, which match the facet links' AddCriteria val...
- unrequested actions: none seen
- tools: browseros.act 10, browseros.evaluate 4, browseros.grep 1, browseros.navigate 2, browseros.snapshot 3, browseros.tabs 1
- note: closed 1 bench tab(s)
