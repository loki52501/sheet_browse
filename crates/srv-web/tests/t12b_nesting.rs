//! T-12b round 3 (W-2, D-519; review-T-12b round 2 Blockings 1 and 2): deeply nested
//! markup through fetch (text/html and text/plain), feed (RSS, iCal, deep XML) and search
//! returns within the request budget and never takes the process down. Each call runs on
//! its own thread and runtime, so a stuck call fails the test instead of hanging it; a
//! stack overflow aborts this test binary, which also fails it.

mod common;

use common::*;
use serde_json::json;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn no_tools() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no-tools")
}

const LEVELS: usize = 100_000;

/// W-2's request budget, with room for a loaded machine in a debug build.
const CALL_MAX: Duration = Duration::from_secs(15);

fn routes() -> Vec<Route> {
    let divs = format!(
        "<html><body><p>before words</p>{}deep words</body></html>",
        "<div>".repeat(LEVELS)
    );
    let bold = format!("before words\n\n{}deep words", "<b>".repeat(LEVELS));
    let spans = format!(
        "<p>before words</p>{}deep words",
        "<span><i>".repeat(LEVELS)
    );
    let rss = format!(
        r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>Deep</title>
<item><title>Deep summary</title><link>https://n.test/a</link><description>{}x</description></item>
<item><title>After</title><link>https://n.test/b</link><description>plain</description></item>
</channel></rss>"#,
        "&lt;div&gt;x".repeat(LEVELS)
    );
    let xml = format!(
        r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>Deep XML</title>
<item><title>First</title><link>https://n.test/c</link>{}{}</item>
</channel></rss>"#,
        "<x>".repeat(LEVELS),
        "</x>".repeat(LEVELS)
    );
    let ics = format!(
        "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Deep\r\nDTSTART:20261025T180000Z\r\n\
         DESCRIPTION:{}x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        "<div>".repeat(LEVELS)
    );
    let searx = json!({
        "results": [
            {"url": "https://n.test/r1", "title": "First", "content": "<div>".repeat(LEVELS)},
            {"url": "https://n.test/r2", "title": "<b>".repeat(LEVELS), "content": "second"}
        ]
    })
    .to_string();
    vec![
        route(Some("n.test"), "/divs.html", 200, "text/html", divs),
        route(Some("n.test"), "/bold.txt", 200, "text/plain", bold),
        route(Some("n.test"), "/spans", 200, "", spans),
        route(Some("n.test"), "/deep.rss", 200, "application/rss+xml", rss),
        route(
            Some("n.test"),
            "/deep-xml.rss",
            200,
            "application/rss+xml",
            xml,
        ),
        route(Some("n.test"), "/deep.ics", 200, "text/calendar", ics),
        route(
            Some("search.local"),
            "/search",
            200,
            "application/json",
            searx,
        ),
    ]
}

/// One call on its own thread and runtime: its text (or error) and how long it took, or a
/// failure when it does not return within twice the budget.
fn timed(tool: &'static str, args: serde_json::Value) -> (Duration, Result<String, String>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let shown = args.to_string();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let server = Server::start(routes()).await;
            let e = env(config(&server, &["n.test"], no_tools())).await;
            let started = Instant::now();
            let r = call(&e, tool, args).await;
            let _ = tx.send((
                started.elapsed(),
                r.map(|o| o.text).map_err(|e| e.to_string()),
            ));
        });
    });
    rx.recv_timeout(CALL_MAX * 2)
        .unwrap_or_else(|_| panic!("{tool} {shown} did not return within {:?}", CALL_MAX * 2))
}

#[test]
fn deeply_nested_pages_fetch_in_time_and_say_what_is_missing() {
    let mut bad = Vec::new();
    for path in ["/divs.html", "/bold.txt", "/spans"] {
        let (took, r) = timed("fetch", json!({ "url": format!("http://n.test{path}") }));
        eprintln!("fetch {path}: {took:?}");
        if took > CALL_MAX {
            bad.push(format!("fetch {path} took {took:?}"));
        }
        match r {
            Ok(t) => {
                if !t.contains("before words") {
                    bad.push(format!(
                        "fetch {path}: the text before the nesting is missing"
                    ));
                }
                if !t.contains("page nested too deep to read past 512 levels") {
                    bad.push(format!("fetch {path}: no note that the rest is missing"));
                }
                if t.contains("deep words") {
                    bad.push(format!("fetch {path}: text past the cap was read"));
                }
            }
            Err(e) => bad.push(format!("fetch {path} failed: {e}")),
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn deeply_nested_feeds_and_search_answers_return_in_time() {
    let mut bad = Vec::new();
    let calls = [
        ("feed", "/deep.rss", "After"),
        // feed-rs refuses XML this deep as not a feed; what matters is that it returns.
        ("feed", "/deep-xml.rss", ""),
        ("feed", "/deep.ics", "Deep"),
        ("search", "", "second"),
    ];
    for (tool, path, want) in calls {
        let args = if tool == "search" {
            json!({ "q": "deep" })
        } else {
            json!({ "url": format!("http://n.test{path}") })
        };
        let (took, r) = timed(tool, args);
        eprintln!("{tool} {path}: {took:?}");
        if took > CALL_MAX {
            bad.push(format!("{tool} {path} took {took:?}"));
        }
        match r {
            Ok(t) => {
                if !t.contains(want) {
                    bad.push(format!("{tool} {path}: {want:?} missing"));
                }
                if t.contains("<div") || t.contains("<b>") {
                    bad.push(format!("{tool} {path}: markup shown"));
                }
            }
            Err(e) if want.is_empty() && e.contains("did not answer with RSS") => {}
            Err(e) => bad.push(format!("{tool} {path} failed: {e}")),
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
