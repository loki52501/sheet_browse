//! T-13: the docs/03 step-2 acceptance items through srv-web (W-7, T-3, T-4, D-560, D-561).
//! The seeded `tools/ashbyhq.com/list_jobs.js` runs against a local board for
//! `jobs.ashbyhq.com/acme` and a local Ashby posting API on `api.ashbyhq.com` (DNS override
//! seam, no network), driven through `harness_core::mcp::dispatch` with MemStore.
//!
//! Item 3 (an ephemeral tool deleted by reflect) belongs to T-16 and is not tested here.

mod common;

use common::*;
use harness_core::memory::ToolStatus;
use harness_core::{Class, MemoryStore};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A private copy of the repo's `tools/` (no `_archive`), so a relocate patches the copy.
fn repo_tools_copy(into: &Path) -> PathBuf {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if p.is_dir() {
                if e.file_name() != "_archive" {
                    copy(&p, &to.join(e.file_name()));
                }
            } else {
                std::fs::copy(&p, to.join(e.file_name())).unwrap();
            }
        }
    }
    let dst = into.join("tools");
    copy(&repo_root().join("tools"), &dst);
    dst
}

/// The Ashby posting API answer for org `acme` (toolstore's fixture, two listed jobs).
fn ashby_json() -> Vec<u8> {
    std::fs::read(repo_root().join("crates/toolstore/tests/fixtures/ashby_job_board_acme.json"))
        .expect("ashby fixture")
}

fn board(body: &str) -> Vec<u8> {
    format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Acme Jobs</title>\
         </head><body>{body}</body></html>"
    )
    .into_bytes()
}

const HOSTS: [&str; 2] = ["jobs.ashbyhq.com", "api.ashbyhq.com"];
const BOARD_URL: &str = "http://jobs.ashbyhq.com/acme";
const API_PATH: &str = "/posting-api/job-board/acme";

fn routes(board_body: &str) -> Vec<Route> {
    vec![
        route(
            Some("jobs.ashbyhq.com"),
            "/acme",
            200,
            "text/html; charset=utf-8",
            board(board_body),
        ),
        route(
            Some("api.ashbyhq.com"),
            API_PATH,
            200,
            "application/json",
            ashby_json(),
        ),
    ]
}

/// A board as a fetch sees it: a nav link back to the board and an apply button.
const BOARD_OK: &str = r#"<nav><a href="/acme">All jobs</a></nav>
<main><h1>Open positions</h1><p>Senior Backend Engineer, Berlin</p><button>Apply</button></main>"#;
/// The planted mismatch: none of the fingerprint's elements are on the page.
const BOARD_BROKEN: &str =
    r#"<main><h1>We are moving</h1><p>Our careers page is being rebuilt.</p></main>"#;
/// The page fixed, with the button renamed: `button"Apply"` relocates to `Apply now`.
const BOARD_FIXED: &str = r#"<nav><a href="/acme">All jobs</a></nav>
<main><h1>Open positions</h1><p>Senior Backend Engineer, Berlin</p><button>Apply now</button></main>"#;

const PLANTED_FP: &str = r#"// @fingerprint link"All jobs" button"Apply""#;

/// Plants `@fingerprint` in the copy's list_jobs.js, after `@gate`.
fn plant_fingerprint(tools: &Path) -> PathBuf {
    let p = tools.join("ashbyhq.com").join("list_jobs.js");
    let src = std::fs::read_to_string(&p).unwrap();
    assert!(
        !src.contains("@fingerprint"),
        "the seeded tool has no fingerprint"
    );
    let src = src.replacen(
        "// @gate Read\n",
        &format!("// @gate Read\n{PLANTED_FP}\n"),
        1,
    );
    assert!(src.contains(PLANTED_FP));
    std::fs::write(&p, src).unwrap();
    p
}

fn tools_line(text: &str) -> String {
    text.lines()
        .find(|l| l.starts_with("tools for this site: "))
        .unwrap_or("")
        .to_string()
}

fn expected_jobs() -> Value {
    json!({
        "org": "acme", "total": 2, "matched": 2, "offset": 0, "shown": 2,
        "jobs": [
            {"title": "Senior Backend Engineer", "location": "Berlin (remote)", "department": "Engineering",
             "url": "https://jobs.ashbyhq.com/acme/0a1b2c3d-0000-4000-8000-000000000001"},
            {"title": "Product Designer", "location": "Paris, France", "department": "Design",
             "url": "https://jobs.ashbyhq.com/acme/0a1b2c3d-0000-4000-8000-000000000002/application"}
        ]
    })
}

/// The JSON part of a site tool's output (notes follow it on their own lines).
fn json_part(text: &str) -> Value {
    let end = text.rfind("\n}").map(|i| i + 2).unwrap_or(text.len());
    serde_json::from_str(&text[..end]).unwrap_or_else(|e| panic!("{e}: {text}"))
}

async fn row(e: &Env) -> harness_core::memory::ToolHealth {
    e.store
        .tool_health("ashbyhq.com")
        .await
        .unwrap()
        .into_iter()
        .find(|h| h.name == "list_jobs")
        .expect("list_jobs health row")
}

async fn fetch_board(e: &Env) -> String {
    call(e, "fetch", json!({ "url": BOARD_URL }))
        .await
        .expect("fetch")
        .text
}

fn api_calls(server: &Server) -> usize {
    server
        .requests()
        .iter()
        .filter(|r| r.host == "api.ashbyhq.com")
        .count()
}

// ---------------------------------------------------------------------------------------
// docs/03 acceptance 1 (T-3, W-7, D-501)

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accept_1_seeded_list_jobs_loads_on_fetch_and_returns_its_json() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let server = Server::start(routes(BOARD_OK)).await;
    let e = env(config(&server, &HOSTS, tools)).await;

    let out = fetch_board(&e).await;
    assert_eq!(
        tools_line(&out),
        "tools for this site: ashbyhq.list_jobs(org?,offset?,q?) · generic.read_table(id?,max?)",
        "{out}"
    );
    let spec = e.registry.get("ashbyhq.list_jobs").expect("registered");
    assert_eq!(spec.class, Class::Read);

    let run = call(&e, "ashbyhq.list_jobs", json!({}))
        .await
        .expect("list_jobs");
    assert_eq!(json_part(&run.text), expected_jobs(), "{}", run.text);
    assert!(!run.text.contains("note:"), "{}", run.text);
    // Short text only: no description HTML or body text (CLAUDE.md rule 5).
    for banned in ["FIXTURE_HTML_MARKER", "FIXTURE_PLAIN_MARKER", "<p>"] {
        assert!(!run.text.contains(banned), "{banned}: {}", run.text);
    }

    // D-501: page.api reached api.ashbyhq.com (same registrable domain as the board),
    // as one JSON GET for the page's org.
    let api: Vec<Seen> = server
        .requests()
        .into_iter()
        .filter(|r| r.host == "api.ashbyhq.com")
        .collect();
    assert_eq!(api.len(), 1);
    assert_eq!(api[0].method, "GET");
    assert_eq!(api[0].target, API_PATH);
    assert_eq!(api[0].header("accept"), Some("application/json"));

    // An explicit org goes to that org's board on the same API host.
    let other = call(&e, "ashbyhq.list_jobs", json!({"org": "nobody"}))
        .await
        .expect_err("unknown org");
    assert!(other.to_string().contains("HTTP 404"), "{other}");

    // The clean run was recorded (a hand-written tool's first row is `saved`), and the gated
    // call is in the ledger at class Read.
    let h = row(&e).await;
    assert_eq!((h.status, h.version, h.runs), (ToolStatus::Saved, 1, 0));
    assert_eq!(h.fails, 1, "the 404 org run counts as a failure");
    let ev = events(&e);
    assert!(ev
        .iter()
        .any(|x| x.tool == "ashbyhq.list_jobs" && x.class == Some(Class::Read) && x.ok));
}

// ---------------------------------------------------------------------------------------
// docs/03 acceptance 2 (T-4, D-506, D-560, D-561)

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accept_2_planted_miss_flips_needs_repair_and_the_fixed_page_relocates_to_version_2() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let file = plant_fingerprint(&tools);
    let server = Server::start(routes(BOARD_BROKEN)).await;
    let e = env(config(&server, &HOSTS, tools)).await;

    // The planted mismatch: the tool is offered (its row is healthy), the run misses.
    let out = fetch_board(&e).await;
    assert!(tools_line(&out).contains("ashbyhq.list_jobs"), "{out}");
    let err = call(&e, "ashbyhq.list_jobs", json!({}))
        .await
        .expect_err("fingerprint miss");
    assert!(err.to_string().contains("needs_repair"), "{err}");
    let h = row(&e).await;
    assert_eq!((h.status, h.version), (ToolStatus::NeedsRepair, 1));
    assert!(
        h.why.as_deref().unwrap_or("").contains("fingerprint miss"),
        "{h:?}"
    );
    assert!(e.registry.get("ashbyhq.list_jobs").is_none());
    assert_eq!(api_calls(&server), 0, "a miss never runs the reader");

    // Still broken: not offered again.
    let out = fetch_board(&e).await;
    assert!(!tools_line(&out).contains("ashbyhq.list_jobs"), "{out}");
    assert!(e.registry.get("ashbyhq.list_jobs").is_none());

    // The page is fixed (the button is renamed): offered again, and the run relocates.
    server.set_routes(routes(BOARD_FIXED));
    let out = fetch_board(&e).await;
    assert!(tools_line(&out).contains("ashbyhq.list_jobs"), "{out}");
    let run = call(&e, "ashbyhq.list_jobs", json!({}))
        .await
        .expect("repair run");
    assert_eq!(json_part(&run.text), expected_jobs(), "{}", run.text);
    assert!(
        run.text.contains("note: fingerprint relocated")
            && run.text.contains("version 2")
            && run.text.contains("not committed"),
        "{}",
        run.text
    );
    let h = row(&e).await;
    assert_eq!((h.status, h.version), (ToolStatus::Repaired, 2), "{h:?}");
    assert!(
        h.why.as_deref().unwrap_or("").starts_with("relocated:"),
        "{h:?}"
    );
    let patched = std::fs::read_to_string(&file).unwrap();
    assert!(
        patched.contains(r#"// @fingerprint link"All jobs" button"Apply now""#),
        "{patched}"
    );
    assert!(!patched.contains(PLANTED_FP));
    assert!(patched.contains("// @gate Read") && patched.contains("// @phase 1"));
    let ev = events(&e);
    assert!(
        ev.iter()
            .any(|x| x.server == "core" && x.tool == "tools.relocate" && x.ok),
        "relocate ledger event"
    );

    // Rule 4 (T-13b): the re-registered tool keeps class max(@gate, Read) = Read.
    assert_eq!(
        e.registry
            .get("ashbyhq.list_jobs")
            .expect("re-registered")
            .class,
        Class::Read
    );

    // The patched tool is the one registered now: a plain match, no note, a clean run.
    let again = call(&e, "ashbyhq.list_jobs", json!({}))
        .await
        .expect("run after repair");
    assert!(!again.text.contains("note:"), "{}", again.text);
    let last = events(&e)
        .into_iter()
        .rfind(|x| x.server == "web" && x.tool == "ashbyhq.list_jobs")
        .expect("ledger event of the run after repair");
    assert_eq!((last.class, last.ok), (Some(Class::Read), true));
    let h = row(&e).await;
    assert_eq!((h.status, h.version, h.runs), (ToolStatus::Repaired, 2, 1));
    // And a later fetch offers it as a healthy tool.
    let out = fetch_board(&e).await;
    assert!(tools_line(&out).contains("ashbyhq.list_jobs"), "{out}");
}

/// A broken tool whose page comes back exactly as it was is repaired by a clean run: record
/// Ok moves needs_repair to repaired at version 2 (D-304); the file is not touched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broken_tool_on_a_matching_page_is_repaired_by_a_clean_run() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let file = plant_fingerprint(&tools);
    let before = std::fs::read_to_string(&file).unwrap();
    let server = Server::start(routes(BOARD_BROKEN)).await;
    let e = env(config(&server, &HOSTS, tools)).await;
    fetch_board(&e).await;
    call(&e, "ashbyhq.list_jobs", json!({}))
        .await
        .expect_err("miss");
    assert_eq!(row(&e).await.status, ToolStatus::NeedsRepair);

    server.set_routes(routes(BOARD_OK));
    let out = fetch_board(&e).await;
    assert!(tools_line(&out).contains("ashbyhq.list_jobs"), "{out}");
    let run = call(&e, "ashbyhq.list_jobs", json!({}))
        .await
        .expect("repair run");
    assert_eq!(json_part(&run.text), expected_jobs());
    let h = row(&e).await;
    assert_eq!((h.status, h.version, h.runs), (ToolStatus::Repaired, 2, 1));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
}

/// D-505 stands for a healthy tool: a partial match runs with a note and nothing is patched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_healthy_tool_on_a_partial_page_runs_with_a_note_and_is_not_patched() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let file = plant_fingerprint(&tools);
    let before = std::fs::read_to_string(&file).unwrap();
    let server = Server::start(routes(BOARD_FIXED)).await;
    let e = env(config(&server, &HOSTS, tools)).await;
    fetch_board(&e).await;
    let run = call(&e, "ashbyhq.list_jobs", json!({}))
        .await
        .expect("partial run");
    assert!(
        run.text
            .contains(r#"note: fingerprint partial, missing button"Apply""#),
        "{}",
        run.text
    );
    let h = row(&e).await;
    assert_eq!((h.status, h.version, h.runs), (ToolStatus::Saved, 1, 1));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    assert!(!events(&e).iter().any(|x| x.tool == "tools.relocate"));
}

/// B-560, D-565: page.api reads JSON up to its own cap (32 MB by default), not the 5 MB page
/// cap: a board answer larger than the page cap is listed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_api_reads_json_past_the_page_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let server = Server::start(routes(BOARD_OK)).await;
    let cfg = srv_web::Config {
        max_bytes: 1500,
        ..config(&server, &HOSTS, tools)
    };
    assert_eq!(cfg.json_max_bytes, 32 * 1024 * 1024);
    assert!(ashby_json().len() > 1500 && board(BOARD_OK).len() < 1500);
    let e = env(cfg).await;
    fetch_board(&e).await;
    let run = call(&e, "ashbyhq.list_jobs", json!({}))
        .await
        .expect("a board over the page cap is still read");
    assert_eq!(json_part(&run.text), expected_jobs(), "{}", run.text);
}

/// A posting-API answer with `n` listed jobs (and one unlisted), each carrying a long
/// description the tool must drop. Every 7th job is in Machine Learning; every 11th has
/// "ML" in its title; job 5's team (not department) is "Applied ML".
fn big_board_json(n: usize) -> Vec<u8> {
    let mut jobs: Vec<Value> = (0..n)
        .map(|i| {
            let title = if i % 11 == 0 {
                format!("Senior ML Engineer {i}")
            } else {
                format!("Role {i}")
            };
            let dept = if i % 7 == 0 {
                "Machine Learning"
            } else {
                "Sales"
            };
            let mut j = json!({
                "title": title, "department": dept, "location": "Remote",
                "isListed": true, "jobUrl": format!("https://jobs.ashbyhq.com/acme/{i}"),
                "descriptionHtml": "<p>FIXTURE_HTML_MARKER</p>".repeat(50)
            });
            if i == 5 {
                j["team"] = json!("Applied ML");
            }
            j
        })
        .collect();
    jobs.push(json!({"title": "Hidden ML", "isListed": false}));
    serde_json::to_vec(&json!({ "jobs": jobs })).unwrap()
}

fn big_routes(n: usize) -> Vec<Route> {
    let mut r = routes(BOARD_OK);
    r[1] = route(
        Some("api.ashbyhq.com"),
        API_PATH,
        200,
        "application/json",
        big_board_json(n),
    );
    r
}

fn titles(v: &Value) -> Vec<String> {
    v["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["title"].as_str().unwrap().to_string())
        .collect()
}

/// Ladder rung 4 (T-13c): a 66-job board is listed in two calls, 60 then 6, through
/// `next_offset`; the second page starts where the first ended and nothing repeats.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_jobs_pages_past_60_with_offset() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let server = Server::start(big_routes(66)).await;
    let e = env(config(&server, &HOSTS, tools)).await;
    fetch_board(&e).await;

    let first = call(&e, "ashbyhq.list_jobs", json!({}))
        .await
        .expect("page 1");
    let a = json_part(&first.text);
    assert_eq!(
        (
            &a["total"],
            &a["matched"],
            &a["offset"],
            &a["shown"],
            &a["next_offset"]
        ),
        (&json!(66), &json!(66), &json!(0), &json!(60), &json!(60)),
        "{}",
        first.text
    );
    assert!(!first.text.contains("FIXTURE_HTML_MARKER"));
    assert!(!first.text.contains("truncated:"), "{}", first.text);

    let second = call(&e, "ashbyhq.list_jobs", json!({"offset": a["next_offset"]}))
        .await
        .expect("page 2");
    let b = json_part(&second.text);
    assert_eq!(
        (&b["offset"], &b["shown"], &b["matched"]),
        (&json!(60), &json!(6), &json!(66)),
        "{}",
        second.text
    );
    assert!(b.get("next_offset").is_none(), "{}", second.text);
    let mut all = titles(&a);
    all.extend(titles(&b));
    assert_eq!(all.len(), 66);
    assert_eq!(all[60], "Role 60");
    assert_eq!(all[65], "Role 65");
    let mut dedup = all.clone();
    dedup.sort();
    dedup.dedup();
    assert_eq!(dedup.len(), 66, "no job twice");

    // Past the end: an empty page, not an error; a bad offset is refused in words.
    let past = call(&e, "ashbyhq.list_jobs", json!({"offset": 500}))
        .await
        .expect("past the end");
    assert_eq!(json_part(&past.text)["shown"], 0);
    let bad = call(&e, "ashbyhq.list_jobs", json!({"offset": -1}))
        .await
        .expect_err("negative offset");
    assert!(bad.to_string().contains("offset"), "{bad}");
}

/// D-569: 60 long job rows whose pretty JSON is over the 20 000-character output cap come
/// back compact and whole, so `next_offset` (after `jobs`) is not cut off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sixty_long_rows_are_shown_whole_with_next_offset() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let jobs: Vec<Value> = (0..66)
        .map(|i| {
            json!({"title": format!("{i:03} {}", "Staff Engineer, Platform ".repeat(6)),
                   "department": "Infrastructure Engineering", "location": "San Francisco, California",
                   "isListed": true, "jobUrl": format!("https://jobs.ashbyhq.com/acme/{i:08}-0000-4000-8000-000000000000")})
        })
        .collect();
    let mut r = routes(BOARD_OK);
    r[1] = route(
        Some("api.ashbyhq.com"),
        API_PATH,
        200,
        "application/json",
        serde_json::to_vec(&json!({ "jobs": jobs })).unwrap(),
    );
    let server = Server::start(r).await;
    let e = env(config(&server, &HOSTS, tools)).await;
    fetch_board(&e).await;
    let run = call(&e, "ashbyhq.list_jobs", json!({}))
        .await
        .expect("page 1");
    assert!(!run.text.contains("truncated:"), "{}", &run.text[..200]);
    let v: Value = serde_json::from_str(run.text.lines().next().unwrap()).expect("compact");
    assert!(
        serde_json::to_string_pretty(&v).unwrap().chars().count() > 20_000,
        "the fixture must be over the cap when pretty"
    );
    assert_eq!((&v["shown"], &v["next_offset"]), (&json!(60), &json!(60)));
}

/// Ladder rung 4 (T-13c): `q` is a case-insensitive substring over title, department and
/// team, so "ML roles" is one call; `matched` counts the filtered jobs, `total` the board.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_jobs_q_filters_title_department_and_team() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let server = Server::start(big_routes(66)).await;
    let e = env(config(&server, &HOSTS, tools)).await;
    fetch_board(&e).await;

    let run = call(&e, "ashbyhq.list_jobs", json!({"q": "ml"}))
        .await
        .expect("q");
    let v = json_part(&run.text);
    // Titles with "ML" (i % 11 == 0), job 5's team; "Machine Learning" holds no "ml".
    let want: Vec<String> = (0..66)
        .filter(|i| i % 11 == 0 || *i == 5)
        .map(|i| {
            if i % 11 == 0 {
                format!("Senior ML Engineer {i}")
            } else {
                format!("Role {i}")
            }
        })
        .collect();
    assert_eq!(titles(&v), want, "{}", run.text);
    assert_eq!(v["total"], 66);
    assert_eq!(v["matched"], want.len());
    assert!(v.get("next_offset").is_none());
    assert!(!run.text.contains("Hidden ML"), "unlisted jobs stay out");

    let dept = call(&e, "ashbyhq.list_jobs", json!({"q": "  MACHINE learning "}))
        .await
        .expect("department");
    let d = json_part(&dept.text);
    assert_eq!(d["matched"], (0..66).filter(|i| i % 7 == 0).count());
    assert!(titles(&d).contains(&"Role 7".to_string()));

    let none = call(&e, "ashbyhq.list_jobs", json!({"q": "astronaut"}))
        .await
        .expect("no hits");
    let n = json_part(&none.text);
    assert_eq!((&n["matched"], &n["shown"]), (&json!(0), &json!(0)));
}

/// The site tool's description carries the first paragraph of its header prose (D-568), so
/// the model learns offset and q before its first call; later notes stay out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_jobs_description_explains_offset_and_q() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let server = Server::start(routes(BOARD_OK)).await;
    let e = env(config(&server, &HOSTS, tools)).await;
    fetch_board(&e).await;
    let spec = e.registry.get("ashbyhq.list_jobs").expect("registered");
    for want in ["60 per call", "next_offset", "q keeps jobs", "department"] {
        assert!(
            spec.description.contains(want),
            "{want}: {}",
            spec.description
        );
    }
    assert!(!spec.description.contains("D-561"), "{}", spec.description);
    assert_eq!(spec.args["properties"]["offset"]["type"], "number");
    assert_eq!(spec.args["properties"]["q"]["type"], "string");
}

/// B-560: a board over the body cap fails with the cap named, not "not JSON" (a cut JSON
/// body never parses). page.api's cap is `json_max_bytes` (D-565).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_board_over_the_cap_names_the_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let server = Server::start(routes(BOARD_OK)).await;
    let cfg = srv_web::Config {
        json_max_bytes: 1500,
        ..config(&server, &HOSTS, tools)
    };
    assert!(ashby_json().len() > 1500);
    let e = env(cfg).await;
    fetch_board(&e).await;
    let err = call(&e, "ashbyhq.list_jobs", json!({}))
        .await
        .expect_err("over the cap");
    assert!(
        err.to_string()
            .contains("api.ashbyhq.com answered more than the 1500 byte cap"),
        "{err}"
    );
}

// ---------------------------------------------------------------------------------------
// Live (network): `cargo test -p srv-web --test accept_t13 -- --ignored`

/// The seeded tool against a real public Ashby board. `SB_ASHBY_ORG` picks the org
/// (default `ashby`, Ashby's own board; `linear` also passed on 2026-10-26; `openai`, whose
/// posting API answer is over 5 MB, needs page.api's 32 MB JSON cap, B-560 / D-565).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "network: fetches jobs.ashbyhq.com and api.ashbyhq.com"]
async fn live_ashby_board_list_jobs() {
    let org = std::env::var("SB_ASHBY_ORG")
        .ok()
        .filter(|o| !o.trim().is_empty())
        .unwrap_or_else(|| "ashby".into());
    let cfg = srv_web::Config {
        tools_root: repo_root().join("tools"),
        ..srv_web::Config::default()
    };
    let e = env(cfg).await;
    let url = format!("https://jobs.ashbyhq.com/{org}");
    let out = call(&e, "fetch", json!({ "url": url }))
        .await
        .expect("fetch");
    assert!(
        tools_line(&out.text).contains("ashbyhq.list_jobs(org?,offset?,q?)"),
        "{}",
        out.text
    );
    let run = call(&e, "ashbyhq.list_jobs", json!({}))
        .await
        .expect("list_jobs");
    let v = json_part(&run.text);
    assert_eq!(v["org"], org.as_str());
    assert!(v["total"].as_u64().unwrap_or(0) > 0, "{}", run.text);
    let first = &v["jobs"][0];
    assert!(
        !first["title"].as_str().unwrap_or("").is_empty(),
        "{}",
        run.text
    );
    assert!(
        first["url"]
            .as_str()
            .unwrap_or("")
            .starts_with("https://jobs.ashbyhq.com/"),
        "{}",
        run.text
    );
}
