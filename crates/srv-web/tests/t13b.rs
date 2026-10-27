//! T-13b: follow-ups to the docs/03 step-2 acceptance through srv-web (W-7, T-3, T-4).
//! Concurrent repairs stay at version 2 (D-577); the same-run re-enable B-561 kept (D-581);
//! the class after a re-register; a site tool never sees a password value and stops on a
//! challenge (D-5, rule 3, rule 5, D-580); the ledger names the board list_jobs read (D-579).
//! Offline: local server plus DNS overrides, driven through `harness_core::mcp::dispatch`.

mod common;

use common::*;
use harness_core::memory::{LedgerMode, ToolStatus};
use harness_core::{dispatch, Class, MemoryStore, ToolError, ToolOutput, ToolRegistry};
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
const TOOL: &str = "ashbyhq.list_jobs";

const BOARD_OK: &str = r#"<nav><a href="/acme">All jobs</a></nav>
<main><h1>Open positions</h1><p>Senior Backend Engineer, Berlin</p><button>Apply</button></main>"#;
const BOARD_BROKEN: &str =
    r#"<main><h1>We are moving</h1><p>Our careers page is being rebuilt.</p></main>"#;
const BOARD_FIXED: &str = r#"<nav><a href="/acme">All jobs</a></nav>
<main><h1>Open positions</h1><p>Senior Backend Engineer, Berlin</p><button>Apply now</button></main>"#;

const PLANTED_FP: &str = r#"// @fingerprint link"All jobs" button"Apply""#;

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
        route(
            Some("api.ashbyhq.com"),
            "/posting-api/job-board/other",
            200,
            "application/json",
            ashby_json(),
        ),
    ]
}

/// Plants `@fingerprint` in the copy's list_jobs.js after `@gate`, and sets `@gate` to
/// `gate`.
fn plant(tools: &Path, gate: &str) -> PathBuf {
    let p = tools.join("ashbyhq.com").join("list_jobs.js");
    let src = std::fs::read_to_string(&p).unwrap();
    assert!(src.contains("// @gate Read\n") && !src.contains("@fingerprint"));
    let src = src.replacen(
        "// @gate Read\n",
        &format!("// @gate {gate}\n{PLANTED_FP}\n"),
        1,
    );
    std::fs::write(&p, src).unwrap();
    p
}

fn tools_line(text: &str) -> String {
    text.lines()
        .find(|l| l.starts_with("tools for this site: "))
        .unwrap_or("")
        .to_string()
}

async fn row(e: &Env) -> Option<harness_core::memory::ToolHealth> {
    e.store
        .tool_health("ashbyhq.com")
        .await
        .unwrap()
        .into_iter()
        .find(|h| h.name == "list_jobs")
}

async fn fetch_board(e: &Env) -> String {
    call(e, "fetch", json!({ "url": BOARD_URL }))
        .await
        .expect("fetch")
        .text
}

fn relocates(e: &Env) -> usize {
    events(e)
        .iter()
        .filter(|x| x.server == "core" && x.tool == "tools.relocate")
        .count()
}

/// A second srv-web server (another run's process) on the same store and tools folder.
struct Other {
    registry: ToolRegistry,
}

impl Other {
    fn new(server: &Server, tools: &Path) -> Other {
        Other {
            registry: ToolRegistry::new(srv_web::tools_with(config(
                server,
                &HOSTS,
                tools.to_path_buf(),
            ))),
        }
    }

    async fn call(&self, e: &Env, tool: &str, args: Value) -> Result<ToolOutput, ToolError> {
        dispatch("web", &self.registry, &e.harness, tool, args).await
    }
}

/// The broken page, the miss (needs_repair, disabled), then the fixed page fetched.
async fn broken_then_fixed(gate: &str) -> (tempfile::TempDir, PathBuf, PathBuf, Server, Env) {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let file = plant(&tools, gate);
    let server = Server::start(routes(BOARD_BROKEN)).await;
    let e = env(config(&server, &HOSTS, tools.clone())).await;
    fetch_board(&e).await;
    call(&e, TOOL, json!({})).await.expect_err("miss");
    assert_eq!(row(&e).await.unwrap().status, ToolStatus::NeedsRepair);
    server.set_routes(routes(BOARD_FIXED));
    let out = fetch_board(&e).await;
    assert!(tools_line(&out).contains(TOOL), "{out}");
    (tmp, tools, file, server, e)
}

// ---------------------------------------------------------------------------------------
// D-577: concurrent repairs

/// Two calls of the broken tool at once on the fixed page (the model calls list_jobs twice
/// in parallel): one relocates, the other waits for it and runs as a healthy tool. Version 2,
/// one tools.relocate event, one relocate note.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_repairs_in_one_server_leave_version_2() {
    let (_tmp, _tools, file, _server, e) = broken_then_fixed("Read").await;
    let (a, b) = tokio::join!(
        call(&e, TOOL, json!({})),
        call(&e, TOOL, json!({"q": "engineer"}))
    );
    let (a, b) = (a.expect("first"), b.expect("second"));
    let h = row(&e).await.unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Repaired, 2), "{h:?}");
    assert_eq!(relocates(&e), 1);
    let notes = [&a, &b]
        .iter()
        .filter(|o| o.text.contains("note: fingerprint relocated"))
        .count();
    assert_eq!(notes, 1, "{}\n---\n{}", a.text, b.text);
    let patched = std::fs::read_to_string(&file).unwrap();
    assert!(
        patched.contains(r#"// @fingerprint link"All jobs" button"Apply now""#),
        "{patched}"
    );
}

/// Two servers (two runs) that both saw the miss both repair on the fixed page, one after
/// the other: the second finds the file already patched, so the row stays at version 2.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_servers_repairing_the_same_tool_leave_version_2() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let file = plant(&tools, "Read");
    let server = Server::start(routes(BOARD_BROKEN)).await;
    let e = env(config(&server, &HOSTS, tools.clone())).await;
    let other = Other::new(&server, &tools);

    // Both servers offer the (still healthy) tool on the broken page, and both runs miss.
    fetch_board(&e).await;
    other
        .call(&e, "fetch", json!({ "url": BOARD_URL }))
        .await
        .expect("fetch b");
    call(&e, TOOL, json!({})).await.expect_err("miss a");
    other.call(&e, TOOL, json!({})).await.expect_err("miss b");
    let h = row(&e).await.unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::NeedsRepair, 1));

    server.set_routes(routes(BOARD_FIXED));
    fetch_board(&e).await;
    other
        .call(&e, "fetch", json!({ "url": BOARD_URL }))
        .await
        .expect("fetch b");
    let a = call(&e, TOOL, json!({})).await.expect("repair a");
    let after_a = std::fs::read_to_string(&file).unwrap();
    assert_eq!(row(&e).await.unwrap().version, 2);
    let b = other.call(&e, TOOL, json!({})).await.expect("repair b");
    let h = row(&e).await.unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Repaired, 2), "{h:?}");
    assert_eq!(relocates(&e), 1);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), after_a);
    // D-583: only the run that wrote the patch says so; the second says it found it done.
    assert!(
        a.text
            .contains("list_jobs.js patched to version 2, status repaired"),
        "{}",
        a.text
    );
    assert!(
        b.text.contains(
            "note: fingerprint already relocated by another run (was missing button\"Apply\"); \
             tools/ashbyhq.com/list_jobs.js is already at version 2, status repaired; this \
             call patched nothing"
        ),
        "{}",
        b.text
    );
    assert!(!b.text.contains("patched to version"), "{}", b.text);
}

/// D-527 (review-T-12b round 2 should-fix 3): the tool is retired after the first server's
/// repair, but its archive step failed (D-316), so the patched file is still in place. The
/// second server's repair finds the file already patched; it must not record that run as
/// clean or offer the retired tool again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retired_tool_is_not_revived_by_a_second_repair() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let file = plant(&tools, "Read");
    let server = Server::start(routes(BOARD_BROKEN)).await;
    let e = env(config(&server, &HOSTS, tools.clone())).await;
    let other = Other::new(&server, &tools);
    fetch_board(&e).await;
    other
        .call(&e, "fetch", json!({ "url": BOARD_URL }))
        .await
        .expect("fetch b");
    call(&e, TOOL, json!({})).await.expect_err("miss a");
    other.call(&e, TOOL, json!({})).await.expect_err("miss b");

    server.set_routes(routes(BOARD_FIXED));
    fetch_board(&e).await;
    other
        .call(&e, "fetch", json!({ "url": BOARD_URL }))
        .await
        .expect("fetch b");
    call(&e, TOOL, json!({})).await.expect("repair a");
    let after_a = std::fs::read_to_string(&file).unwrap();
    // Retired, file still in place (the archive step failed).
    let mut h = row(&e).await.unwrap();
    h.status = ToolStatus::Retired;
    h.why = Some("retired by the test".into());
    e.store.tool_health_set(h.clone()).await.unwrap();

    let b = other.call(&e, TOOL, json!({})).await.expect("run b");
    let after = row(&e).await.unwrap();
    assert_eq!(after.status, ToolStatus::Retired, "{after:?}");
    assert_eq!(
        (after.version, after.runs),
        (h.version, h.runs),
        "{after:?}"
    );
    assert_eq!(after.last_ok, h.last_ok, "{after:?}");
    assert!(
        other.registry.get(TOOL).is_none(),
        "the retired tool is offered again"
    );
    assert!(b.text.contains("retired"), "{}", b.text);
    assert!(!b.text.contains("patched to version"), "{}", b.text);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), after_a);
}

// ---------------------------------------------------------------------------------------
// D-581 (B-561): the same-run re-enable

/// T-4 as amended by D-561 / D-581: a missed tool stays off in this run on every page it
/// misses (here and in a fresh server, from the needs_repair row alone), is offered again in
/// the same run on a page it matches, and its clean run there repairs it. Back on a page it
/// misses it is refused again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_run_re_enable_only_on_a_page_the_fingerprint_holds_on() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    plant(&tools, "Read");
    let server = Server::start(routes(BOARD_BROKEN)).await;
    let e = env(config(&server, &HOSTS, tools.clone())).await;
    fetch_board(&e).await;
    call(&e, TOOL, json!({})).await.expect_err("miss");
    assert!(e.registry.get(TOOL).is_none());

    // Off on the page it misses: in this server, and in a fresh one (row only).
    assert!(!tools_line(&fetch_board(&e).await).contains(TOOL));
    let other = Other::new(&server, &tools);
    let out = other
        .call(&e, "fetch", json!({ "url": BOARD_URL }))
        .await
        .expect("fetch b");
    assert!(!tools_line(&out.text).contains(TOOL), "{}", out.text);
    assert!(other.registry.get(TOOL).is_none());

    // The same run, a page it matches: offered again and repaired by a clean run.
    server.set_routes(routes(BOARD_OK));
    assert!(tools_line(&fetch_board(&e).await).contains(TOOL));
    call(&e, TOOL, json!({})).await.expect("repair run");
    let h = row(&e).await.unwrap();
    assert_eq!((h.status, h.version, h.runs), (ToolStatus::Repaired, 2, 1));

    // Back on a page it misses: the call-time check refuses it again (T-4).
    server.set_routes(routes(BOARD_BROKEN));
    fetch_board(&e).await;
    let err = call(&e, TOOL, json!({})).await.expect_err("miss again");
    assert!(err.to_string().contains("needs_repair"), "{err}");
    assert_eq!(row(&e).await.unwrap().status, ToolStatus::NeedsRepair);
}

// ---------------------------------------------------------------------------------------
// Rule 4: a repair never lowers a class

/// After the relocate re-registers the patched tool, its class is still max(@gate, Read):
/// Read for the seeded tool's @gate Read, Fill for a copy whose @gate is Fill, in the
/// registry and in the gated call's ledger event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn class_after_relocate_is_still_max_of_gate_and_read() {
    for (gate, class) in [("Read", Class::Read), ("Fill", Class::Fill)] {
        let (_tmp, _tools, file, _server, e) = broken_then_fixed(gate).await;
        assert_eq!(e.registry.get(TOOL).unwrap().class, class, "{gate}");
        let run = call(&e, TOOL, json!({})).await.expect("repair run");
        assert!(run.text.contains("fingerprint relocated"), "{}", run.text);
        assert!(std::fs::read_to_string(&file)
            .unwrap()
            .contains(&format!("// @gate {gate}\n")));
        assert_eq!(e.registry.get(TOOL).unwrap().class, class, "{gate}");
        call(&e, TOOL, json!({})).await.expect("run after repair");
        let ev: Vec<_> = events(&e)
            .into_iter()
            .filter(|x| x.server == "web" && x.tool == TOOL && x.ok)
            .collect();
        assert_eq!(ev.len(), 2, "{gate}");
        assert!(ev.iter().all(|x| x.class == Some(class)), "{gate}: {ev:?}");
    }
}

// ---------------------------------------------------------------------------------------
// D-5, rule 5: password fields and challenges (docs/03 site tools, review-T-13 item 4)

const PW1: &str = "pw-value-not-real-a81";
const PW2: &str = "pw-value-not-real-b82";
const PW3: &str = "pw-value-not-real-c83";

/// A reader that returns everything `page` gives it.
const DUMP_JS: &str = r#"// @name dump
// @kind reader
// @match /*
// @gate Read
// @phase 1
export default function (page, args) {
  const ids = page.findAll('input').concat(page.findAll('password'), page.findAll('textbox'));
  const found = [];
  for (const q of ['password', 'input "Password"', 'textbox "Code"']) {
    try { found.push(page.find(q)); } catch (e) { found.push(String(e)); }
  }
  const tables = [];
  for (let i = 0; i < 4; i++) tables.push(page.table(String(i)));
  return {
    url: page.url, title: page.title, text: page.text(), main: page.text('main'),
    form: page.text('login'), ids, found, tables,
  };
}
"#;

fn login_page() -> Vec<u8> {
    format!(
        "<!doctype html><html><head><title>Sign in</title></head><body><main><h1>Sign in</h1>\
         <form id=\"login\"><label>Username <input name=\"u\" value=\"alice-visible\"></label>\
         <label>Password <input type=\"password\" name=\"pw\" value=\"{PW1}\"></label>\
         <label>Code <input autocomplete=\"current-password\" value=\"{PW2}\"></label>\
         <button>Sign in</button></form>\
         <table><tr><th>Field</th></tr><tr><td><input type=\"password\" value=\"{PW3}\"></td></tr></table>\
         </main></body></html>"
    )
    .into_bytes()
}

/// A site tool on a page with password fields gets the page through the same reader as
/// fetch: it can see that a field is filled, never its value (rule 2, rule 5). Nothing of
/// the values reaches the output or the ledger.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_site_tool_never_sees_a_password_value() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tmp.path().join("tools");
    std::fs::create_dir_all(tools.join("acmelogin.com")).unwrap();
    std::fs::write(tools.join("acmelogin.com").join("dump.js"), DUMP_JS).unwrap();
    let server = Server::start(vec![route(
        Some("portal.acmelogin.com"),
        "/login",
        200,
        "text/html",
        login_page(),
    )])
    .await;
    let e = env(config(&server, &["portal.acmelogin.com"], tools)).await;
    let out = call(
        &e,
        "fetch",
        json!({ "url": "http://portal.acmelogin.com/login" }),
    )
    .await
    .expect("fetch");
    assert!(
        tools_line(&out.text).contains("acmelogin.dump"),
        "{}",
        out.text
    );
    let run = call(&e, "acmelogin.dump", json!({})).await.expect("dump");
    // The tool did read the page: the fields' labels, the form and the table are there.
    for want in ["Username", "Password", "Code", "| Field |"] {
        assert!(run.text.contains(want), "{want}: {}", run.text);
    }
    // The same index in sheet mode shows a plain field's value, so values do reach the
    // index; the password ones are only ever 'filled' (H-2, B-5).
    let sheet = call(
        &e,
        "fetch",
        json!({ "url": "http://portal.acmelogin.com/login", "mode": "sheet" }),
    )
    .await
    .expect("sheet");
    assert!(sheet.text.contains("alice-visible"), "{}", sheet.text);
    let ledger = ledger_dump(&e);
    for pw in [PW1, PW2, PW3] {
        assert!(
            !sheet.text.contains(pw),
            "a password value reached the sheet"
        );
        assert!(!run.text.contains(pw), "a password value reached the tool");
        assert!(!out.text.contains(pw), "a password value reached fetch");
        assert!(!ledger.contains(pw), "a password value reached the ledger");
    }
}

fn challenge_api(status: u16, headers: Vec<(String, String)>, body: &str) -> Vec<Route> {
    let mut r = routes(BOARD_OK);
    r[1] = Route {
        host: Some("api.ashbyhq.com".into()),
        path: API_PATH.into(),
        reply: Reply::Fixed {
            status,
            headers,
            body: body.as_bytes().to_vec(),
        },
    };
    r
}

const CHALLENGE_MARKER: &str = "CHALLENGE_BODY_MARKER";
/// A same-host challenge path page.api and api must not follow a redirect to.
const CHALLENGE_PATH: &str = "/cdn-cgi/challenge-platform/h/g/orchestrate";

fn interstitial() -> String {
    format!("<html><head><title>Just a moment...</title></head><body><p>{CHALLENGE_MARKER}</p></body></html>")
}

/// The page.api answers that are a challenge (D-580, D-582), by case name: Cloudflare's
/// cf-mitigated header on a 403, a Turnstile widget in a 200 HTML answer, a 503 interstitial
/// without cf-mitigated, and redirects to a challenge URL on another host and on this one.
/// Each route list also serves the same-host challenge path, which must never be asked for.
fn challenge_cases() -> Vec<(&'static str, Vec<Route>)> {
    let html_type = || ("Content-Type".to_string(), "text/html".to_string());
    let with_path = |mut r: Vec<Route>| {
        r.push(route(
            Some("api.ashbyhq.com"),
            CHALLENGE_PATH,
            200,
            "text/html",
            interstitial(),
        ));
        r
    };
    let redirect_to = |loc: &str| {
        let mut r = routes(BOARD_OK);
        r[1] = redirect(Some("api.ashbyhq.com"), API_PATH, loc);
        with_path(r)
    };
    vec![
        (
            "403 cf-mitigated",
            with_path(challenge_api(
                403,
                vec![html_type(), ("cf-mitigated".into(), "challenge".into())],
                &interstitial(),
            )),
        ),
        (
            "200 turnstile",
            with_path(challenge_api(
                200,
                vec![html_type()],
                &format!(
                    "<html><head><title>Check</title></head><body><p>{CHALLENGE_MARKER}</p>\
                     <div class=\"cf-turnstile\" data-sitekey=\"k\"></div></body></html>"
                ),
            )),
        ),
        (
            "503 interstitial, no cf-mitigated",
            with_path(challenge_api(503, vec![html_type()], &interstitial())),
        ),
        (
            "redirect to challenges.cloudflare.com",
            redirect_to(
                "https://challenges.cloudflare.com/cdn-cgi/challenge-platform/h/b/turnstile",
            ),
        ),
        (
            "redirect to hcaptcha.com",
            redirect_to("http://www.hcaptcha.com/checkbox"),
        ),
        (
            "redirect to the same host's challenge path",
            redirect_to(CHALLENGE_PATH),
        ),
    ]
}

/// The call stopped on a challenge (D-5): class Challenge, the hand-back text, nothing of
/// the body anywhere, no tool health failure, one core Stop event, and the same-host
/// challenge path never requested.
async fn assert_challenge_stop(e: &Env, server: &Server, out: &ToolOutput, case: &str) {
    let stop = out
        .stopped
        .as_ref()
        .unwrap_or_else(|| panic!("{case}: did not stop: {}", out.text));
    assert_eq!(stop.class, Class::Challenge, "{case}");
    assert!(
        out.text.contains("I do not solve these"),
        "{case}: {}",
        out.text
    );
    assert!(!out.text.contains(CHALLENGE_MARKER), "{case}: body shown");
    assert!(!ledger_dump(e).contains(CHALLENGE_MARKER), "{case}");
    // Not the tool's failure: no health outcome was recorded.
    let rows = e.store.tool_health("ashbyhq.com").await.unwrap();
    assert!(rows.iter().all(|h| h.fails == 0), "{case}: {rows:?}");
    let stops = events(e)
        .iter()
        .filter(|x| x.server == "core" && x.mode == Some(LedgerMode::Stop))
        .count();
    assert_eq!(stops, 1, "{case}");
    assert!(
        server
            .requests()
            .iter()
            .all(|x| !x.target.starts_with("/cdn-cgi/")),
        "{case}: a redirect to a challenge was followed"
    );
}

/// D-580: a challenge reached through page.api stops the site tool call with class
/// Challenge, shows nothing of the body, and is not recorded as the tool's failure; a
/// redirect to a challenge URL (another host, or this host's challenge path) is not followed
/// and stops the same way, not as an ordinary refused redirect (review T-13b items 1, 2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_challenge_behind_page_api_stops_the_site_tool_call() {
    for (case, r) in challenge_cases() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = repo_tools_copy(tmp.path());
        let server = Server::start(r).await;
        let e = env(config(&server, &HOSTS, tools)).await;
        fetch_board(&e).await;
        let out = call(&e, TOOL, json!({}))
            .await
            .unwrap_or_else(|err| panic!("{case}: an error, not a stop: {err}"));
        assert_challenge_stop(&e, &server, &out, case).await;
    }
}

/// A reader that catches every page.api error, calls page.api again, and returns normally.
const PROBE_JS: &str = r#"// @name probe
// @kind reader
// @match /*
// @gate Read
// @phase 1
export default function (page, args) {
  const errs = [];
  for (let i = 0; i < 3; i++) {
    try {
      page.api('http://api.ashbyhq.com/posting-api/job-board/acme');
    } catch (e) {
      errs.push(String(e));
    }
  }
  return { swallowed: errs };
}
"#;

/// D-580, D-582 (review T-13b items 2, 3): a reader that catches the challenge error and
/// returns normally still ends as a challenge stop (the JS cannot swallow it), and its later
/// page.api calls in that call refuse without a request: the API host saw one request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reader_that_catches_the_challenge_still_stops_and_sends_nothing_more() {
    for (case, r) in challenge_cases() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = repo_tools_copy(tmp.path());
        std::fs::write(tools.join("ashbyhq.com").join("probe.js"), PROBE_JS).unwrap();
        let server = Server::start(r).await;
        let e = env(config(&server, &HOSTS, tools)).await;
        assert!(tools_line(&fetch_board(&e).await).contains("ashbyhq.probe"));
        let out = call(&e, "ashbyhq.probe", json!({}))
            .await
            .unwrap_or_else(|err| panic!("{case}: an error, not a stop: {err}"));
        assert_challenge_stop(&e, &server, &out, case).await;
        assert!(!out.text.contains("swallowed"), "{case}: {}", out.text);
        let api = server
            .requests()
            .iter()
            .filter(|x| x.host == "api.ashbyhq.com")
            .count();
        assert_eq!(api, 1, "{case}: page.api kept sending after the challenge");
    }
}

/// The `api` tool stops on a redirect to a challenge URL, on another host or this host's
/// challenge path, and does not follow it (review T-13b item 1, D-580).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_api_tool_stops_on_a_redirect_to_a_challenge() {
    for (case, r) in challenge_cases() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = repo_tools_copy(tmp.path());
        let server = Server::start(r).await;
        let e = env(config(&server, &HOSTS, tools)).await;
        let out = call(
            &e,
            "api",
            json!({ "url": format!("http://api.ashbyhq.com{API_PATH}") }),
        )
        .await
        .unwrap_or_else(|err| panic!("{case}: an error, not a stop: {err}"));
        assert_challenge_stop(&e, &server, &out, case).await;
    }
}

/// D-582 (review T-13b note a): a challenge on a site, met by a site tool's page.api or by
/// fetch of another page of it, drops that site's cached pages, so no site tool runs on the
/// older cached board page; fetching the board again (after the user's check) brings it back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_challenge_drops_the_sites_cached_pages() {
    // page.api meets the challenge.
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let (_, r) = challenge_cases().swap_remove(0);
    let server = Server::start(r).await;
    let e = env(config(&server, &HOSTS, tools)).await;
    fetch_board(&e).await;
    let out = call(&e, TOOL, json!({})).await.expect("stop");
    assert!(out.stopped.is_some(), "{}", out.text);
    let err = call(&e, TOOL, json!({})).await.expect_err("no cached page");
    assert!(
        err.to_string().contains("no page from ashbyhq.com"),
        "{err}"
    );
    let err = call(&e, "find", json!({ "q": "Apply", "url": BOARD_URL }))
        .await
        .expect_err("board page dropped");
    assert!(err.to_string().contains("not fetched in this run"), "{err}");
    let api = |s: &Server| {
        s.requests()
            .iter()
            .filter(|x| x.host == "api.ashbyhq.com")
            .count()
    };
    assert_eq!(api(&server), 1);

    // fetch meets the challenge on another page of the site.
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let mut r = routes(BOARD_OK);
    r.push(Route {
        host: Some("jobs.ashbyhq.com".into()),
        path: "/other".into(),
        reply: Reply::Fixed {
            status: 403,
            headers: vec![
                ("Content-Type".into(), "text/html".into()),
                ("cf-mitigated".into(), "challenge".into()),
            ],
            body: interstitial().into_bytes(),
        },
    });
    let server = Server::start(r).await;
    let e = env(config(&server, &HOSTS, tools)).await;
    fetch_board(&e).await;
    let out = call(
        &e,
        "fetch",
        json!({ "url": "http://jobs.ashbyhq.com/other" }),
    )
    .await
    .expect("stop");
    assert_eq!(out.stopped.map(|s| s.class), Some(Class::Challenge));
    let err = call(&e, TOOL, json!({})).await.expect_err("no cached page");
    assert!(
        err.to_string().contains("no page from ashbyhq.com"),
        "{err}"
    );
    assert_eq!(api(&server), 0);
    // After the user's check the model fetches the board again, and the tool runs.
    fetch_board(&e).await;
    let run = call(&e, TOOL, json!({})).await.expect("runs again");
    assert!(
        run.stopped.is_none() && run.text.contains("\"total\""),
        "{}",
        run.text
    );
}

/// D-5: a board page that is a challenge stops fetch, is never cached, and offers no site
/// tool, so no site tool can run on it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_challenge_page_offers_no_site_tool() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let mut r = routes(BOARD_OK);
    r[0] = Route {
        host: Some("jobs.ashbyhq.com".into()),
        path: "/acme".into(),
        reply: Reply::Fixed {
            status: 403,
            headers: vec![
                ("Content-Type".into(), "text/html".into()),
                ("cf-mitigated".into(), "challenge".into()),
            ],
            body: format!("<html><head><title>Just a moment...</title></head><body>{CHALLENGE_MARKER}</body></html>").into_bytes(),
        },
    };
    let server = Server::start(r).await;
    let e = env(config(&server, &HOSTS, tools)).await;
    let out = call(&e, "fetch", json!({ "url": BOARD_URL }))
        .await
        .expect("stopped, not an error");
    assert_eq!(
        out.stopped.as_ref().map(|s| s.class),
        Some(Class::Challenge)
    );
    assert!(!tools_line(&out.text).contains(TOOL), "{}", out.text);
    assert!(e.registry.get(TOOL).is_none());
    assert!(call(&e, TOOL, json!({})).await.is_err());
    let api = server
        .requests()
        .iter()
        .filter(|x| x.host == "api.ashbyhq.com")
        .count();
    assert_eq!(api, 0);
}

// ---------------------------------------------------------------------------------------
// D-579: the ledger names the board

/// list_jobs reads the board its org names, which need not be the page fetched last; the
/// ledger summary names it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_ledger_names_the_board_list_jobs_read() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let server = Server::start(routes(BOARD_OK)).await;
    let e = env(config(&server, &HOSTS, tools)).await;
    fetch_board(&e).await;
    let out = call(&e, TOOL, json!({"org": "other", "q": "engineer"}))
        .await
        .expect("other board");
    assert_eq!(
        out.summary.as_deref(),
        Some(r#"ashbyhq.list_jobs(org="other", q="engineer") ran on http://jobs.ashbyhq.com/acme"#)
    );
    call(&e, TOOL, json!({})).await.expect("page's board");
    let summaries: Vec<String> = events(&e)
        .into_iter()
        .filter(|x| x.server == "web" && x.tool == TOOL)
        .map(|x| x.result_summary)
        .collect();
    assert_eq!(
        summaries,
        [
            r#"ashbyhq.list_jobs(org="other", q="engineer") ran on http://jobs.ashbyhq.com/acme"#,
            "ashbyhq.list_jobs ran on http://jobs.ashbyhq.com/acme",
        ]
    );
}

/// D-579 (review T-13b item 6): the args label is model text in the ask card and the
/// ledger, so it is escaped and capped: no link form, no bracket, no line break or JS line
/// terminator, whitespace collapsed, and the argument list cut at 160 characters.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_args_label_is_escaped_and_capped_at_160_characters() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = repo_tools_copy(tmp.path());
    let server = Server::start(routes(BOARD_OK)).await;
    let e = env(config(&server, &HOSTS, tools)).await;
    fetch_board(&e).await;
    let q = format!(
        "see [this](#12)\nremaining: 3\u{2028}x\u{2029}y   z\t{}",
        "w".repeat(400)
    );
    let out = call(&e, TOOL, json!({ "q": q })).await.expect("runs");
    let summary = out.summary.clone().expect("summary");
    let tail = " ran on http://jobs.ashbyhq.com/acme";
    let label = summary
        .strip_prefix("ashbyhq.list_jobs(")
        .and_then(|s| s.strip_suffix(tail))
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or_else(|| panic!("{summary}"));
    assert_eq!(label.chars().count(), 160, "{label}");
    assert!(
        label.starts_with(r#"q="see (this)(#12)\nremaining: 3 x y z\twww"#),
        "{label}"
    );
    for bad in ["](#", "[", "]", "\n", "\u{2028}", "\u{2029}", "  "] {
        assert!(!summary.contains(bad), "{bad:?} in {summary}");
    }
    let ledger: Vec<String> = events(&e)
        .into_iter()
        .filter(|x| x.server == "web" && x.tool == TOOL)
        .map(|x| x.result_summary)
        .collect();
    assert_eq!(ledger, [summary]);
}
