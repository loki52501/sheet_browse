//! Real Chromium (D-34 build) through `Driver::launch`: bench headless over the pipe, the
//! auto-attach setup, a session command, events on both broadcasts and in the registry
//! (B-4 #2), and a drop that leaves no process and no bench profile behind (B-2, D-601).

mod common;

use sb_cdp::process_running;
use sb_driver::{Driver, DriverError, LaunchEnv, LaunchOpts, Profile, TabId};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;

const CAP: Duration = Duration::from_secs(30);

async fn next_matching(
    rx: &mut broadcast::Receiver<Arc<sb_cdp::Event>>,
    what: &str,
    mut pred: impl FnMut(&sb_cdp::Event) -> bool,
) -> Arc<sb_cdp::Event> {
    tokio::time::timeout(CAP, async {
        loop {
            match rx.recv().await {
                Ok(e) if pred(&e) => return e,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("stream closed before {what}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} within {CAP:?}"))
}

/// A tiny HTTP server on 127.0.0.1. `/` embeds a srcdoc frame and `http://localhost:<port>/child`,
/// a different site, so Chromium puts that child in its own process (an OOPIF target).
async fn serve() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut n = 0;
                while n < buf.len() {
                    match sock.read(&mut buf[n..]).await {
                        Ok(0) | Err(_) => break,
                        Ok(k) => n += k,
                    }
                    if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/");
                let (status, body) = match path {
                    "/" => (
                        "200 OK",
                        format!(
                            "<!doctype html><title>T18</title>\
                             <iframe srcdoc=\"<p>same</p>\"></iframe>\
                             <iframe src=\"http://localhost:{port}/child\"></iframe>"
                        ),
                    ),
                    "/child" => (
                        "200 OK",
                        "<!doctype html><title>child</title><p>x</p>".into(),
                    ),
                    _ => ("404 Not Found", String::new()),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    port
}

#[tokio::test]
async fn bench_headless_launch_registry_and_cleanup() {
    let Some(chrome) = common::chromium() else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let env = LaunchEnv {
        sb_chromium: None,
        sb_home: Some(home.path().to_path_buf()),
        cwd: home.path().to_path_buf(),
    };
    let opts = LaunchOpts {
        profile: Profile::Bench,
        headless: true,
        chromium: Some(chrome),
    };
    let driver = Driver::launch_with(opts, env).await.expect("launch");
    let profile = driver.profile_dir().to_path_buf();
    assert!(profile.is_dir());
    assert_eq!(profile.parent().unwrap(), home.path().join("profiles"));
    assert!(profile
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("bench-"));
    let cdp = driver.cdp().clone();
    let mut all = cdp.events();

    // A new about:blank target, auto-attached with a flattened session.
    let created = cdp
        .call(None, "Target.createTarget", json!({"url": "about:blank"}))
        .await
        .expect("Target.createTarget");
    let target = created["targetId"].as_str().unwrap().to_string();
    let (tab, session) = driver
        .wait_registry(CAP, |r| {
            let t = r.tab_by_target(&target)?;
            Some((t, r.session(t)?.to_string()))
        })
        .await
        .expect("registry has the new tab");
    let attached = next_matching(&mut all, "Target.attachedToTarget", |e| {
        e.method == "Target.attachedToTarget" && e.params["targetInfo"]["targetId"] == *target
    })
    .await;
    assert_eq!(attached.params["sessionId"], *session);

    let r = cdp
        .call(
            Some(&session),
            "Runtime.evaluate",
            json!({"expression": "1+1", "returnByValue": true}),
        )
        .await
        .expect("Runtime.evaluate");
    assert_eq!(r["result"]["value"], 2);

    // Navigate to a page with a same-origin and a cross-site child frame: Page events on the
    // session broadcast, and the registry tracks the navigation (generation), both frames,
    // the cross-site one as an opaque OOPIF with its own session, and the main context.
    let port = serve().await;
    let gen0 = driver.with_registry(|r| r.generation(tab, 0)).unwrap();
    let mut sess = cdp.session_events(&session);
    let url = format!("http://127.0.0.1:{port}/");
    cdp.call(Some(&session), "Page.navigate", json!({"url": url}))
        .await
        .expect("Page.navigate");
    let nav = next_matching(&mut sess, "main Page.frameNavigated", |e| {
        e.method == "Page.frameNavigated" && e.params["frame"].get("parentId").is_none()
    })
    .await;
    assert_eq!(nav.session.as_deref(), Some(session.as_str()));
    let child_url = format!("http://localhost:{port}/child");
    let frames = driver
        .wait_registry(CAP, |r| {
            let f = r.frames(tab)?;
            let same = f
                .iter()
                .any(|x| x.parent == Some(0) && x.url == "about:srcdoc");
            let cross = f.iter().any(|x| x.parent == Some(0) && x.url == child_url);
            let ctx = r.default_context(tab, 0).is_some();
            (same && cross && ctx).then_some(f)
        })
        .await
        .unwrap_or_else(|| {
            panic!(
                "frames and main context in the registry: {:?}",
                driver.with_registry(|r| r.frames(tab))
            )
        });
    assert_eq!(frames[0].frame, 0);
    assert_eq!(frames[0].url, url);
    assert!(!frames[0].opaque);
    let same = frames.iter().find(|f| f.url == "about:srcdoc").unwrap();
    assert!(!same.opaque, "srcdoc inherits its parent's origin");
    let cross = frames.iter().find(|f| f.url == child_url).unwrap();
    assert!(cross.opaque, "cross-site child is opaque");
    let cross_session = driver
        .with_registry(|r| r.frame_session(tab, cross.frame).map(str::to_string))
        .unwrap();
    assert_ne!(
        cross_session, session,
        "the cross-site child is out of process"
    );
    let gen1 = driver.with_registry(|r| r.generation(tab, 0)).unwrap();
    assert_ne!(gen0, gen1, "navigation bumps the main frame generation");
    assert!(driver.with_registry(|r| !r.is_current(tab, 0, gen0)));

    // tabs() and frames() read the registry.
    let tabs = driver.tabs().await.unwrap();
    assert!(
        tabs.iter().any(|(t, u, _)| *t == tab && *u == url),
        "{tabs:?}"
    );
    assert_eq!(tabs, driver.with_registry(|r| r.tabs()));
    assert_eq!(driver.frames(tab).await.unwrap(), frames);
    assert!(matches!(
        driver.frames(TabId(999)).await,
        Err(DriverError::TabGone(TabId(999)))
    ));

    // A popup opened by this tab is "opened by tab N".
    cdp.call(
        Some(&session),
        "Runtime.evaluate",
        json!({"expression": "window.open('about:blank#popup'); 0", "userGesture": true}),
    )
    .await
    .expect("window.open");
    let popup = driver
        .wait_registry(CAP, |r| {
            r.tabs()
                .into_iter()
                .map(|(t, _, _)| t)
                .find(|t| *t != tab && r.opener(*t) == Some(tab))
        })
        .await
        .expect("popup tab with opener");
    assert!(popup > tab);

    // Drop: every browser process gone, bench profile removed.
    let pids = driver.pids();
    assert!(pids.contains(&driver.pid()), "{pids:?}");
    drop(driver);
    for pid in &pids {
        assert!(!process_running(*pid), "pid {pid} still running after drop");
    }
    assert!(!profile.exists(), "bench profile {profile:?} not removed");
}
