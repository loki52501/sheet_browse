//! T-20 acceptance (tester, rounds 1 and 2) against the real D-34 build, through the public API
//! only: docs/05 acceptance 2 (covered, retry, disabled, 5 s cap), the D-603 backoff
//! schedule and terminal whys, D-604 bases after navigation, B-4 #11 (a lost context is
//! retried exactly once) and H-2 (step labels and errors carry no values).
//!
//! Attempts are observed in the page: a probe in the `sb` world (never the page's own
//! world) wraps `Number.isInteger`, which `__sb.act` calls once on entry (in `resolve`),
//! and `document.elementFromPoint`, which it calls once per hit test. `__sb` itself is
//! frozen, so the probe sits on what `act` calls. Pages come from a tiny HTTP server on
//! 127.0.0.1; no network. Run with `--test-threads=1` on a loaded machine: each test
//! launches its own browser.

mod common;

use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::EntryKind;
use harness_core::{Class, Harness, RunId, ToolCtx, ToolRegistry, Vault};
use sb_driver::{ActResult, Driver, LaunchEnv, LaunchOpts, Profile, StepOutcome, TabId};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

const CAP: Duration = Duration::from_secs(30);
const ACT_CAP: Duration = Duration::from_secs(5);
/// Allowed overrun of the 5 s cap: timer and task scheduling only. Since round 2 no call or
/// wait of the step ends after the budget (D-606; the same tolerance the coder's test uses).
const CAP_TOLERANCE: Duration = Duration::from_millis(40);
/// T-21: `Driver::act` reads the live index for the per-step gate (rule 4) before the step's
/// 5 s budget starts: one round trip on these small pages.
const GATE_READ: Duration = Duration::from_millis(60);
/// D-603 waits, then 500 ms steps.
const SCHEDULE_MS: [f64; 5] = [0.0, 50.0, 100.0, 250.0, 500.0];

const STYLE: &str = "<style>body{margin:0} button,input{position:absolute;left:100px;\
                     top:100px;width:120px;height:40px}</style>";
const COVER: &str = "<div class=\"cookie\" style=\"position:fixed;inset:0;background:#eee;\
                     z-index:10\">We use cookies</div>";
/// A page that keeps mutating (so an in-page settle never settles) and navigates to `next`
/// 400 ms after it loads.
fn churn(next: &str) -> String {
    format!(
        "<title>churn</title>{STYLE}<p id=c>0</p><script>let n = 0; \
         setInterval(() => {{ document.getElementById('c').textContent = String(++n); }}, 30); \
         setTimeout(() => {{ location.href = '{next}'; }}, 400);</script>"
    )
}

fn page(path: &str) -> Option<String> {
    let body = match path {
        "/covered" => format!("<title>covered</title>{STYLE}<button id=b>Buy</button>{COVER}"),
        "/disabled" => format!("<title>disabled</title>{STYLE}<button id=b disabled>Go</button>"),
        "/plain" => format!("<title>plain</title>{STYLE}<h1>one</h1><button id=b>Go</button>"),
        "/two" => format!("<title>two</title>{STYLE}<p>second page</p><button id=n>Next</button>"),
        "/challenge" => format!("<title>Just a moment...</title>{STYLE}<button id=b>Go</button>"),
        "/form" => format!("<title>form</title>{STYLE}<input id=e name=email>"),
        "/calm" => format!("<title>calm</title>{STYLE}<p>calm</p>"),
        "/m2" => churn("/calm"),
        // Round 2: a covered button on a page that mutates every 10 ms, so the in-page settle
        // between attempts never goes quiet (D-606 row 5 on a busy page).
        "/churn-covered" => format!(
            "<title>churn-covered</title>{STYLE}<button id=b>Buy</button>{COVER}<p id=c>0</p>             <script>let n = 0; setInterval(() => {{ document.getElementById('c').textContent              = String(++n); }}, 10);</script>"
        ),
        // Round 2: page ids that spell the driver's own refs (a fixed cover, then a plain
        // absolute one) cover the button.
        "/forged" => format!(
            "<title>forged</title>{STYLE}<button id=b>Buy</button><div id=\"x (OVERLAY #12)\"              style=\"position:fixed;inset:0;background:#eee;z-index:10\">cookies</div>"
        ),
        "/forged2" => format!(
            "<title>forged2</title>{STYLE}<button id=b>Buy</button><div id=\"x (#12)\"              style=\"position:absolute;left:0;top:0;width:400px;height:300px;z-index:10\"></div>"
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

struct Rig {
    d: Driver,
    /// Held for the whole test (see [`SERIAL`]); dropped after the browser.
    _serial: tokio::sync::MutexGuard<'static, ()>,
    port: u16,
    store: Arc<MemStore>,
    ctx: ToolCtx,
    home: tempfile::TempDir,
}

impl Rig {
    fn url(&self, p: &str) -> String {
        format!("http://127.0.0.1:{}{p}", self.port)
    }
}

/// One browser at a time in this binary, also under the default parallel test runner: the
/// timing rows (1, 4, 5) measure the driver, not seven browsers competing for the CPU.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn rig(vault: Vault) -> Rig {
    let serial = SERIAL.lock().await;
    let Some(chrome) = common::chromium() else {
        panic!("acceptance needs the D-34 Chromium (set SB_CHROMIUM)");
    };
    let home = tempfile::tempdir().unwrap();
    let env = LaunchEnv {
        sb_chromium: None,
        sb_home: Some(home.path().to_path_buf()),
        cwd: home.path().to_path_buf(),
    };
    let d = Driver::launch_with(
        LaunchOpts {
            profile: Profile::Bench,
            headless: true,
            chromium: Some(chrome),
        },
        env,
    )
    .await
    .expect("launch");
    let port = serve().await;
    let store = Arc::new(MemStore::new());
    let harness = Harness {
        store: store.clone(),
        vault: Arc::new(vault),
        asker: Arc::new(ScriptedAsker::new()),
        runs_root: home.path().join("runs"),
        run: None,
    };
    // T-21: the highest class below Challenge, so the per-step gate (rule 4) never stops
    // these actionability rows ("Buy" buttons are Pay); a CHALLENGE still stops.
    let ctx = ToolCtx::new(
        RunId("accept-t20".into()),
        home.path().join("runs").join("accept-t20"),
        Class::Pay,
        &harness,
        ToolRegistry::new(vec![]),
    );
    Rig {
        d,
        _serial: serial,
        port,
        store,
        ctx,
        home,
    }
}

/// Call `decl(args)` in the frame-0 `sb` world as the registry knows it now, without the
/// driver (no based walk, no retry): the probe's own channel.
async fn sb_eval(d: &Driver, tab: TabId, decl: &str, args: &[Value]) -> Value {
    let ctx = d
        .wait_registry(CAP, |r| r.sb_context(tab, 0).cloned())
        .await
        .expect("sb context");
    let args: Vec<Value> = args.iter().map(|a| json!({ "value": a })).collect();
    let v = d
        .cdp()
        .call(
            Some(&ctx.session),
            "Runtime.callFunctionOn",
            json!({"functionDeclaration": decl, "executionContextId": ctx.id,
                   "arguments": args, "returnByValue": true, "awaitPromise": true}),
        )
        .await
        .expect("Runtime.callFunctionOn in sb");
    assert!(v.get("exceptionDetails").is_none(), "sb_eval threw: {v}");
    v.pointer("/result/value").cloned().unwrap_or(Value::Null)
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

async fn id_of(d: &Driver, tab: TabId, sel: &str) -> u32 {
    let v = sb_eval(
        d,
        tab,
        "function (sel) { return globalThis.__sb.id(document.querySelector(sel)); }",
        &[json!(sel)],
    )
    .await;
    let id = v.as_u64().unwrap_or_else(|| panic!("id of {sel}: {v}"));
    u32::try_from(id).unwrap()
}

/// Every element's id in the current document (after the driver's based walk).
async fn all_ids(d: &Driver, tab: TabId) -> Vec<u64> {
    let v = sb_eval(
        d,
        tab,
        "function () { return Array.from(document.querySelectorAll('*'), (e) => globalThis.__sb.id(e)); }",
        &[],
    )
    .await;
    v.as_array()
        .unwrap_or_else(|| panic!("ids: {v}"))
        .iter()
        .map(|x| {
            x.as_u64()
                .unwrap_or_else(|| panic!("not an id: {x} in {v}"))
        })
        .collect()
}

const PROBE: &str = "function (id) {
  const P = { starts: [], hits: [], what: [], indexHits: 0 };
  globalThis.__probe = P;
  const isInt = Number.isInteger;
  Number.isInteger = function (n) {
    if (n === id) P.starts.push(performance.now());
    return isInt(n);
  };
  const efp = document.elementFromPoint;
  document.elementFromPoint = function (x, y) {
    const hit = efp.call(document, x, y);
    // The gate's and the consequence window's index() hit-test rows too: those are
    // counted apart, so every other hit test (act()'s, one per attempt) still counts.
    const lim = Error.stackTraceLimit;
    Error.stackTraceLimit = 200;
    const st = String(new Error().stack);
    Error.stackTraceLimit = lim;
    if (/\\bat (?:\\S+\\.)?index \\(/.test(st)) { P.indexHits += 1; return hit; }
    P.hits.push(performance.now());
    P.what.push(hit ? (hit.getAttribute('class') || hit.id || hit.localName) : 'nothing');
    return hit;
  };
  return true;
}";

/// Install the attempt probe for element `id` in the frame-0 sb world.
async fn probe(d: &Driver, tab: TabId, id: u32) {
    assert_eq!(sb_eval(d, tab, PROBE, &[json!(id)]).await, json!(true));
}

#[derive(Debug)]
struct Probed {
    starts: Vec<f64>,
    hits: Vec<f64>,
    what: Vec<String>,
}

async fn probed(d: &Driver, tab: TabId) -> Probed {
    let v = sb_eval(d, tab, "function () { return globalThis.__probe; }", &[]).await;
    let nums = |k: &str| -> Vec<f64> {
        v[k].as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect()
    };
    Probed {
        starts: nums("starts"),
        hits: nums("hits"),
        what: v["what"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect(),
    }
}

/// A page-clock mark: `(page ms, Rust instant before the call, Rust instant after)`.
async fn mark(d: &Driver, tab: TabId) -> (f64, Instant, Instant) {
    let ts = Instant::now();
    let p = sb_eval(d, tab, "function () { return performance.now(); }", &[])
        .await
        .as_f64()
        .unwrap();
    (p, ts, Instant::now())
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn why_of(r: &ActResult, i: usize) -> &str {
    match &r.steps[i].outcome {
        StepOutcome::Failed { why } => why,
        StepOutcome::Ok => panic!("step {} did not fail: {:?}", i + 1, r.steps[i]),
    }
}

/// Two animation frames in the page: a fresh window's compositor can stall once.
async fn warm(d: &Driver, tab: TabId) {
    page_script(
        d,
        tab,
        "new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r(0))))",
    )
    .await;
}

async fn navigate_and_wait(d: &Driver, tab: TabId, url: &str) {
    let old = d.with_registry(|r| r.sb_context(tab, 0).cloned());
    let session = d
        .with_registry(|r| r.session(tab).map(str::to_string))
        .unwrap();
    d.cdp()
        .call(Some(&session), "Page.navigate", json!({ "url": url }))
        .await
        .expect("Page.navigate");
    wait_new_doc(d, tab, url, old).await;
}

/// The tab's main-frame URL, from the registry.
fn main_url(r: &sb_driver::Registry, tab: TabId) -> Option<String> {
    r.tabs()
        .into_iter()
        .find(|(t, _, _)| *t == tab)
        .map(|(_, u, _)| u)
}

async fn wait_new_doc(d: &Driver, tab: TabId, url: &str, old: Option<sb_driver::ExecContext>) {
    d.wait_registry(CAP, |r| {
        let ctx = r.sb_context(tab, 0)?;
        (main_url(r, tab).as_deref() == Some(url) && Some(ctx) != old.as_ref()).then_some(())
    })
    .await
    .unwrap_or_else(|| panic!("navigated to {url} with a new sb world"));
}

// ------------------------------------------------------------------------------------ rows

/// Rows 1 and 2: a covered button's first answer comes back in under 50 ms and is the
/// cover; a page script removes the cover 300 ms later and the retry succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row1_row2_covered_first_answer_under_50ms_then_retry_succeeds() {
    let rig = rig(Vault::in_memory([])).await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/covered")).await.expect("open /covered");
    let b = id_of(d, tab, "#b").await;
    probe(d, tab, b).await;
    warm(d, tab).await;
    let mut firsts = Vec::new();
    for run in 0..5 {
        if run > 0 {
            page_script(
                d,
                tab,
                &format!("document.body.insertAdjacentHTML('beforeend', '{COVER}'); 0"),
            )
            .await;
            warm(d, tab).await;
        }
        sb_eval(
            d,
            tab,
            "function () { const P = globalThis.__probe; P.starts.length = 0; P.hits.length = 0; P.what.length = 0; return 0; }",
            &[],
        )
        .await;
        page_script(
            d,
            tab,
            "setTimeout(() => document.querySelector('.cookie').remove(), 300); 0",
        )
        .await;
        let (p_mark, t_s, t_r) = mark(d, tab).await;
        let t_a = Instant::now();
        let r = d.act(tab, &format!("c#{b}"), &rig.ctx).await.expect("act");
        let took = t_a.elapsed();
        let p = probed(d, tab).await;
        // Upper bound of (first answer in Rust) - (act start): the page's first hit test,
        // mapped to the Rust clock through the mark, plus one round trip for the reply.
        let first_ub = (p.hits[0] - p_mark) - ms(t_a - t_r) + ms(t_r - t_s);
        eprintln!(
            "row1/2 run {run}: first answer <= {first_ub:.1} ms (first hit {:?}; attempt 1 \
             entered the page <= {:.1} ms after act, took {:.1} ms in the page; mark round \
             trip {:.1} ms), {} attempts, hits {:?}, act took {took:?}, step {:?}",
            p.what[0],
            (p.starts[0] - p_mark) - ms(t_a - t_r),
            p.hits[0] - p.starts[0],
            ms(t_r - t_s),
            p.starts.len(),
            p.what,
            r.steps[0].outcome
        );
        assert_eq!(
            p.what[0], "cookie",
            "first answer: the button is covered by div.cookie"
        );
        firsts.push(first_ub);
        // Row 2: the retry after the cover went succeeds, and the click is done (T-21).
        assert_eq!(r.steps.len(), 1);
        assert_eq!(r.steps[0].outcome, StepOutcome::Ok, "{:?}", r.steps[0]);
        assert!(p.starts.len() >= 2, "it was retried: {p:?}");
        assert_eq!(p.what.last().map(String::as_str), Some("b"), "{p:?}");
        assert!(took >= Duration::from_millis(250), "it waited: {took:?}");
        assert!(took < Duration::from_millis(1000), "woke soon: {took:?}");
    }
    eprintln!("row1 first answers (ms, upper bounds): {firsts:?}");
    assert!(
        firsts.iter().all(|ms| *ms < 50.0),
        "every first answer under 50 ms: {firsts:?}"
    );
    rig.d.shutdown(Duration::from_secs(10)).await;
}

/// Row 1b: the first act on a freshly opened tab (the common `open` then `act` flow) also
/// answers in under 50 ms, on every tab, as a maximum rather than a median. The first act
/// on each tab runs right after `open` with no warm-up: since round 2 `open` waits for the
/// document's first paint (D-607), so the first act after launch no longer waits out
/// `nextFrame`'s 100 ms fallback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row1b_first_act_on_a_fresh_tab_under_50ms() {
    let rig = rig(Vault::in_memory([])).await;
    let d = &rig.d;
    let (mut first, mut second) = (Vec::new(), Vec::new());
    for _ in 0..4 {
        let tab = d.open(&rig.url("/covered")).await.expect("open /covered");
        let b = id_of(d, tab, "#b").await;
        probe(d, tab, b).await;
        for k in 0..2 {
            if k > 0 {
                page_script(
                    d,
                    tab,
                    &format!("document.body.insertAdjacentHTML('beforeend', '{COVER}'); 0"),
                )
                .await;
                warm(d, tab).await;
            }
            sb_eval(
                d,
                tab,
                "function () { const P = globalThis.__probe; P.starts.length = 0; P.hits.length = 0; P.what.length = 0; return 0; }",
                &[],
            )
            .await;
            page_script(
                d,
                tab,
                "setTimeout(() => document.querySelector('.cookie').remove(), 300); 0",
            )
            .await;
            let (p_mark, t_s, t_r) = mark(d, tab).await;
            let t_a = Instant::now();
            d.act(tab, &format!("c#{b}"), &rig.ctx).await.expect("act");
            let p = probed(d, tab).await;
            assert_eq!(p.what[0], "cookie");
            let ub = (p.hits[0] - p_mark) - ms(t_a - t_r) + ms(t_r - t_s);
            eprintln!(
                "row1b act {k}: first answer <= {ub:.1} ms; attempt 1 entered <= {:.1} ms, \
                 took {:.1} ms in the page; mark round trip {:.1} ms",
                (p.starts[0] - p_mark) - ms(t_a - t_r),
                p.hits[0] - p.starts[0],
                ms(t_r - t_s)
            );
            if k == 0 {
                first.push(ub)
            } else {
                second.push(ub)
            }
        }
    }
    eprintln!("row1b first act per fresh tab (ms): {first:?}; second act (ms): {second:?}");
    assert!(
        first.iter().chain(&second).all(|ms| *ms < 50.0),
        "first answers over 50 ms: first acts {first:?}, second acts {second:?}"
    );
    rig.d.shutdown(Duration::from_secs(10)).await;
}

/// Row 3: a disabled button answers `disabled`, retried to the cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row3_disabled_button_gives_disabled() {
    let rig = rig(Vault::in_memory([])).await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/disabled")).await.expect("open /disabled");
    let b = id_of(d, tab, "#b").await;
    probe(d, tab, b).await;
    warm(d, tab).await;
    let t0 = Instant::now();
    let r = d.act(tab, &format!("c#{b}"), &rig.ctx).await.expect("act");
    let took = t0.elapsed();
    let p = probed(d, tab).await;
    eprintln!(
        "row3: {:?} after {took:?}, {} attempts",
        r.steps[0].outcome,
        p.starts.len()
    );
    assert_eq!(why_of(&r, 0), "disabled after 5.0 s");
    assert!(
        p.hits.is_empty(),
        "disabled is answered before the hit test"
    );
    assert!(p.starts.len() > 5, "disabled is retryable (D-603): {p:?}");
    assert!(took <= ACT_CAP + CAP_TOLERANCE + GATE_READ, "{took:?}");
    rig.d.shutdown(Duration::from_secs(10)).await;
}

/// Rows 4 and 5: a permanent cover fails the step at the 5 s cap with the cover named, and
/// the waits between attempts follow 0, 50, 100, 250, 500, then 500 ms steps.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row4_row5_five_second_cap_and_backoff_schedule() {
    let rig = rig(Vault::in_memory([])).await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/covered")).await.expect("open /covered");
    let b = id_of(d, tab, "#b").await;
    probe(d, tab, b).await;
    warm(d, tab).await;
    let (p_mark, t_s, t_r) = mark(d, tab).await;
    let t_a = Instant::now();
    let r = d.act(tab, &format!("c#{b}"), &rig.ctx).await.expect("act");
    let took = t_a.elapsed();
    let p = probed(d, tab).await;
    let why = why_of(&r, 0);
    eprintln!("row4: act took {took:?}: {why:?}");
    assert!(
        why.starts_with("covered by div.cookie (OVERLAY #") && why.ends_with(") after 5.0 s"),
        "{why}"
    );
    assert!(r.stopped.is_none());
    assert!(took >= ACT_CAP, "{took:?}");
    assert!(took <= ACT_CAP + CAP_TOLERANCE + GATE_READ, "{took:?}");

    // Row 5: one start and one hit test per attempt (the page never changes).
    assert_eq!(p.starts.len(), p.hits.len(), "{p:?}");
    assert!(p.what.iter().all(|w| w == "cookie"), "{p:?}");
    let n = p.starts.len();
    let waits: Vec<f64> = (1..n).map(|i| p.starts[i] - p.hits[i - 1]).collect();
    let attempt: Vec<f64> = (0..n).map(|i| p.hits[i] - p.starts[i]).collect();
    // The act started (Rust t_a) no earlier than page time p_mark + (t_a - t_r).
    let t0_page = p_mark + ms(t_a - t_r);
    let last_start = p.starts[n - 1] - t0_page;
    let first_ub = (p.hits[0] - p_mark) - ms(t_a - t_r) + ms(t_r - t_s);
    eprintln!("row4: first answer <= {first_ub:.1} ms (not asserted here; row 1 asserts it)");
    eprintln!(
        "row5: {n} attempts; waits between attempts (ms) {:?}; attempt durations (ms) {:?}; \
         last attempt started {last_start:.0} ms after act",
        waits.iter().map(|w| w.round()).collect::<Vec<_>>(),
        attempt.iter().map(|w| w.round()).collect::<Vec<_>>()
    );
    for (i, w) in waits.iter().enumerate() {
        let want = SCHEDULE_MS.get(i).copied().unwrap_or(500.0);
        // The wait in the page is a settle capped at the slot, then the slot itself (D-606);
        // around it are two CDP round trips (one for a 0 ms wait, which sends nothing). A
        // wait the 5 s budget cuts short is followed by no attempt, so the last observed
        // wait is normally a full slot too.
        let last = i + 1 == waits.len();
        assert!(*w <= want + 80.0, "wait {i}: {w:.1} ms > {want} + 80");
        if !last {
            assert!(*w >= want - 2.0, "wait {i}: {w:.1} ms < {want}");
        }
    }
    assert!(
        (8..=16).contains(&n),
        "about 12 attempts fit 5 s on this schedule: {n}"
    );
    // The last wait is capped by what is left of the budget (D-603), so the last attempt
    // starts at the edge plus the wait's round trips; never a whole step past it.
    assert!(
        last_start <= ms(ACT_CAP) + 50.0,
        "no attempt starts after the budget: {last_start:.1}"
    );
    rig.d.shutdown(Duration::from_secs(10)).await;
}

/// Row 6: gone, stale and challenge fail at once with one attempt; a challenge stops the
/// program with class Challenge and the element.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row6_gone_stale_challenge_fail_at_once() {
    let rig = rig(Vault::in_memory([])).await;
    let d = &rig.d;

    // gone
    let tab = d.open(&rig.url("/plain")).await.expect("open /plain");
    let b = id_of(d, tab, "#b").await;
    probe(d, tab, b).await;
    page_script(d, tab, "document.querySelector('#b').remove(); 0").await;
    let t0 = Instant::now();
    let r = d
        .act(tab, &format!("c#{b}; c#{b}"), &rig.ctx)
        .await
        .expect("act");
    let took = t0.elapsed();
    let p = probed(d, tab).await;
    eprintln!(
        "row6 gone: {:?} in {took:?}, {} attempts",
        r.steps,
        p.starts.len()
    );
    assert_eq!(r.steps.len(), 1, "stops at the failed step");
    assert_eq!(why_of(&r, 0), "gone");
    assert_eq!(p.starts.len(), 1, "gone is not retried");
    assert!(r.stopped.is_none());
    assert!(took < Duration::from_millis(500), "{took:?}");

    // stale: the same id after the tab navigated
    let tab2 = d.open(&rig.url("/plain")).await.expect("open /plain");
    let b2 = id_of(d, tab2, "#b").await;
    navigate_and_wait(d, tab2, &rig.url("/two")).await;
    probe(d, tab2, b2).await;
    let t0 = Instant::now();
    let r = d
        .act(tab2, &format!("c#{b2}"), &rig.ctx)
        .await
        .expect("act");
    let took = t0.elapsed();
    let p = probed(d, tab2).await;
    eprintln!(
        "row6 stale: {:?} in {took:?}, {} attempts",
        r.steps,
        p.starts.len()
    );
    assert_eq!(why_of(&r, 0), "stale");
    assert_eq!(p.starts.len(), 1, "stale is not retried");
    assert!(r.stopped.is_none());
    assert!(took < Duration::from_millis(500), "{took:?}");

    // challenge
    let ch = d
        .open(&rig.url("/challenge"))
        .await
        .expect("open /challenge");
    let cb = id_of(d, ch, "#b").await;
    probe(d, ch, cb).await;
    let t0 = Instant::now();
    let r = d
        .act(ch, &format!("c#{cb}; c#{cb}"), &rig.ctx)
        .await
        .expect("act");
    let took = t0.elapsed();
    let p = probed(d, ch).await;
    eprintln!(
        "row6 challenge: {:?} stopped {:?} in {took:?}, {} attempts",
        r.steps,
        r.stopped,
        p.starts.len()
    );
    assert_eq!(r.steps.len(), 1);
    // T-21: the per-step gate sees the challenge page in the live index and stops before
    // any actionability attempt (H-1.4, rule 4).
    assert!(why_of(&r, 0).starts_with("CHALLENGE at"), "{:?}", r.steps);
    assert_eq!(p.starts.len(), 0, "a challenge is not attempted");
    let stop = r.stopped.expect("a challenge stops the program");
    assert_eq!(stop.class, Class::Challenge);
    let want = format!("#{cb}");
    assert_eq!(stop.element.as_deref(), Some(want.as_str()));
    assert!(took < Duration::from_millis(500), "{took:?}");
    rig.d.shutdown(Duration::from_secs(10)).await;
}

/// Row 7: after a navigation (driver-initiated and page-initiated) the new document has the
/// runtime, and its ids never repeat an earlier document's (D-604).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row7_runtime_reinstalled_and_ids_never_repeat() {
    let rig = rig(Vault::in_memory([])).await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/plain")).await.expect("open /plain");
    let ids1 = all_ids(d, tab).await;

    navigate_and_wait(d, tab, &rig.url("/two")).await;
    let has = sb_eval(
        d,
        tab,
        "function () { return typeof globalThis.__sb === 'object' && typeof globalThis.__sb.act === 'function'; }",
        &[],
    )
    .await;
    assert_eq!(has, json!(true), "the runtime is in the new document");
    // Any driver call into the new document walks it with a fresh base first.
    let r = d.act(tab, "w(50ms)", &rig.ctx).await.expect("act w()");
    assert_eq!(r.steps[0].outcome, StepOutcome::Ok);
    let ids2 = all_ids(d, tab).await;
    let n = id_of(d, tab, "#n").await;

    // Page-initiated navigation back to the first URL.
    let old = d.with_registry(|r| r.sb_context(tab, 0).cloned());
    let plain = rig.url("/plain");
    page_script(d, tab, "location.href = '/plain'; 0").await;
    wait_new_doc(d, tab, &plain, old).await;
    let r = d.act(tab, "w(50ms)", &rig.ctx).await.expect("act w()");
    assert_eq!(r.steps[0].outcome, StepOutcome::Ok);
    let ids3 = all_ids(d, tab).await;
    let b3 = id_of(d, tab, "#b").await;
    eprintln!("row7: doc1 ids {ids1:?}; doc2 ids {ids2:?}; doc3 ids {ids3:?}");

    let (max1, min2, max2, min3) = (
        *ids1.iter().max().unwrap(),
        *ids2.iter().min().unwrap(),
        *ids2.iter().max().unwrap(),
        *ids3.iter().min().unwrap(),
    );
    assert!(min2 > max1, "doc2 ids start after doc1's");
    assert!(min3 > max2, "doc3 ids start after doc2's");
    assert!(min2 >= 100_001 && min3 >= 200_001, "D-604 bases");
    for id in &ids2 {
        assert!(!ids1.contains(id) && !ids3.contains(id), "{id} repeated");
    }
    // A new id works on the live document; the previous document's id is stale.
    let r = d.act(tab, &format!("c#{b3}"), &rig.ctx).await.expect("act");
    assert_eq!(r.steps[0].outcome, StepOutcome::Ok, "{:?}", r.steps);
    let r = d.act(tab, &format!("c#{n}"), &rig.ctx).await.expect("act");
    assert_eq!(why_of(&r, 0), "stale");
    rig.d.shutdown(Duration::from_secs(10)).await;
}

/// Row 8 (B-4 #11): a call whose context is destroyed mid-call is retried once in the new
/// document. One navigation under a `w(3s)` settle: the retry answers. Two navigations in a
/// row: the second loss is not retried, the step errors.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row8_lost_context_retried_exactly_once() {
    let rig = rig(Vault::in_memory([])).await;
    let d = &rig.d;
    // A page that never settles, then navigates once to a calm page.
    let tab = d.open(&rig.url("/plain")).await.expect("open /plain");
    page_script(
        d,
        tab,
        "let n = 0; setInterval(() => { document.body.dataset.n = String(++n); }, 30); \
         setTimeout(() => { location.href = '/calm'; }, 300); 0",
    )
    .await;
    let t0 = Instant::now();
    let r = d.act(tab, "w(3s)", &rig.ctx).await;
    let took = t0.elapsed();
    eprintln!("row8 one loss: {r:?} in {took:?}");
    let r = r.expect("one lost context is retried in the new document");
    assert_eq!(r.steps[0].outcome, StepOutcome::Ok);
    assert_eq!(r.steps[0].settled, Some(true), "settled on the calm page");
    assert!(took < Duration::from_secs(3), "{took:?}");
    assert_eq!(
        d.with_registry(|r| main_url(r, tab)),
        Some(rig.url("/calm"))
    );

    // Two losses: /plain (churning) -> /m2 (churning, navigates after 400 ms) -> /calm.
    let tab = d.open(&rig.url("/plain")).await.expect("open /plain");
    page_script(
        d,
        tab,
        "let n = 0; setInterval(() => { document.body.dataset.n = String(++n); }, 30); \
         setTimeout(() => { location.href = '/m2'; }, 300); 0",
    )
    .await;
    let t0 = Instant::now();
    let r = d.act(tab, "w(3s)", &rig.ctx).await;
    let took = t0.elapsed();
    eprintln!("row8 two losses: {r:?} in {took:?}");
    let e = r.expect_err("a second loss is not retried");
    assert!(
        e.to_string().contains("context went away"),
        "the error names the lost context: {e}"
    );
    assert!(took < Duration::from_secs(3), "{took:?}");
    rig.d.shutdown(Duration::from_secs(10)).await;
}

/// Row 9 (H-2): step labels, step errors, stop reasons and parse errors never carry an
/// `@vault(..)` value or the text after `=` in a `t#` step. The vault value is
/// `SB_VAULT_CANARY` (or a generated one); it is never printed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row9_labels_and_errors_carry_no_values() {
    let from_env = std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|v| !v.is_empty());
    eprintln!("row9: canary from SB_VAULT_CANARY: {}", from_env.is_some());
    let canary = from_env.unwrap_or_else(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("sbcanary{nanos}x{}", std::process::id())
    });
    let literal = "SECRETCANARY";
    let vault = Vault::in_memory([("canary".to_string(), canary.clone(), EntryKind::Secret)]);
    let rig = rig(vault).await;
    let d = &rig.d;
    let clean = |what: &str, s: &str| {
        // Never print `s`: it would carry the leak into the output.
        assert!(!s.contains(&canary), "{what}: vault value leaked");
        assert!(!s.contains(literal), "{what}: t# literal leaked");
        assert!(!s.contains("@vault"), "{what}: @vault text leaked");
        assert!(!s.contains("canary)"), "{what}: vault key leaked");
    };
    let check = |what: &str, r: &ActResult| {
        clean(what, &format!("{r:?}"));
        clean(what, &serde_json::to_string(&r.steps).unwrap());
        clean(what, &serde_json::to_string(&r.stopped).unwrap());
        for s in &r.steps {
            clean(what, &s.step);
            if let StepOutcome::Failed { why } = &s.outcome {
                clean(what, why);
            }
        }
    };

    let tab = d.open(&rig.url("/form")).await.expect("open /form");
    let e = id_of(d, tab, "#e").await;
    let programs = [
        "t#1=@vault(canary); t#2=SECRETCANARY".to_string(),
        format!("t#{e}=@vault(canary)"),
        format!("t#{e}=SECRETCANARY"),
        format!("w(50ms); t#{e}=@vault(canary)"),
        format!("w(50ms); t#{e}=SECRETCANARY"),
        "t#99999=@vault(canary); t#2=SECRETCANARY".to_string(),
        "w(50ms); t#99999=SECRETCANARY".to_string(),
    ];
    for (i, prog) in programs.iter().enumerate() {
        let r = d.act(tab, prog, &rig.ctx).await.expect("act");
        check(&format!("program {i}"), &r);
        let labels: Vec<&str> = r.steps.iter().map(|s| s.step.as_str()).collect();
        eprintln!("row9 program {i}: labels {labels:?}");
    }
    // The labels are exactly the target.
    let r = d
        .act(tab, &format!("w(50ms); t#{e}=SECRETCANARY"), &rig.ctx)
        .await
        .unwrap();
    assert_eq!(r.steps[1].step, format!("t#{e}"));
    let r = d
        .act(tab, &format!("t#{e}=@vault(canary)"), &rig.ctx)
        .await
        .unwrap();
    assert_eq!(r.steps[0].step, format!("t#{e}"));

    // Stale and challenge paths.
    let old = e;
    navigate_and_wait(d, tab, &rig.url("/two")).await;
    let r = d
        .act(tab, &format!("t#{old}=@vault(canary)"), &rig.ctx)
        .await
        .unwrap();
    assert_eq!(why_of(&r, 0), "stale");
    check("stale", &r);
    let ch = d
        .open(&rig.url("/challenge"))
        .await
        .expect("open /challenge");
    let cb = id_of(d, ch, "#b").await;
    for prog in [
        format!("t#{cb}=@vault(canary)"),
        format!("t#{cb}=SECRETCANARY"),
    ] {
        let r = d.act(ch, &prog, &rig.ctx).await.unwrap();
        assert!(r.stopped.is_some());
        check("challenge", &r);
    }

    // Parse errors carry no value either.
    for prog in [
        "t#1=SECRETCANARY; c#",
        "t#1=@vault(canary); zz(",
        "t#1=@vault(canary) t#2=SECRETCANARY",
        "t#=SECRETCANARY",
    ] {
        let Err(err) = d.act(tab, prog, &rig.ctx).await else {
            // A program that parses is checked like any other result.
            continue;
        };
        clean("parse error Display", &err.to_string());
        clean("parse error Debug", &format!("{err:?}"));
        eprintln!("row9 parse error checked");
    }

    // Nothing reached the ledger or the run folder.
    clean("ledger", &format!("{:?}", rig.store.ledger()));
    let mut files = 0;
    let mut stack = vec![rig.home.path().join("runs")];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(bytes) = std::fs::read(&p) {
                files += 1;
                clean("run folder file", &String::from_utf8_lossy(&bytes));
            }
        }
    }
    eprintln!("row9: {files} run-folder files scanned");
    rig.d.shutdown(Duration::from_secs(10)).await;
}

/// Shared by rows 4 and 5 (round 2): run a permanent cover to the cap on `path` and check
/// the cap (<= 5 s + 40 ms) and the D-603/D-606 slots, written as the spec's numbers.
async fn cap_and_schedule(path: &str, label: &str) {
    let rig = rig(Vault::in_memory([])).await;
    let d = &rig.d;
    let tab = d.open(&rig.url(path)).await.expect("open");
    let b = id_of(d, tab, "#b").await;
    probe(d, tab, b).await;
    warm(d, tab).await;
    let (p_mark, _t_s, t_r) = mark(d, tab).await;
    let t_a = Instant::now();
    let r = d.act(tab, &format!("c#{b}"), &rig.ctx).await.expect("act");
    let took = t_a.elapsed();
    let p = probed(d, tab).await;
    let why = why_of(&r, 0);
    let n = p.starts.len();
    let waits: Vec<f64> = (1..n).map(|i| p.starts[i] - p.hits[i - 1]).collect();
    let t0_page = p_mark + ms(t_a - t_r);
    let last_start = p.starts[n - 1] - t0_page;
    eprintln!(
        "{label}: act took {took:?}: {why:?}; {n} attempts; waits (ms) {:?}; last attempt \
         started {last_start:.0} ms after act",
        waits.iter().map(|w| w.round()).collect::<Vec<_>>()
    );
    assert!(
        why.starts_with("covered by div.cookie (OVERLAY #") && why.ends_with(") after 5.0 s"),
        "{why}"
    );
    assert!(took >= ACT_CAP, "{took:?}");
    assert!(took <= ACT_CAP + CAP_TOLERANCE + GATE_READ, "cap: {took:?}");
    assert_eq!(p.starts.len(), p.hits.len(), "{p:?}");
    for (i, w) in waits.iter().enumerate() {
        let want = SCHEDULE_MS.get(i).copied().unwrap_or(500.0);
        assert!(*w <= want + 80.0, "wait {i}: {w:.1} ms > {want} + 80");
        assert!(*w >= want - 2.0, "wait {i}: {w:.1} ms < {want}");
    }
    assert!((10..=16).contains(&n), "attempts in 5 s: {n}");
    assert!(
        last_start <= ms(ACT_CAP),
        "no attempt starts after the budget: {last_start:.1}"
    );
    rig.d.shutdown(Duration::from_secs(10)).await;
}

/// Row 5b (round 2): rows 4 and 5 on a page that mutates every 10 ms, so the settle between
/// attempts never ends early; every wait (the last included) is the slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row5b_cap_and_backoff_on_a_page_that_mutates_constantly() {
    cap_and_schedule("/churn-covered", "row5b churning").await;
}

/// Row 5c (round 2): the same strict check on the quiet page (settle ends early there, so
/// the slot wait itself must hold the schedule).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row5c_cap_and_backoff_on_a_quiet_page_every_wait() {
    cap_and_schedule("/covered", "row5c quiet").await;
}

/// Every `#<digits>` ref in a why.
fn refs(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, _) in s.match_indices('#') {
        let n: String = s[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !n.is_empty() {
            out.push(n);
        }
    }
    out
}

/// Row 13 (round 2): a page element whose id spells a driver ref, `x (OVERLAY #12)` or
/// `x (#12)`, covers the button: the why names the cover by its real ref only, with no
/// `#12` and no `(OVERLAY #12)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row13_forged_overlay_id_cannot_forge_a_ref() {
    let rig = rig(Vault::in_memory([])).await;
    let d = &rig.d;
    for (path, sel, forged) in [
        ("/forged", "[id=\"x (OVERLAY #12)\"]", "(OVERLAY #12)"),
        ("/forged2", "[id=\"x (#12)\"]", "(#12)"),
    ] {
        let tab = d.open(&rig.url(path)).await.expect("open forged");
        let b = id_of(d, tab, "#b").await;
        let cover = id_of(d, tab, sel).await;
        assert_ne!(
            cover, 12,
            "the cover's real id must differ from the forged one"
        );
        let t0 = Instant::now();
        let r = d.act(tab, &format!("c#{b}"), &rig.ctx).await.expect("act");
        let why = why_of(&r, 0).to_string();
        eprintln!(
            "row13 {path}: {why:?} after {:?}; cover's real id {cover}",
            t0.elapsed()
        );
        assert!(why.starts_with("covered by div#x"), "{why}");
        assert!(!why.contains("#12"), "{why}");
        assert!(!why.contains(forged), "{why}");
        assert!(!why.contains("(OVERLAY #12)"), "{why}");
        assert!(!refs(&why).iter().any(|n| n == "12"), "no ref to 12: {why}");
        assert!(why.ends_with(" after 5.0 s"), "{why}");
    }
    rig.d.shutdown(Duration::from_secs(10)).await;
}

/// Row 14 (round 2): a `w(d)` the page does not answer in time (its main thread is busy)
/// is a failed step that keeps the earlier steps' results, not an `Err` for the program.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row14_w_not_answered_in_time_is_a_failed_step() {
    let rig = rig(Vault::in_memory([])).await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/calm")).await.expect("open /calm");
    // The page churns (no settle), and 500 ms from now blocks its main thread for 2 s: past
    // w(1s)'s d + 300 ms deadline, which falls about 1.5 s after act starts.
    page_script(
        d,
        tab,
        "let n = 0; setInterval(() => { document.body.dataset.n = String(++n); }, 20); \
         setTimeout(() => { const t = performance.now(); while (performance.now() - t < 2000) {} }, 500); 0",
    )
    .await;
    let t0 = Instant::now();
    let r = d.act(tab, "w(200ms); w(1s)", &rig.ctx).await;
    let took = t0.elapsed();
    eprintln!("row14: {r:?} in {took:?}");
    let r = r.expect("a w() timeout is a failed step, not Err");
    assert_eq!(r.steps.len(), 2, "{r:?}");
    assert_eq!(
        r.steps[0].outcome,
        StepOutcome::Ok,
        "the first w keeps its result"
    );
    assert_eq!(r.steps[0].step, "w(200ms)");
    assert_eq!(r.steps[1].step, "w(1s)");
    let why = why_of(&r, 1);
    assert!(why.contains("did not answer"), "{why}");
    assert!(r.stopped.is_none());
    assert!(
        took < Duration::from_millis(2500),
        "it did not wait for the page: {took:?}"
    );
    // Let the renderer finish its busy loop before the browser closes, so the next test is
    // not timed against a spinning renderer.
    page_script(d, tab, "0").await;
    rig.d.shutdown(Duration::from_secs(10)).await;
}
