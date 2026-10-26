//! T-22 against the real D-34 build (chromium-1248), through the public API: docs/05
//! acceptance 4 (registry: a popup with its opener, a same-origin iframe with its parent, a
//! cross-origin iframe as one opaque row never read inside) and acceptance 5 (settle(): an
//! SPA that renders after a slow fetch and 400 ms after load gives a non-empty skeleton from
//! `Driver::index`; the 3 s cap is never exceeded on endless mutations or never-ending
//! requests), plus B-601 (fresh titles in `tabs()`), `close()`, `getFrameTree` seeding of a
//! target attached while running (T-18c), and B-614 (a slow XHR submit is not "no visible
//! change").
//!
//! Pages come from tiny HTTP servers on 127.0.0.1 (a second port for cross-origin, and the
//! host name `localhost` for a cross-site, out-of-process frame); no network. One browser at
//! a time (`SERIAL`); run with `--test-threads=1` on a loaded machine.

mod common;

use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::EntryKind;
use harness_core::{Class, Harness, RunId, ToolCtx, ToolRegistry, Vault};
use sb_driver::{
    Consequence, Driver, DriverError, FrameInfo, LaunchEnv, LaunchOpts, Profile, SettleOpts, TabId,
};
use sb_sheet::{IndexRow, PageMeta, SkeletonOpts};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CAP: Duration = Duration::from_secs(30);
/// Acceptance 5: the settle cap plus this is the most a settle may take.
const CAP_SLACK: Duration = Duration::from_millis(40);
/// How long the slow API answers after.
const SLOW_API_MS: u64 = 1200;
/// How long the save API answers after (longer than the 500 ms act window).
const SAVE_API_MS: u64 = 900;

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "canary-t22-not-a-secret".to_owned())
}

const STYLE: &str = "<style>body{margin:0;font:16px sans-serif} button{display:block;\
                     margin:8px;width:160px;height:32px} iframe{width:300px;height:120px}</style>";

/// What the server does for a path.
enum Reply {
    Page(String),
    /// Answer `body` after `ms` (a slow API).
    Slow(u64, String),
    /// Never answer (a request that never ends).
    Hang,
}

fn route(path: &str, query: &str) -> Option<Reply> {
    let html = |b: String| Reply::Page(format!("<!doctype html><html><body>{b}</body></html>"));
    let p2 = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("p2="))
        .unwrap_or("0");
    Some(match path {
        "/a4" => html(format!(
            "<title>A4</title>{STYLE}<h1>Registry</h1>\
             <button id=pop onclick=\"window.open('/popup', 'pop', 'width=400,height=300')\">\
               Open popup</button>\
             <iframe id=same title=Same src=/inner></iframe>\
             <iframe id=port title=Port src=\"http://127.0.0.1:{p2}/inner\"></iframe>\
             <iframe id=site title=Site src=\"http://localhost:{p2}/inner\"></iframe>\
             <button id=after>After frames</button>"
        )),
        "/inner" => html(format!(
            "<title>Inner</title>{STYLE}<h2>Inner</h2><button>Inner button</button>\
             <iframe id=leaf title=Leaf src=/leaf></iframe>"
        )),
        "/leaf" => html("<title>Leaf</title><a href=/x>Leaf link</a>".into()),
        "/popup" => html("<title>Popup</title><h1>popup</h1>".into()),
        "/titled" => html("<title>Before</title><h1>Titled</h1>".into()),
        // An SPA: its shell renders 400 ms after load (so the in-page content wait ends
        // there), its data only when a fetch the server delays answers.
        "/spa" => html(
            "<title>SPA</title><div id=app></div><script>\
             const app = document.getElementById('app');\
             let data = null;\
             addEventListener('load', () => setTimeout(() => {\
               if (data === null) app.innerHTML = '<h1>Loading</h1>';\
             }, 400));\
             fetch('/api/slow').then((r) => r.text()).then((t) => {\
               data = t;\
               app.innerHTML = '<h1>Loaded ' + t + '</h1>' +\
                 '<label>Email <input id=email></label><button>Continue</button>';\
             });</script>"
                .into(),
        ),
        // Renders 400 ms after load, no request.
        "/spa400" => html(
            "<title>SPA400</title><div id=app></div><script>\
             addEventListener('load', () => setTimeout(() => {\
               document.getElementById('app').innerHTML = '<h1>Rendered</h1><button>Go</button>';\
             }, 400));</script>"
                .into(),
        ),
        "/api/slow" => Reply::Slow(SLOW_API_MS, "data".into()),
        "/endless" => html(
            "<title>Endless</title><h1>Clock</h1><p id=t>0</p><script>\
             setInterval(() => { document.getElementById('t').textContent = String(Date.now()); }, 10);\
             </script>"
                .into(),
        ),
        // A new request every 50 ms (so one starts inside every quiet window) that the
        // server never answers.
        "/hang-page" => html(
            "<title>Hang</title><h1>Waiting</h1><script>\
             setInterval(() => { fetch('/hang?' + Math.random()).catch(() => {}); }, 50);\
             </script>"
                .into(),
        ),
        "/hang" => Reply::Hang,
        "/save" | "/save-hang" => {
            let api = if path == "/save" { "/api/save" } else { "/hang" };
            html(format!(
                "<title>Save</title>{STYLE}<form id=f onsubmit=\"event.preventDefault();\
                   const x = new XMLHttpRequest(); x.open('POST', '{api}');\
                   x.onload = () => {{ document.getElementById('status').innerHTML = '<h2>Saved</h2>'; }};\
                   x.send('n=1');\">\
                 <label>Note <input id=n></label><button id=save type=submit>Save</button></form>\
                 <div id=status></div>"
            ))
        }
        "/api/save" => Reply::Slow(SAVE_API_MS, "ok".into()),
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
                let (status, ct, body) = match route(path, query) {
                    Some(Reply::Page(b)) => ("200 OK", "text/html", b),
                    Some(Reply::Slow(ms, b)) => {
                        // The fixture server's latency (a slow API), not a driver wait.
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                        ("200 OK", "text/plain", b)
                    }
                    Some(Reply::Hang) => {
                        // A request that never ends: the connection stays open, unanswered,
                        // until the test's runtime goes.
                        std::future::pending::<()>().await;
                        return;
                    }
                    None => ("404 Not Found", "text/plain", String::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n",
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
    harness: Harness,
}

impl Rig {
    fn url(&self, p: &str) -> String {
        format!("http://127.0.0.1:{}{p}", self.port)
    }

    fn a4(&self) -> String {
        self.url(&format!("/a4?p2={}", self.port2))
    }

    fn ctx(&self, class: Class) -> ToolCtx {
        ToolCtx::new(
            RunId("accept-t22".into()),
            self.home.path().join("runs").join("accept-t22"),
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
    let harness = Harness {
        store: Arc::new(MemStore::new()),
        vault: Arc::new(Vault::in_memory([(
            "pw".to_string(),
            canary(),
            EntryKind::Secret,
        )])),
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
        harness,
    }
}

/// `js` in the page's own world (frame 0).
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

/// The a4 page's frames by URL, once all five are registered (a condition, capped).
struct A4Frames {
    same: FrameInfo,
    leaf: FrameInfo,
    port: FrameInfo,
    port_leaf: FrameInfo,
    site: FrameInfo,
}

async fn a4_frames(rig: &Rig, tab: TabId) -> A4Frames {
    let (p1, p2) = (rig.port, rig.port2);
    let want = [
        format!("http://127.0.0.1:{p1}/inner"),
        format!("http://127.0.0.1:{p1}/leaf"),
        format!("http://127.0.0.1:{p2}/inner"),
        format!("http://127.0.0.1:{p2}/leaf"),
        format!("http://localhost:{p2}/inner"),
    ];
    let got = rig
        .d
        .wait_registry(CAP, |r| {
            let frames = r.frames(tab)?;
            let by = |u: &str| frames.iter().find(|f| f.url == u).cloned();
            Some(A4Frames {
                same: by(&want[0])?,
                leaf: by(&want[1])?,
                port: by(&want[2])?,
                port_leaf: by(&want[3])?,
                site: by(&want[4])?,
            })
        })
        .await;
    match got {
        Some(f) => f,
        None => panic!(
            "all five frames registered within 30 s: {:?}",
            rig.d.frames(tab).await
        ),
    }
}

/// The parents and opacity acceptance 4 asks for.
fn check_a4_frames(f: &A4Frames) {
    assert_eq!(f.same.parent, Some(0), "same-origin iframe under the page");
    assert!(!f.same.opaque, "same-origin is readable");
    assert_eq!(f.leaf.parent, Some(f.same.frame), "nested under its parent");
    assert!(!f.leaf.opaque);
    assert_eq!(f.port.parent, Some(0));
    assert!(f.port.opaque, "cross-origin (another port) is opaque");
    assert_eq!(f.port_leaf.parent, Some(f.port.frame));
    assert!(f.port_leaf.opaque, "inside an opaque frame");
    assert_eq!(f.site.parent, Some(0));
    assert!(f.site.opaque, "cross-site is opaque");
}

fn by_name<'a>(ix: &'a [IndexRow], name: &str) -> &'a IndexRow {
    ix.iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("a row named {name:?} in {ix:#?}"))
}

/// The merged index around the a4 frames: each readable frame's rows follow its iframe row
/// with refs `#fN.id`; each opaque frame is one OPAQUE row with its child_frame, and nothing
/// from inside it.
fn check_a4_index(ix: &[IndexRow], f: &A4Frames) {
    let pos = |pred: &dyn Fn(&IndexRow) -> bool| ix.iter().position(pred);
    let same_row = pos(&|r| r.child_frame == Some(f.same.frame)).expect("same-origin iframe row");
    assert_eq!(ix[same_row].frame, 0);
    assert!(!ix[same_row].flags.opaque);
    let inner = by_name(ix, "Inner button");
    assert_eq!(inner.frame, f.same.frame, "{inner:?}");
    assert!(inner
        .elem()
        .to_string()
        .starts_with(&format!("#f{}.", f.same.frame)));
    let inner_pos = pos(&|r| r.name == "Inner button").unwrap();
    assert!(
        inner_pos > same_row,
        "the frame's rows follow its iframe row"
    );
    let leaf_row = pos(&|r| r.child_frame == Some(f.leaf.frame)).expect("nested iframe row");
    assert_eq!(ix[leaf_row].frame, f.same.frame);
    let link = pos(&|r| r.name == "Leaf link").expect("nested frame read");
    assert_eq!(ix[link].frame, f.leaf.frame);
    assert!(link > leaf_row);
    for opaque in [f.port.frame, f.site.frame] {
        let rows: Vec<&IndexRow> = ix
            .iter()
            .filter(|r| r.child_frame == Some(opaque))
            .collect();
        assert_eq!(rows.len(), 1, "one row for opaque frame {opaque}: {ix:#?}");
        assert_eq!(rows[0].frame, 0);
        assert!(rows[0].flags.opaque, "{:?}", rows[0]);
        assert_eq!(rows[0].role, "iframe");
    }
    for never in [f.port.frame, f.port_leaf.frame, f.site.frame] {
        assert!(
            !ix.iter().any(|r| r.frame == never),
            "frame {never} is never read inside"
        );
    }
    assert_eq!(
        ix.iter().filter(|r| r.name == "Inner button").count(),
        1,
        "the cross-origin copies of /inner are not read"
    );
    let after = pos(&|r| r.name == "After frames").expect("main rows after the frames");
    assert!(after > link, "document order");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acceptance_4_registry_has_popup_frames_and_opaque_rows() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.a4()).await.expect("open a4");
    let f = a4_frames(&rig, tab).await;
    check_a4_frames(&f);
    let site_session = d.with_registry(|r| r.frame_session(tab, f.site.frame).map(String::from));
    let tab_session = d.with_registry(|r| r.session(tab).map(String::from));
    assert!(
        site_session.is_some() && site_session != tab_session,
        "the cross-site frame is out of process: {site_session:?}"
    );
    let ix = d.index(tab).await.expect("index");
    check_a4_index(&ix, &f);

    // The popup, opened by a click (a user gesture), with its opener.
    let pop = by_name(&ix, "Open popup").elem();
    let r = d
        .act(tab, &format!("c{pop}"), &rig.ctx(Class::Submit))
        .await
        .expect("act");
    let opened = r.steps[0]
        .consequences
        .iter()
        .find_map(|c| match c {
            Consequence::OpenedTab { tab, title } => Some((*tab, title.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("opened tab in {:?}", r.steps));
    assert_eq!(opened.1, "Popup");
    let tabs = d.tabs().await.expect("tabs");
    let row = tabs
        .iter()
        .find(|t| t.0 == opened.0)
        .expect("the popup is a tab");
    assert_eq!(row.1, rig.url("/popup"));
    assert_eq!(row.2, "Popup");
    assert_eq!(d.with_registry(|r| r.opener(opened.0)), Some(tab));
    assert_eq!(d.with_registry(|r| r.opener(tab)), None);
    let pf = d.frames(opened.0).await.expect("popup frames");
    assert_eq!(pf.len(), 1);
    assert_eq!(pf[0].parent, None);
    eprintln!(
        "acceptance 4: tab {tab:?} frames {:?}; popup {:?} opened by {tab:?}",
        d.frames(tab).await.unwrap(),
        opened.0
    );
}

/// T-18c: a target that was already running (frames loaded, an OOPIF in place) when the
/// driver attached is seeded from `Page.getFrameTree`, and its OOPIF from its own: the same
/// frames, parents and opacity, and an index read through the replayed contexts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_target_attached_while_running_is_seeded_from_its_frame_tree() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.a4()).await.expect("open a4");
    a4_frames(&rig, tab).await;
    let (target, session) = d
        .with_registry(|r| Some((r.target_id(tab)?.to_string(), r.session(tab)?.to_string())))
        .expect("tab");
    d.cdp()
        .call(
            None,
            "Target.detachFromTarget",
            json!({ "sessionId": session }),
        )
        .await
        .expect("detach");
    d.wait_registry(CAP, |r| (!r.has_tab(tab)).then_some(()))
        .await
        .expect("the detached tab leaves the registry");
    // Attach again: the page is running, with every frame already there.
    d.cdp()
        .call(
            None,
            "Target.attachToTarget",
            json!({ "targetId": target, "flatten": true }),
        )
        .await
        .expect("attach");
    let again = d
        .wait_registry(CAP, |r| r.tab_by_target(&target))
        .await
        .expect("a new tab for the running target");
    assert_ne!(again, tab, "tab numbers are never reused");
    let f = a4_frames(&rig, again).await;
    check_a4_frames(&f);
    assert_eq!(
        d.with_registry(|r| r.generation(again, f.same.frame)),
        Some(0),
        "seeding is not a navigation"
    );
    let ix = d.index(again).await.expect("index of the seeded tab");
    check_a4_index(&ix, &f);
}

/// B-601: Chromium sends no targetInfoChanged for a title change; `tabs()` still shows it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_title_change_shows_in_tabs() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/titled")).await.expect("open");
    let before = d.tabs().await.expect("tabs");
    assert_eq!(before.iter().find(|t| t.0 == tab).unwrap().2, "Before");
    page_js(d, tab, "document.title = 'After change'; 1").await;
    let after = d.tabs().await.expect("tabs");
    assert_eq!(
        after.iter().find(|t| t.0 == tab).unwrap().2,
        "After change",
        "{after:?}"
    );
}

/// I-9 `close`: the tab, its sessions (its OOPIF's too) and frames are gone; the browser
/// target is gone; another tab still works; a second close is TabGone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_removes_the_tab_its_sessions_and_frames() {
    let rig = rig().await;
    let d = &rig.d;
    let a = d.open(&rig.a4()).await.expect("open a4");
    let f = a4_frames(&rig, a).await;
    let b = d.open(&rig.url("/titled")).await.expect("open titled");
    let (target, page_session, oopif_session) = d
        .with_registry(|r| {
            Some((
                r.target_id(a)?.to_string(),
                r.session(a)?.to_string(),
                r.frame_session(a, f.site.frame)?.to_string(),
            ))
        })
        .expect("tab a");
    assert_ne!(page_session, oopif_session);
    d.close(a).await.expect("close");
    let tabs = d.tabs().await.expect("tabs");
    assert!(tabs.iter().all(|t| t.0 != a), "{tabs:?}");
    assert!(tabs.iter().any(|t| t.0 == b));
    assert!(matches!(d.frames(a).await, Err(DriverError::TabGone(t)) if t == a));
    assert!(matches!(d.index(a).await, Err(DriverError::TabGone(_))));
    assert!(matches!(
        d.settle(a, SettleOpts::default()).await,
        Err(DriverError::TabGone(_))
    ));
    d.with_registry(|r| {
        assert!(!r.tracks(&page_session), "page session gone");
        assert!(!r.tracks(&oopif_session), "OOPIF session gone");
        assert_eq!(r.tab_by_target(&target), None);
    });
    let targets = d
        .cdp()
        .call(None, "Target.getTargets", json!({}))
        .await
        .expect("targets");
    assert!(
        !targets.to_string().contains(&target),
        "the browser closed it"
    );
    assert!(matches!(d.close(a).await, Err(DriverError::TabGone(_))));
    let ix = d.index(b).await.expect("the other tab still works");
    by_name(&ix, "Titled");
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

/// Acceptance 5: `open` settles on the network too, so an SPA that renders after a slow
/// fetch (and 400 ms more), or 400 ms after load, has a non-empty skeleton from
/// `Driver::index` right after `open`; `settle` afterwards is quick and settled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acceptance_5_spa_skeleton_is_not_empty_after_open() {
    let rig = rig().await;
    let d = &rig.d;
    let t0 = Instant::now();
    let tab = d.open(&rig.url("/spa")).await.expect("open spa");
    let took = t0.elapsed();
    let ix = d.index(tab).await.expect("index");
    let sk = skeleton(&ix, &rig.url("/spa"), "SPA");
    eprintln!("acceptance 5: /spa open {took:?}; skeleton:\n{sk}");
    assert!(sk.contains("Loaded data"), "{sk}");
    assert!(sk.contains("1 button, 1 field"), "{sk}");
    assert!(
        !ix.iter().any(|r| r.name == "Loading"),
        "the shell was replaced"
    );
    by_name(&ix, "Continue");
    assert!(
        took >= Duration::from_millis(SLOW_API_MS),
        "open waited for the fetch: {took:?}"
    );
    let t1 = Instant::now();
    let s = d.settle(tab, SettleOpts::default()).await.expect("settle");
    eprintln!(
        "acceptance 5: /spa settle after open {:?} {s:?}",
        t1.elapsed()
    );
    assert!(s.settled, "{s:?}");

    let tab = d.open(&rig.url("/spa400")).await.expect("open spa400");
    let ix = d.index(tab).await.expect("index");
    let sk = skeleton(&ix, &rig.url("/spa400"), "SPA400");
    eprintln!("acceptance 5: /spa400 skeleton:\n{sk}");
    assert!(sk.contains("Rendered"), "{sk}");
}

/// Acceptance 5: the fixed 3 s cap is never exceeded, on a page whose DOM never stops
/// changing and on one that keeps starting requests that never end: settled is false and
/// the call takes at most 3 s + 40 ms, three times each.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acceptance_5_the_3_s_cap_is_never_exceeded() {
    let rig = rig().await;
    let d = &rig.d;
    let cap = Duration::from_millis(u64::from(SettleOpts::default().cap_ms));
    for page in ["/endless", "/hang-page"] {
        let tab = d.open(&rig.url(page)).await.expect("open");
        let mut worst = Duration::ZERO;
        for _ in 0..3 {
            let t0 = Instant::now();
            let s = d.settle(tab, SettleOpts::default()).await.expect("settle");
            let took = t0.elapsed();
            eprintln!("acceptance 5: {page} settle {took:?} {s:?}");
            assert!(!s.settled, "{page}: {s:?}");
            assert!(took <= cap + CAP_SLACK, "{page}: {took:?}");
            worst = worst.max(took);
        }
        if page == "/endless" {
            let s = d.settle(tab, SettleOpts::default()).await.unwrap();
            assert!(s.changed > 10, "the mutations are counted: {s:?}");
        }
        eprintln!("acceptance 5: {page} worst {worst:?} (cap {cap:?} + {CAP_SLACK:?})");
    }
}

/// B-614, T-21b (4): a Submit whose XHR outlasts the 500 ms window reports what it did
/// (the diff), not "no visible change"; one whose request never ends never claims "no
/// visible change" either, and the window still ends near its 3 s cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_xhr_submit_is_not_no_visible_change() {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let tab = d.open(&rig.url("/save")).await.expect("open save");
    let ix = d.index(tab).await.expect("index");
    let save = by_name(&ix, "Save").elem();
    let t0 = Instant::now();
    let r = d.act(tab, &format!("c{save}"), &ctx).await.expect("act");
    eprintln!(
        "B-614: /save act {:?}: {:?} diff {:?}",
        t0.elapsed(),
        r.steps,
        r.diff
    );
    assert!(
        !r.steps[0]
            .consequences
            .contains(&Consequence::NoVisibleChange),
        "{:?}",
        r.steps
    );
    assert!(
        r.diff.as_deref().is_some_and(|d| d.contains("Saved")),
        "the XHR's effect is in the diff: {:?}",
        r.diff
    );
    assert_eq!(r.steps[0].settled, Some(true));

    let tab = d
        .open(&rig.url("/save-hang"))
        .await
        .expect("open save-hang");
    let ix = d.index(tab).await.expect("index");
    let save = by_name(&ix, "Save").elem();
    let t0 = Instant::now();
    let r = d.act(tab, &format!("c{save}"), &ctx).await.expect("act");
    let took = t0.elapsed();
    eprintln!("B-614: /save-hang act {took:?}: {:?}", r.steps);
    assert!(
        !r.steps[0]
            .consequences
            .contains(&Consequence::NoVisibleChange),
        "{:?}",
        r.steps
    );
    assert_eq!(r.steps[0].settled, Some(false));
    assert!(took < Duration::from_millis(4500), "{took:?}");
}
