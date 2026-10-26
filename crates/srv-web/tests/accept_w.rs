//! docs/04 "Acceptance (step 2 and 3)", the srv-web items, offline: a local HTTP server
//! answers for docs.rs and api.github.com through the Config DNS override, with a saved
//! copy of https://docs.rs/tokio (fixtures/docs_rs_tokio.html) and of the GitHub repo JSON.
//! Every call goes through harness_core::mcp::dispatch (classify, gate, scrub, ledger).

mod common;

use common::*;
use harness_core::memory::LedgerMode;
use harness_core::Class;
use serde_json::json;
use std::path::PathBuf;

fn no_tools() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no-tools")
}

async fn docs_rs() -> (Server, Env) {
    let server = Server::start(vec![
        redirect(Some("docs.rs"), "/tokio", "/tokio/latest/tokio/"),
        route(
            Some("docs.rs"),
            "/tokio/latest/tokio/",
            200,
            "text/html; charset=utf-8",
            fixture("docs_rs_tokio.html"),
        ),
    ])
    .await;
    let e = env(config(&server, &["docs.rs"], no_tools())).await;
    (server, e)
}

/// The read part of a fresh fetch: everything after the skeleton.
fn read_part(text: &str) -> &str {
    text.split_once("\n\n").map(|(_, r)| r).unwrap_or("")
}

/// Prose paragraphs of a read chunk (no fences, outline, cursor or chunk header).
fn prose(md: &str) -> Vec<String> {
    let body = md.split("\n\nremaining:").next().unwrap_or("");
    body.split("\n\n")
        .map(str::trim)
        .filter(|p| {
            p.len() > 80
                && !p.contains("```")
                && !p.starts_with("chunk ")
                && !p.starts_with("cursor:")
        })
        .map(str::to_string)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn docs_rs_read_heading_no_nav_remaining_and_cursor() {
    let (server, e) = docs_rs().await;
    let out = call(
        &e,
        "fetch",
        json!({"url": "http://docs.rs/tokio", "mode": "read"}),
    )
    .await
    .expect("fetch docs.rs/tokio");
    let read = read_part(&out.text);

    // First heading is the crate name.
    let first_heading = read
        .lines()
        .find(|l| l.starts_with('#'))
        .expect("a heading");
    assert!(
        first_heading.starts_with("# Crate tokio"),
        "first heading: {first_heading}"
    );
    // No nav links: docs.rs top bar and rustdoc sidebar stay out of the read.
    for nav in [
        "Privacy policy",
        "Build queue",
        "The Cargo Guide",
        "Find crate",
        "All Items",
        "Docs.rs crate page",
    ] {
        assert!(!read.contains(nav), "nav text {nav:?} in the read");
    }
    // remaining: lists sections.
    let remaining = read
        .split("\nremaining:\n")
        .nth(1)
        .expect("remaining: outline");
    let sections: Vec<&str> = remaining
        .lines()
        .take_while(|l| l.starts_with("- "))
        .collect();
    assert!(sections.len() >= 2, "sections: {sections:?}");
    assert!(sections.iter().all(|s| s.ends_with(" words)")));
    // cursor continues without repeating text, from the cache (no second request).
    let cursor = out
        .text
        .lines()
        .find_map(|l| l.strip_prefix("cursor: "))
        .expect("cursor line")
        .to_string();
    let before = server.requests().len();
    let next = call(
        &e,
        "fetch",
        json!({"url": "http://docs.rs/tokio", "cursor": cursor}),
    )
    .await
    .expect("continue");
    assert_eq!(
        server.requests().len(),
        before,
        "a cursor read must not refetch"
    );
    assert!(
        next.text.starts_with("chunk 2 of "),
        "{}",
        &next.text[..80.min(next.text.len())]
    );
    let first = prose(read);
    let second = prose(&next.text);
    assert!(!second.is_empty() && !first.is_empty());
    for p in &second {
        assert!(!first.contains(p), "repeated paragraph: {p}");
    }
    // The last section the outline promised is read in chunk 2.
    let promised = sections[sections.len() - 1]
        .trim_start_matches("- ")
        .rsplit_once(" (")
        .map(|(h, _)| h)
        .unwrap_or("");
    let promised_text = promised.trim_start_matches('#').trim();
    assert!(
        next.text.contains(promised_text),
        "chunk 2 does not hold {promised_text:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn find_search_on_docs_page_returns_the_search_input_first() {
    let (_server, e) = docs_rs().await;
    call(&e, "fetch", json!({"url": "http://docs.rs/tokio"}))
        .await
        .expect("fetch");
    let out = call(&e, "find", json!({"q": "search"}))
        .await
        .expect("find");
    let first = out.text.lines().next().unwrap_or("");
    assert!(
        first.contains("\"Find crate by search query\"") && first.contains(" input "),
        "first find line: {first}"
    );
    // Default k = 3 (W-6) is applied: three hits (this page has no challenge, overlay or
    // winning-submit row to append), and k = 1 gives one.
    assert_eq!(out.text.lines().count(), 3, "{}", out.text);
    let one = call(&e, "find", json!({"q": "search", "k": 1}))
        .await
        .expect("find k=1");
    assert_eq!(one.text.lines().count(), 1, "{}", one.text);
    assert_eq!(one.text.lines().next(), out.text.lines().next());
}

async fn github() -> (Server, Env) {
    let server = Server::start(vec![
        route(
            Some("api.github.com"),
            "/repos/tokio-rs/tokio",
            200,
            "application/json; charset=utf-8",
            fixture("github_repo_tokio.json"),
        ),
        Route {
            host: Some("api.github.com".into()),
            path: "/echo".into(),
            reply: Reply::EchoHeader("authorization".into()),
        },
        Route {
            host: Some("api.github.com".into()),
            path: "/echo64".into(),
            reply: Reply::EchoHeaderBase64("authorization".into()),
        },
    ])
    .await;
    let cfg = config(&server, &["api.github.com"], no_tools())
        .with_resolve("down.example", "127.0.0.1:1".parse().unwrap());
    let e = env(cfg).await;
    (server, e)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_returns_json_and_a_vault_header_never_reaches_the_ledger() {
    let (server, e) = github().await;
    let canary = canary();
    let out = call(
        &e,
        "api",
        json!({
            "url": "http://api.github.com/repos/tokio-rs/tokio",
            "headers": {"Authorization": "token @vault(gh_token)", "Accept": "application/vnd.github+json"}
        }),
    )
    .await
    .expect("api");
    assert!(
        out.text.starts_with("HTTP 200 application/json"),
        "{}",
        out.text.lines().next().unwrap_or("")
    );
    assert!(out.text.contains("\"full_name\": \"tokio-rs/tokio\""));
    // Resolved at the sink: the server got the real header value.
    let req = server.requests().pop().expect("a request");
    let want = format!("token {canary}");
    assert!(
        req.header("authorization") == Some(want.as_str()),
        "the Authorization header was not resolved from the vault"
    );
    // Never in the ledger (table rows and the jsonl mirror) or the returned text.
    let dump = ledger_dump(&e);
    assert!(!dump.contains(&canary), "vault value in the ledger");
    assert!(
        dump.contains("@vault(gh_token)"),
        "the ledger should show the literal"
    );
    assert!(
        !out.text.contains(&canary),
        "vault value in the returned text"
    );
    let ev = events(&e);
    let tool = ev
        .iter()
        .find(|x| x.tool == "api" && x.server == "web")
        .expect("tool event");
    assert_eq!(tool.class, Some(Class::Read));
    assert!(tool.ok);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_echoed_vault_value_is_scrubbed() {
    let (_server, e) = github().await;
    let canary = canary();
    let out = call(
        &e,
        "api",
        json!({"url": "http://api.github.com/echo", "headers": {"Authorization": "token @vault(gh_token)"}}),
    )
    .await
    .expect("api echo");
    assert!(
        !out.text.contains(&canary),
        "echoed vault value reached the model"
    );
    assert!(
        out.text.contains("token @vault(gh_token)"),
        "the echo should be scrubbed to the literal"
    );
    assert!(
        !ledger_dump(&e).contains(&canary),
        "vault value in the ledger"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs T-05d base64 scrub (D-77)"]
async fn api_base64_echoed_vault_value_is_scrubbed() {
    let (_server, e) = github().await;
    let canary = canary();
    let out = call(
        &e,
        "api",
        json!({"url": "http://api.github.com/echo64", "headers": {"Authorization": "token @vault(gh_token)"}}),
    )
    .await
    .expect("api echo64");
    let encoded = base64(format!("token {canary}").as_bytes());
    assert!(
        !out.text.contains(&encoded),
        "base64 of the vault value reached the model"
    );
    assert!(
        !ledger_dump(&e).contains(&encoded),
        "base64 of the vault value in the ledger"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_unreachable_host_error_names_the_host_only() {
    let (_server, e) = github().await;
    let canary = canary();
    let err = call(
        &e,
        "api",
        json!({"url": "http://down.example/repos?x=1", "headers": {"Authorization": "token @vault(gh_token)"}}),
    )
    .await
    .expect_err("unreachable");
    let msg = err.to_string();
    assert!(!msg.contains(&canary), "vault value in the error");
    assert!(
        msg.contains("down.example") && !msg.contains("x=1"),
        "{msg}"
    );
    let dump = ledger_dump(&e);
    assert!(!dump.contains(&canary), "vault value in the ledger");
    let ev = events(&e);
    assert!(ev.iter().any(|x| x.tool == "api" && !x.ok));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_refuses_vault_outside_headers_and_classes_follow_the_method() {
    let (server, e) = github().await;
    for args in [
        json!({"url": "http://api.github.com/x?t=@vault(gh_token)"}),
        json!({"url": "http://api.github.com/x", "method": "POST", "body": {"t": "@vault(gh_token)"}}),
        json!({"url": "http://api.github.com/x", "method": "POST", "body": "@vault(gh_token)"}),
    ] {
        let err = call(&e, "api", args)
            .await
            .expect_err("vault outside headers");
        assert!(err.to_string().contains("only in header values"), "{err}");
    }
    assert!(
        server.requests().is_empty(),
        "a refused call must not send anything"
    );

    // POST is Submit (allowed by default), DELETE is Delete (asks; answered no here).
    let _ = call(
        &e,
        "api",
        json!({"url": "http://api.github.com/repos/tokio-rs/tokio", "method": "POST", "body": {"a": 1}}),
    )
    .await;
    e.asker.push(harness_core::AskOutcome::AnsweredSkip);
    let del = call(
        &e,
        "api",
        json!({"url": "http://api.github.com/repos/tokio-rs/tokio", "method": "DELETE"}),
    )
    .await
    .expect("skipped, not an error");
    assert!(del.text.starts_with("skipped:"), "{}", del.text);
    let cards = e.asker.cards();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].class, Class::Delete);
    let ev = events(&e);
    let post = ev
        .iter()
        .find(|x| x.tool == "api" && x.server == "web" && x.mode == Some(LedgerMode::Allow))
        .expect("POST event");
    assert_eq!(post.class, Some(Class::Submit));
    let methods: Vec<String> = server.requests().iter().map(|r| r.method.clone()).collect();
    assert_eq!(methods, ["POST"]);
    let posted = &server.requests()[0];
    assert_eq!(posted.body, br#"{"a":1}"#);
    assert_eq!(posted.header("content-type"), Some("application/json"));
}
