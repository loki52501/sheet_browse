//! T-22 acceptance, tester rounds 1 and 2, against the real D-34 build (chromium-1248), public API
//! only (plus the registry and CDP views the driver exposes). docs/05 acceptance 4 (registry:
//! two popups with their opener, a same-origin iframe with a nested one, a cross-port and a
//! cross-site iframe as one opaque row each, never read inside) and acceptance 5 (settle(): SPA
//! skeletons are not empty; the fixed 3 s cap is never exceeded through settle(), open() and
//! w(3s) on endless mutations, never-ending requests, CSS animation loops, an EventSource
//! stream and a mix), plus B-4 #2 (close), B-4 #8 (a request started after settle began holds
//! it), B-4 #9 / B-614 (slow submit), B-601 (titles), and the password rule in a frame.
//!
//! Round 2 (rows 13a to 13f): D-625 (no settled answer while a request is in flight, w(10s),
//! settle cap 6000, a request started before settle), the act window ending by input + 3 s
//! across a document change, the row scrub inside a same-origin frame, act refused in a
//! cross-origin frame with nothing clicked, and challenge-widget frames never walked (D-628).
//!
//! Every page is written here and served from 127.0.0.1 (a second port for cross-origin, the
//! host name `localhost` for a cross-site, out-of-process frame). No network. One browser at a
//! time (`SERIAL`); run with `--test-threads=1`.
//!
//! Vault: `SB_VAULT_CANARY` (dashed, unique per run) is the value of key `pw`. It is typed only
//! through `@vault(pw)` and is never printed: every check for it runs before any string that
//! could hold it is shown.

mod common;

use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::EntryKind;
use harness_core::{Class, Harness, RunId, ToolCtx, ToolRegistry, Vault};
use sb_driver::{
    Consequence, Driver, DriverError, FrameInfo, LaunchEnv, LaunchOpts, Profile, SettleOpts,
    StepOutcome, TabId,
};
use sb_sheet::{FieldValue, IndexRow, PageMeta, SkeletonOpts};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CAP: Duration = Duration::from_secs(30);
/// The settle cap (B-4 #8) and the most any capped wait may exceed it by.
const SETTLE_CAP: Duration = Duration::from_millis(3000);
const SLACK: Duration = Duration::from_millis(40);
/// D-621: the act window waits for a step's request at most 3 s after the input, then makes
/// one in-page call capped at 500 ms.
const ACT_WINDOW_MAX: Duration = Duration::from_millis(3540);
/// Text inside every opaque frame; it must never reach `index()`.
const OPAQUE_MARK: &str = "zq-opaque-marker-t22t";
/// A preset password attribute (not a vault value); it must never reach `index()`.
const PRESET_PW: &str = "zq-preset-pw-t22t";
const RUNS: usize = 5;
/// D-626 sanity: open()'s navigation part on loopback (round 1 measured 0.25 to 0.53 s).
const NAV_SANITY: Duration = Duration::from_millis(1500);
/// Round 2 (b): block the page's main thread 200 ms of every 220 ms.
const BUSY_JS: &str = "setInterval(() => { const t = performance.now(); \
                       while (performance.now() - t < 200) {} }, 220);";
/// Round 2 (d): requests to `/clicked` (a click inside a cross-origin frame), any port.
static CLICKS: AtomicUsize = AtomicUsize::new(0);

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "canary-t22-tester-not-a-secret".to_owned())
}

/// True when `s` holds the canary (ASCII case-insensitive, as core's scrubber matches).
fn has_canary(s: &str) -> bool {
    s.to_ascii_lowercase()
        .contains(&canary().to_ascii_lowercase())
}

const STYLE: &str = "<style>body{margin:0;font:16px sans-serif} button,a{display:block;\
                     margin:6px;width:200px;height:28px} iframe{width:320px;height:140px;\
                     display:block} input{display:block;margin:6px}</style>";

enum Reply {
    Page(String),
    /// Answer `body` after `ms`.
    Slow(u64, String),
    /// Never answer.
    Hang,
    /// An EventSource stream: one message every 100 ms, forever.
    Sse,
    /// A body that never ends: one byte every 100 ms.
    Drip,
}

fn param<'a>(query: &'a str, k: &str) -> Option<&'a str> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix(k).and_then(|r| r.strip_prefix('=')))
}

/// The scripts of the cap pages; `p2` is the second server's port (hanging requests go there
/// so they do not use up the six connections Chromium allows per host and port).
fn cap_script(kind: &str, p2: &str) -> String {
    let mutate = "setInterval(() => { document.getElementById('t').textContent = \
                  String(Date.now()); }, 10);";
    let fetch = format!(
        "fetch('/drip').then((r) => {{ const rd = r.body.getReader(); \
         const pump = () => rd.read().then((x) => {{ if (!x.done) pump(); }}); pump(); }})\
         .catch(() => {{}}); \
         setInterval(() => {{ fetch('http://127.0.0.1:{p2}/hang?' + Math.random())\
         .catch(() => {{}}); }}, 50);"
    );
    let anim = "const b = document.getElementById('b'); b.addEventListener('animationend', \
                () => { b.classList.remove('p'); void b.offsetWidth; b.classList.add('p'); });";
    let sse = "const es = new EventSource('/sse'); es.onmessage = (e) => { \
               document.getElementById('n').textContent = e.data; };";
    match kind {
        "mut" => mutate.into(),
        "fetch" => fetch,
        "anim" => anim.into(),
        "sse" => sse.into(),
        _ => format!("{mutate} {fetch} {anim} {sse}"),
    }
}

fn route(path: &str, query: &str) -> Option<Reply> {
    let html = |b: String| Reply::Page(format!("<!doctype html><html><body>{b}</body></html>"));
    let p2 = param(query, "p2").unwrap_or("0").to_string();
    Some(match path {
        // Acceptance 4.
        "/t4" => html(format!(
            "<title>T4 main</title>{STYLE}<h1>Registry tester</h1>\
             <button id=wo onclick=\"window.open('/pop-a', 'popa', 'width=420,height=320')\">\
               Window open popup</button>\
             <a id=bl href=/pop-b target=_blank>Blank target popup</a>\
             <iframe id=so title=\"Same outer\" src=/same-outer></iframe>\
             <iframe id=cp title=\"Cross port\" src=\"http://127.0.0.1:{p2}/opaque?m=port\"></iframe>\
             <iframe id=cs title=\"Cross site\" src=\"http://localhost:{p2}/opaque?m=site\"></iframe>\
             <button id=ma>Main after</button>"
        )),
        "/same-outer" => html(format!(
            "<title>Outer</title>{STYLE}<h2>Outer heading</h2><button>Outer button</button>\
             <iframe id=si title=\"Same inner\" src=/same-inner></iframe>\
             <button>Outer after</button>"
        )),
        "/same-inner" => html(format!(
            "<title>Inner</title>{STYLE}<button>Inner leaf button</button>"
        )),
        "/opaque" => html(format!(
            "<title>{OPAQUE_MARK} title</title>{STYLE}<h1>{OPAQUE_MARK} heading</h1>\
             <button>{OPAQUE_MARK} button</button>\
             <input placeholder=\"{OPAQUE_MARK} field\" value=\"{OPAQUE_MARK} value\">\
             <a href=/x>{OPAQUE_MARK} link</a><p>{OPAQUE_MARK} text</p>"
        )),
        "/pop-a" => html("<title>Popup A</title><h1>Popup A</h1>".into()),
        "/pop-b" => html("<title>Popup B</title><h1>Popup B</h1>".into()),
        // Acceptance 5: SPAs.
        "/spa-400" => html(
            "<title>SPA 400</title><div id=app></div><script>\
             addEventListener('load', () => setTimeout(() => {\
               document.getElementById('app').innerHTML = \
                 '<h1>Rendered late</h1><button>Late button</button>';\
             }, 400));</script>"
                .into(),
        ),
        // A shell at once (so content alone does not settle it), the data after a 1.5 s fetch.
        "/spa-fetch" => html(
            "<title>SPA fetch</title><div id=app><h1>Loading shell</h1></div><script>\
             fetch('/api/slow?ms=1500').then((r) => r.text()).then((t) => {\
               document.getElementById('app').innerHTML = '<h1>Fetched ' + t + '</h1>' +\
                 '<label>Email <input id=email></label><button>Continue</button>';\
             });</script>"
                .into(),
        ),
        // Two fetches in a chain (the second starts when the first ends), 700 ms each.
        "/spa-chain" => html(
            "<title>SPA chain</title><div id=app><h1>Loading shell</h1></div><script>\
             fetch('/api/slow?ms=700').then((r) => r.text())\
               .then(() => fetch('/api/slow?ms=700')).then((r) => r.text()).then((t) => {\
                 document.getElementById('app').innerHTML = '<h1>Chained ' + t + '</h1>' +\
                   '<button>Chain button</button>';\
               });</script>"
                .into(),
        ),
        // Cap pages.
        "/cap" => {
            let kind = param(query, "k").unwrap_or("mix").to_string();
            html(format!(
                "<title>Cap {kind}</title><style>\
                 @keyframes spin{{from{{transform:rotate(0)}}to{{transform:rotate(360deg)}}}}\
                 @keyframes pulse{{from{{opacity:1}}to{{opacity:.3}}}}\
                 #s{{width:40px;height:40px;background:#08f;animation:spin 1s linear infinite}}\
                 .p{{animation:pulse 200ms}}</style>\
                 <h1>Cap {kind}</h1><div id=s></div><p id=b class=p>Pulse</p>\
                 <p id=t>0</p><p id=n>-</p><button>Cap button</button>\
                 <script>{}</script>",
                cap_script(&kind, &p2)
            ))
        }
        "/drip" => Reply::Drip,
        "/sse" => Reply::Sse,
        // Settled correctness.
        "/quiet" => html(format!(
            "<title>Quiet</title>{STYLE}<h1>Quiet</h1><p id=o>none</p><button>Quiet button</button>"
        )),
        // Slow submit.
        "/save" => {
            let api = match param(query, "k") {
                Some("hang") => "/hang".to_string(),
                _ => "/api/slow?ms=900".to_string(),
            };
            let via_fetch = param(query, "via") == Some("fetch");
            let send = if via_fetch {
                format!(
                    "fetch('{api}', {{method: 'POST', body: 'n=1'}}).then((r) => r.text())\
                     .then(() => {{ document.getElementById('status').innerHTML = \
                     '<h2>Saved by fetch</h2>'; }});"
                )
            } else {
                format!(
                    "const x = new XMLHttpRequest(); x.open('POST', '{api}');\
                     x.onload = () => {{ document.getElementById('status').innerHTML = \
                     '<h2>Saved by XHR</h2>'; }}; x.send('n=1');"
                )
            };
            html(format!(
                "<title>Save</title>{STYLE}<form id=f onsubmit=\"event.preventDefault(); {send}\">\
                 <label>Note <input id=n></label><button id=save type=submit>Save note</button>\
                 </form><div id=status></div>"
            ))
        }
        // Titles.
        "/title" => html(format!(
            "<title>Before</title>{STYLE}<h1>Titles</h1>\
             <label>Echo <input id=e oninput=\"document.title = this.value + ' #12 [x](#3)'\">\
             </label>"
        )),
        // Password in a same-origin frame.
        "/pw-host" => html(format!(
            "<title>PW host</title>{STYLE}<h1>Password host</h1>\
             <iframe id=pf title=\"Login frame\" src=/pw-frame></iframe>"
        )),
        "/pw-frame" => html(format!(
            "<title>PW frame</title>{STYLE}<form><label>User <input id=u></label>\
             <label>Secret <input id=pw type=password></label>\
             <label>Preset <input id=pre type=password value=\"{PRESET_PW}\"></label>\
             <label>Hinted <input id=hint type=password placeholder=Hint value=\"{PRESET_PW}\"></label>\
             <button type=button>Sign in</button></form>"
        )),
        // Round 2 (b): the act window. A click logs `t22-click` (the reference instant, from
        // Runtime.consoleAPICalled), then: `m=net` starts a never-answered request and endless
        // mutations, `m=mut` endless mutations only; both navigate to the never-settling mix
        // page `at` ms later (a document change inside the window). `m=busy`: as `net`, and
        // the main thread is blocked 200 ms of every 220 ms (in-page calls answer late); the
        // next document is busy the same way.
        "/win" => {
            let at = param(query, "at").unwrap_or("1000").to_string();
            let m = param(query, "m").unwrap_or("mut");
            let net = if m == "net" || m == "busy" {
                format!("fetch('http://127.0.0.1:{p2}/hang?w').catch(() => {{}});")
            } else {
                String::new()
            };
            let (busy, to) = if m == "busy" {
                (BUSY_JS.to_string(), format!("/busy-page?p2={p2}"))
            } else {
                (String::new(), format!("/cap?k=mix&p2={p2}"))
            };
            html(format!(
                "<title>Window</title>{STYLE}<h1>Window</h1><p id=t>0</p>\
                 <button id=go onclick=\"console.log('t22-click'); {net} {busy} \
                 setInterval(() => {{ document.getElementById('t').textContent = \
                 String(Date.now()); }}, 10); setTimeout(() => {{ \
                 location.href = '{to}'; }}, {at});\">Go window</button>"
            ))
        }
        "/busy-page" => html(format!(
            "<title>Busy</title>{STYLE}<h1>Busy</h1><p id=t>0</p><button>Busy button</button>\
             <script>fetch('http://127.0.0.1:{p2}/hang?b').catch(() => {{}}); {BUSY_JS} \
             setInterval(() => {{ document.getElementById('t').textContent = \
             String(Date.now()); }}, 10);</script>"
        )),
        // Round 2 (c): the canary goes into this same-origin frame at run time only.
        "/scrub-host" => html(format!(
            "<title>Scrub host</title>{STYLE}<h1>Scrub host</h1>\
             <iframe id=sf title=\"Scrub frame\" src=/scrub-frame \
             style=\"width:640px;height:420px\"></iframe><button>Host after</button>"
        )),
        "/scrub-frame" => html(format!(
            "<title>Scrub frame</title>{STYLE}<h2 id=h>Heading</h2><p id=near>near</p>\
             <div id=cap class=captcha style=\"width:120px;height:40px\"></div>\
             <button id=t>text</button><button id=al>x</button>\
             <label>Titled <input id=ti></label><input id=v aria-label=Value>\
             <label>Pick <select id=sel><option>one</option><option id=o2>two</option>\
             </select></label>"
        )),
        // Round 2 (d): cross-origin frames with a counter the test reads (the frame's own
        // world and the server's /clicked log).
        "/xo-host" => html(format!(
            "<title>XO host</title>{STYLE}<h1>XO host</h1>\
             <iframe id=xp title=\"Cross port\" src=\"http://127.0.0.1:{p2}/xo-frame?m=port\"></iframe>\
             <iframe id=xs title=\"Cross site\" src=\"http://localhost:{p2}/xo-frame?m=site\"></iframe>\
             <iframe id=xn title=\"Cross never\" src=\"http://127.0.0.1:{p2}/xo-frame?m=never\"></iframe>\
             <button>Host after</button>"
        )),
        "/xo-frame" => html(format!(
            "<title>XO frame</title>{STYLE}\
             <button id=b onclick=\"window.__n = (window.__n || 0) + 1; \
             fetch('/clicked?' + location.search.slice(1)).catch(() => {{}});\">Frame button</button>\
             <input id=i aria-label=\"Frame field\">"
        )),
        "/clicked" => {
            CLICKS.fetch_add(1, Ordering::SeqCst);
            Reply::Page("ok".into())
        }
        // Round 2 (e): a challenge widget frame holding a frame that later adds a third; a
        // normal same-origin frame beside it (walked, so the probe can tell the two apart).
        "/chw-host" => html(format!(
            "<title>Challenge host</title>{STYLE}<h1>Challenge host</h1>\
             <iframe id=cw title=\"Widget containing a Cloudflare security challenge\" \
             src=/chw1 style=\"width:360px;height:220px\"></iframe>\
             <iframe id=nf title=\"Normal frame\" src=/chw-normal></iframe>\
             <button>Outside button</button>"
        )),
        "/chw1" => html(format!(
            "{STYLE}<button>Inside one</button><iframe title=two src=/chw2 \
             style=\"width:300px;height:160px\"></iframe>"
        )),
        "/chw2" => html(format!(
            "{STYLE}<button>Inside two</button><script>setTimeout(() => {{ \
             const f = document.createElement('iframe'); f.title = 'three'; f.src = '/chw3'; \
             document.body.append(f); }}, 300);</script>"
        )),
        "/chw3" => html("<button>Inside three</button>".into()),
        "/chw-normal" => html(format!("{STYLE}<button>Normal frame button</button>")),
        "/api/slow" => {
            let ms = param(query, "ms")
                .and_then(|m| m.parse().ok())
                .unwrap_or(1000);
            Reply::Slow(ms, "slow".into())
        }
        "/hang" => Reply::Hang,
        _ => return None,
    })
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
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let target = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
                let cors = "Access-Control-Allow-Origin: *\r\n";
                let (status, ct, body) = match route(path, query) {
                    Some(Reply::Page(b)) => ("200 OK", "text/html", b),
                    Some(Reply::Slow(ms, b)) => {
                        // The fixture server's latency (a slow API), not a driver wait.
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        ("200 OK", "text/plain", b)
                    }
                    Some(Reply::Hang) => {
                        // A request that never ends: open and unanswered until the runtime goes.
                        std::future::pending::<()>().await;
                        return;
                    }
                    Some(kind @ (Reply::Sse | Reply::Drip)) => {
                        let sse = matches!(kind, Reply::Sse);
                        let ct = if sse {
                            "text/event-stream"
                        } else {
                            "text/plain"
                        };
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nCache-Control: no-cache\r\n\
                             {cors}Connection: close\r\n\r\n"
                        );
                        if sock.write_all(head.as_bytes()).await.is_err() {
                            return;
                        }
                        let mut i = 0u64;
                        loop {
                            let chunk = if sse {
                                format!("data: {i}\n\n")
                            } else {
                                "x".to_string()
                            };
                            if sock.write_all(chunk.as_bytes()).await.is_err() {
                                return;
                            }
                            i += 1;
                            // The stream's own pace (a server pushing every 100 ms).
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                    None => ("404 Not Found", "text/plain", String::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n\
                     {cors}Connection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    port
}

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Rig {
    d: Driver,
    _serial: tokio::sync::MutexGuard<'static, ()>,
    port: u16,
    port2: u16,
    home: tempfile::TempDir,
    store: Arc<MemStore>,
    vault: Arc<Vault>,
    harness: Harness,
}

impl Rig {
    fn url(&self, p: &str) -> String {
        format!("http://127.0.0.1:{}{p}", self.port)
    }

    fn with_p2(&self, p: &str) -> String {
        let sep = if p.contains('?') { '&' } else { '?' };
        self.url(&format!("{p}{sep}p2={}", self.port2))
    }

    fn run_dir(&self) -> std::path::PathBuf {
        self.home.path().join("runs").join("accept-t22-tester")
    }

    fn ctx(&self, class: Class) -> ToolCtx {
        ToolCtx::new(
            RunId("accept-t22-tester".into()),
            self.run_dir(),
            class,
            &self.harness,
            ToolRegistry::new(vec![]),
        )
    }
}

async fn rig() -> Rig {
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
    let (port, port2) = (serve().await, serve().await);
    let store = Arc::new(MemStore::new());
    let vault = Arc::new(Vault::in_memory([(
        "pw".to_string(),
        canary(),
        EntryKind::Secret,
    )]));
    let harness = Harness {
        store: store.clone(),
        vault: vault.clone(),
        asker: Arc::new(ScriptedAsker::new()),
        runs_root: home.path().join("runs"),
        run: None,
    };
    Rig {
        d,
        _serial: serial,
        port,
        port2,
        home,
        store,
        vault,
        harness,
    }
}

/// `js` in the page's own world of frame 0.
async fn page_js(d: &Driver, tab: TabId, js: &str) -> Value {
    let ctx = d
        .wait_registry(CAP, |r| r.default_context(tab, 0).cloned())
        .await
        .expect("main world context");
    let v = d
        .cdp()
        .call(
            Some(&ctx.session),
            "Runtime.evaluate",
            json!({"expression": js, "contextId": ctx.id, "returnByValue": true}),
        )
        .await
        .expect("Runtime.evaluate");
    v.pointer("/result/value").cloned().unwrap_or(Value::Null)
}

fn by_name<'a>(ix: &'a [IndexRow], name: &str) -> &'a IndexRow {
    let names: Vec<&str> = ix.iter().map(|r| r.name.as_str()).collect();
    ix.iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("a row named {name:?} among {names:?}"))
}

fn skeleton(ix: &[IndexRow], url: &str, title: &str) -> String {
    sb_sheet::skeleton(
        ix,
        &[],
        &SkeletonOpts {
            meta: PageMeta {
                url: url.into(),
                title: title.into(),
                ..PageMeta::default()
            },
        },
    )
}

/// Every file under `dir` whose bytes hold `needle` (ASCII case-insensitive).
fn files_containing(dir: &std::path::Path, needle: &str) -> Vec<String> {
    let mut hits = Vec::new();
    let needle = needle.to_ascii_lowercase();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                let low = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
                if low.contains(&needle) {
                    hits.push(path.display().to_string());
                }
            }
        }
    }
    hits
}

// ---------------------------------------------------------------------------------------
// Row 1: acceptance 4.

struct T4Frames {
    outer: FrameInfo,
    inner: FrameInfo,
    port: FrameInfo,
    site: FrameInfo,
}

async fn t4_frames(rig: &Rig, tab: TabId) -> T4Frames {
    let (p1, p2) = (rig.port, rig.port2);
    let want = [
        format!("http://127.0.0.1:{p1}/same-outer"),
        format!("http://127.0.0.1:{p1}/same-inner"),
        format!("http://127.0.0.1:{p2}/opaque?m=port"),
        format!("http://localhost:{p2}/opaque?m=site"),
    ];
    let got = rig
        .d
        .wait_registry(CAP, |r| {
            let frames = r.frames(tab)?;
            let by = |u: &str| frames.iter().find(|f| f.url == u).cloned();
            Some(T4Frames {
                outer: by(&want[0])?,
                inner: by(&want[1])?,
                port: by(&want[2])?,
                site: by(&want[3])?,
            })
        })
        .await;
    match got {
        Some(f) => f,
        None => panic!(
            "all four frames registered within 30 s: {:?}",
            rig.d.frames(tab).await
        ),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row01_registry_popups_frames_and_opaque_rows() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.with_p2("/t4")).await.expect("open t4");
    let f = t4_frames(&rig, tab).await;

    // frames(): the public view agrees with the registry; parents and opacity.
    let frames = d.frames(tab).await.expect("frames");
    println!("row01 frames(main): {frames:?}");
    assert_eq!(
        frames.len(),
        5,
        "main + outer + inner + port + site: {frames:?}"
    );
    assert_eq!(frames[0].frame, 0);
    assert_eq!(frames[0].parent, None);
    assert!(!frames[0].opaque);
    assert_eq!(f.outer.parent, Some(0), "same-origin iframe under the page");
    assert!(!f.outer.opaque);
    assert_eq!(
        f.inner.parent,
        Some(f.outer.frame),
        "nested under its parent"
    );
    assert!(!f.inner.opaque);
    assert_eq!(f.port.parent, Some(0));
    assert!(f.port.opaque, "cross-port is opaque");
    assert_eq!(f.site.parent, Some(0));
    assert!(f.site.opaque, "cross-site is opaque");
    let (tab_sess, site_sess, port_sess) = d.with_registry(|r| {
        (
            r.session(tab).map(String::from),
            r.frame_session(tab, f.site.frame).map(String::from),
            r.frame_session(tab, f.port.frame).map(String::from),
        )
    });
    assert!(
        site_sess.is_some() && site_sess != tab_sess,
        "cross-site is an OOPIF"
    );
    assert_eq!(
        port_sess, tab_sess,
        "cross-port is in process (cross-origin only)"
    );

    // index(): merged, document order, opaque frames one row each, never read inside.
    let ix = d.index(tab).await.expect("index");
    let dump = serde_json::to_string(&ix).unwrap();
    let low = dump.to_ascii_lowercase();
    assert!(
        !low.contains(OPAQUE_MARK),
        "the opaque frames' content is in index(): {dump}"
    );
    assert!(!format!("{ix:?}").to_ascii_lowercase().contains(OPAQUE_MARK));
    let sk = skeleton(&ix, &rig.with_p2("/t4"), "T4 main");
    assert!(!sk.to_ascii_lowercase().contains(OPAQUE_MARK), "{sk}");
    let pos = |pred: &dyn Fn(&IndexRow) -> bool| ix.iter().position(pred);
    let outer_row = pos(&|r| r.child_frame == Some(f.outer.frame)).expect("outer iframe row");
    assert_eq!(ix[outer_row].frame, 0);
    assert!(!ix[outer_row].flags.opaque);
    let outer_btn = pos(&|r| r.name == "Outer button").expect("outer frame read");
    assert_eq!(ix[outer_btn].frame, f.outer.frame);
    assert!(ix[outer_btn]
        .elem()
        .to_string()
        .starts_with(&format!("#f{}.", f.outer.frame)));
    let inner_row = pos(&|r| r.child_frame == Some(f.inner.frame)).expect("inner iframe row");
    assert_eq!(
        ix[inner_row].frame, f.outer.frame,
        "the nested row is in the outer frame"
    );
    let inner_btn = pos(&|r| r.name == "Inner leaf button").expect("nested frame read");
    assert_eq!(ix[inner_btn].frame, f.inner.frame);
    let outer_after = pos(&|r| r.name == "Outer after").expect("outer after");
    let main_after = pos(&|r| r.name == "Main after").expect("main after");
    assert!(
        outer_row < outer_btn
            && outer_btn < inner_row
            && inner_row < inner_btn
            && inner_btn < outer_after
            && outer_after < main_after,
        "document order: {outer_row} {outer_btn} {inner_row} {inner_btn} {outer_after} {main_after}"
    );
    for (what, op) in [("port", f.port.frame), ("site", f.site.frame)] {
        let rows: Vec<&IndexRow> = ix.iter().filter(|r| r.child_frame == Some(op)).collect();
        assert_eq!(rows.len(), 1, "{what}: one row: {ix:#?}");
        assert_eq!(rows[0].frame, 0);
        assert!(rows[0].flags.opaque, "{what}: OPAQUE flag {:?}", rows[0]);
        assert!(
            !ix.iter().any(|r| r.frame == op),
            "{what}: no row from inside the frame"
        );
        println!("row01 opaque {what} row: {:?}", rows[0]);
    }

    // Popups: window.open (button) and target=_blank (link), each with its opener.
    let ctx = rig.ctx(Class::Submit);
    let mut popups = Vec::new();
    for (name, url, title) in [
        ("Window open popup", "/pop-a", "Popup A"),
        ("Blank target popup", "/pop-b", "Popup B"),
    ] {
        let el = by_name(&ix, name).elem();
        let r = d.act(tab, &format!("c{el}"), &ctx).await.expect("act");
        println!("row01 {name}: {:?}", r.steps);
        let opened = r.steps[0]
            .consequences
            .iter()
            .find_map(|c| match c {
                Consequence::OpenedTab { tab, title } => Some((*tab, title.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("opened tab in {:?}", r.steps));
        assert_eq!(opened.1, title);
        assert_eq!(
            d.with_registry(|r| r.opener(opened.0)),
            Some(tab),
            "{name}: opened by the main tab"
        );
        let pf = d.frames(opened.0).await.expect("popup frames");
        assert_eq!(pf.len(), 1, "{pf:?}");
        assert_eq!(pf[0].parent, None);
        assert_eq!(pf[0].url, rig.url(url));
        popups.push((opened.0, url, title));
    }
    assert_ne!(popups[0].0, popups[1].0);
    let tabs = d.tabs().await.expect("tabs");
    println!("row01 tabs(): {tabs:?}");
    for (p, url, title) in &popups {
        let row = tabs.iter().find(|t| t.0 == *p).expect("popup in tabs()");
        assert_eq!(row.1, rig.url(url));
        assert_eq!(row.2, *title);
    }
    assert_eq!(
        d.with_registry(|r| r.opener(tab)),
        None,
        "the main tab has no opener"
    );
    let main = tabs.iter().find(|t| t.0 == tab).expect("main in tabs()");
    assert_eq!(main.2, "T4 main");
    // frames() of the main tab is unchanged by the popups.
    assert_eq!(d.frames(tab).await.unwrap().len(), 5);
}

// ---------------------------------------------------------------------------------------
// Row 2: acceptance 5, skeletons.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row02_spa_skeletons_are_not_empty() {
    let rig = rig().await;
    let d = &rig.d;
    for (path, title, want, button) in [
        ("/spa-400", "SPA 400", "Rendered late", "Late button"),
        ("/spa-fetch", "SPA fetch", "Fetched slow", "Continue"),
        ("/spa-chain", "SPA chain", "Chained slow", "Chain button"),
    ] {
        // Right after open.
        let t0 = Instant::now();
        let tab = d.open(&rig.url(path)).await.expect("open");
        let took = t0.elapsed();
        let ix = d.index(tab).await.expect("index");
        let sk = skeleton(&ix, &rig.url(path), title);
        println!("row02 {path}: open {took:?}; skeleton after open:\n{sk}");
        assert!(
            sk.contains(want),
            "{path}: skeleton after open lacks {want:?}: {sk}"
        );
        by_name(&ix, button);
        assert!(
            !ix.iter().any(|r| r.name == "Loading shell"),
            "{path}: the shell was replaced"
        );
        d.close(tab).await.expect("close");

        // Right after open + settle.
        let tab = d.open(&rig.url(path)).await.expect("open");
        let t1 = Instant::now();
        let s = d.settle(tab, SettleOpts::default()).await.expect("settle");
        let st = t1.elapsed();
        let ix = d.index(tab).await.expect("index");
        let sk = skeleton(&ix, &rig.url(path), title);
        println!("row02 {path}: settle after open {st:?} {s:?}");
        assert!(sk.contains(want), "{path}: skeleton after settle: {sk}");
        assert!(s.settled, "{path}: {s:?}");
        d.close(tab).await.expect("close");
    }
}

// ---------------------------------------------------------------------------------------
// Row 3: the cap through settle(), w(3s) and open().

/// Main-frame commits seen on the CDP event stream, with the time they arrived.
fn commit_watch(d: &Driver) -> Arc<Mutex<Vec<(Instant, String)>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut rx = d.cdp().events();
    let out = seen.clone();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if ev.method == "Page.frameNavigated"
                        && ev.params.pointer("/frame/parentId").is_none()
                    {
                        let url = ev
                            .params
                            .pointer("/frame/url")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        out.lock().unwrap().push((Instant::now(), url));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            }
        }
    });
    seen
}

/// settle() and w(3s), 5 runs each, on one tab of the cap page `kind`.
async fn cap_settle_and_w(kind: &str) {
    let rig = rig().await;
    let d = &rig.d;
    let url = rig.with_p2(&format!("/cap?k={kind}"));
    let limit = SETTLE_CAP + SLACK;
    let tab = d.open(&url).await.expect("open");
    let mut worst = Duration::ZERO;
    let mut results = Vec::new();
    for _ in 0..RUNS {
        let t0 = Instant::now();
        let s = d.settle(tab, SettleOpts::default()).await.expect("settle");
        let took = t0.elapsed();
        worst = worst.max(took);
        results.push((took, s.settled, s.changed));
    }
    println!("row03 {kind} settle(): worst {worst:?}; runs {results:?}");
    let ctx = rig.ctx(Class::Read);
    let mut w_worst = Duration::ZERO;
    let mut w_results = Vec::new();
    for _ in 0..RUNS {
        let t0 = Instant::now();
        let r = d.act(tab, "w(3s)", &ctx).await.expect("w(3s)");
        let took = t0.elapsed();
        w_worst = w_worst.max(took);
        assert_eq!(r.steps.len(), 1, "{r:?}");
        assert_eq!(r.steps[0].outcome, StepOutcome::Ok, "{r:?}");
        w_results.push((took, r.steps[0].settled));
    }
    println!("row03 {kind} w(3s): worst {w_worst:?}; runs {w_results:?}");
    println!("row03 {kind} SUMMARY settle {worst:?} w {w_worst:?} (limit {limit:?})");
    assert!(worst <= limit, "{kind}: settle() {worst:?} > {limit:?}");
    assert!(w_worst <= limit, "{kind}: w(3s) {w_worst:?} > {limit:?}");
}

/// open(), 5 runs, each in a new tab closed after: the total, and from the main-frame commit
/// (Page.frameNavigated as the event stream saw it) to the return. D-626: the 3 s cap of
/// B-4 #8 applies to what follows the commit (paint wait, settle, based walk, reply), so the
/// commit-to-return part is at most 3 s + 40 ms, and the total at most the time to the
/// commit + 3 s + 40 ms. The navigation itself keeps its own cap.
async fn cap_open(kind: &str) {
    let rig = rig().await;
    let d = &rig.d;
    let url = rig.with_p2(&format!("/cap?k={kind}"));
    let limit = SETTLE_CAP + SLACK;
    let commits = commit_watch(d);
    let mut o_worst = Duration::ZERO;
    let mut o_after_worst = Duration::ZERO;
    let mut o_results = Vec::new();
    for _ in 0..RUNS {
        commits.lock().unwrap().clear();
        let t0 = Instant::now();
        let tab = d.open(&url).await.expect("open");
        let took = t0.elapsed();
        let back = Instant::now();
        let commit = commits
            .lock()
            .unwrap()
            .iter()
            .find(|(_, u)| u == &url)
            .map(|(at, _)| *at);
        let after = commit.map(|c| back.saturating_duration_since(c));
        o_worst = o_worst.max(took);
        if let Some(a) = after {
            o_after_worst = o_after_worst.max(a);
        }
        o_results.push((took, after));
        d.close(tab).await.expect("close");
        let commit = commit.unwrap_or_else(|| panic!("{kind}: no main-frame commit seen"));
        let to_commit = commit.saturating_duration_since(t0);
        // Tester round 2, D-626 check: only the navigation (createTarget, the install ack,
        // Page.navigate, the commit) is outside the 3 s; on loopback it is short, so no
        // settle work hides before the commit.
        assert!(
            to_commit <= NAV_SANITY,
            "{kind}: open() took {to_commit:?} to the commit > {NAV_SANITY:?}"
        );
        let after = back.saturating_duration_since(commit);
        assert!(
            after <= limit,
            "{kind}: open() commit-to-return {after:?} > {limit:?}"
        );
        assert!(
            took <= to_commit + limit,
            "{kind}: open() total {took:?} > commit at {to_commit:?} + {limit:?}"
        );
    }
    println!(
        "row03 {kind} open(): worst total {o_worst:?}, worst commit-to-return {o_after_worst:?}; runs (total, commit-to-return) {o_results:?}"
    );
    println!(
        "row03 {kind} SUMMARY open {o_worst:?} open-after-commit {o_after_worst:?} (limit {limit:?})"
    );
    assert!(
        o_after_worst <= limit,
        "{kind}: open() commit-to-return {o_after_worst:?} > {limit:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row03a_settle_w_endless_mutations() {
    cap_settle_and_w("mut").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row03b_settle_w_never_ending_fetch() {
    cap_settle_and_w("fetch").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row03c_settle_w_css_animation_loop() {
    cap_settle_and_w("anim").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row03d_settle_w_event_source() {
    cap_settle_and_w("sse").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row03e_settle_w_mix_of_all_four() {
    cap_settle_and_w("mix").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row03f_open_endless_mutations() {
    cap_open("mut").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row03g_open_never_ending_fetch() {
    cap_open("fetch").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row03h_open_css_animation_loop() {
    cap_open("anim").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row03i_open_event_source() {
    cap_open("sse").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row03j_open_mix_of_all_four() {
    cap_open("mix").await;
}

// ---------------------------------------------------------------------------------------
// Row 4: a request started after settle began holds it; settled is false at the cap.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row04_a_request_started_meanwhile_holds_settle() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/quiet")).await.expect("open");
    let s = d.settle(tab, SettleOpts::default()).await.expect("settle");
    assert!(s.settled, "the quiet page settles: {s:?}");

    // (a) fetch 1200 ms, started 100 ms after settle begins.
    page_js(
        d,
        tab,
        "document.getElementById('o').textContent = 'none'; setTimeout(() => \
         fetch('/api/slow?ms=1200').then((r) => r.text()).then((t) => { \
         document.getElementById('o').textContent = 'got ' + t; }), 100); 1",
    )
    .await;
    let t0 = Instant::now();
    let s = d.settle(tab, SettleOpts::default()).await.expect("settle");
    let took = t0.elapsed();
    let o = page_js(d, tab, "document.getElementById('o').textContent").await;
    println!("row04 (a) fetch 1200 ms: settle {took:?} {s:?}; #o {o}");
    assert!(s.settled, "(a) {s:?}");
    assert!(took >= Duration::from_millis(1200), "(a) held: {took:?}");
    assert!(took <= SETTLE_CAP + SLACK, "(a) {took:?}");
    assert_eq!(
        o,
        json!("got slow"),
        "(a) the response landed before settle returned"
    );

    // (b) XHR 1000 ms, started 50 ms after settle begins (inside its 150 ms quiet window).
    page_js(
        d,
        tab,
        "document.getElementById('o').textContent = 'none'; setTimeout(() => { \
         const x = new XMLHttpRequest(); x.open('GET', '/api/slow?ms=1000'); \
         x.onload = () => { document.getElementById('o').textContent = 'xhr ' + x.responseText; }; \
         x.send(); }, 50); 1",
    )
    .await;
    let t0 = Instant::now();
    let s = d.settle(tab, SettleOpts::default()).await.expect("settle");
    let took = t0.elapsed();
    let o = page_js(d, tab, "document.getElementById('o').textContent").await;
    println!("row04 (b) XHR 1000 ms: settle {took:?} {s:?}; #o {o}");
    assert!(s.settled, "(b) {s:?}");
    assert!(took >= Duration::from_millis(1000), "(b) held: {took:?}");
    assert_eq!(o, json!("xhr slow"), "(b)");

    // (c) fetch 5000 ms, started 100 ms after settle begins: the cap, settled false.
    page_js(
        d,
        tab,
        "setTimeout(() => fetch('/api/slow?ms=5000').catch(() => {}), 100); 1",
    )
    .await;
    let t0 = Instant::now();
    let s = d.settle(tab, SettleOpts::default()).await.expect("settle");
    let took = t0.elapsed();
    println!("row04 (c) fetch 5000 ms: settle {took:?} {s:?}");
    assert!(!s.settled, "(c) settled is false at the cap: {s:?}");
    assert!(
        took >= SETTLE_CAP - Duration::from_millis(100),
        "(c) {took:?}"
    );
    assert!(took <= SETTLE_CAP + SLACK, "(c) {took:?}");

    // (d) a never-answered request started 100 ms after settle begins: the same.
    page_js(
        d,
        tab,
        &format!(
            "setTimeout(() => fetch('http://127.0.0.1:{}/hang?d').catch(() => {{}}), 100); 1",
            rig.port2
        ),
    )
    .await;
    let t0 = Instant::now();
    let s = d.settle(tab, SettleOpts::default()).await.expect("settle");
    let took = t0.elapsed();
    println!("row04 (d) never answered: settle {took:?} {s:?}");
    assert!(!s.settled, "(d) {s:?}");
    assert!(took <= SETTLE_CAP + SLACK, "(d) {took:?}");
}

// ---------------------------------------------------------------------------------------
// Row 5: slow submit.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row05_slow_submit_is_not_no_visible_change() {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    for (q, via) in [("k=slow", "XHR"), ("k=slow&via=fetch", "fetch")] {
        let tab = d.open(&rig.url(&format!("/save?{q}"))).await.expect("open");
        let ix = d.index(tab).await.expect("index");
        let save = by_name(&ix, "Save note").elem();
        let t0 = Instant::now();
        let r = d.act(tab, &format!("c{save}"), &ctx).await.expect("act");
        let took = t0.elapsed();
        println!(
            "row05 900 ms {via}: act {took:?}: {:?} diff {:?}",
            r.steps, r.diff
        );
        let c = &r.steps[0].consequences;
        assert!(
            !c.contains(&Consequence::NoVisibleChange),
            "{via}: {:?}",
            r.steps
        );
        assert!(
            r.diff.is_some() || !c.is_empty(),
            "{via}: a consequence or a diff"
        );
        assert!(
            r.diff
                .as_deref()
                .is_some_and(|d| d.contains(&format!("Saved by {via}"))),
            "{via}: the effect is in the diff: {:?}",
            r.diff
        );
        d.close(tab).await.expect("close");
    }
    for (q, via) in [("k=hang", "XHR"), ("k=hang&via=fetch", "fetch")] {
        let tab = d.open(&rig.url(&format!("/save?{q}"))).await.expect("open");
        let ix = d.index(tab).await.expect("index");
        let save = by_name(&ix, "Save note").elem();
        let t0 = Instant::now();
        let r = d.act(tab, &format!("c{save}"), &ctx).await.expect("act");
        let took = t0.elapsed();
        println!(
            "row05 never-answering {via}: act {took:?}: {:?} diff {:?}",
            r.steps, r.diff
        );
        assert!(
            !r.steps[0]
                .consequences
                .contains(&Consequence::NoVisibleChange),
            "{via}: {:?}",
            r.steps
        );
        assert_eq!(r.steps[0].settled, Some(false), "{via}");
        assert!(
            took <= ACT_WINDOW_MAX,
            "{via}: {took:?} > {ACT_WINDOW_MAX:?}"
        );
        d.close(tab).await.expect("close");
    }
}

// ---------------------------------------------------------------------------------------
// Row 6: titles.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row06a_a_title_change_shows_in_tabs() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/title")).await.expect("open");
    let before = d.tabs().await.expect("tabs");
    assert_eq!(before.iter().find(|t| t.0 == tab).unwrap().2, "Before");
    page_js(d, tab, "document.title = 'After a change'; 1").await;
    let after = d.tabs().await.expect("tabs");
    println!("row06a tabs(): {after:?}");
    assert_eq!(
        after.iter().find(|t| t.0 == tab).unwrap().2,
        "After a change"
    );
    // A second change, then a SPA route change (same document).
    page_js(
        d,
        tab,
        "history.pushState({}, '', '/title-route'); document.title = 'Route title'; 1",
    )
    .await;
    let again = d.tabs().await.expect("tabs");
    println!("row06a tabs() after a route change: {again:?}");
    let row = again.iter().find(|t| t.0 == tab).unwrap();
    assert_eq!(row.2, "Route title");
    assert_eq!(row.1, rig.url("/title-route"));
}

/// The title the page sets from a typed `@vault(pw)` value plus harness syntax.
async fn echo_title(rig: &Rig) -> TabId {
    let d = &rig.d;
    let tab = d.open(&rig.url("/title")).await.expect("open");
    let ix = d.index(tab).await.expect("index");
    let e = by_name(&ix, "Echo").elem();
    let r = d
        .act(tab, &format!("t{e}=@vault(pw)"), &rig.ctx(Class::Fill))
        .await
        .expect("act");
    assert!(
        !has_canary(&format!("{r:?}")),
        "the act result holds the canary"
    );
    assert_eq!(r.steps[0].outcome, StepOutcome::Ok, "{r:?}");
    let echoed = page_js(
        d,
        tab,
        &format!(
            "document.title.toLowerCase() === '{} #12 [x](#3)'.toLowerCase()",
            canary()
        ),
    )
    .await;
    assert_eq!(
        echoed,
        json!(true),
        "the page echoed the typed value into its title"
    );
    tab
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row06b_a_title_with_the_canary_is_scrubbed() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = echo_title(&rig).await;
    let raw = d.tabs().await.expect("tabs");
    let raw_leak = raw.iter().any(|t| has_canary(&format!("{t:?}")));
    println!("row06b note: tabs() before install_global holds the canary: {raw_leak}");
    // The MCP server installs the vault process-wide at start.
    rig.vault.install_global();
    let tabs = d.tabs().await.expect("tabs");
    let leak = tabs.iter().any(|t| has_canary(&format!("{t:?}")));
    assert!(!leak, "tabs() holds the canary after install_global");
    let frames = d.frames(tab).await.expect("frames");
    assert!(!has_canary(&format!("{frames:?}")));
    let title = &tabs.iter().find(|t| t.0 == tab).unwrap().2;
    println!("row06b title in tabs(): {title:?}");
    assert!(
        title.contains("@vault(pw)"),
        "shown as the literal: {title:?}"
    );
    let ledger = format!("{:?}", rig.store.ledger());
    assert!(!has_canary(&ledger), "the ledger holds the canary");
    let in_run = files_containing(&rig.run_dir(), &canary());
    assert!(
        in_run.is_empty(),
        "run dir files hold the canary: {in_run:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row06c_a_title_with_harness_syntax_is_escaped() {
    let rig = rig().await;
    let d = &rig.d;
    rig.vault.install_global();
    let tab = echo_title(&rig).await;
    let tabs = d.tabs().await.expect("tabs");
    let title = tabs.iter().find(|t| t.0 == tab).unwrap().2.clone();
    assert!(!has_canary(&title), "the title holds the canary");
    println!("row06c title in tabs(): {title:?}");
    // The same page string as an OpenedTab title (T-21) is `＃12 (x)(＃3)`.
    let c: Vec<char> = title.chars().collect();
    for (i, ch) in c.iter().enumerate() {
        assert!(
            !matches!(ch, '[' | ']' | '`'),
            "raw link syntax in {title:?}"
        );
        if *ch == '#' {
            let n = c.get(i + 1).copied();
            assert!(
                !n.is_some_and(|x| x.is_ascii_digit()),
                "a harness ref (#12, #3) survives in {title:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// Row 7: close().

/// An event as the test saw it: when it arrived, its session, its method.
type Seen = (Instant, Option<String>, String);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row07_close_leaves_nothing_behind() {
    let rig = rig().await;
    let d = &rig.d;
    let a = d.open(&rig.with_p2("/t4")).await.expect("open a");
    let f = t4_frames(&rig, a).await;
    let b = d.open(&rig.url("/quiet")).await.expect("open b");
    let (target, sessions) = d
        .with_registry(|r| {
            Some((
                r.target_id(a)?.to_string(),
                vec![
                    r.session(a)?.to_string(),
                    r.frame_session(a, f.site.frame)?.to_string(),
                ],
            ))
        })
        .expect("tab a");
    assert_ne!(sessions[0], sessions[1], "a has an OOPIF session");
    let ix_a = d.index(a).await.expect("index a");
    let main_after = by_name(&ix_a, "Main after").elem();

    // Every event from here on, with the time it arrived.
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    {
        let mut rx = d.cdp().events();
        let out = seen.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => out.lock().unwrap().push((
                        Instant::now(),
                        ev.session.clone(),
                        ev.method.clone(),
                    )),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                }
            }
        });
    }

    let t0 = Instant::now();
    d.close(a).await.expect("close");
    let closed_at = Instant::now();
    println!("row07 close took {:?}", t0.elapsed());

    let tabs = d.tabs().await.expect("tabs");
    assert!(tabs.iter().all(|t| t.0 != a), "{tabs:?}");
    assert!(tabs.iter().any(|t| t.0 == b));
    assert!(matches!(d.frames(a).await, Err(DriverError::TabGone(t)) if t == a));
    assert!(matches!(d.index(a).await, Err(DriverError::TabGone(t)) if t == a));
    let ctx = rig.ctx(Class::Submit);
    let acted = d.act(a, &format!("c{main_after}"), &ctx).await;
    println!("row07 act on the closed tab: {acted:?}");
    assert!(
        matches!(acted, Err(DriverError::TabGone(t)) if t == a),
        "act: TabGone"
    );
    let w = d.act(a, "w(1s)", &rig.ctx(Class::Read)).await;
    assert!(
        matches!(w, Err(DriverError::TabGone(_))),
        "w(): TabGone: {w:?}"
    );
    assert!(matches!(
        d.settle(a, SettleOpts::default()).await,
        Err(DriverError::TabGone(_))
    ));
    assert!(matches!(d.close(a).await, Err(DriverError::TabGone(_))));
    d.with_registry(|r| {
        for s in &sessions {
            assert!(!r.tracks(s), "session still tracked");
            assert_eq!(r.tab_by_session(s), None);
        }
        assert_eq!(r.tab_by_target(&target), None);
        assert!(!r.has_tab(a));
    });
    let targets = d
        .cdp()
        .call(None, "Target.getTargets", json!({}))
        .await
        .expect("targets");
    assert!(
        !targets.to_string().contains(&target),
        "the browser closed the target"
    );

    // The other tab still works: index, act, settle, w().
    let ix_b = d.index(b).await.expect("b index");
    let btn = by_name(&ix_b, "Quiet button").elem();
    let r = d.act(b, &format!("c{btn}"), &ctx).await.expect("b act");
    assert_eq!(r.steps[0].outcome, StepOutcome::Ok, "{r:?}");
    let s = d.settle(b, SettleOpts::default()).await.expect("b settle");
    assert!(s.settled, "{s:?}");
    page_js(d, b, "document.title = 'B after close'; 1").await;
    let tabs = d.tabs().await.expect("tabs");
    assert_eq!(tabs.iter().find(|t| t.0 == b).unwrap().2, "B after close");
    // b makes a request: its Network events arrive on its own session (a condition, capped).
    let b_sess = d.with_registry(|r| r.session(b).map(String::from)).unwrap();
    page_js(d, b, "fetch('/api/slow?ms=10').catch(() => {}); 1").await;
    let t_wait = Instant::now();
    while !seen.lock().unwrap().iter().any(|(at, s, m)| {
        *at > closed_at && s.as_deref() == Some(b_sess.as_str()) && m.starts_with("Network.")
    }) {
        assert!(t_wait.elapsed() < CAP, "no Network event for b within 30 s");
        // Polling the test's own event log (the condition is above, capped at 30 s).
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // No event arrived for a's sessions after close() returned; b's did.
    let seen = seen.lock().unwrap().clone();
    let late: Vec<&Seen> = seen
        .iter()
        .filter(|(at, s, _)| *at > closed_at && s.as_ref().is_some_and(|s| sessions.contains(s)))
        .collect();
    let b_events = seen
        .iter()
        .filter(|(at, s, _)| *at > closed_at && s.as_deref() == Some(b_sess.as_str()))
        .count();
    println!(
        "row07 events after close: {} total, {} for b, {} for a's sessions",
        seen.iter().filter(|(at, _, _)| *at > closed_at).count(),
        b_events,
        late.len()
    );
    assert!(
        late.is_empty(),
        "events routed for the closed tab: {late:?}"
    );
    assert!(b_events > 0, "b's events still flow");
}

// ---------------------------------------------------------------------------------------
// Row 8: a password field in a same-origin frame.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row08_password_in_a_frame_is_only_empty_or_filled() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/pw-host")).await.expect("open");
    let pf = rig.url("/pw-frame");
    let frame = d
        .wait_registry(CAP, |r| {
            r.frames(tab)?
                .into_iter()
                .find(|f| f.url == pf)
                .map(|f| f.frame)
        })
        .await
        .expect("the frame");
    let ix = d.index(tab).await.expect("index");
    let dump = serde_json::to_string(&ix).unwrap();
    assert!(
        !dump.contains(PRESET_PW),
        "a preset password value is in index()"
    );
    let secret = by_name(&ix, "Secret");
    let preset = by_name(&ix, "Preset");
    let hinted = by_name(&ix, "Hinted");
    println!(
        "row08 before: Secret {:?} Preset {:?} Hinted {:?}",
        secret.state.value, preset.state.value, hinted.state.value
    );
    assert_eq!(secret.frame, frame);
    assert_eq!(secret.state.value, Some(FieldValue::Empty));
    // A preset value with no evidence the page can show without reading it (no placeholder,
    // not required, caret at 0) is reported empty by design (injected.js filledNoRead, H-2).
    assert!(matches!(
        preset.state.value,
        Some(FieldValue::Empty) | Some(FieldValue::Filled)
    ));
    assert_eq!(
        hinted.state.value,
        Some(FieldValue::Filled),
        "placeholder not shown"
    );

    let el = secret.elem();
    let r = d
        .act(tab, &format!("t{el}=@vault(pw)"), &rig.ctx(Class::Pay))
        .await
        .expect("act");
    let shown = format!("{r:?}");
    assert!(!has_canary(&shown), "the act result holds the canary");
    assert_eq!(r.steps[0].outcome, StepOutcome::Ok, "{shown}");
    let typed = d
        .cdp()
        .call(
            d.with_registry(|r| r.frame_session(tab, frame).map(String::from))
                .as_deref(),
            "Runtime.evaluate",
            json!({
                "expression": "document.getElementById('pw').value.length",
                "contextId": d.with_registry(|r| r.default_context(tab, frame).map(|c| c.id)),
                "returnByValue": true
            }),
        )
        .await
        .expect("evaluate");
    assert_eq!(
        typed.pointer("/result/value").and_then(Value::as_u64),
        Some(canary().len() as u64),
        "the value landed in the field"
    );
    let ix = d.index(tab).await.expect("index");
    let dump = serde_json::to_string(&ix).unwrap();
    assert!(!has_canary(&dump), "the typed password value is in index()");
    assert!(!has_canary(&format!("{ix:?}")));
    assert!(!dump.contains(PRESET_PW));
    let secret = by_name(&ix, "Secret");
    println!("row08 after: Secret {:?}", secret.state.value);
    assert_eq!(secret.state.value, Some(FieldValue::Filled));
    for row in ix
        .iter()
        .filter(|r| r.state.input_type.as_deref() == Some("password"))
    {
        assert!(
            matches!(
                row.state.value,
                Some(FieldValue::Empty) | Some(FieldValue::Filled)
            ),
            "a password row shows only empty or filled: {:?}",
            row.state.value
        );
    }
    let sk = skeleton(&ix, &rig.url("/pw-host"), "PW host");
    assert!(!has_canary(&sk) && !sk.contains(PRESET_PW));
    let ledger = format!("{:?}", rig.store.ledger());
    assert!(!has_canary(&ledger), "the ledger holds the canary");
    let in_run = files_containing(&rig.run_dir(), &canary());
    assert!(
        in_run.is_empty(),
        "run dir files hold the canary: {in_run:?}"
    );
}
// ---------------------------------------------------------------------------------------
// Round 2: the review-T-22 round 1 fixes (D-625, the act window cap, the row scrub in a
// frame, D-628 opaque and challenge frames at call time).

/// `js` in the page's own world of `frame` (a debug path: the frame's own state).
async fn frame_js(d: &Driver, tab: TabId, frame: u32, js: &str) -> Value {
    let ctx = d
        .wait_registry(CAP, |r| r.default_context(tab, frame).cloned())
        .await
        .expect("the frame's main world");
    let v = d
        .cdp()
        .call(
            Some(&ctx.session),
            "Runtime.evaluate",
            json!({"expression": js, "contextId": ctx.id, "returnByValue": true}),
        )
        .await
        .expect("Runtime.evaluate in the frame");
    v.pointer("/result/value").cloned().unwrap_or(Value::Null)
}

/// `decl` called in the `sb` world of `frame` (a debug path into the runtime's own state).
async fn sb_probe(d: &Driver, tab: TabId, frame: u32, decl: &str) -> Value {
    let ctx = d
        .wait_registry(CAP, |r| r.sb_context(tab, frame).cloned())
        .await
        .expect("the frame's sb world");
    let v = d
        .cdp()
        .call(
            Some(&ctx.session),
            "Runtime.callFunctionOn",
            json!({"functionDeclaration": decl, "executionContextId": ctx.id,
                   "returnByValue": true}),
        )
        .await
        .expect("probe");
    v.pointer("/result/value").cloned().unwrap_or(Value::Null)
}

/// Whether the frame's runtime ever ran a based walk: `index()` answers `no base` until the
/// first `walk({base})` (D-405 (2)), and this probe issues no ids.
const WALKED_PROBE: &str = "function () { const sb = globalThis.__sb; \
    if (!sb) return 'no runtime'; const r = sb.index(); \
    return r && r.why ? r.why : 'walked'; }";

/// Wait (a condition, capped) until the page world's `expr` is true.
async fn until_page(d: &Driver, tab: TabId, expr: &str, cap: Duration) -> bool {
    let t0 = Instant::now();
    loop {
        if page_js(d, tab, expr).await == json!(true) {
            return true;
        }
        if t0.elapsed() >= cap {
            return false;
        }
        // Polling the page's own flag; the condition is above, capped.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// (a) review blocking 1: `w(10s)` on a 5 s request and `settle` with cap_ms 6000 on a 4 s
/// request, each started 100 ms in. Neither may say settled while its request is in flight:
/// a settled answer needs the request's response to have landed (the page's flag).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row13a_long_waits_never_report_settled_while_a_request_is_in_flight() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/quiet")).await.expect("open");
    assert!(d.settle(tab, SettleOpts::default()).await.unwrap().settled);
    let ctx = rig.ctx(Class::Read);
    for run in 0..2 {
        // w(10s), fetch 5000 ms.
        page_js(
            d,
            tab,
            "window.__d = false; setTimeout(() => fetch('/api/slow?ms=5000').then((r) => r.text())\
             .then(() => { window.__d = true; }), 100); 1",
        )
        .await;
        let t0 = Instant::now();
        let r = d.act(tab, "w(10s)", &ctx).await.expect("w(10s)");
        let took = t0.elapsed();
        let done = page_js(d, tab, "window.__d").await;
        let settled = r.steps[0].settled;
        println!(
            "row13a run {run} w(10s) on 5 s: {took:?} settled {settled:?} response landed {done}"
        );
        assert_eq!(r.steps[0].outcome, StepOutcome::Ok, "{r:?}");
        assert!(
            settled != Some(true) || done == json!(true),
            "w(10s) said settled at {took:?} while the 5 s request was in flight"
        );
        assert!(took <= Duration::from_millis(10_340), "{took:?}");
        assert!(
            until_page(d, tab, "window.__d", CAP).await,
            "the 5 s request ended"
        );

        // settle cap 6000, fetch 4000 ms.
        page_js(
            d,
            tab,
            "window.__d = false; setTimeout(() => fetch('/api/slow?ms=4000').then((r) => r.text())\
             .then(() => { window.__d = true; }), 100); 1",
        )
        .await;
        let t0 = Instant::now();
        let s = d
            .settle(
                tab,
                SettleOpts {
                    cap_ms: 6000,
                    ..SettleOpts::default()
                },
            )
            .await
            .expect("settle 6000");
        let took = t0.elapsed();
        let done = page_js(d, tab, "window.__d").await;
        println!("row13a run {run} settle(cap 6000) on 4 s: {took:?} {s:?} response landed {done}");
        assert!(
            !s.settled || done == json!(true),
            "settle(cap 6000) said settled at {took:?} while the 4 s request was in flight"
        );
        assert!(took <= Duration::from_millis(6040), "{took:?}");
        assert!(
            until_page(d, tab, "window.__d", CAP).await,
            "the 4 s request ended"
        );
    }
}

/// (b) review blocking 2: the act window, its last call and that call's answer included,
/// ends by input + 3 s + 40 ms, also when the document changes inside the window. The input
/// instant is the click handler's `console.log` as the event stream saw it (later than the
/// input, so the measured window is a little shorter than the real one; the act's own start,
/// before the gate read and actionability, is reported too).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row13b1_the_act_window_ends_by_input_plus_3_s_with_a_document_change() {
    act_window(&[
        ("net", 1000),
        ("net", 2000),
        ("net", 2800),
        ("mut", 300),
        ("mut", 450),
    ])
    .await;
}

/// (b) as above on a page whose main thread is blocked 200 ms of every 220 ms, so in-page
/// calls answer late: without a document change, and with one at 1 s (to a busy page too).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row13b2_the_act_window_ends_by_input_plus_3_s_on_a_busy_page() {
    act_window(&[("busy", 99_999), ("busy", 1000)]).await;
}

/// Each `(mode, at)` of `/win` twice, each in a new tab: act `c` on its button, timed from
/// the click's console line to act()'s return.
async fn act_window(variants: &[(&str, u32)]) {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let limit = SETTLE_CAP + SLACK;
    let mut worst = Duration::ZERO;
    let mut worst_total = Duration::ZERO;
    for &(mode, at) in variants {
        for run in 0..2 {
            let url = rig.with_p2(&format!("/win?m={mode}&at={at}"));
            let tab = d.open(&url).await.expect("open");
            let ix = d.index(tab).await.expect("index");
            let go = by_name(&ix, "Go window").elem();
            let mut rx = d.cdp().events();
            let click = tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(ev)
                            if ev.method == "Runtime.consoleAPICalled"
                                && ev.params.pointer("/args/0/value").and_then(Value::as_str)
                                    == Some("t22-click") =>
                        {
                            return Some(Instant::now())
                        }
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            continue
                        }
                        Err(_) => return None,
                    }
                }
            });
            let t0 = Instant::now();
            let r = d.act(tab, &format!("c{go}"), &ctx).await.expect("act");
            let end = Instant::now();
            let total = end - t0;
            let at_click = tokio::time::timeout(Duration::from_secs(1), click)
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .expect("the click's console line was seen");
            let window = end.saturating_duration_since(at_click);
            let navigated = r.steps[0]
                .consequences
                .iter()
                .any(|c| matches!(c, Consequence::Navigated { .. }));
            println!(
                "row13b {mode} nav at {at} ms run {run}: act {total:?}, from the click {window:?}, \
                 settled {:?}, navigated {navigated}, outcome {:?}",
                r.steps[0].settled, r.steps[0].outcome
            );
            worst = worst.max(window);
            worst_total = worst_total.max(total);
            assert_eq!(r.steps[0].outcome, StepOutcome::Ok, "{r:?}");
            assert_eq!(r.steps[0].settled, Some(false), "never settles: {r:?}");
            assert!(
                window <= limit,
                "{mode} nav at {at}: the act ended {window:?} after the click > {limit:?}"
            );
            d.close(tab).await.expect("close");
        }
    }
    println!("row13b SUMMARY worst from the click {worst:?}, worst act total {worst_total:?}");
}

/// (c) review blocking 3, rule 2: the canary in role, name (text, aria-label, title), value,
/// heading, nearby text and an option, inside a same-origin iframe: 0 hits in index() (serde,
/// Debug, skeleton), the literal in its place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row13c_canary_in_a_same_origin_frame_never_reaches_index() {
    let rig = rig().await;
    rig.vault.install_global();
    let d = &rig.d;
    let tab = d.open(&rig.url("/scrub-host")).await.expect("open");
    let fu = rig.url("/scrub-frame");
    let frame = d
        .wait_registry(CAP, |r| {
            r.frames(tab)?
                .into_iter()
                .find(|f| f.url == fu && !f.opaque)
                .map(|f| f.frame)
        })
        .await
        .expect("the same-origin frame");
    let c = serde_json::to_string(&canary()).unwrap();
    let js = format!(
        "(() => {{ const c = {c}; const $ = (i) => document.getElementById(i);\
         $('cap').setAttribute('role', c);\
         $('t').textContent = 'text ' + c;\
         $('al').setAttribute('aria-label', 'label ' + c);\
         $('ti').setAttribute('title', 'title ' + c);\
         $('v').value = c;\
         $('h').textContent = 'Heading ' + c;\
         $('near').textContent = 'near ' + c;\
         $('o2').textContent = 'opt ' + c;\
         return 1; }})()"
    );
    assert_eq!(frame_js(d, tab, frame, &js).await, json!(1));
    let ix = d.index(tab).await.expect("index");
    let wire = serde_json::to_string(&ix).unwrap();
    let dbg = format!("{ix:?}");
    let sk = skeleton(&ix, &rig.url("/scrub-host"), "Scrub host");
    let hits = [&wire, &dbg, &sk].iter().filter(|s| has_canary(s)).count();
    let mine: Vec<&IndexRow> = ix.iter().filter(|r| r.frame == frame).collect();
    let lit = |s: &str| s.contains("@vault(pw)");
    let count = |f: &dyn Fn(&IndexRow) -> bool| mine.iter().filter(|r| f(r)).count();
    let roles = count(&|r| lit(&r.role));
    let names = count(&|r| lit(&r.name));
    let values = count(&|r| matches!(&r.state.value, Some(FieldValue::Text(t)) if lit(t)));
    let headings = count(&|r| r.heading.as_deref().is_some_and(lit));
    let nearby = count(&|r| r.nearby.as_deref().is_some_and(lit));
    let options = count(&|r| r.state.options.iter().any(|o| lit(o)));
    println!(
        "row13c frame {frame}: {} rows in the frame; canary hits {hits}; literal in role {roles}, \
         name {names}, value {values}, heading {headings}, nearby {nearby}, options {options}",
        mine.len()
    );
    assert_eq!(
        hits, 0,
        "a vault value is in index() (serde, Debug or skeleton)"
    );
    assert!(!mine.is_empty(), "the frame was read");
    assert!(roles >= 1, "the canary reached a role (and was scrubbed)");
    assert!(names >= 3, "text, aria-label and title names: {names}");
    assert!(values >= 1, "a field value");
    assert!(headings >= 1, "a heading");
    let ledger = format!("{:?}", rig.store.ledger());
    assert!(!has_canary(&ledger), "the ledger holds the canary");
}

/// (d) D-628, review should-fix 2: act on `#fN.x` in a cross-origin frame (cross-port in
/// process, cross-site OOPIF) fails before any read, and nothing is clicked or typed: the
/// frame's own counter, its field and the server's /clicked log stay empty. The ids exist:
/// the test walks the frame itself through the debug path first, so a driver that did not
/// refuse would find and click them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row13d_act_in_a_cross_origin_frame_fails_and_clicks_nothing() {
    let rig = rig().await;
    let d = &rig.d;
    CLICKS.store(0, Ordering::SeqCst);
    let tab = d.open(&rig.with_p2("/xo-host")).await.expect("open");
    let p2 = rig.port2;
    let want = [
        ("port", format!("http://127.0.0.1:{p2}/xo-frame?m=port")),
        ("site", format!("http://localhost:{p2}/xo-frame?m=site")),
    ];
    let ix = d.index(tab).await.expect("index");
    for (what, u) in &want {
        let frame = d
            .wait_registry(CAP, |r| {
                let f = r.frames(tab)?.into_iter().find(|f| &f.url == u)?;
                r.sb_context(tab, f.frame)?;
                Some(f.frame)
            })
            .await
            .unwrap_or_else(|| panic!("{what}: the frame with an sb world"));
        assert!(
            d.frames(tab)
                .await
                .unwrap()
                .iter()
                .any(|f| f.frame == frame && f.opaque),
            "{what}: opaque"
        );
        assert!(!ix.iter().any(|r| r.frame == frame), "{what}: never read");
        let ids = sb_probe(
            d,
            tab,
            frame,
            "function () { const sb = globalThis.__sb; if (!sb) return 'no runtime';\
             sb.walk({base: 1}); const rows = sb.index();\
             if (!Array.isArray(rows)) return JSON.stringify(rows);\
             const f = (n) => { const r = rows.find((x) => x.name === n); return r ? r.id : null; };\
             return [f('Frame button'), f('Frame field')]; }",
        )
        .await;
        println!("row13d {what}: frame {frame}, ids from the debug walk {ids}");
        let b = ids
            .pointer("/0")
            .and_then(Value::as_u64)
            .expect("button id");
        let i = ids.pointer("/1").and_then(Value::as_u64).expect("field id");
        for prog in [
            format!("c#f{frame}.{b}"),
            format!("t#f{frame}.{i}=zq-typed"),
        ] {
            let r = d
                .act(tab, &prog, &rig.ctx(Class::Submit))
                .await
                .expect("act");
            println!("row13d {what} {prog}: {:?}", r.steps);
            match &r.steps[0].outcome {
                StepOutcome::Failed { why } => assert!(
                    why.contains("OPAQUE") || why.contains("cross-origin"),
                    "{what}: {why}"
                ),
                o => panic!("{what} {prog}: the step ran: {o:?}"),
            }
        }
        let state = frame_js(
            d,
            tab,
            frame,
            "[window.__n || 0, document.getElementById('i').value]",
        )
        .await;
        println!("row13d {what}: in-frame counter and field {state}");
        assert_eq!(
            state,
            json!([0, ""]),
            "{what}: something was clicked or typed"
        );
    }
    // A cross-origin frame nobody walked: an act into it must not read inside (the driver's
    // based walk would show in the frame's runtime), and must click nothing.
    let nu = format!("http://127.0.0.1:{p2}/xo-frame?m=never");
    let never = d
        .wait_registry(CAP, |r| {
            let f = r.frames(tab)?.into_iter().find(|f| f.url == nu)?;
            r.sb_context(tab, f.frame)?;
            Some(f.frame)
        })
        .await
        .expect("the never-walked frame");
    assert_eq!(
        sb_probe(d, tab, never, WALKED_PROBE).await,
        json!("no base")
    );
    for prog in [format!("c#f{never}.1"), format!("t#f{never}.2=zq-typed")] {
        let r = d
            .act(tab, &prog, &rig.ctx(Class::Submit))
            .await
            .expect("act");
        println!("row13d never {prog}: {:?}", r.steps);
        assert!(
            matches!(&r.steps[0].outcome, StepOutcome::Failed { .. }),
            "never {prog}: the step ran: {r:?}"
        );
    }
    let walked = sb_probe(d, tab, never, WALKED_PROBE).await;
    let state = frame_js(
        d,
        tab,
        never,
        "[window.__n || 0, document.getElementById('i').value]",
    )
    .await;
    println!("row13d never: frame {never} runtime {walked}, counter and field {state}");
    assert_eq!(
        walked,
        json!("no base"),
        "the driver read inside frame {never}"
    );
    assert_eq!(
        state,
        json!([0, ""]),
        "never: something was clicked or typed"
    );
    // A click would have sent /clicked at once; one page round trip first, so a request
    // already sent has reached the server.
    page_js(d, tab, "1").await;
    assert_eq!(CLICKS.load(Ordering::SeqCst), 0, "the server saw a click");
    // Control: the counter works (a click through the frame's own world).
    let port_frame = d
        .with_registry(|r| {
            r.frames(tab)?
                .into_iter()
                .find(|f| f.url == want[0].1)
                .map(|f| f.frame)
        })
        .unwrap();
    frame_js(
        d,
        tab,
        port_frame,
        "document.getElementById('b').click(); 1",
    )
    .await;
    let t0 = Instant::now();
    while CLICKS.load(Ordering::SeqCst) == 0 && t0.elapsed() < CAP {
        // Waiting for the test server's own log (the condition), capped.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        CLICKS.load(Ordering::SeqCst),
        1,
        "control: the log counts a click"
    );
}

/// (e) D-5, D-628: a challenge widget frame (by its title) holding a same-origin frame that
/// holds a third one added later is never walked: the runtime in each of the three frames
/// still answers `no base` after index() (twice), settle(), frames(), tabs() and acts into
/// two of them (each a CHALLENGE stop). A normal same-origin frame beside it is walked (the
/// probe tells the two apart).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row13e_nested_frames_in_a_challenge_widget_are_never_walked() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/chw-host")).await.expect("open");
    let p = rig.port;
    let urls = ["/chw1", "/chw2", "/chw3"].map(|u| format!("http://127.0.0.1:{p}{u}"));
    let normal = format!("http://127.0.0.1:{p}/chw-normal");
    let ix1 = d.index(tab).await.expect("index");
    let frames: Vec<u32> = d
        .wait_registry(CAP, |r| {
            let all = r.frames(tab)?;
            let mut out = Vec::new();
            for u in &urls {
                let f = all.iter().find(|f| &f.url == u)?;
                r.sb_context(tab, f.frame)?;
                out.push(f.frame);
            }
            Some(out)
        })
        .await
        .expect("three nested frames with sb worlds");
    let nf = d
        .wait_registry(CAP, |r| {
            r.frames(tab)?
                .into_iter()
                .find(|f| f.url == normal)
                .map(|f| f.frame)
        })
        .await
        .expect("the normal frame");
    let frames = &frames;
    let check = |stage: &'static str| async move {
        for f in frames {
            let w = sb_probe(d, tab, *f, WALKED_PROBE).await;
            assert_eq!(w, json!("no base"), "{stage}: frame {f} was walked");
        }
    };
    check("after the first index()").await;
    let ix = d.index(tab).await.expect("index");
    for ix in [&ix1, &ix] {
        let widget = ix
            .iter()
            .find(|r| r.flags.challenge)
            .unwrap_or_else(|| panic!("a CHALLENGE row: {ix:#?}"));
        assert_eq!(widget.child_frame, Some(frames[0]), "{widget:?}");
        by_name(ix, "Outside button");
        for f in frames {
            assert!(!ix.iter().any(|r| r.frame == *f), "frame {f} was read");
        }
        for n in ["Inside one", "Inside two", "Inside three"] {
            assert!(!ix.iter().any(|r| r.name == n), "{n} is in index()");
        }
    }
    by_name(&ix, "Normal frame button");
    assert_eq!(
        sb_probe(d, tab, nf, WALKED_PROBE).await,
        json!("walked"),
        "control: the normal frame is walked"
    );
    check("after the second index()").await;
    let s = d.settle(tab, SettleOpts::default()).await.expect("settle");
    println!("row13e settle {s:?}");
    d.frames(tab).await.expect("frames");
    d.tabs().await.expect("tabs");
    check("after settle, frames and tabs").await;
    for f in [frames[1], frames[2]] {
        let r = d
            .act(tab, &format!("c#f{f}.1"), &rig.ctx(Class::Submit))
            .await
            .expect("act");
        println!(
            "row13e act into frame {f}: stopped {:?} steps {:?}",
            r.stopped, r.steps
        );
        assert_eq!(
            r.stopped.as_ref().map(|s| s.class),
            Some(Class::Challenge),
            "{r:?}"
        );
    }
    check("after the acts").await;
    println!("row13e frames {frames:?} never walked; normal frame {nf} walked");
}

/// (f) D-625: a request the tab started before settle began is not waited for, but while it
/// is in flight at the end, settle() and w(d) say settled false (for any cap); once it ends,
/// settle says true. Another tab's request in flight does not make this tab unsettled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row13f_a_request_started_before_settle_makes_it_unsettled() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/quiet")).await.expect("open");
    assert!(d.settle(tab, SettleOpts::default()).await.unwrap().settled);
    let mut rx = d.cdp().events();
    page_js(
        d,
        tab,
        "window.__d = false; fetch('/api/slow?ms=4000').then((r) => r.text())\
         .then(() => { window.__d = true; }); 1",
    )
    .await;
    // The driver has seen the request once the test has (the same event stream).
    tokio::time::timeout(CAP, async {
        loop {
            match rx.recv().await {
                Ok(ev)
                    if ev.method == "Network.requestWillBeSent"
                        && ev
                            .params
                            .pointer("/request/url")
                            .and_then(Value::as_str)
                            .is_some_and(|u| u.contains("ms=4000")) =>
                {
                    return
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => panic!("event stream closed"),
            }
        }
    })
    .await
    .expect("the request was announced");
    let t0 = Instant::now();
    let s = d.settle(tab, SettleOpts::default()).await.expect("settle");
    let took = t0.elapsed();
    let done = page_js(d, tab, "window.__d").await;
    println!("row13f settle with a pre-settle request: {took:?} {s:?} landed {done}");
    assert!(
        !s.settled,
        "settled while the earlier request is in flight: {s:?}"
    );
    assert!(
        took < Duration::from_millis(1500),
        "not waited for: {took:?}"
    );
    let t0 = Instant::now();
    let s = d
        .settle(
            tab,
            SettleOpts {
                cap_ms: 6000,
                ..SettleOpts::default()
            },
        )
        .await
        .expect("settle 6000");
    let done = page_js(d, tab, "window.__d").await;
    println!(
        "row13f settle(cap 6000): {:?} {s:?} landed {done}",
        t0.elapsed()
    );
    assert!(
        !s.settled || done == json!(true),
        "settle(cap 6000) settled while the earlier request is in flight"
    );
    let t0 = Instant::now();
    let r = d.act(tab, "w(2s)", &rig.ctx(Class::Read)).await.expect("w");
    let done = page_js(d, tab, "window.__d").await;
    println!(
        "row13f w(2s): {:?} {:?} landed {done}",
        t0.elapsed(),
        r.steps
    );
    assert!(
        r.steps[0].settled != Some(true) || done == json!(true),
        "w(2s) settled while the request is in flight"
    );
    assert!(
        until_page(d, tab, "window.__d", CAP).await,
        "the request ended"
    );
    let s = d.settle(tab, SettleOpts::default()).await.expect("settle");
    println!("row13f settle after it ended: {s:?}");
    assert!(s.settled, "{s:?}");
    // Another tab's request in flight does not count for this tab.
    let b = d.open(&rig.url("/quiet")).await.expect("open b");
    page_js(
        d,
        b,
        &format!(
            "fetch('http://127.0.0.1:{}/hang?other').catch(() => {{}}); 1",
            rig.port2
        ),
    )
    .await;
    let s = d.settle(tab, SettleOpts::default()).await.expect("settle");
    println!("row13f settle with another tab's request in flight: {s:?}");
    assert!(s.settled, "another tab's request: {s:?}");
}
