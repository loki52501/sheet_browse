//! review-T-12 round 2 fixes, offline: rule 5 fails closed by construction (D-508), the
//! private-address guard covers names and `#id` links, one deadline covers redirects and
//! body (W-2), tool-health read failures unregister site tools (D-506), and the site tool
//! output cap.

mod common;

use common::*;
use harness_core::ToolError;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn no_tools() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no-tools")
}

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

/// A route with an optional content type and raw body bytes.
fn raw(host: &str, path: &str, ctype: Option<&str>, body: Vec<u8>) -> Route {
    Route {
        host: Some(host.to_string()),
        path: path.to_string(),
        reply: Reply::Fixed {
            status: 200,
            headers: ctype
                .map(|c| vec![("Content-Type".to_string(), c.to_string())])
                .unwrap_or_default(),
            body,
        },
    }
}

fn utf16(s: &str, big_endian: bool) -> Vec<u8> {
    let mut out = if big_endian {
        vec![0xFE, 0xFF]
    } else {
        vec![0xFF, 0xFE]
    };
    for u in s.encode_utf16() {
        let b = if big_endian {
            u.to_be_bytes()
        } else {
            u.to_le_bytes()
        };
        out.extend_from_slice(&b);
    }
    out
}

// ---------------------------------------------------------------------------------------
// Rule 5: every round-2 bypass, through fetch and api

const DOC: &str = "<!DOCTYPE html><html><head><title>T</title></head><body>\
    <p>Hello visible words</p><ul><li>Item one</li></ul>\
    <script>stealCookies()</script></body></html>";

/// Markup that must never reach the model (compared lowercased).
const FORBIDDEN: &[&str] = &[
    "<script",
    "<!doctype",
    "<html",
    "<head",
    "<body",
    "<p>",
    "<ul",
    "<li",
    "<tr",
    "<td",
    "<em",
    "<pre",
    "<my-el",
    "stealcookies",
];

/// (name, body, a word the HTML walk must show when fetch reads it).
fn bypasses() -> Vec<(&'static str, Vec<u8>, Option<&'static str>)> {
    let s = |x: &str| x.as_bytes().to_vec();
    let script = "<script>stealCookies()</script>";
    vec![
        (
            "utf-16le bom",
            utf16(DOC, false),
            Some("Hello visible words"),
        ),
        (
            "utf-16be bom",
            utf16(DOC, true),
            Some("Hello visible words"),
        ),
        ("unclosed comment", s(&format!("<!-- note\n{DOC}")), None),
        (
            "empty comment",
            s(&format!("<!-->{DOC}")),
            Some("Hello visible words"),
        ),
        (
            "text prefix",
            s(&format!("Some text first\n{DOC}")),
            Some("Hello visible words"),
        ),
        (
            "xssi prefix",
            s(&format!(")]}}'\n{DOC}")),
            Some("Hello visible words"),
        ),
        (
            "json prefix",
            s(&format!("{{\"ok\":1}}{DOC}")),
            Some("Hello visible words"),
        ),
        (
            "zero-width space",
            s(&format!("\u{200B}{DOC}")),
            Some("Hello visible words"),
        ),
        ("nul", s(&format!("\0{DOC}")), Some("Hello visible words")),
        (
            "doctype two spaces",
            s(&DOC.replace("<!DOCTYPE html>", "<!DOCTYPE  html>")),
            Some("Hello visible words"),
        ),
        (
            "doctype newline",
            s(&DOC.replace("<!DOCTYPE html>", "<!doctype\nhtml>")),
            Some("Hello visible words"),
        ),
        (
            "li fragment",
            s(&format!("<li>Item one</li><li>Item two</li>{script}")),
            Some("Item two"),
        ),
        (
            "tr fragment",
            s(&format!("<tr><td>Cell one</td></tr>{script}")),
            Some("Cell one"),
        ),
        (
            "em fragment",
            s(&format!("<em>Emphasis words</em>{script}")),
            Some("Emphasis words"),
        ),
        (
            "pre fragment",
            s(&format!("<pre>Preformatted words</pre>{script}")),
            Some("Preformatted words"),
        ),
        (
            "custom element fragment",
            s(&format!("<my-el>Custom words</my-el>{script}")),
            Some("Custom words"),
        ),
    ]
}

fn leaks(text: &str) -> Vec<&'static str> {
    let t = text.to_ascii_lowercase();
    FORBIDDEN
        .iter()
        .copied()
        .filter(|f| t.contains(f))
        .collect()
}

fn shown(r: &Result<harness_core::ToolOutput, ToolError>) -> String {
    match r {
        Ok(o) => o.text.clone(),
        Err(e) => e.to_string(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn round2_rule5_bypasses_never_reach_the_model() {
    let cases = bypasses();
    let mut routes = Vec::new();
    for (i, (_, body, _)) in cases.iter().enumerate() {
        routes.push(raw(
            "x.test",
            &format!("/{i}/plain"),
            Some("text/plain"),
            body.clone(),
        ));
        routes.push(raw("x.test", &format!("/{i}/none"), None, body.clone()));
        routes.push(raw(
            "x.test",
            &format!("/{i}/xml"),
            Some("application/xml"),
            body.clone(),
        ));
    }
    let server = Server::start(routes).await;
    let e = env(config(&server, &["x.test"], no_tools())).await;
    let mut failures = Vec::new();
    for (i, (name, _, visible)) in cases.iter().enumerate() {
        for ty in ["plain", "none"] {
            let url = format!("http://x.test/{i}/{ty}");
            let r = call(&e, "fetch", json!({ "url": url })).await;
            let t = shown(&r);
            let leaked = leaks(&t);
            if !leaked.is_empty() {
                failures.push(format!("LEAK fetch {ty} [{name}]: {leaked:?}"));
            }
            if let Some(v) = visible {
                if !t.contains(v) {
                    failures.push(format!("fetch {ty} [{name}]: the walk did not show {v:?}"));
                }
            }
        }
        for ty in ["plain", "none", "xml"] {
            let url = format!("http://x.test/{i}/{ty}");
            let r = call(&e, "api", json!({ "url": url })).await;
            let t = shown(&r);
            let leaked = leaks(&t);
            if !leaked.is_empty() {
                failures.push(format!("LEAK api {ty} [{name}]: {leaked:?}"));
            }
            if !t.contains("not shown (not JSON or plain text; use fetch to read pages)") {
                failures.push(format!("api {ty} [{name}]: not the not-shown line"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Raw-text and RCDATA elements hold markup the parser does not read (`<plaintext>` takes
/// the rest of the body): the walk must not emit it as text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_text_elements_never_carry_markup_to_the_model() {
    let script = "<script>stealCookies()</script>";
    let bodies = [
        format!("<p>Kept words</p><plaintext>{DOC}"),
        format!("<p>Kept words</p><xmp>{DOC}</xmp>"),
        format!("<p>Kept words</p><noembed>{script}</noembed><noframes>{script}</noframes>"),
        format!("<title>{script}<li>x</li></title><p>Kept words</p>"),
        format!("<p>Kept words</p><textarea aria-label=\"Note\">{script}<li>x</li></textarea>"),
    ];
    let mut routes = Vec::new();
    for (i, b) in bodies.iter().enumerate() {
        for (ty, ct) in [("html", Some("text/html")), ("plain", Some("text/plain"))] {
            routes.push(raw(
                "r.test",
                &format!("/{i}/{ty}"),
                ct,
                b.clone().into_bytes(),
            ));
        }
    }
    let server = Server::start(routes).await;
    let e = env(config(&server, &["r.test"], no_tools())).await;
    let mut failures = Vec::new();
    for i in 0..bodies.len() {
        for ty in ["html", "plain"] {
            for mode in ["read", "sheet"] {
                let url = format!("http://r.test/{i}/{ty}");
                let r = call(&e, "fetch", json!({ "url": url, "mode": mode })).await;
                let t = shown(&r);
                let leaked = leaks(&t);
                if !leaked.is_empty() {
                    failures.push(format!("LEAK fetch {ty} {mode} body {i}: {leaked:?}"));
                }
                // The sheet lists elements, not prose; read shows the text.
                if mode == "read" && !t.contains("Kept words") {
                    failures.push(format!("fetch {ty} {mode} body {i}: lost the page text"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_shows_only_json_or_tag_free_plain_text() {
    let j = Some("application/json");
    let server = Server::start(vec![
        raw(
            "a.test",
            "/json-html",
            j,
            br#"{"body_html":"<b>hi</b>"}"#.to_vec(),
        ),
        raw(
            "a.test",
            "/vnd",
            Some("application/vnd.github+json; charset=utf-8"),
            br#"{"full_name":"tokio-rs/tokio"}"#.to_vec(),
        ),
        raw("a.test", "/none-json", None, br#"{"a":1}"#.to_vec()),
        raw("a.test", "/xssi", j, b")]}'\n{\"guarded\":true}".to_vec()),
        raw(
            "a.test",
            "/text",
            Some("text/plain"),
            b"just words, a < b and 3<4".to_vec(),
        ),
        raw(
            "a.test",
            "/text-json",
            Some("text/plain"),
            br#"{"k":"v"}"#.to_vec(),
        ),
        raw(
            "a.test",
            "/xml",
            Some("application/xml"),
            b"<?xml version=\"1.0\"?><r>x</r>".to_vec(),
        ),
        raw(
            "a.test",
            "/html",
            Some("text/html"),
            b"<p>hello</p>".to_vec(),
        ),
        raw("a.test", "/bad-json", j, b"<html>oops</html>".to_vec()),
    ])
    .await;
    let e = env(config(&server, &["a.test"], no_tools())).await;
    let get = |p: &'static str| {
        let e = &e;
        async move {
            call(e, "api", json!({ "url": format!("http://a.test{p}") }))
                .await
                .unwrap_or_else(|err| panic!("api {p}: {err}"))
                .text
        }
    };
    let t = get("/json-html").await;
    assert!(t.contains(r#""body_html": "<b>hi</b>""#), "{t}");
    let t = get("/vnd").await;
    assert!(t.contains(r#""full_name": "tokio-rs/tokio""#), "{t}");
    let t = get("/none-json").await;
    assert!(t.contains(r#""a": 1"#), "{t}");
    let t = get("/xssi").await;
    assert!(t.contains(r#""guarded": true"#), "{t}");
    let t = get("/text").await;
    assert!(t.contains("just words, a < b and 3<4"), "{t}");
    let t = get("/text-json").await;
    assert!(t.contains(r#""k": "v""#), "{t}");
    let t = get("/xml").await;
    assert_eq!(
        t,
        "HTTP 200 application/xml, 29 bytes, not shown (not JSON or plain text; use fetch to \
         read pages)"
    );
    let t = get("/html").await;
    assert_eq!(
        t,
        "HTTP 200 text/html, 12 bytes, not shown (not JSON or plain text; use fetch to read \
         pages)"
    );
    let t = get("/bad-json").await;
    assert!(t.contains("not shown") && !t.contains("<html"), "{t}");
}

// ---------------------------------------------------------------------------------------
// feed: iCal text through html_to_text, decoded sniffing, no body echo

#[test]
fn ical_text_fields_lose_their_html() {
    let ics = "BEGIN:VCALENDAR\r\nX-WR-CALNAME:<b>Team</b> calendar\r\nBEGIN:VEVENT\r\n\
        SUMMARY:<b>Standup</b>\r\nLOCATION:<i>Room</i> 1\r\n\
        DESCRIPTION:Join <a href=\"https://x.test/j\">here</a><br>and <b>bring</b> snacks\
        <script>stealCookies()</script>\r\nDTSTART:20261025T090000Z\r\nEND:VEVENT\r\n\
        END:VCALENDAR\r\n";
    let p = srv_web::feed::parse(ics.as_bytes()).expect("ics");
    assert_eq!(p.title, "Team calendar");
    assert_eq!(p.items[0].title, "Standup");
    assert_eq!(p.items[0].summary, "at Room 1. Join here and bring snacks");
    let all = format!("{p:?}");
    assert!(leaks(&all).is_empty() && !all.contains("<a "), "{all}");
}

#[test]
fn utf16_ical_is_sniffed_after_decoding() {
    let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Wide\r\nDTSTART:20261101\r\n\
        END:VEVENT\r\nEND:VCALENDAR\r\n";
    let p = srv_web::feed::parse(&utf16(ics, false)).expect("a UTF-16 calendar");
    assert_eq!(p.kind, "ical");
    assert_eq!(p.items[0].title, "Wide");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feed_never_echoes_a_non_feed_body() {
    let server = Server::start(vec![
        raw(
            "f.test",
            "/plain",
            Some("text/plain"),
            DOC.as_bytes().to_vec(),
        ),
        raw("f.test", "/none", None, utf16(DOC, false)),
        raw(
            "f.test",
            "/words",
            Some("text/plain"),
            b"secret words".to_vec(),
        ),
    ])
    .await;
    let e = env(config(&server, &["f.test"], no_tools())).await;
    for p in ["/plain", "/none", "/words"] {
        let err = call(&e, "feed", json!({ "url": format!("http://f.test{p}") }))
            .await
            .expect_err("not a feed");
        let msg = err.to_string();
        assert!(msg.contains("did not answer with RSS"), "{p}: {msg}");
        assert!(
            leaks(&msg).is_empty() && !msg.contains("secret words"),
            "{p}: {msg}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// Private-address guard: `#id` links into loopback

fn link_id(sheet: &str, name: &str) -> String {
    let want = format!("link \"{name}\"");
    sheet
        .lines()
        .find(|l| l.contains(&want))
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or_else(|| panic!("no {want} in the sheet:\n{sheet}"))
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_ref_from_a_public_page_into_loopback_is_refused() {
    let secret = Server::start(vec![Route {
        host: None,
        path: "/secret".into(),
        reply: Reply::Fixed {
            status: 200,
            headers: vec![("Content-Type".into(), "text/html".into())],
            body: b"<p>internal admin page</p>".to_vec(),
        },
    }])
    .await;
    let port = secret.addr.port();
    let links = format!(
        "<p><a href=\"http://127.0.0.1:{port}/secret\">loop</a> \
         <a href=\"http://localhost:{port}/secret\">named</a> \
         <a href=\"http://10.0.0.1/\">lan</a></p>"
    );
    let pages = Server::start(vec![
        raw(
            "public.test",
            "/",
            Some("text/html"),
            links.clone().into_bytes(),
        ),
        Route {
            host: None,
            path: "/local".into(),
            reply: Reply::Fixed {
                status: 200,
                headers: vec![("Content-Type".into(), "text/html".into())],
                body: links.into_bytes(),
            },
        },
    ])
    .await;
    let e = env(config(&pages, &["public.test"], no_tools())).await;
    let sheet = call(
        &e,
        "fetch",
        json!({"url": "http://public.test/", "mode": "sheet"}),
    )
    .await
    .expect("public page")
    .text;
    for name in ["loop", "named", "lan"] {
        let err = call(&e, "fetch", json!({ "url": link_id(&sheet, name) }))
            .await
            .expect_err("a public page's link into a private address");
        assert!(
            err.to_string().contains("loopback, private or link-local"),
            "{name}: {err}"
        );
    }
    assert!(
        secret.requests().is_empty(),
        "a public page's link reached the user's machine"
    );
    // A page fetched from loopback may link within loopback.
    let local = format!("http://127.0.0.1:{}/local", pages.addr.port());
    let sheet = call(&e, "fetch", json!({"url": local, "mode": "sheet"}))
        .await
        .expect("loopback page")
        .text;
    let ok = call(&e, "fetch", json!({ "url": link_id(&sheet, "loop") }))
        .await
        .expect("loopback to loopback");
    assert!(ok.text.contains("internal admin page"), "{}", ok.text);
}

// ---------------------------------------------------------------------------------------
// W-2: one 15 s deadline across redirects and body

/// Answers every request with a redirect to `to`, after `delay`.
async fn slow_redirect(to: String, delay: Duration) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let to = to.clone();
            tokio::spawn(async move {
                // Every request on the connection (the client pools it).
                loop {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 1024];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    }
                    // A slow upstream is the condition under test; nothing to wait on.
                    tokio::time::sleep(delay).await;
                    let resp = format!(
                        "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\n\r\n"
                    );
                    if sock.write_all(resp.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    addr
}

/// Sends headers and the start of a body, then never finishes.
async fn stalled_body() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((mut sock, _)) = listener.accept().await {
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
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 100000\r\n\r\n<p>partial",
                )
                .await;
            held.push(sock);
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_deadline_covers_redirects_and_body() {
    let body = stalled_body().await;
    let hop = slow_redirect("http://body.test/".into(), Duration::from_millis(1200)).await;
    let cfg = srv_web::Config {
        tools_root: no_tools(),
        timeout: Duration::from_secs(2),
        ..srv_web::Config::default()
    }
    .with_resolve("hop.test", hop)
    .with_resolve("body.test", body);
    let e = env(cfg).await;
    for tool in ["fetch", "feed"] {
        let start = Instant::now();
        let err = call(&e, tool, json!({"url": "http://hop.test/"}))
            .await
            .expect_err("a stalled body must time out");
        let took = start.elapsed();
        assert!(
            err.to_string().contains("did not answer in time"),
            "{tool}: {err}"
        );
        assert!(
            took >= Duration::from_millis(1900) && took < Duration::from_millis(2800),
            "{tool}: gave up after {took:?}; the redirect and the body share one 2 s deadline"
        );
    }
}

// ---------------------------------------------------------------------------------------
// D-506: a health read failure unregisters the site's tools

mod flaky {
    use async_trait::async_trait;
    use harness_core::memory::*;
    use harness_core::testing::MemStore;
    use harness_core::RunId;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// MemStore whose `tool_health` fails while `fail` is set.
    pub struct Flaky {
        pub inner: Arc<MemStore>,
        pub fail: Arc<AtomicBool>,
    }

    #[async_trait]
    impl MemoryStore for Flaky {
        async fn remember(&self, f: Fact) -> Result<FactId> {
            self.inner.remember(f).await
        }
        async fn forget(&self, id: FactId) -> Result<()> {
            self.inner.forget(id).await
        }
        async fn link(&self, src: NodeId, rel: &str, dst: NodeId, source: Source) -> Result<()> {
            self.inner.link(src, rel, dst, source).await
        }
        async fn node(&self, kind: &str, name: &str) -> Result<NodeId> {
            self.inner.node(kind, name).await
        }
        async fn neighbors(&self, n: NodeId, rel: Option<&str>, depth: u8) -> Result<Vec<Node>> {
            self.inner.neighbors(n, rel, depth).await
        }
        async fn recall(&self, q: &str, k: usize, filter: Option<Filter>) -> Result<Vec<Hit>> {
            self.inner.recall(q, k, filter).await
        }
        async fn search(&self, q: &str, k: usize) -> Result<Vec<Hit>> {
            self.inner.search(q, k).await
        }
        async fn contact(&self, person: &str) -> Result<Option<Contact>> {
            self.inner.contact(person).await
        }
        async fn touch_contact(&self, person: &str, channel: &str, note: &str) -> Result<()> {
            self.inner.touch_contact(person, channel, note).await
        }
        async fn due_reviews(&self, now: Ts, k: usize) -> Result<Vec<Review>> {
            self.inner.due_reviews(now, k).await
        }
        async fn grade_review(&self, id: ReviewId, grade: u8) -> Result<()> {
            self.inner.grade_review(id, grade).await
        }
        async fn baseline_get(&self, watch: &str) -> Result<Option<Baseline>> {
            self.inner.baseline_get(watch).await
        }
        async fn baseline_set(&self, watch: &str, b: Baseline) -> Result<()> {
            self.inner.baseline_set(watch, b).await
        }
        async fn append(&self, e: LedgerEvent) -> Result<LedgerId> {
            self.inner.append(e).await
        }
        async fn tool_health(&self, domain: &str) -> Result<Vec<ToolHealth>> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(MemoryError::Invalid(
                    "tool health unavailable (test)".into(),
                ));
            }
            self.inner.tool_health(domain).await
        }
        async fn tool_health_set(&self, h: ToolHealth) -> Result<()> {
            self.inner.tool_health_set(h).await
        }
        async fn gate_rule(&self, domain: &str, class: &str) -> Result<Option<GateRule>> {
            self.inner.gate_rule(domain, class).await
        }
        async fn run_start(&self, run: Run) -> Result<()> {
            self.inner.run_start(run).await
        }
        async fn run_end(&self, id: &RunId, status: RunStatus, ended: Ts) -> Result<()> {
            self.inner.run_end(id, status, ended).await
        }
        async fn run_get(&self, id: &RunId) -> Result<Option<Run>> {
            self.inner.run_get(id).await
        }
        async fn schedule_add(&self, s: Schedule) -> Result<i32> {
            self.inner.schedule_add(s).await
        }
        async fn schedule_list(&self) -> Result<Vec<Schedule>> {
            self.inner.schedule_list().await
        }
        async fn schedule_rm(&self, id: i32) -> Result<()> {
            self.inner.schedule_rm(id).await
        }
        async fn schedule_fired(&self, id: i32, at: Ts) -> Result<()> {
            self.inner.schedule_fired(id, at).await
        }
        async fn gate_rule_set(
            &self,
            domain: &str,
            class: &str,
            rule: GateRule,
            note: &str,
        ) -> Result<()> {
            self.inner.gate_rule_set(domain, class, rule, note).await
        }
        async fn review_add(&self, item: &str, answer: &str) -> Result<ReviewId> {
            self.inner.review_add(item, answer).await
        }
        async fn ledger_events(&self, run: &RunId) -> Result<Vec<(LedgerId, LedgerEvent)>> {
            self.inner.ledger_events(run).await
        }
    }
}

const SHOP: &str = r#"<!doctype html><html><head><title>Items</title></head><body><header><nav aria-label="Main">
    <a href="/">Home</a> <a href="/shop/">Shop</a> <a href="/about">About us</a>
    <a href="/contact">Contact</a> <a href="/help">Help</a> <a href="/login">Log in</a>
  </nav><form role="search"><input type="search" aria-label="Search the shop"><button>Search</button></form></header>
  <main><h1>Items</h1>
  <ul><li>Apple <a href="/shop/apple">Details</a></li><li>Pear <a href="/shop/pear">Details</a></li></ul>
  <label for="qty">Quantity</label><input id="qty" type="number" value="1">
  <button>Add to cart</button> <button>Save for later</button>
  </main><footer><a href="/privacy">Privacy</a> <a href="/terms">Terms</a></footer></body></html>"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_read_failure_unregisters_the_sites_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tools_copy(tmp.path());
    let server = Server::start(vec![raw(
        "shop.example.com",
        "/shop/items",
        Some("text/html"),
        SHOP.as_bytes().to_vec(),
    )])
    .await;
    let mut e = env(config(&server, &["shop.example.com"], tools)).await;
    let fail = Arc::new(AtomicBool::new(false));
    e.harness.store = Arc::new(flaky::Flaky {
        inner: e.store.clone(),
        fail: fail.clone(),
    });
    let fetch = || {
        call(
            &e,
            "fetch",
            json!({"url": "http://shop.example.com/shop/items"}),
        )
    };
    let out = fetch().await.expect("fetch");
    assert!(out.text.contains("tools for this site: "), "{}", out.text);
    assert!(e.registry.get("example.list_items").is_some());
    assert!(e.registry.get("generic.word_count").is_some());

    fail.store(true, Ordering::SeqCst);
    let out = fetch().await.expect("fetch with health unreadable");
    assert!(!out.text.contains("tools for this site: "), "{}", out.text);
    for name in [
        "example.list_items",
        "example.strict_read",
        "example.fp_miss",
        "generic.word_count",
    ] {
        assert!(
            e.registry.get(name).is_none(),
            "{name} stayed registered while its health could not be read"
        );
    }
}

// ---------------------------------------------------------------------------------------
// Site tool output cap (SITE_OUTPUT_MAX_CHARS = 20 000)

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn site_tool_output_is_capped_at_20000_chars() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tools_copy(tmp.path());
    std::fs::write(
        tools.join("example.com").join("big_out.js"),
        "// @name big_out\n// @kind reader\n// @match /*\n// @gate Read\n// @phase 1\n\
         // srv-web test: output longer than the 20 000 character cap.\n\
         export default function (page) {\n  return { s: \"y\".repeat(30000) };\n}\n",
    )
    .unwrap();
    let server = Server::start(vec![raw(
        "shop.example.com",
        "/shop/items",
        Some("text/html"),
        SHOP.as_bytes().to_vec(),
    )])
    .await;
    let e = env(config(&server, &["shop.example.com"], tools)).await;
    call(
        &e,
        "fetch",
        json!({"url": "http://shop.example.com/shop/items"}),
    )
    .await
    .expect("fetch");
    let out = call(&e, "example.big_out", json!({}))
        .await
        .expect("big_out");
    let trailer = "\ntruncated: output cut at 20000 characters";
    let body = out
        .text
        .split(trailer)
        .next()
        .expect("text before the trailer");
    assert!(out.text.contains(trailer), "no truncation line");
    assert_eq!(body.chars().count(), 20_000);
    assert!(body.starts_with("{\n  \"s\": \"yyy"), "{}", &body[..40]);
}
