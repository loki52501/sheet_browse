//! T-12 acceptance (tester): the W-2 sheet mode, the 5 MB cap and the 15 s timeout, a full
//! vault canary sweep of the api flow (returned text, errors, ledger rows, ledger.jsonl and
//! every file under the runs root), and the W-7 rule that a site tool's class is never
//! below its `@gate`. Offline: a local listener answers through the Config DNS override.
//! The live docs.rs read (remaining + cursor) is `#[ignore]`d (network).

mod common;

use common::*;
use harness_core::{AskOutcome, Class};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn no_tools() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no-tools")
}

fn html(body: &str) -> Vec<u8> {
    format!("<!doctype html><html><head><title>Fixture</title></head><body>{body}</body></html>")
        .into_bytes()
}

// ---------------------------------------------------------------------------------------
// W-2 sheet mode

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sheet_mode_returns_numbered_element_lines() {
    let page = r#"<main><h1>Items</h1><p>Some prose that is not an element.</p>
        <label for="qty">Quantity</label><input id="qty" type="number" value="1">
        <button>Add to cart</button> <a href="/next">Next page</a></main>"#;
    let server = Server::start(vec![route(
        Some("sheet.test"),
        "/",
        200,
        "text/html",
        html(page),
    )])
    .await;
    let e = env(config(&server, &["sheet.test"], no_tools())).await;
    let out = call(
        &e,
        "fetch",
        json!({"url": "http://sheet.test/", "mode": "sheet"}),
    )
    .await
    .expect("sheet");
    let body = out.text.split_once("\n\n").map(|(_, b)| b).unwrap_or("");
    let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    // Every element line is `#<id> <role> "<name>" ...`.
    let numbered: Vec<(u32, &str)> = lines
        .iter()
        .filter_map(|l| {
            let rest = l.trim_start().strip_prefix('#')?;
            let (id, tail) = rest.split_once(' ')?;
            Some((id.parse().ok()?, tail))
        })
        .collect();
    assert!(numbered.len() >= 3, "sheet:\n{body}");
    for want in [
        ("input|spinbutton", "\"Quantity\""),
        ("button", "\"Add to cart\""),
        ("link", "\"Next page\""),
    ] {
        assert!(
            numbered
                .iter()
                .any(|(_, t)| want.0.split('|').any(|r| t.starts_with(r)) && t.contains(want.1)),
            "no {want:?} line in:\n{body}"
        );
    }
    // Document order: ids increase.
    let ids: Vec<u32> = numbered.iter().map(|(i, _)| *i).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "sheet not in document order:\n{body}");
    // Sheet mode is element lines, not the read renderer's markdown.
    assert!(
        !body.contains("# Items\n"),
        "read output in sheet mode:\n{body}"
    );
    assert!(!body.contains("chunk 1 of"), "{body}");
}

// ---------------------------------------------------------------------------------------
// W-2 5 MB cap

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bodies_over_5_mb_are_cut_and_the_cut_is_said() {
    assert_eq!(srv_web::config::DEFAULT_MAX_BYTES, 5 * 1024 * 1024);
    assert_eq!(srv_web::Config::default().max_bytes, 5 * 1024 * 1024);
    let five = 5 * 1024 * 1024;
    // A comment pads the page cheaply for the HTML walk; the tail marker sits past the cap.
    let big = |pad: usize| {
        let mut s = String::from(
            "<!doctype html><html><head><title>Big</title></head><body><main><h1>Big page</h1><p>head-marker-kept</p><!--",
        );
        s.push_str(&"x".repeat(pad));
        s.push_str("--><p>tail-marker-past-cap</p></main></body></html>");
        s.into_bytes()
    };
    let over = big(five + 1024 * 1024);
    let under = big(five - 4096);
    assert!(under.len() < five);
    let mut json_over = String::from("{\"pad\":\"");
    json_over.push_str(&"y".repeat(five + 1024));
    json_over.push_str("\",\"tail\":\"tail-marker-past-cap\"}");
    let server = Server::start(vec![
        route(Some("big.test"), "/over", 200, "text/html", over),
        route(Some("big.test"), "/under", 200, "text/html", under),
        route(
            Some("big.test"),
            "/json",
            200,
            "application/json",
            json_over.into_bytes(),
        ),
    ])
    .await;
    // config() keeps Config::default() limits (5 MB, 15 s).
    let e = env(config(&server, &["big.test"], no_tools())).await;

    let out = call(&e, "fetch", json!({"url": "http://big.test/over"}))
        .await
        .expect("over-cap fetch is read, not an error");
    assert!(
        out.text.contains("page cut at the 5 MB cap"),
        "{}",
        &out.text[..out.text.len().min(400)]
    );
    assert!(out.text.contains("head-marker-kept"));
    assert!(!out.text.contains("tail-marker-past-cap"));

    let ok = call(&e, "fetch", json!({"url": "http://big.test/under"}))
        .await
        .expect("under-cap fetch");
    assert!(!ok.text.contains("5 MB cap"), "an under-cap page was cut");
    assert!(ok.text.contains("tail-marker-past-cap"));

    let api = call(&e, "api", json!({"url": "http://big.test/json"})).await;
    let t = text(&api);
    assert!(
        t.lines()
            .next()
            .unwrap_or("")
            .contains("(body cut at the 5 MB cap)"),
        "{}",
        &t[..t.len().min(200)]
    );
    assert!(!t.contains("tail-marker-past-cap"));
}

// ---------------------------------------------------------------------------------------
// W-2 15 s timeout

/// A listener that accepts and never answers (`stall_body = false`), or sends headers and
/// part of the body and then stalls. Sockets are held open until the task is dropped.
async fn staller(stall_body: bool) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((mut sock, _)) = listener.accept().await {
            if stall_body {
                // Read the request head first: hyper rejects a response that arrives before
                // the request was written.
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 100000\r\n\r\n<html><body><p>partial",
                    )
                    .await;
            }
            held.push(sock);
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_time_out_at_15_s() {
    assert_eq!(srv_web::config::DEFAULT_TIMEOUT, Duration::from_secs(15));
    assert_eq!(srv_web::Config::default().timeout, Duration::from_secs(15));
    let silent = staller(false).await;
    let slow_body = staller(true).await;
    let cfg = srv_web::Config {
        tools_root: no_tools(),
        ..srv_web::Config::default()
    }
    .with_resolve("silent.test", silent)
    .with_resolve("slowbody.test", slow_body);
    let e = env(cfg).await;
    let start = Instant::now();
    let timed = |tool: &'static str, url: &'static str| {
        let e = &e;
        async move {
            let r = call(e, tool, json!({ "url": url })).await;
            (tool, url, r, start.elapsed())
        }
    };
    let results = tokio::join!(
        timed("fetch", "http://silent.test/"),
        timed("fetch", "http://slowbody.test/"),
        timed("api", "http://silent.test/x"),
        timed("feed", "http://slowbody.test/feed"),
    );
    for (tool, url, r, took) in [results.0, results.1, results.2, results.3] {
        let err = r.expect_err("a stalled server must time out");
        let msg = err.to_string();
        assert!(
            msg.contains("did not answer in time"),
            "{tool} {url} after {took:?}: {msg}"
        );
        assert!(
            took >= Duration::from_millis(14_500) && took < Duration::from_secs(25),
            "{tool} {url}: timed out after {took:?}, want about 15 s"
        );
    }
}

// ---------------------------------------------------------------------------------------
// Acceptance: a header with @vault(gh_token) never appears anywhere

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}

fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vault_canary_never_leaves_the_api_flow() {
    let canary = canary();
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
        route(
            Some("api.github.com"),
            "/missing",
            404,
            "application/json",
            br#"{"message":"Not Found"}"#.to_vec(),
        ),
        route(
            Some("api.github.com"),
            "/page",
            200,
            "text/html",
            html("<p>html</p>"),
        ),
        redirect(Some("api.github.com"), "/away", "http://other.test/catch"),
    ])
    .await;
    let cfg = config(&server, &["api.github.com", "other.test"], no_tools())
        .with_resolve("down.example", "127.0.0.1:1".parse().unwrap());
    let e = env(cfg).await;
    let hdr = json!({"Authorization": "token @vault(gh_token)", "X-Probe": "@vault(gh_token)"});
    let mut seen_text = Vec::new();
    for (url, method) in [
        ("http://api.github.com/repos/tokio-rs/tokio", "GET"),
        ("http://api.github.com/echo", "GET"),
        ("http://api.github.com/missing", "GET"),
        ("http://api.github.com/page", "GET"),
        ("http://api.github.com/away", "GET"),
        ("http://down.example/x", "GET"),
        ("http://api.github.com/repos/tokio-rs/tokio", "POST"),
    ] {
        let mut args = json!({"url": url, "method": method, "headers": hdr});
        if method == "POST" {
            args["body"] = json!({"a": 1});
        }
        let r = call(&e, "api", args).await;
        seen_text.push(text(&r));
        if let Err(err) = &r {
            seen_text.push(format!("{err:?}"));
        }
    }
    // The flow is real: the first request carried the resolved header.
    let first = server
        .requests()
        .into_iter()
        .find(|r| r.target == "/repos/tokio-rs/tokio")
        .expect("request");
    assert!(
        first.header("authorization") == Some(format!("token {canary}").as_str()),
        "the header was not resolved at the sink"
    );
    assert!(seen_text[0].contains("\"full_name\": \"tokio-rs/tokio\""));
    // Returned text and errors.
    let leaked_text = seen_text.iter().filter(|t| t.contains(&canary)).count();
    assert_eq!(leaked_text, 0, "vault value in returned text or errors");
    // Ledger rows (table) and the jsonl mirror, plus their Debug form.
    assert!(
        !ledger_dump(&e).contains(&canary),
        "vault value in the ledger"
    );
    assert!(
        !format!("{:?}", events(&e)).contains(&canary),
        "vault value in a ledger event"
    );
    assert!(events(&e).len() >= 7, "every call is in the ledger");
    // Every file under the runs root (run folder, ledger.jsonl, cards, anything else).
    let mut files = Vec::new();
    walk(&e.harness.runs_root, &mut files);
    assert!(
        files.iter().any(|p| p.ends_with("ledger.jsonl")),
        "no ledger.jsonl under the runs root: {files:?}"
    );
    let leaked: Vec<&PathBuf> = files
        .iter()
        .filter(|p| contains_bytes(&std::fs::read(p).unwrap_or_default(), canary.as_bytes()))
        .collect();
    assert!(
        leaked.is_empty(),
        "vault value in run files: {} file(s)",
        leaked.len()
    );
}

// ---------------------------------------------------------------------------------------
// W-7: a site tool's class is never below its @gate

fn tools_copy(into: &Path) -> PathBuf {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if p.is_dir() {
                copy(&p, &to.join(e.file_name()));
            } else {
                std::fs::copy(&p, to.join(e.file_name())).unwrap();
            }
        }
    }
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tools");
    let dst = into.join("tools");
    copy(&src, &dst);
    dst
}

/// A reader whose header raises the gate to Delete (written into the private copy only).
const WIPE_READ: &str = "// @name wipe_read
// @kind reader
// @match /*
// @gate Delete
// @phase 1
// T-12 acceptance fixture: a reader whose @gate is Delete must be gated as Delete.
export default function (page) {
  return { title: page.title };
}
";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn site_tool_class_is_never_below_its_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tools_copy(tmp.path());
    std::fs::write(tools.join("example.com").join("wipe_read.js"), WIPE_READ).unwrap();
    let server = Server::start(vec![route(
        Some("shop.example.com"),
        "/shop/items",
        200,
        "text/html",
        html(r#"<main><h1>Items</h1><button>Add to cart</button></main>"#),
    )])
    .await;
    let e = env(config(&server, &["shop.example.com"], tools)).await;
    let out = call(
        &e,
        "fetch",
        json!({"url": "http://shop.example.com/shop/items"}),
    )
    .await
    .expect("fetch");
    let line = out
        .text
        .lines()
        .find(|l| l.starts_with("tools for this site: "))
        .unwrap_or_else(|| panic!("no tools line in {}", out.text));
    assert!(line.contains("example.wipe_read"), "{line}");
    assert!(line.contains("example.strict_read"), "{line}");
    // Registered at the header's class, never lowered to the reader default.
    assert_eq!(
        e.registry.get("example.wipe_read").unwrap().class,
        Class::Delete
    );
    assert_eq!(
        e.registry.get("example.strict_read").unwrap().class,
        Class::Submit
    );
    assert_eq!(
        e.registry.get("example.list_items").unwrap().class,
        Class::Read
    );

    // Running it goes through the gate at that class: Submit runs (allowed by default)
    // and is ledgered as Submit; Delete asks first.
    let strict = call(&e, "example.strict_read", json!({}))
        .await
        .expect("strict_read");
    assert!(strict.text.contains("\"title\""), "{}", strict.text);
    e.asker.push(AskOutcome::AnsweredSkip);
    let wipe = call(&e, "example.wipe_read", json!({}))
        .await
        .expect("skipped, not an error");
    assert!(wipe.text.starts_with("skipped:"), "{}", wipe.text);
    let cards = e.asker.cards();
    assert_eq!(cards.len(), 1, "Delete must ask");
    assert_eq!(cards[0].class, Class::Delete);
    e.asker.push(AskOutcome::AnsweredProceed);
    let ran = call(&e, "example.wipe_read", json!({}))
        .await
        .expect("proceeds after yes");
    assert!(ran.text.contains("\"title\""), "{}", ran.text);

    let ev = events(&e);
    for (tool, want) in [
        ("example.strict_read", Class::Submit),
        ("example.wipe_read", Class::Delete),
    ] {
        let classes: Vec<Option<Class>> = ev
            .iter()
            .filter(|x| x.tool == tool)
            .map(|x| x.class)
            .collect();
        assert!(!classes.is_empty(), "{tool} not in the ledger");
        assert!(
            classes.iter().all(|c| *c == Some(want)),
            "{tool} ledgered as {classes:?}, want {want:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// Live acceptance item 1 in full (remaining + cursor), network only.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "network: fetches https://docs.rs/tokio"]
async fn live_docs_rs_tokio_heading_remaining_and_cursor() {
    let e = env(srv_web::Config {
        tools_root: no_tools(),
        ..srv_web::Config::default()
    })
    .await;
    let out = call(
        &e,
        "fetch",
        json!({"url": "https://docs.rs/tokio", "mode": "read"}),
    )
    .await
    .expect("fetch");
    let read = out.text.split_once("\n\n").map(|(_, r)| r).unwrap_or("");
    let first = read.lines().find(|l| l.starts_with('#')).unwrap_or("");
    assert!(first.starts_with("# Crate tokio"), "first heading: {first}");
    for nav in [
        "Privacy policy",
        "Build queue",
        "The Cargo Guide",
        "All Items",
    ] {
        assert!(!read.contains(nav), "nav text {nav:?} in the read");
    }
    let remaining = read
        .split("\nremaining:\n")
        .nth(1)
        .expect("remaining: outline");
    let sections: Vec<&str> = remaining
        .lines()
        .take_while(|l| l.starts_with("- "))
        .collect();
    assert!(sections.len() >= 2, "sections: {sections:?}");
    let cursor = out
        .text
        .lines()
        .find_map(|l| l.strip_prefix("cursor: "))
        .expect("cursor line")
        .to_string();
    let next = call(
        &e,
        "fetch",
        json!({"url": "https://docs.rs/tokio", "cursor": cursor}),
    )
    .await
    .expect("continue");
    assert!(
        next.text.starts_with("chunk 2 of "),
        "{}",
        &next.text[..80.min(next.text.len())]
    );
    let paras = |md: &str| -> Vec<String> {
        md.split("\n\nremaining:")
            .next()
            .unwrap_or("")
            .split("\n\n")
            .map(str::trim)
            .filter(|p| p.len() > 80 && !p.contains("```") && !p.starts_with("chunk "))
            .map(str::to_string)
            .collect()
    };
    let a = paras(read);
    let b = paras(&next.text);
    assert!(!a.is_empty() && !b.is_empty());
    for p in &b {
        assert!(!a.contains(p), "repeated paragraph: {p}");
    }
}
