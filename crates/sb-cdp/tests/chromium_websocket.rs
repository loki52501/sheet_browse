//! Attach to a running Chrome over ws:// (B-3): `--remote-debugging-port=0`, the port read
//! from `DevToolsActivePort` with a capped condition wait.

mod common;

use sb_cdp::{devtools_active_port, Cdp, Transport, WebSocket};
use serde_json::json;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn websocket_attaches_to_a_running_chrome() {
    let Some(chrome) = common::chromium() else {
        return;
    };
    let profile = tempfile::Builder::new()
        .prefix("sb-cdp-ws-")
        .tempdir()
        .unwrap();
    let mut child = Command::new(&chrome)
        .arg("--remote-debugging-port=0")
        .arg(format!("--user-data-dir={}", profile.path().display()))
        .args([
            "--no-first-run",
            "--no-default-browser-check",
            "--headless=new",
            "about:blank",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn chrome");

    let url = devtools_active_port(profile.path(), Duration::from_secs(30))
        .await
        .expect("DevToolsActivePort within 30 s");
    assert!(url.starts_with("ws://127.0.0.1:"), "{url}");
    let ws = WebSocket::connect(&url).await.expect("connect");
    let cdp = Cdp::new(Arc::new(ws) as Arc<dyn Transport>);

    let version = cdp
        .call(None, "Browser.getVersion", json!({}))
        .await
        .unwrap();
    assert!(version["product"]
        .as_str()
        .unwrap_or_default()
        .contains("Chrome/"));
    let targets = cdp
        .call(None, "Target.getTargets", json!({}))
        .await
        .unwrap();
    let pages = targets["targetInfos"]
        .as_array()
        .map(|a| a.iter().filter(|t| t["type"] == "page").count())
        .unwrap_or(0);
    assert!(pages >= 1, "{targets}");

    // Flattened session over ws: attach to the page and evaluate inside it.
    let target = targets["targetInfos"]
        .as_array()
        .and_then(|a| a.iter().find(|t| t["type"] == "page"))
        .and_then(|t| t["targetId"].as_str())
        .unwrap()
        .to_string();
    let attached = cdp
        .call(
            None,
            "Target.attachToTarget",
            json!({"targetId": target, "flatten": true}),
        )
        .await
        .unwrap();
    let session = attached["sessionId"].as_str().unwrap();
    let r = cdp
        .call(
            Some(session),
            "Runtime.evaluate",
            json!({"expression": "6*7", "returnByValue": true}),
        )
        .await
        .unwrap();
    assert_eq!(r["result"]["value"], 42);

    // Close the browser and wait for it to exit (a condition with a cap), so the profile
    // directory is free to delete.
    // Queued synchronously by `start`; the reply may never arrive as the browser exits.
    drop(cdp.start(None, "Browser.close", json!({})));
    let exited = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(move || {
            let status = child.wait();
            (child, status)
        }),
    )
    .await;
    match exited {
        Ok(Ok((_, status))) => assert!(status.is_ok()),
        _ => panic!("chrome did not exit within 30 s of Browser.close"),
    }
    tokio::time::timeout(Duration::from_secs(10), cdp.closed())
        .await
        .expect("socket closed after Browser.close");
}
