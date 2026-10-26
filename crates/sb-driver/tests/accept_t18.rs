//! T-18 acceptance (tester, round 1) against the real D-34 build: the pinned version is the
//! one launched over the pipe, per-session events stay in their session, a closed tab's
//! session channel closes, and drop leaves no process and no bench profile (B-2, B-3,
//! B-4 #2, D-600, D-601).

mod common;

use sb_cdp::process_running;
use sb_driver::{Driver, LaunchEnv, LaunchOpts, Profile, CHROMIUM_VERSION};
use serde_json::json;
use std::time::Duration;
use tokio::sync::broadcast;

const CAP: Duration = Duration::from_secs(30);

#[tokio::test]
async fn accept_d34_pipe_sessions_detach_and_cleanup() {
    let Some(chrome) = common::chromium() else {
        panic!("acceptance needs the D-34 Chromium (set SB_CHROMIUM)");
    };
    let home = tempfile::tempdir().unwrap();
    let env = LaunchEnv {
        sb_chromium: None,
        sb_home: Some(home.path().to_path_buf()),
        cwd: home.path().to_path_buf(),
    };
    let driver = Driver::launch_with(
        LaunchOpts {
            profile: Profile::Bench,
            headless: true,
            chromium: Some(chrome),
        },
        env,
    )
    .await
    .expect("launch");
    let cdp = driver.cdp().clone();

    // Row 1: the browser on the other end of the pipe is the pinned build.
    let v = cdp
        .call(None, "Browser.getVersion", json!({}))
        .await
        .expect("Browser.getVersion");
    let product = v["product"].as_str().unwrap_or_default();
    assert!(product.ends_with(CHROMIUM_VERSION), "product {product}");
    assert_eq!(CHROMIUM_VERSION, "156.0.8078.4");

    // Two tabs, each with its own flattened session.
    let mut tabs = Vec::new();
    for _ in 0..2 {
        let t = cdp
            .call(None, "Target.createTarget", json!({"url": "about:blank"}))
            .await
            .expect("Target.createTarget");
        let target = t["targetId"].as_str().unwrap().to_string();
        let (tab, sid) = driver
            .wait_registry(CAP, |r| {
                let tab = r.tab_by_target(&target)?;
                Some((tab, r.session(tab)?.to_string()))
            })
            .await
            .expect("tab in registry");
        tabs.push((tab, target, sid));
    }
    let (_, target_a, sid_a) = tabs[0].clone();
    let (_, _, sid_b) = tabs[1].clone();
    assert_ne!(sid_a, sid_b);

    // Row 1 + 3: a session command returns, and the session's events land on its channel
    // and not on the other tab's.
    let mut ev_a = cdp.session_events(&sid_a);
    let mut ev_b = cdp.session_events(&sid_b);
    let r = cdp
        .call(
            Some(&sid_a),
            "Runtime.evaluate",
            json!({"expression": "location.hash = 'x'; 40+2", "returnByValue": true}),
        )
        .await
        .expect("Runtime.evaluate on session A");
    assert_eq!(r["result"]["value"], 42);
    let got = tokio::time::timeout(CAP, async {
        loop {
            match ev_a.recv().await {
                Ok(e) if e.method == "Page.navigatedWithinDocument" => return e,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("session A channel ended: {e}"),
            }
        }
    })
    .await
    .expect("Page.navigatedWithinDocument on session A");
    assert_eq!(got.session.as_deref(), Some(sid_a.as_str()));
    loop {
        match ev_b.try_recv() {
            Ok(e) => assert_eq!(e.session.as_deref(), Some(sid_b.as_str()), "{e:?}"),
            Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => break,
        }
    }

    // Row 3: closing tab A detaches its session, which closes its channel; B stays.
    cdp.call(None, "Target.closeTarget", json!({"targetId": target_a}))
        .await
        .expect("Target.closeTarget");
    let end = tokio::time::timeout(CAP, async {
        loop {
            match ev_a.recv().await {
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    })
    .await;
    assert!(end.is_ok(), "session A channel not closed within {CAP:?}");
    let gone = driver
        .wait_registry(CAP, |r| r.tab_by_target(&target_a).is_none().then_some(()))
        .await;
    assert!(gone.is_some(), "closed tab still in registry");
    let still = cdp
        .call(
            Some(&sid_b),
            "Runtime.evaluate",
            json!({"expression": "1", "returnByValue": true}),
        )
        .await
        .expect("session B still answers");
    assert_eq!(still["result"]["value"], 1);

    // Row 7: drop kills every process of the browser and removes the bench profile.
    let profile = driver.profile_dir().to_path_buf();
    let pids = driver.pids();
    assert!(pids.len() > 1, "{pids:?}");
    drop(cdp);
    drop(driver);
    let alive: Vec<u32> = pids
        .iter()
        .copied()
        .filter(|p| process_running(*p))
        .collect();
    assert!(alive.is_empty(), "still running after drop: {alive:?}");
    assert!(!profile.exists(), "bench profile {profile:?} left behind");
    let leftovers: Vec<_> = std::fs::read_dir(home.path().join("profiles"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    assert!(leftovers.is_empty(), "profiles/ not empty: {leftovers:?}");
}
