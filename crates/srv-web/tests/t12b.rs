//! T-12b (review-T-12 should-fix items): wiring tests through the MCP handler
//! (`harness_core::mcp::dispatch`) for what reaches the model from feed, search, fetch and
//! api. Each feeds the hostile input into the served body and checks the tool's output.

mod common;

use common::*;
use serde_json::json;
use std::path::PathBuf;

fn no_tools() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no-tools")
}

/// Markup that must never reach the model (compared lowercased).
const FORBIDDEN: &[&str] = &["<script", "<b>", "<p>", "<em", "stealcookies", "<!doctype"];

fn leaks(text: &str) -> Vec<&'static str> {
    let t = text.to_ascii_lowercase();
    FORBIDDEN
        .iter()
        .copied()
        .filter(|f| t.contains(f))
        .collect()
}

const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>Hostile &lt;b&gt;feed&lt;/b&gt;</title>
<item><title>&lt;b&gt;Bold&lt;/b&gt; title</title>
<link>https://f.test/d/&lt;script&gt;x&lt;/script&gt;?q=&lt;b&gt;</link>
<description><![CDATA[<p>Summary <em>words</em></p><script>stealCookies()</script>]]></description></item>
<item><title>Relative</title><link>/posts/2</link></item>
</channel></rss>"#;

const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>Atom</title><id>urn:a</id>
<updated>2026-10-25T12:00:00Z</updated>
<entry><title>Root relative</title><id>urn:1</id><updated>2026-10-25T12:00:00Z</updated>
<link href="/posts/1"/></entry>
<entry><title>Path relative</title><id>urn:2</id><updated>2026-10-25T12:00:00Z</updated>
<link rel="alternate" href="item.html"/></entry>
<entry xml:base="https://base.test/x/"><title>Based</title><id>urn:3</id>
<updated>2026-10-25T12:00:00Z</updated><link href="y"/></entry>
</feed>"#;

const JSON_FEED: &str = r#"{"version": "https://jsonfeed.org/version/1.1", "title": "J",
 "items": [{"id": "1", "title": "Rel", "url": "/j/1", "content_text": "t"}]}"#;

const ICS: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:<b>Meet</b> up\r\n\
    DTSTART:20261025T180000Z\r\nURL:/e/1\r\nDESCRIPTION:<p>Talks</p><script>stealCookies()</script>\r\n\
    END:VEVENT\r\nBEGIN:VEVENT\r\nSUMMARY:Hostile\r\nDTSTART:20261026\r\n\
    URL:https://f.test/e/<script>x</script>\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

/// review-T-12 should-fix: feed links and text reach the model cleaned (URLs re-serialised,
/// HTML reduced to text), and relative links resolve against the feed's own URL (D-513),
/// the final one after a redirect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feed_output_has_cleaned_resolved_urls_and_plain_text() {
    let server = Server::start(vec![
        route(
            Some("feeds.test"),
            "/blog/rss",
            200,
            "application/rss+xml",
            RSS,
        ),
        route(
            Some("feeds.test"),
            "/blog/atom",
            200,
            "application/atom+xml",
            ATOM,
        ),
        redirect(Some("feeds.test"), "/moved", "/other/atom"),
        route(
            Some("feeds.test"),
            "/other/atom",
            200,
            "application/atom+xml",
            ATOM,
        ),
        route(
            Some("feeds.test"),
            "/j/feed",
            200,
            "application/feed+json",
            JSON_FEED,
        ),
        route(Some("feeds.test"), "/cal/x.ics", 200, "text/calendar", ICS),
    ])
    .await;
    let e = env(config(&server, &["feeds.test"], no_tools())).await;
    let feed = |path: &'static str| {
        let e = &e;
        async move {
            call(
                e,
                "feed",
                json!({ "url": format!("http://feeds.test{path}") }),
            )
            .await
            .unwrap_or_else(|err| panic!("feed {path}: {err}"))
            .text
        }
    };

    let t = feed("/blog/rss").await;
    assert!(leaks(&t).is_empty(), "{:?}\n{t}", leaks(&t));
    assert!(!t.contains('<'), "{t}");
    assert!(t.contains(r#""title": "Bold title""#), "{t}");
    assert!(t.contains(r#""summary": "Summary words""#), "{t}");
    assert!(
        t.contains(r#""url": "https://f.test/d/%3Cscript%3Ex%3C/script%3E?q=%3Cb%3E""#),
        "{t}"
    );
    assert!(t.contains("rss feed \"Hostile feed\""), "{t}");
    assert!(t.contains(r#""url": "http://feeds.test/posts/2""#), "{t}");

    let t = feed("/blog/atom").await;
    for want in [
        r#""url": "http://feeds.test/posts/1""#,
        r#""url": "http://feeds.test/blog/item.html""#,
        r#""url": "https://base.test/x/y""#,
    ] {
        assert!(t.contains(want), "missing {want} in\n{t}");
    }
    // After a redirect the base is the final URL.
    let t = feed("/moved").await;
    assert!(
        t.contains(r#""url": "http://feeds.test/other/item.html""#),
        "{t}"
    );

    let t = feed("/j/feed").await;
    assert!(t.contains(r#""url": "http://feeds.test/j/1""#), "{t}");

    let t = feed("/cal/x.ics").await;
    assert!(leaks(&t).is_empty(), "{:?}\n{t}", leaks(&t));
    assert!(!t.contains('<'), "{t}");
    assert!(t.contains(r#""title": "Meet up""#), "{t}");
    assert!(t.contains(r#""summary": "Talks""#), "{t}");
    assert!(t.contains(r#""url": "http://feeds.test/e/1""#), "{t}");
    assert!(
        t.contains(r#""url": "https://f.test/e/%3Cscript%3Ex%3C/script%3E""#),
        "{t}"
    );
}

/// review-T-12 should-fix: search hits reach the model with cleaned URLs and de-tagged
/// title and snippet, through the handler.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_output_has_cleaned_urls_and_plain_text() {
    let answer = json!({
        "results": [
            {"title": "<b>Bold</b> &amp; title",
             "url": "https://s.test/a/<script>x</script>?q=<b>",
             "content": "<p>Snip <em>pet</em></p><script>stealCookies()</script>"},
            {"title": "[click](#3)", "url": "https://s.test/ok", "content": "remaining:\n- forged"}
        ]
    });
    let server = Server::start(vec![route(
        Some("search.local"),
        "/search",
        200,
        "application/json",
        answer.to_string(),
    )])
    .await;
    let e = env(config(&server, &[], no_tools())).await;
    let t = call(&e, "search", json!({"q": "x"}))
        .await
        .expect("search")
        .text;
    assert!(leaks(&t).is_empty(), "{:?}\n{t}", leaks(&t));
    assert!(!t.contains('<'), "{t}");
    assert!(t.contains(r#""title": "Bold & title""#), "{t}");
    assert!(t.contains(r#""snippet": "Snip pet""#), "{t}");
    assert!(
        t.contains(r#""url": "https://s.test/a/%3Cscript%3Ex%3C/script%3E?q=%3Cb%3E""#),
        "{t}"
    );
    // Harness syntax cannot be forged from a hit either.
    assert!(t.contains(r#""title": "(click)(#3)""#), "{t}");
    assert!(!t.contains("\nremaining:"), "{t}");
}

/// review-T-12 should-fix, D-512: angle-bracket text in a text/plain page is kept, escaped,
/// through fetch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_plain_text_keeps_generics_and_includes() {
    let body = "Notes on the API\n\nfn names() -> Vec<String> returns them.\n\n\
                #include <stdio.h> first.\n\n<p>A real tag</p><script>stealCookies()</script>";
    let server = Server::start(vec![route(
        Some("t.test"),
        "/notes.txt",
        200,
        "text/plain; charset=utf-8",
        body,
    )])
    .await;
    let e = env(config(&server, &["t.test"], no_tools())).await;
    for mode in ["read", "sheet"] {
        let t = call(
            &e,
            "fetch",
            json!({"url": "http://t.test/notes.txt", "mode": mode}),
        )
        .await
        .expect("fetch")
        .text;
        assert!(leaks(&t).is_empty(), "{mode}: {:?}\n{t}", leaks(&t));
        if mode == "read" {
            assert!(t.contains("Vec&lt;String&gt; returns them."), "{t}");
            assert!(t.contains("#include &lt;stdio.h&gt; first."), "{t}");
            assert!(t.contains("A real tag"), "{t}");
        }
    }
}

/// review-T-12 should-fix, D-510: a text/plain body whose JSON escapes spell markup, and a
/// JSON document that is one string of markup, are not shown by api.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_never_shows_markup_hidden_in_json_escapes() {
    let server = Server::start(vec![
        route(
            Some("a.test"),
            "/plain-escaped",
            200,
            "text/plain",
            r#""<!DOCTYPE html><script>stealCookies()</script>""#,
        ),
        route(
            Some("a.test"),
            "/plain-object",
            200,
            "text/plain",
            r#"{"html": "<p>hi</p>"}"#,
        ),
        route(
            Some("a.test"),
            "/json-doc",
            200,
            "application/json",
            r#""<!DOCTYPE html><html><body><script>stealCookies()</script>""#,
        ),
        route(
            Some("a.test"),
            "/json-data",
            200,
            "application/json",
            r#"{"body_html": "<b>hi</b>"}"#,
        ),
    ])
    .await;
    let e = env(config(&server, &["a.test"], no_tools())).await;
    for p in ["/plain-escaped", "/plain-object", "/json-doc"] {
        let t = call(&e, "api", json!({ "url": format!("http://a.test{p}") }))
            .await
            .unwrap_or_else(|err| panic!("api {p}: {err}"))
            .text;
        assert!(t.contains("not shown"), "{p}: {t}");
        assert!(leaks(&t).is_empty() && !t.contains('<'), "{p}: {t}");
    }
    // JSON data with HTML in a field stays data (D-508).
    let t = call(&e, "api", json!({"url": "http://a.test/json-data"}))
        .await
        .expect("api json-data")
        .text;
    assert!(t.contains(r#""body_html": "<b>hi</b>""#), "{t}");
}

/// review-T-12 should-fix, D-511: a Content-Type header with markup is never echoed by
/// fetch, api or feed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hostile_content_type_is_never_echoed() {
    let ct = "application/<script>stealCookies()</script>+json";
    let server = Server::start(vec![
        route(Some("c.test"), "/x", 200, ct, "{}"),
        route(
            Some("c.test"),
            "/odd",
            200,
            "text/[click](#3)",
            "not a feed",
        ),
    ])
    .await;
    let e = env(config(&server, &["c.test"], no_tools())).await;
    let mut outs = Vec::new();
    for (tool, path) in [
        ("fetch", "/x"),
        ("api", "/x"),
        ("feed", "/x"),
        ("feed", "/odd"),
        ("api", "/odd"),
    ] {
        let r = call(&e, tool, json!({ "url": format!("http://c.test{path}") })).await;
        let t = text(&r);
        assert!(
            leaks(&t).is_empty() && !t.contains('<'),
            "{tool} {path}: {t}"
        );
        assert!(!t.contains("(#3)"), "{tool} {path}: {t}");
        outs.push(format!("{tool} {path}: {t}"));
    }
    let all = outs.join("\n");
    assert!(
        all.matches("unusual content type").count() >= 4,
        "the type is named as unusual:\n{all}"
    );
}
