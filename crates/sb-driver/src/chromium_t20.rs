//! T-20 acceptance against real Chromium (docs/05 acceptance 2, B-4 #3, #6, #11): runtime
//! install, open, based walks after navigation, actionability with retry and its 5 s cap.
//! In-crate so it can time the first in-page answer of `actionable`. Pages come from a tiny
//! HTTP server on 127.0.0.1; no network.

#[path = "../tests/common/mod.rs"]
mod common;

use crate::actionable::{WhyKind, ACT_CAP};
use crate::{Driver, LaunchEnv, LaunchOpts, Profile, StepOutcome, TabId};
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::{Class, Harness, RunId, ToolCtx, ToolRegistry, Vault};
use sb_sheet::ElemRef;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

const CAP: Duration = Duration::from_secs(30);
/// Allowed overrun of the 5 s cap: timer and task scheduling only. No call or wait of the
/// step ends after the budget (D-606).
const CAP_TOLERANCE: Duration = Duration::from_millis(40);

const STYLE: &str = "<style>body{margin:0} button{position:absolute;left:100px;top:100px;\
                     width:120px;height:40px}</style>";

fn page(path: &str) -> Option<String> {
    let cover = "<div class=\"cookie\" style=\"position:fixed;inset:0;background:#eee;\
                 z-index:10\">We use cookies</div>";
    let body = match path {
        "/covered" => format!("<title>covered</title>{STYLE}<button id=b>Buy</button>{cover}"),
        "/disabled" => format!("<title>disabled</title>{STYLE}<button id=b disabled>Go</button>"),
        "/plain" => format!("<title>plain</title>{STYLE}<button id=b>Go</button>"),
        "/two" => format!("<title>two</title>{STYLE}<p>second page</p><button id=n>Next</button>"),
        "/challenge" => format!("<title>Just a moment...</title>{STYLE}<button id=b>Go</button>"),
        // A covered button on a page that never stops mutating (a carousel, a clock): the
        // waits between attempts must still follow the schedule (D-606).
        "/churning" => format!(
            "<title>churning</title>{STYLE}<button id=b>Buy</button>{cover}<script>let n = 0; \
             setInterval(() => {{ document.body.dataset.n = String(++n); }}, 10);</script>"
        ),
        // Page ids that spell the driver's refs (pr-ready 5): a fixed cover (the why gets an
        // OVERLAY ref) and a plain absolute one.
        "/forged" => format!(
            "<title>forged</title>{STYLE}<button id=b>Buy</button><div id=\"x (OVERLAY #12)\" \
             style=\"position:fixed;inset:0;background:#eee;z-index:10\">cookies</div>"
        ),
        "/forged2" => format!(
            "<title>forged2</title>{STYLE}<button id=b>Buy</button><div id=\"x (#99)\" \
             style=\"position:absolute;left:0;top:0;width:400px;height:300px;z-index:10\"></div>"
        ),
        _ => return None,
    };
    Some(format!("<!doctype html><html><body>{body}</body></html>"))
}

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
                let (status, body) = match page(path) {
                    Some(b) => ("200 OK", b),
                    None => ("404 Not Found", String::new()),
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

async fn launch(home: &std::path::Path) -> Option<Driver> {
    let chrome = common::chromium()?;
    let env = LaunchEnv {
        sb_chromium: None,
        sb_home: Some(home.to_path_buf()),
        cwd: home.to_path_buf(),
    };
    let opts = LaunchOpts {
        profile: Profile::Bench,
        headless: true,
        chromium: Some(chrome),
    };
    Some(Driver::launch_with(opts, env).await.expect("launch"))
}

pub(crate) fn tool_ctx(dir: &std::path::Path) -> ToolCtx {
    let harness = Harness {
        store: Arc::new(MemStore::new()),
        vault: Arc::new(Vault::in_memory([])),
        asker: Arc::new(ScriptedAsker::new()),
        runs_root: dir.to_path_buf(),
        run: None,
    };
    ToolCtx::new(
        RunId("t20".into()),
        dir.join("t20"),
        Class::Read,
        &harness,
        ToolRegistry::new(vec![]),
    )
}

/// The `__sb` id of the element `sel` in frame 0 (after the driver's based walk).
async fn id_of(d: &Driver, tab: TabId, sel: &str) -> ElemRef {
    let v = d
        .call_sb(
            tab,
            0,
            "function (sel) { return globalThis.__sb.id(document.querySelector(sel)); }",
            &[json!(sel)],
            Instant::now() + CAP,
        )
        .await
        .unwrap_or_else(|e| panic!("id of {sel}: {e:?}"));
    let id = v.as_u64().unwrap_or_else(|| panic!("id of {sel}: {v}"));
    ElemRef::main(u32::try_from(id).unwrap())
}

/// Run `js` in the page's own (main) world, as a page script would.
async fn page_script(d: &Driver, tab: TabId, js: &str) -> Value {
    let ctx = d
        .wait_registry(CAP, |r| r.default_context(tab, 0).cloned())
        .await
        .expect("main world context");
    let v = d
        .cdp()
        .call(
            Some(&ctx.session),
            "Runtime.evaluate",
            json!({"expression": js, "contextId": ctx.id, "returnByValue": true, "awaitPromise": true}),
        )
        .await
        .expect("Runtime.evaluate");
    v.pointer("/result/value").cloned().unwrap_or(Value::Null)
}

/// Centre of `sel` in the frame's viewport, read in the main world.
async fn centre(d: &Driver, tab: TabId, sel: &str) -> (f64, f64) {
    let v = page_script(
        d,
        tab,
        &format!(
            "(() => {{ const r = document.querySelector('{sel}').getBoundingClientRect(); \
             return [r.left + r.width / 2, r.top + r.height / 2]; }})()"
        ),
    )
    .await;
    (v[0].as_f64().unwrap(), v[1].as_f64().unwrap())
}

/// The attempt probe (the tester's technique, accept_t20.rs): in the sb world only, wrap
/// `Number.isInteger`, which `__sb.act` calls once on entry with the id, and
/// `document.elementFromPoint`, which it calls once per hit test; record page times.
const PROBE: &str = "function (id) {
  const P = { starts: [], hits: [] };
  globalThis.__probe = P;
  const isInt = Number.isInteger;
  Number.isInteger = function (n) {
    if (n === id) P.starts.push(performance.now());
    return isInt(n);
  };
  const efp = document.elementFromPoint;
  document.elementFromPoint = function (x, y) {
    const hit = efp.call(document, x, y);
    P.hits.push(performance.now());
    return hit;
  };
  return true;
}";

async fn probe(d: &Driver, tab: TabId, id: u32) {
    let v = d
        .call_sb(tab, 0, PROBE, &[json!(id)], Instant::now() + CAP)
        .await
        .expect("probe");
    assert_eq!(v, json!(true));
}

struct Probed {
    starts: Vec<f64>,
    hits: Vec<f64>,
}

async fn probed(d: &Driver, tab: TabId) -> Probed {
    let v = d
        .call_sb(
            tab,
            0,
            "function () { return globalThis.__probe; }",
            &[],
            Instant::now() + CAP,
        )
        .await
        .expect("probe read");
    let nums = |k: &str| -> Vec<f64> {
        v[k].as_array()
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default()
    };
    Probed {
        starts: nums("starts"),
        hits: nums("hits"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acceptance_2_actionability_with_retry_and_runtime_install() {
    let home = tempfile::tempdir().unwrap();
    let Some(mut d) = launch(home.path()).await else {
        return;
    };
    let port = serve().await;
    let url = |p: &str| format!("http://127.0.0.1:{port}{p}");
    let ctx = tool_ctx(home.path());

    // open: committed, settled and walked with a base (the first document's base is 1).
    let covered = d.open(&url("/covered")).await.expect("open /covered");
    let disabled = d.open(&url("/disabled")).await.expect("open /disabled");
    let capped = d.open(&url("/churning")).await.expect("open /churning");
    for t in [covered, disabled, capped] {
        assert!(
            d.with_registry(|r| r.sb_context(t, 0).is_some()),
            "{t:?} has its sb world (B-4 #3)"
        );
    }
    let b = id_of(&d, covered, "#b").await;
    assert!(b.id >= 1 && b.id < 100_001, "first document ids: {b}");

    // (a) A covered button answers `covered by ...`, and every first answer comes in under
    // 50 ms, the first act on the first tab after launch included: open() waited for a
    // rendered frame, so act(id) does not wait out nextFrame's 100 ms fallback.
    let mut firsts = Vec::new();
    for _ in 0..3 {
        let w = d
            .actionable_within(covered, b, Duration::from_millis(300))
            .await
            .expect("driver")
            .expect_err("covered button is not actionable");
        eprintln!(
            "(a) first answer {:?}, {} attempts, final {:?}",
            w.first_answer, w.attempts, w.text
        );
        assert!(
            w.text.starts_with("covered by div.cookie (OVERLAY #"),
            "{}",
            w.text
        );
        assert!(
            w.text.ends_with(" after 0.3 s"),
            "retried to the cap: {}",
            w.text
        );
        assert_eq!(w.kind, WhyKind::Retry);
        assert!(w.attempts > 1, "a covered element is retried");
        firsts.push(w.first_answer);
    }
    assert!(
        firsts.iter().all(|f| *f < Duration::from_millis(50)),
        "every first answer under 50 ms: {firsts:?}"
    );

    // (b) A page script removes the cover 300 ms from now; the retry then succeeds.
    page_script(
        &d,
        covered,
        "setTimeout(() => document.querySelector('.cookie').remove(), 300); 0",
    )
    .await;
    let t0 = Instant::now();
    let p = d
        .actionable(covered, b)
        .await
        .expect("driver")
        .expect("actionable once the cover is gone");
    let took = t0.elapsed();
    eprintln!("(b) actionable after {took:?} at ({}, {})", p.x, p.y);
    // The timer started just before t0; the first attempt (about 10 ms) was still covered.
    assert!(took >= Duration::from_millis(250), "it waited: {took:?}");
    assert!(
        took < Duration::from_millis(1000),
        "woke soon after the cover went: {took:?}"
    );
    let (cx, cy) = centre(&d, covered, "#b").await;
    assert!((p.x - cx).abs() < 1.0 && (p.y - cy).abs() < 1.0, "{p:?}");

    // B-4 #3: several opens at once each get the runtime in their first document (a
    // navigation that raced the install committed without it, D-604). After (a) and (b):
    // the closed tabs share the covered tab's renderer process, and their teardown keeps its
    // main thread busy for about 100 ms, which (a) would measure instead of the driver.
    let (u1, u2) = (url("/plain"), url("/two"));
    let (o1, o2, o3) = tokio::join!(d.open(&u1), d.open(&u2), d.open(&u1));
    for t in [o1, o2, o3] {
        let t = t.expect("concurrent open");
        assert!(d.with_registry(|r| r.sb_context(t, 0).is_some()));
        let target = d
            .with_registry(|r| r.target_id(t).map(str::to_string))
            .unwrap();
        d.cdp()
            .call(None, "Target.closeTarget", json!({ "targetId": target }))
            .await
            .expect("close");
    }

    // (c) disabled and (d) the 5 s cap, in two tabs at once. (d) goes through Driver::act,
    // so the final why is the step error.
    let db = id_of(&d, disabled, "#b").await;
    let cb = id_of(&d, capped, "#b").await;
    probe(&d, capped, cb.id).await;
    let program = format!("c{cb}");
    let (c, dd) = tokio::join!(
        async {
            let t0 = Instant::now();
            let r = d.actionable(disabled, db).await;
            (r, t0.elapsed())
        },
        async {
            let t0 = Instant::now();
            let r = d.act(capped, &program, &ctx).await;
            (r, t0.elapsed())
        }
    );
    let (c, c_took) = c;
    let w = c.expect("driver").expect_err("disabled is not actionable");
    eprintln!(
        "(c) first answer {:?}, final {:?} after {c_took:?}",
        w.first_answer, w.text
    );
    assert_eq!(w.text, "disabled after 5.0 s");
    assert!(c_took <= ACT_CAP + CAP_TOLERANCE, "{c_took:?}");
    let (r, d_took) = dd;
    let r = r.expect("act");
    eprintln!("(d) act took {d_took:?}: {:?}", r.steps);
    assert_eq!(r.steps.len(), 1);
    assert_eq!(r.steps[0].step, program);
    let StepOutcome::Failed { why } = &r.steps[0].outcome else {
        panic!("covered step did not fail: {:?}", r.steps[0]);
    };
    assert!(
        why.starts_with("covered by div.cookie (OVERLAY #") && why.ends_with(") after 5.0 s"),
        "{why}"
    );
    assert!(r.stopped.is_none());
    assert!(d_took >= ACT_CAP, "{d_took:?}");
    assert!(d_took <= ACT_CAP + CAP_TOLERANCE, "{d_took:?}");

    // D-606, observed in the page: on a page that mutates every 10 ms, attempt k still
    // starts at its slot (0, 50, 100, 250, 500, then 500 ms after the previous answer). The
    // wait in the page is measured from the previous attempt's hit test to the next
    // attempt's entry, so it includes two CDP hops and must not be shorter than the slot.
    let p = probed(&d, capped).await;
    let waits: Vec<f64> = (1..p.starts.len())
        .map(|i| p.starts[i] - p.hits[i - 1])
        .collect();
    let since0: Vec<f64> = p.starts.iter().map(|s| s - p.starts[0]).collect();
    eprintln!(
        "(D-606) {} attempts; attempt starts (ms after the first) {:?}; waits {:?}",
        p.starts.len(),
        since0.iter().map(|w| w.round()).collect::<Vec<_>>(),
        waits.iter().map(|w| w.round()).collect::<Vec<_>>()
    );
    assert_eq!(p.starts.len(), p.hits.len(), "one hit test per attempt");
    for (i, w) in waits.iter().enumerate() {
        // The spec's schedule written out here, not read from the code under test.
        let want = [0.0, 50.0, 100.0, 250.0, 500.0]
            .get(i)
            .copied()
            .unwrap_or(500.0);
        assert!(
            *w >= want - 2.0 && *w <= want + 80.0,
            "wait {i}: {w:.1} ms, slot {want} ms: {waits:?}"
        );
    }
    assert!(
        (10..=16).contains(&p.starts.len()),
        "about 14 attempts fit the 5 s budget on this schedule: {}",
        p.starts.len()
    );

    // (e) gone: removed from the page, fails at once without retrying.
    page_script(&d, disabled, "document.querySelector('#b').remove(); 0").await;
    let t0 = Instant::now();
    let w = d
        .actionable(disabled, db)
        .await
        .expect("driver")
        .expect_err("gone");
    eprintln!("(e) gone in {:?}", t0.elapsed());
    assert_eq!(
        (w.text.as_str(), w.kind, w.attempts),
        ("gone", WhyKind::Gone, 1)
    );
    assert!(
        t0.elapsed() < Duration::from_millis(500),
        "{:?}",
        t0.elapsed()
    );

    // (f) Navigate the covered tab: the runtime is there in the new document, which is
    // walked with a fresh base before any read, so the old id is stale (e) and the new
    // page's button is actionable.
    let session = d
        .with_registry(|r| r.session(covered).map(str::to_string))
        .unwrap();
    d.cdp()
        .call(Some(&session), "Page.navigate", json!({"url": url("/two")}))
        .await
        .expect("Page.navigate");
    let two = url("/two");
    d.wait_registry(CAP, |r| {
        (r.frame_url(covered, 0) == Some(two.as_str()) && r.sb_context(covered, 0).is_some())
            .then_some(())
    })
    .await
    .expect("navigated with an sb world");
    let t0 = Instant::now();
    let w = d
        .actionable(covered, b)
        .await
        .expect("driver")
        .expect_err("an id from the old document");
    eprintln!("(e) stale in {:?}", t0.elapsed());
    assert_eq!(
        (w.text.as_str(), w.kind, w.attempts),
        ("stale", WhyKind::Stale, 1)
    );
    assert!(
        t0.elapsed() < Duration::from_millis(500),
        "{:?}",
        t0.elapsed()
    );
    let n = id_of(&d, covered, "#n").await;
    assert!(
        n.id >= 100_001,
        "the second document of the frame starts at the next base: {n}"
    );
    let t0 = Instant::now();
    let p = d
        .actionable(covered, n)
        .await
        .expect("driver")
        .expect("button on the new page");
    eprintln!("(f) actionable on the new page in {:?}", t0.elapsed());
    let (cx, cy) = centre(&d, covered, "#n").await;
    assert!((p.x - cx).abs() < 1.0 && (p.y - cy).abs() < 1.0, "{p:?}");

    // Through Driver::act: a stale id fails the step at once, and the input itself is T-21.
    let r = d.act(covered, &format!("c{b}"), &ctx).await.expect("act");
    assert_eq!(
        r.steps[0].outcome,
        StepOutcome::Failed {
            why: "stale".into()
        }
    );
    let r = d
        .act(covered, &format!("c{n}; c{n}"), &ctx)
        .await
        .expect("act");
    assert_eq!(r.steps.len(), 1, "stops at the first step not done");
    assert!(
        matches!(&r.steps[0].outcome, StepOutcome::Failed { why } if why.starts_with("not done: input synthesis is T-21")),
        "{:?}",
        r.steps[0]
    );

    // A challenge page stops the program (H-1.4) without retrying.
    let ch = d.open(&url("/challenge")).await.expect("open /challenge");
    let cb = id_of(&d, ch, "#b").await;
    let t0 = Instant::now();
    let r = d
        .act(ch, &format!("w(100ms); c{cb}; c{cb}"), &ctx)
        .await
        .expect("act");
    assert!(
        t0.elapsed() < Duration::from_millis(1000),
        "{:?}",
        t0.elapsed()
    );
    assert_eq!(r.steps.len(), 2, "{:?}", r.steps);
    assert_eq!(r.steps[0].outcome, StepOutcome::Ok, "w() settles");
    assert_eq!(
        r.steps[1].outcome,
        StepOutcome::Failed {
            why: "challenge".into()
        }
    );
    let stop = r.stopped.expect("a challenge stops the program");
    assert_eq!(stop.class, Class::Challenge);
    assert_eq!(stop.element.as_deref(), Some(cb.to_string().as_str()));

    // B-4 #11: the document goes away while act is retrying; the call is retried once in
    // the new context, where the old id is stale.
    let plain = d.open(&url("/plain")).await.expect("open /plain");
    let pb = id_of(&d, plain, "#b").await;
    page_script(
        &d,
        plain,
        "document.body.insertAdjacentHTML('beforeend', '<div class=\"cookie\" \
         style=\"position:fixed;inset:0;z-index:9\">x</div>'); \
         setTimeout(() => { location.href = '/two'; }, 200); 0",
    )
    .await;
    let t0 = Instant::now();
    let w = d
        .actionable(plain, pb)
        .await
        .expect("driver")
        .expect_err("navigated away under the step");
    eprintln!("(B-4 #11) {:?} after {:?}", w.text, t0.elapsed());
    assert_eq!(w.kind, WhyKind::Stale, "{}", w.text);
    assert!(w.text.starts_with("stale after "), "{}", w.text);
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());

    // B-4 #11 on a single call: its context is destroyed while it waits (the page navigates
    // after 100 ms); the call runs once more in the new document and answers from there.
    let one = d.open(&url("/plain")).await.expect("open /plain");
    page_script(
        &d,
        one,
        "setTimeout(() => { location.href = '/two'; }, 100); 0",
    )
    .await;
    let v = d
        .call_sb(
            one,
            0,
            "function () { return new Promise((r) => setTimeout(() => r(location.pathname), 1500)); }",
            &[],
            Instant::now() + CAP,
        )
        .await
        .expect("retried once in the new context");
    assert_eq!(v, json!("/two"));

    // D-605: a popup opened in the same window hides its opener; actionable fronts the
    // opener once and answers without waiting out the schedule.
    let opener = d.open(&url("/plain")).await.expect("open /plain");
    let ob = id_of(&d, opener, "#b").await;
    let session = d
        .with_registry(|r| r.session(opener).map(str::to_string))
        .unwrap();
    d.cdp()
        .call(
            Some(&session),
            "Runtime.evaluate",
            json!({"expression": "window.open('/two', '_blank'); 0", "userGesture": true}),
        )
        .await
        .expect("window.open");
    d.wait_registry(CAP, |r| {
        r.tabs()
            .into_iter()
            .find(|(t, _, _)| r.opener(*t) == Some(opener))
    })
    .await
    .expect("popup tab");
    // The condition: visibilitychange to hidden, capped at 5 s.
    let hidden = page_script(
        &d,
        opener,
        "new Promise((r) => { if (document.visibilityState === 'hidden') return r(true);          document.addEventListener('visibilitychange', () => r(document.visibilityState === 'hidden'));          setTimeout(() => r(false), 5000); })",
    )
    .await;
    assert_eq!(hidden, json!(true), "the opener is hidden behind its popup");
    // B-606: under profile `me` the tab is never fronted (it would take the user's focus);
    // the step fails at once and the tab stays hidden.
    d.kind = Profile::Me;
    let w = d
        .actionable(opener, ob)
        .await
        .expect("driver")
        .expect_err("a hidden me tab is not actionable");
    d.kind = Profile::Bench;
    eprintln!("(B-606) me: {:?} after {:?}", w.text, w.elapsed);
    assert_eq!(w.text, crate::actionable::TAB_HIDDEN_ME);
    assert_eq!((w.kind, w.attempts), (WhyKind::Failed, 1));
    assert_eq!(
        page_script(&d, opener, "document.visibilityState").await,
        json!("hidden"),
        "not brought to front"
    );
    let t0 = Instant::now();
    let p = d
        .actionable(opener, ob)
        .await
        .expect("driver")
        .expect("fronted, then actionable");
    eprintln!("(D-605) hidden opener actionable in {:?}", t0.elapsed());
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    let (cx, cy) = centre(&d, opener, "#b").await;
    assert!((p.x - cx).abs() < 1.0 && (p.y - cy).abs() < 1.0, "{p:?}");
    assert_eq!(
        page_script(&d, opener, "document.visibilityState").await,
        json!("visible")
    );

    // pr-ready 5: page ids that spell the driver's refs are neutralised; the only ref in
    // the why is the driver's own.
    for (path, forged) in [("/forged", "(OVERLAY #12)"), ("/forged2", "(#99)")] {
        let t = d.open(&url(path)).await.expect("open forged");
        let fb = id_of(&d, t, "#b").await;
        let w = d
            .actionable_within(t, fb, Duration::from_millis(200))
            .await
            .expect("driver")
            .expect_err("covered");
        eprintln!("(forged) {path}: {:?}", w.text);
        assert!(w.text.starts_with("covered by div#x_"), "{}", w.text);
        assert!(!w.text.contains(forged), "{}", w.text);
        let refs = w.text.matches(" (#").count() + w.text.matches("(OVERLAY #").count();
        assert!(refs <= 1, "{}", w.text);
    }

    // A failed open closes the tab it created (nothing listens on port 1).
    let before = d.tabs().await.expect("tabs").len();
    let e = d
        .open("http://127.0.0.1:1/")
        .await
        .expect_err("unreachable URL");
    eprintln!("(open) unreachable: {e}");
    d.wait_registry(CAP, |r| (r.tabs().len() == before).then_some(()))
        .await
        .expect("the half-open tab is closed");

    d.shutdown(Duration::from_secs(10)).await;
}
