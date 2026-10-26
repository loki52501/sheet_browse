//! T-13c: the Phase 1 ladder fixes in `api` through `harness_core::mcp::dispatch`
//! (bench/results/ladder-phase1-servers.md, rungs 2 and 4).
//!
//! * B-562 / D-567 (rung 2): `api` sends its own browser User-Agent; a caller's User-Agent or
//!   From is refused, Authorization / Proxy-Authorization / Cookie only from the vault, and an
//!   email address in any other header makes the call a Send that asks first.
//! * B-560 / D-565 / D-566 (rung 4): `pick` projects a large JSON answer so it fits in one
//!   call; JSON is read up to the JSON cap; a cut answer names its shape and suggests pick.
//! * The tool descriptions steer to site tools, pick, and citing this run's results.

mod common;

use common::*;
use harness_core::{AskOutcome, Class, ToolError};
use serde_json::{json, Value};

const HOST: &str = "api.example.com";

fn board_json(n: usize) -> Vec<u8> {
    let jobs: Vec<Value> = (0..n)
        .map(|i| {
            json!({
                "id": format!("id-{i}"), "title": format!("Role {i}"), "location": "Berlin",
                "jobUrl": format!("https://jobs.example.com/{i}"),
                "descriptionHtml": "<p>FIXTURE_HTML_MARKER lorem ipsum</p>".repeat(40),
                "descriptionPlain": "FIXTURE_PLAIN_MARKER ".repeat(80)
            })
        })
        .collect();
    serde_json::to_vec(&json!({"apiVersion": "1", "jobs": jobs})).unwrap()
}

async fn setup(routes: Vec<Route>) -> (Server, Env) {
    let server = Server::start(routes).await;
    let e = env(config(&server, &[HOST], no_tools())).await;
    (server, e)
}

fn no_tools() -> std::path::PathBuf {
    std::env::temp_dir().join("srv-web-t13c-no-tools")
}

fn json_route(path: &str, body: Vec<u8>) -> Route {
    route(Some(HOST), path, 200, "application/json", body)
}

fn url(path: &str) -> String {
    format!("http://{HOST}{path}")
}

// ------------------------------------------------------------------------------ B-562

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_sends_its_own_browser_user_agent() {
    let (server, e) = setup(vec![json_route("/x", b"{\"ok\":true}".to_vec())]).await;
    call(&e, "api", json!({"url": url("/x")}))
        .await
        .expect("api");
    let seen = server.requests();
    let ua = seen[0].header("user-agent").unwrap_or("");
    assert!(
        ua.starts_with("Mozilla/5.0 (Windows NT 10.0; Win64; x64)"),
        "{ua}"
    );
    assert!(seen[0].header("from").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identity_and_raw_credential_headers_are_refused_before_any_request() {
    let (server, e) = setup(vec![json_route("/x", b"{}".to_vec())]).await;
    let canary = canary();
    for (name, value) in [
        ("User-Agent", "research-bot (someone@example.com)"),
        ("User-Agent", "@vault(gh_token)"),
        ("From", "someone@example.com"),
        ("from", "@vault(gh_token)"),
        ("Authorization", "Bearer raw-token-value"),
        ("Proxy-Authorization", "Basic raw-token-value"),
        ("Cookie", "session=raw-token-value"),
    ] {
        let err = call(
            &e,
            "api",
            json!({"url": url("/x"), "headers": {name: value}}),
        )
        .await
        .expect_err(name);
        assert!(matches!(err, ToolError::Args(_)), "{name}: {err}");
        let m = err.to_string();
        assert!(m.contains("refused") && m.contains("B-562"), "{name}: {m}");
        assert!(!m.contains('\n'), "one line: {m}");
        // Rule 2 and no echo: neither the vault value nor the literal comes back.
        assert!(!m.contains(&canary), "{name}: vault value in a refusal");
        for literal in ["someone@example.com", "raw-token-value"] {
            assert!(!m.contains(literal), "{name}: value echoed: {m}");
        }
    }
    assert!(server.requests().is_empty(), "a refused call sent nothing");
    let dump = ledger_dump(&e);
    assert!(!dump.contains(&canary), "vault value in the ledger");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vault_cookie_is_sent_and_never_shown() {
    let (server, e) = setup(vec![json_route("/x", b"{\"ok\":1}".to_vec())]).await;
    let canary = canary();
    let url = format!("http://127.0.0.1:{}/x", server.addr.port());
    let out = call(
        &e,
        "api",
        json!({"url": url, "headers": {"Cookie": "sid=@vault(gh_token)"}}),
    )
    .await
    .expect("vault cookie");
    assert!(!out.text.contains(&canary));
    let seen = server.requests();
    assert!(seen[0]
        .header("cookie")
        .is_some_and(|c| c == format!("sid={canary}")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_email_in_a_header_is_a_send_that_asks_with_the_host_as_recipient() {
    let (server, e) = setup(vec![json_route("/x", b"{\"ok\":1}".to_vec())]).await;
    e.asker.push(AskOutcome::AnsweredSkip);
    let out = call(
        &e,
        "api",
        json!({"url": url("/x"), "headers": {"X-Contact": "someone@example.com"}}),
    )
    .await
    .expect("skipped, not an error");
    assert!(out.text.starts_with("skipped:"), "{}", out.text);
    assert!(server.requests().is_empty(), "skipped: nothing sent");
    let cards = e.asker.cards();
    assert_eq!(cards.len(), 1, "a Send asks");
    assert_eq!(cards[0].class, Class::Send);
    assert!(cards[0].what.contains(HOST), "{}", cards[0].what);
    let rows: Vec<String> = cards[0]
        .values
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    assert!(
        rows.iter()
            .any(|r| r.contains("new recipients") && r.contains(HOST)),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.contains("x-contact") && r.contains("someone@example.com")),
        "the card shows what would be sent: {rows:?}"
    );

    // Said yes: sent, and ledgered as Send.
    e.asker.push(AskOutcome::AnsweredProceed);
    call(
        &e,
        "api",
        json!({"url": url("/x"), "headers": {"X-Contact": "someone@example.com"}}),
    )
    .await
    .expect("proceeds");
    assert_eq!(server.requests().len(), 1);
    assert!(events(&e)
        .iter()
        .any(|x| x.tool == "api" && x.class == Some(Class::Send) && x.ok));

    // A header without personal data stays Read: no card.
    call(
        &e,
        "api",
        json!({"url": url("/x"), "headers": {"Accept": "application/json"}}),
    )
    .await
    .expect("read");
    assert_eq!(e.asker.cards().len(), 2);
}

// ------------------------------------------------------------------------------ B-560

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_answer_is_cut_with_its_shape_and_pick_fits_it_in_one_call() {
    let body = board_json(70);
    assert!(body.len() > 200_000);
    let (_server, e) = setup(vec![json_route("/board", body)]).await;

    let cut = call(&e, "api", json!({"url": url("/board")}))
        .await
        .expect("api");
    let last = cut.text.lines().last().unwrap_or("");
    assert!(last.starts_with("truncated: "), "{last}");
    assert!(last.contains("jobs (array of 70 objects)"), "{last}");
    assert!(last.contains("pick"), "{last}");
    assert!(cut.text.chars().count() < 21_500);

    let picked = call(
        &e,
        "api",
        json!({"url": url("/board"), "pick": ["jobs[].title", "jobs[].location", "jobs[].jobUrl"]}),
    )
    .await
    .expect("pick");
    assert!(!picked.text.contains("truncated:"), "{}", picked.text);
    for i in [0, 35, 69] {
        assert!(picked.text.contains(&format!("\"Role {i}\"")), "{i}");
        assert!(picked
            .text
            .contains(&format!("https://jobs.example.com/{i}")));
    }
    for banned in [
        "FIXTURE_HTML_MARKER",
        "FIXTURE_PLAIN_MARKER",
        "descriptionHtml",
    ] {
        assert!(!picked.text.contains(banned), "{banned}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pick_call_reads_json_up_to_the_json_cap() {
    let body = board_json(5);
    let server = Server::start(vec![json_route("/board", body.clone())]).await;
    let cfg = srv_web::Config {
        max_bytes: 1000,
        ..config(&server, &[HOST], no_tools())
    };
    assert!(body.len() > 1000 && body.len() < cfg.json_max_bytes);
    let e = env(cfg).await;
    let out = call(
        &e,
        "api",
        json!({"url": url("/board"), "pick": ["jobs[].title"]}),
    )
    .await
    .expect("api");
    assert!(out.text.contains("\"Role 4\""), "{}", out.text);
    assert!(!out.text.contains("cut at"), "{}", out.text);

    // Without pick the W-2 page cap holds (accept_t12): the cut is named and pick offered.
    let out = call(&e, "api", json!({"url": url("/board")}))
        .await
        .expect("api");
    assert!(
        out.text.contains("body cut at the 1000 byte cap")
            && out.text.contains("cannot be parsed")
            && out
                .text
                .contains("call again with pick, which reads JSON up to 32 MB"),
        "{}",
        out.text
    );
    assert!(!out.text.contains("Role 0"), "{}", out.text);

    // Over the JSON cap even with pick: the cap is named and the cut JSON is not shown.
    let cfg = srv_web::Config {
        max_bytes: 1000,
        json_max_bytes: 2000,
        ..config(&server, &[HOST], no_tools())
    };
    let e = env(cfg).await;
    let out = call(
        &e,
        "api",
        json!({"url": url("/board"), "pick": ["jobs[].title"]}),
    )
    .await
    .expect("api");
    assert!(
        out.text.contains("body cut at the 2000 byte cap") && out.text.contains("cannot be parsed"),
        "{}",
        out.text
    );
    assert!(!out.text.contains("call again with pick"), "{}", out.text);
    assert!(!out.text.contains("Role 0"), "{}", out.text);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_pick_is_refused_in_words() {
    let (server, e) = setup(vec![json_route("/x", b"{}".to_vec())]).await;
    let err = call(
        &e,
        "api",
        json!({"url": url("/x"), "pick": ["jobs[0].title"]}),
    )
    .await
    .expect_err("bad pick");
    assert!(matches!(err, ToolError::Args(_)), "{err}");
    assert!(err.to_string().contains("like jobs[].title"), "{err}");
    assert!(server.requests().is_empty());
}

// ------------------------------------------------------------------------- descriptions

#[tokio::test]
async fn descriptions_steer_to_site_tools_pick_and_this_runs_sources() {
    let e = env(srv_web::Config::default()).await;
    let d = |n: &str| e.registry.get(n).expect(n).description.clone();
    let fetch = d("fetch");
    assert!(
        fetch.contains("announces that site's tools") && fetch.contains("prefer them"),
        "{fetch}"
    );
    let api = d("api");
    assert!(
        api.contains("pick") && api.contains("jobs[].title"),
        "{api}"
    );
    let search = d("search");
    assert!(search.contains("Cite only what search and fetch returned in this run"));
    for (n, text) in [("fetch", &fetch), ("api", &api), ("search", &search)] {
        assert!(text.chars().count() < 700, "{n} description is long");
        assert!(!text.contains('\u{2014}'), "{n}: no em dash");
    }
}
