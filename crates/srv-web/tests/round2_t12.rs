//! review-T-12 round 1 fixes, offline: HTML under a wrong or missing content type (rule 5),
//! vault headers over cleartext and across origins (rule 2), classify/call pinning (rule 4),
//! the private-address redirect guard, challenges in feed and api (D-5), and tool health on
//! registration (T-4, D-505, D-506).

mod common;

use async_trait::async_trait;
use common::*;
use harness_core::memory::{ToolHealth, ToolStatus};
use harness_core::{AskCard, AskOutcome, Asker, Class, MemoryStore, Mode, ToolError};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Notify;

fn no_tools() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no-tools")
}

/// A private copy of tests/fixtures/tools (record() writes health-driven changes).
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

fn fixed(
    host: Option<&str>,
    path: &str,
    status: u16,
    headers: &[(&str, &str)],
    body: &str,
) -> Route {
    Route {
        host: host.map(str::to_string),
        path: path.to_string(),
        reply: Reply::Fixed {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
        },
    }
}

fn page(title: &str, body: &str) -> String {
    format!("<!doctype html><html><head><title>{title}</title></head><body>{body}</body></html>")
}

fn stopped_by_challenge(r: &Result<harness_core::ToolOutput, ToolError>, what: &str) {
    let out = r
        .as_ref()
        .unwrap_or_else(|e| panic!("{what}: an error, not a stop: {e}"));
    let stop = out
        .stopped
        .as_ref()
        .unwrap_or_else(|| panic!("{what} did not stop: {}", out.text));
    assert_eq!(stop.class, Class::Challenge, "{what}");
}

// ---------------------------------------------------------------------------------------
// Rule 5: HTML sent as text/plain or with no content type

const SNEAKY: &str = "<!DOCTYPE html><html><head><title>Sneaky</title></head><body>\
    <p>Hello <b>visible</b> words</p><p># Not a heading</p>\
    <script>stealCookies()</script></body></html>";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn html_under_a_wrong_or_missing_content_type_never_reaches_the_model() {
    let server = Server::start(vec![
        fixed(
            Some("x.test"),
            "/plain",
            200,
            &[("Content-Type", "text/plain")],
            SNEAKY,
        ),
        fixed(Some("x.test"), "/none", 200, &[], SNEAKY),
        fixed(
            Some("x.test"),
            "/body",
            200,
            &[("Content-Type", "text/plain; charset=utf-8")],
            "\u{feff}\n  <BODY><div>Fragment text</div><script>x()</script></BODY>",
        ),
    ])
    .await;
    let e = env(config(&server, &["x.test"], no_tools())).await;
    for path in ["/plain", "/none", "/body"] {
        let url = format!("http://x.test{path}");
        let out = call(&e, "fetch", json!({ "url": url }))
            .await
            .unwrap_or_else(|err| panic!("fetch {path}: {err}"));
        let t = out.text.to_ascii_lowercase();
        for tag in [
            "<p>",
            "<b>",
            "<script",
            "<div",
            "<body",
            "<!doctype",
            "stealcookies",
            "x()",
        ] {
            assert!(
                !t.contains(tag),
                "fetch {path}: raw HTML {tag:?} reached the model:\n{}",
                out.text
            );
        }
        assert!(
            out.text.contains("Hello visible words") || out.text.contains("Fragment text"),
            "fetch {path}: the HTML walk did not run:\n{}",
            out.text
        );
        assert!(
            !out.text.lines().any(|l| l.starts_with("# Not a heading")),
            "a paragraph passed for a heading:\n{}",
            out.text
        );
        let api = call(&e, "api", json!({ "url": url }))
            .await
            .expect_err("api must refuse HTML");
        assert!(
            api.to_string().contains("answered with HTML"),
            "api {path}: {api}"
        );
        let feed = call(&e, "feed", json!({ "url": url }))
            .await
            .expect_err("feed must refuse HTML");
        let msg = feed.to_string();
        assert!(
            msg.contains("HTML page") && !msg.contains('<'),
            "feed {path}: {msg}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// Rule 2: vault headers over cleartext and across origins

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vault_headers_need_https_or_loopback_and_keep_the_origin() {
    let other = Server::start(vec![Route {
        host: None,
        path: "/catch".into(),
        reply: Reply::EchoHeader("Authorization".into()),
    }])
    .await;
    let port2 = other.addr.port();
    let server = Server::start(vec![
        Route {
            host: None,
            path: "/echo".into(),
            reply: Reply::EchoHeader("Authorization".into()),
        },
        redirect(
            Some("api.test"),
            "/port",
            &format!("http://api.test:{port2}/catch"),
        ),
    ])
    .await;
    let e = env(config(&server, &["api.test"], no_tools())).await;
    let canary = canary();
    let hdr = json!({"Authorization": "token @vault(gh_token)"});

    // Plain http to a host that is not this machine: refused before anything is resolved.
    let err = call(
        &e,
        "api",
        json!({"url": "http://plain.example/x", "headers": hdr}),
    )
    .await
    .expect_err("vault over cleartext");
    assert!(matches!(err, ToolError::Args(_)), "{err}");
    assert!(err.to_string().contains("only over https"), "{err}");

    // Plain http to a loopback literal is fine (and the echo is scrubbed).
    let url = format!("http://127.0.0.1:{}/echo", server.addr.port());
    let ok = call(&e, "api", json!({"url": url, "headers": hdr}))
        .await
        .expect("loopback");
    assert!(!ok.text.contains(&canary), "vault value reached the model");
    assert!(server.requests().iter().any(|r| r.target == "/echo"
        && r.header("Authorization")
            .is_some_and(|v| v.contains(&canary))));

    // A same-host redirect to another port does not carry the vault header.
    let err = call(
        &e,
        "api",
        json!({"url": "http://api.test/port", "headers": hdr}),
    )
    .await
    .expect_err("port change with a vault header");
    assert!(err.to_string().contains("redirected"), "{err}");
    assert!(
        other.requests().is_empty(),
        "a vault header followed a redirect to another port"
    );
    // Without a vault header the same redirect is followed.
    let plain = call(&e, "api", json!({"url": "http://api.test/port"}))
        .await
        .expect("plain redirect");
    assert!(plain.text.starts_with("HTTP 200"), "{}", plain.text);
    assert_eq!(other.requests().len(), 1);
}

// ---------------------------------------------------------------------------------------
// Rule 4: classify and call act on the same page

/// Holds the first card whose `what` contains `hold_on` until released; answers proceed.
struct HoldAsker {
    hold_on: String,
    asked: Notify,
    release: Notify,
}

#[async_trait]
impl Asker for HoldAsker {
    async fn ask(&self, card: &AskCard) -> AskOutcome {
        if card.what.contains(&self.hold_on) {
            self.asked.notify_one();
            self.release.notified().await;
        }
        AskOutcome::AnsweredProceed
    }
}

fn hold(on: &str) -> Arc<HoldAsker> {
    Arc::new(HoldAsker {
        hold_on: on.to_string(),
        asked: Notify::new(),
        release: Notify::new(),
    })
}

/// The id of the `onward` link in a `mode=sheet` output (`#3 link "onward"`).
fn first_link(text: &str) -> String {
    text.lines()
        .find(|l| l.contains("link \"onward\""))
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or_else(|| panic!("no onward link in the sheet:\n{text}"))
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_ref_is_pinned_between_classify_and_call() {
    let server = Server::start(vec![
        fixed(
            Some("one.test"),
            "/",
            200,
            &[("Content-Type", "text/html")],
            &page(
                "One",
                "<p>Go <a href=\"http://a.test/target\">onward</a></p>",
            ),
        ),
        fixed(
            Some("two.test"),
            "/",
            200,
            &[("Content-Type", "text/html")],
            &page(
                "Two",
                "<p>Go <a href=\"http://b.test/target\">onward</a></p>",
            ),
        ),
        fixed(
            Some("a.test"),
            "/target",
            200,
            &[("Content-Type", "text/html")],
            &page("A", "<p>Landed on page A</p>"),
        ),
        fixed(
            Some("b.test"),
            "/target",
            200,
            &[("Content-Type", "text/html")],
            &page("B", "<p>Landed on page B</p>"),
        ),
    ])
    .await;
    let mut e = env(config(
        &server,
        &["one.test", "two.test", "a.test", "b.test"],
        no_tools(),
    ))
    .await;
    let asker = hold("a.test");
    e.harness.asker = asker.clone();
    e.store
        .add_rule("a.test", "read", Mode::Ask, false)
        .unwrap();

    let one = call(
        &e,
        "fetch",
        json!({"url": "http://one.test/", "mode": "sheet"}),
    )
    .await
    .expect("one");
    let link = first_link(&one.text);
    let (pinned, _) = tokio::join!(call(&e, "fetch", json!({ "url": link })), async {
        // The #id call was classified (a.test) and is waiting on its card; the last page
        // changes under it.
        asker.asked.notified().await;
        let two = call(
            &e,
            "fetch",
            json!({"url": "http://two.test/", "mode": "sheet"}),
        )
        .await
        .expect("two");
        assert_eq!(
            first_link(&two.text),
            link,
            "both pages need the same link id"
        );
        asker.release.notify_one();
    });
    let out = pinned.expect("the pinned fetch");
    assert!(out.text.contains("Landed on page A"), "{}", out.text);
    assert!(
        !server.requests().iter().any(|r| r.host == "b.test"),
        "call fetched a URL whose domain the gate never checked"
    );
}

/// The shop page of web_tools.rs (list_items' fingerprint and find need its controls).
const SHOP: &str = r#"<header><nav aria-label="Main">
    <a href="/">Home</a> <a href="/shop/">Shop</a> <a href="/about">About us</a>
    <a href="/contact">Contact</a> <a href="/help">Help</a> <a href="/login">Log in</a>
  </nav><form role="search"><input type="search" aria-label="Search the shop"><button>Search</button></form></header>
  <main><h1>Items</h1>
  <ul><li>Apple <a href="/shop/apple">Details</a></li><li>Pear <a href="/shop/pear">Details</a></li></ul>
  <label for="qty">Quantity</label><input id="qty" type="number" value="1">
  <button>Add to cart</button> <button>Save for later</button>
  </main><footer><a href="/privacy">Privacy</a> <a href="/terms">Terms</a></footer>"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn site_tool_page_is_pinned_between_classify_and_call() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tools_copy(tmp.path());
    let server = Server::start(vec![
        fixed(
            Some("shop.example.com"),
            "/shop/items",
            200,
            &[("Content-Type", "text/html")],
            &page("Items", SHOP),
        ),
        fixed(
            Some("shop.example.com"),
            "/shop/other",
            200,
            &[("Content-Type", "text/html")],
            &page("Other", SHOP),
        ),
        fixed(
            Some("api.example.com"),
            "/stock",
            200,
            &[("Content-Type", "application/json")],
            r#"{"count":3}"#,
        ),
    ])
    .await;
    let mut e = env(config(
        &server,
        &["shop.example.com", "api.example.com"],
        tools,
    ))
    .await;
    let asker = hold("run site tool example.list_items");
    e.harness.asker = asker.clone();
    e.store
        .add_rule("example.com", "read", Mode::Ask, false)
        .unwrap();

    call(
        &e,
        "fetch",
        json!({"url": "http://shop.example.com/shop/items"}),
    )
    .await
    .expect("items");
    let (ran, _) = tokio::join!(call(&e, "example.list_items", json!({})), async {
        asker.asked.notified().await;
        call(
            &e,
            "fetch",
            json!({"url": "http://shop.example.com/shop/other"}),
        )
        .await
        .expect("other");
        asker.release.notify_one();
    });
    let out = ran.expect("site tool");
    let v: serde_json::Value =
        serde_json::from_str(out.text.split("\nnote:").next().unwrap_or(&out.text))
            .unwrap_or_else(|_| panic!("JSON output: {}", out.text));
    assert_eq!(
        v["url"], "http://shop.example.com/shop/items",
        "{}",
        out.text
    );
    assert_eq!(v["title"], "Items");
}

// ---------------------------------------------------------------------------------------
// Redirects from a public URL into private addresses

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redirects_from_a_public_url_into_private_addresses_are_refused() {
    let server = Server::start(vec![fixed(
        None,
        "/secret",
        200,
        &[("Content-Type", "text/html")],
        &page("Secret", "<p>internal admin page</p>"),
    )])
    .await;
    let port = server.addr.port();
    // The redirecting routes live on a second server so they can point at the first.
    let hops = Server::start(vec![
        redirect(
            Some("public.test"),
            "/to-loopback",
            &format!("http://127.0.0.1:{port}/secret"),
        ),
        redirect(
            Some("public.test"),
            "/to-localhost",
            &format!("http://localhost:{port}/secret"),
        ),
        redirect(
            Some("public.test"),
            "/to-metadata",
            "http://169.254.169.254/latest/meta-data/",
        ),
        redirect(
            Some("public.test"),
            "/to-public",
            "http://other.test/secret",
        ),
        redirect(
            Some("127.0.0.1"),
            "/local-hop",
            &format!("http://127.0.0.1:{port}/secret"),
        ),
    ])
    .await;
    let cfg = config(&hops, &["public.test"], no_tools()).with_resolve("other.test", server.addr);
    let e = env(cfg).await;
    for (tool, path) in [
        ("fetch", "/to-loopback"),
        ("fetch", "/to-localhost"),
        ("fetch", "/to-metadata"),
        ("feed", "/to-loopback"),
    ] {
        let err = call(
            &e,
            tool,
            json!({ "url": format!("http://public.test{path}") }),
        )
        .await
        .expect_err("a hop into a private address");
        assert!(
            err.to_string().contains("loopback, private or link-local"),
            "{tool} {path}: {err}"
        );
    }
    assert!(
        server.requests().is_empty(),
        "a public page redirected the reader into a private address"
    );
    // A public-to-public hop is followed.
    let ok = call(&e, "fetch", json!({"url": "http://public.test/to-public"}))
        .await
        .expect("public hop");
    assert!(ok.text.contains("internal admin page"));
    // A start URL that is itself loopback may redirect within loopback.
    let local = format!("http://127.0.0.1:{}/local-hop", hops.addr.port());
    let ok = call(&e, "fetch", json!({ "url": local }))
        .await
        .expect("loopback start");
    assert!(ok.text.contains("internal admin page"));
}

// ---------------------------------------------------------------------------------------
// D-5 in feed and api

const JUST_A_MOMENT: &str =
    "<!doctype html><html><head><title>Just a moment...</title></head><body></body></html>";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn challenges_stop_feed_and_api() {
    let cf = [("Content-Type", "text/html"), ("cf-mitigated", "challenge")];
    let server = Server::start(vec![
        fixed(Some("c.test"), "/cf-feed", 403, &cf, JUST_A_MOMENT),
        fixed(
            Some("c.test"),
            "/page-feed",
            200,
            &[("Content-Type", "application/rss+xml")],
            JUST_A_MOMENT,
        ),
        fixed(
            Some("c.test"),
            "/cf-api",
            403,
            &[
                ("Content-Type", "application/json"),
                ("cf-mitigated", "challenge"),
            ],
            "{}",
        ),
        fixed(
            Some("c.test"),
            "/page-api",
            403,
            &[("Content-Type", "text/plain")],
            JUST_A_MOMENT,
        ),
    ])
    .await;
    let e = env(config(&server, &["c.test"], no_tools())).await;
    for (tool, path) in [
        ("feed", "/cf-feed"),
        ("feed", "/page-feed"),
        ("api", "/cf-api"),
        ("api", "/page-api"),
    ] {
        let r = call(&e, tool, json!({ "url": format!("http://c.test{path}") })).await;
        stopped_by_challenge(&r, &format!("{tool} {path}"));
    }
}

// ---------------------------------------------------------------------------------------
// T-4: tool health on registration, a miss disables for the process (D-506)

fn health(domain: &str, name: &str, status: ToolStatus) -> ToolHealth {
    ToolHealth {
        domain: domain.into(),
        name: name.into(),
        version: 1,
        status,
        runs: 0,
        fails: 0,
        last_ok: None,
        last_fail: None,
        why: None,
        pinned: false,
    }
}

fn tools_line(text: &str) -> String {
    text.lines()
        .find(|l| l.starts_with("tools for this site: "))
        .unwrap_or("")
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_health_is_honoured_and_a_miss_disables_for_the_process() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tools_copy(tmp.path());
    let server = Server::start(vec![fixed(
        Some("shop.example.com"),
        "/shop/items",
        200,
        &[("Content-Type", "text/html")],
        &page("Items", SHOP),
    )])
    .await;
    let e = env(config(&server, &["shop.example.com"], tools)).await;
    e.store
        .tool_health_set(health("example.com", "list_items", ToolStatus::NeedsRepair))
        .await
        .unwrap();
    e.store
        .tool_health_set(health("_generic", "word_count", ToolStatus::Retired))
        .await
        .unwrap();
    let fetch = || {
        call(
            &e,
            "fetch",
            json!({"url": "http://shop.example.com/shop/items"}),
        )
    };
    let out = fetch().await.expect("fetch");
    let line = tools_line(&out.text);
    assert!(line.contains("strict_read"), "{line}");
    assert!(!line.contains("list_items"), "{line}");
    assert!(!line.contains("word_count"), "{line}");
    assert!(e.registry.get("example.list_items").is_none());
    assert!(e.registry.get("generic.word_count").is_none());
    assert!(e.registry.get("example.fp_miss").is_some());

    let err = call(&e, "example.fp_miss", json!({}))
        .await
        .expect_err("fingerprint miss");
    assert!(err.to_string().contains("disabled for this run"), "{err}");
    assert!(e.registry.get("example.fp_miss").is_none());

    // Even with a healthy row again, the tool stays off for this process.
    e.store
        .tool_health_set(health("example.com", "fp_miss", ToolStatus::Saved))
        .await
        .unwrap();
    let out = fetch().await.expect("fetch again");
    assert!(!tools_line(&out.text).contains("fp_miss"), "{}", out.text);
    assert!(e.registry.get("example.fp_miss").is_none());
}
