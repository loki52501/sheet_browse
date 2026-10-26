//! Real Chromium over `--remote-debugging-pipe` (B-3, D-600): framing, replies, events and
//! a kill that leaves no process behind.

mod common;

use sb_cdp::{process_running, Cdp, Pipe, Transport};
use serde_json::json;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn pipe_round_trip_then_kill_leaves_no_process() {
    let Some(chrome) = common::chromium() else {
        return;
    };
    let profile = tempfile::Builder::new()
        .prefix("sb-cdp-pipe-")
        .tempdir()
        .unwrap();
    let args: Vec<OsString> = vec![
        "--remote-debugging-pipe".into(),
        format!("--user-data-dir={}", profile.path().display()).into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--headless=new".into(),
        "about:blank".into(),
    ];
    let pipe = Arc::new(Pipe::launch(&chrome, &args).expect("launch"));
    let cdp = Cdp::new(pipe.clone() as Arc<dyn Transport>);
    let mut events = cdp.events();

    // Two commands in flight at once over the pipe.
    let version = cdp.start(None, "Browser.getVersion", json!({}));
    let discover = cdp.start(None, "Target.setDiscoverTargets", json!({"discover": true}));
    let version = version.await.expect("Browser.getVersion");
    discover.await.expect("Target.setDiscoverTargets");
    let product = version["product"].as_str().unwrap_or_default();
    assert!(product.contains("Chrome/"), "product {product}");

    // setDiscoverTargets reports the existing about:blank page as an event.
    let created = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let e = events.recv().await.expect("event stream open");
            if e.method == "Target.targetCreated" && e.params["targetInfo"]["type"] == "page" {
                return e;
            }
        }
    })
    .await
    .expect("Target.targetCreated within 10 s");
    assert_eq!(created.session, None);

    let err = cdp
        .call(None, "No.suchMethod", json!({}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("No.suchMethod"), "{err}");

    let pids = pipe.pids();
    assert!(pids.contains(&pipe.pid()), "job pids {pids:?}");
    assert!(pids.len() > 1, "browser helpers in the job: {pids:?}");
    assert!(pipe.kill(Duration::from_secs(30)), "all processes exited");
    for pid in &pids {
        assert!(!process_running(*pid), "pid {pid} still running");
    }
    // EOF on fd 4 closes the client.
    tokio::time::timeout(Duration::from_secs(10), cdp.closed())
        .await
        .expect("client closed after the browser died");
}
