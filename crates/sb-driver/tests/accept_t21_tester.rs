//! T-21 acceptance, tester round 1 (docs/05 acceptance 3, B-4 #7 #9, B-7, B-8, H-1.4, H-2).
//! Written by the tester, independent of the coder's `accept_t21.rs`: hostile page strings,
//! confirm and prompt, page-supplied download names that try to leave the files folder,
//! vault canary sweeps (result, errors, diff, ledger, run dir), upload escapes through `..`,
//! absolute paths and a junction, in-page counters for the gate, and a frame click on a
//! scrolled page with a tight target.
//!
//! Round 2 adds: a nested frame below the fold (row 10c), a typed value longer than every
//! cap echoed into headings, the title, the URL, a dialog and a new tab (row 15), input
//! that must not reach a focused descendant (row 16), a timer dialog between steps
//! (row 17), a non-2xx re-fetch and format characters in download names (row 18) and a
//! stale target after a navigation (row 19).
//!
//! Real Chromium (D-34), pages from a local HTTP server, one browser at a time. Run with
//! `--test-threads=1`. Set `SB_VAULT_CANARY` to a unique string; the tests never print it.

mod common;

use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::EntryKind;
use harness_core::{Class, Harness, RunId, ToolCtx, ToolRegistry, Vault};
use sb_driver::{
    ActResult, Consequence, Driver, LaunchEnv, LaunchOpts, Profile, StepOutcome, TabId,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CAP: Duration = Duration::from_secs(30);

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "canary-t21-tester-not-a-secret".to_owned())
}

fn pid() -> u32 {
    std::process::id()
}

/// Body of the download served at `path` (unique per process and path).
fn dl_body(path: &str) -> String {
    format!("T21 tester body {path} {}", pid())
}

/// The long vault value (key `long`): the canary, then enough to cross every cap (60, 120,
/// 150, 200). Never printed.
fn long_value() -> String {
    format!("{}-{}", canary(), "w".repeat(140))
}

/// Unicode format (Cf) characters a page could use to disguise a name.
fn is_cf(c: char) -> bool {
    matches!(c as u32,
        0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x180E | 0x200B..=0x200F
        | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB)
}

/// Headings the long-echo pages fill: the value after a pad of this many `p`s. 0 and 140
/// cross the index's 150 cut; 51, 52 and 53 leave 9, 8 and 7 value characters before the
/// diff's 60 cut.
const PADS: &str = "[0, 10, 51, 52, 53, 140]";

const HOSTILE: &str = "Two #12 [x](#3) ``` fence";

fn page(path: &str) -> Option<(String, String, Vec<u8>)> {
    let html = |b: String| {
        (
            "text/html".to_string(),
            String::new(),
            format!("<!doctype html><html><body>{b}</body></html>").into_bytes(),
        )
    };
    let attach = |disp: String| {
        Some((
            "text/plain".to_string(),
            format!("Content-Disposition: {disp}\r\n"),
            dl_body(path).into_bytes(),
        ))
    };
    let p = pid();
    let style = "<style>body{margin:0;font:16px sans-serif} a,button,input,select{display:block;\
                 margin:8px;width:200px;height:30px}</style>";
    let body = match path {
        "/links" => format!(
            "<title>Links</title>{style}\
             <a id=blank target=_blank href=/evil-title>Open</a>\
             <button id=wo onclick=\"window.open('/plain-two')\">Win</button>\
             <button id=noop>Nothing</button>\
             <a id=nav href=/welcome>Go</a>\
             <form id=f1><input aria-label=Search></form>"
        ),
        "/evil-title" => format!(
            "<title>{HOSTILE} {}</title><h1>evil</h1>",
            "y".repeat(300)
        ),
        "/plain-two" => "<title>Plain two</title><h1>two</h1>".to_string(),
        "/welcome" => "<title>Welcome</title><h1>Welcome, Lokesh</h1><p>hi</p>".to_string(),
        "/dialogs" => format!(
            "<title>Dialogs</title>{style}\
             <button id=al onclick=\"alert('Hi'); document.getElementById('a').textContent = 'continued';\">Alert</button>\
             <button id=later onclick=\"window.laterRan = (window.laterRan || 0) + 1\">Later</button>\
             <button id=cf onclick=\"document.getElementById('c').textContent = String(confirm('Delete it?'))\">Confirm</button>\
             <button id=pr onclick=\"document.getElementById('p').textContent = String(prompt('Your name?', 'def'))\">Prompt</button>\
             <button id=big onclick=\"alert('#12 [x](#3) ``` ' + 'z'.repeat(3000))\">Big</button>\
             <p id=a>waiting</p><p id=c>none</p><p id=p>none</p>"
        ),
        "/dls" => format!(
            "<title>Downloads</title>{style}\
             <a id=e1 href=/dl-evil1>e1</a>\
             <a id=e2 href=/dl-evil2>e2</a>\
             <a id=e3 href=/dl-evil3>e3</a>\
             <a id=con href=/dl-con>con</a>\
             <a id=ref href=/dl-ref>ref</a>\
             <a id=attr download=\"..\\..\\evila-{p}.txt\" href=/dl-attr>attr</a>\
             <a id=no href=/dl-no>no</a>"
        ),
        "/dl-evil1" => return attach(format!("attachment; filename=\"..\\..\\evil1-{p}.txt\"")),
        "/dl-evil2" => return attach(format!("attachment; filename*=UTF-8''..%2F..%2Fevil2-{p}.txt")),
        "/dl-evil3" => return attach(format!("attachment; filename*=UTF-8''..%5C..%5Cevil3-{p}.txt")),
        "/dl-con" => return attach("attachment; filename=\"CON.txt\"".to_string()),
        "/dl-ref" => {
            return attach(format!(
                "attachment; filename=\"[x](#12) ``` {}.txt\"",
                "n".repeat(200)
            ))
        }
        "/dl-attr" => {
            return Some((
                "text/plain".into(),
                String::new(),
                dl_body(path).into_bytes(),
            ))
        }
        "/dl-no" => return attach(format!("attachment; filename=\"refused-{p}.txt\"")),
        "/type" => format!(
            "<title>Type</title>{style}\
             <input id=plain aria-label=Plain>\
             <input id=vplain aria-label=Vplain>\
             <input id=pw type=password aria-label=Password>\
             <button id=echoal onclick=\"alert('You typed ' + document.getElementById('vplain').value)\">Echo</button>\
             <form id=gf action=/welcome method=get><input id=q name=q aria-label=Query></form>"
        ),
        "/gate" => format!(
            "<title>Gate</title>{style}<style>.g-recaptcha{{width:300px;height:78px;background:#eee}}</style>\
             <form id=reg onsubmit=\"event.preventDefault(); window.submits = (window.submits || 0) + 1;\">\
             <input id=user aria-label=User>\
             <button id=sub type=submit onclick=\"window.subclicks = (window.subclicks || 0) + 1\">Register</button></form>\
             <div style=\"height:2000px\"></div>\
             <div id=cap class=g-recaptcha data-sitekey=x \
               onclick=\"window.capclicks = (window.capclicks || 0) + 1\"></div>\
             <script>\
               window.mouse = 0; window.keys = 0;\
               for (const ev of ['mousedown', 'mouseup', 'click', 'pointerdown', 'pointerup']) {{\
                 document.addEventListener(ev, () => {{ window.mouse += 1; }}, true);\
               }}\
               document.addEventListener('keydown', () => {{ window.keys += 1; }}, true);\
             </script>"
        ),
        "/form" => format!(
            "<title>Form</title>{style}<style>#tall{{height:3000px}}</style>\
             <select id=plan aria-label=Plan><option value=b>Basic</option>\
               <option value=p>Plus</option></select>\
             <input type=checkbox id=agree><label for=agree>Agree</label>\
             <form id=sf onsubmit=\"event.preventDefault(); window.submits = (window.submits || 0) + 1;\">\
               <input id=k aria-label=Keys></form>\
             <div id=tall></div><button id=bottom>bottom</button>\
             <script>window.keys = 0; document.getElementById('k').addEventListener('keydown', \
               () => {{ window.keys += 1; }});</script>"
        ),
        "/up" => "<title>Up</title><input type=file id=f aria-label=CV>".to_string(),
        // Row 15: a typed value echoed into headings, the title and the URL (same document),
        // into a new tab's title, and into an alert.
        "/long" => format!(
            "<title>Long</title>{style}\
             <h1 id=h0>Hello</h1><h2 id=h1>a</h2><h2 id=h2>b</h2><h2 id=h3>c</h2>\
             <h2 id=h4>d</h2><h2 id=h5>e</h2>\
             <input id=e aria-label=Echo oninput=\"fill(this.value)\">\
             <button id=open onclick=\"window.open('/long-title')\">Open</button>\
             <button id=al onclick=\"alert('You typed: ' + document.getElementById('e').value)\">Alert</button>\
             <script>function fill(v) {{\
               {PADS}.forEach((p, i) => {{ document.getElementById('h' + i).textContent = 'p'.repeat(p) + v; }});\
               document.title = v;\
               history.replaceState(null, '', '/long?v=' + encodeURIComponent(v));\
             }}</script>"
        ),
        "/long-title" => "<title>T</title><h1>t</h1><script>document.title = 'x'.repeat(100) + \
             opener.document.getElementById('e').value;</script>"
            .to_string(),
        "/long-get" => format!(
            "<title>Long get</title>{style}\
             <form id=lg action=/long-land method=get><input id=q name=q aria-label=Query></form>"
        ),
        "/long-land" => format!(
            "<title>Land</title><h1 id=h0>x</h1><h2 id=h1>x</h2><h2 id=h2>x</h2><h2 id=h3>x</h2>\
             <h2 id=h4>x</h2><h2 id=h5>x</h2>\
             <script>const v = new URLSearchParams(location.search).get('q') || '';\
               {PADS}.forEach((p, i) => {{ document.getElementById('h' + i).textContent = 'p'.repeat(p) + v; }});\
               document.title = v;</script>"
        ),
        // Row 16: a modal dialog autofocuses its Pay button; a password field in a container.
        "/focus" => format!(
            "<title>Focus</title>{style}\
             <dialog id=dlg aria-label=\"Confirm payment\"><p>Pay 10 EUR?</p>\
               <button id=pay autofocus onclick=\"this.dataset.n = +(this.dataset.n || 0) + 1\">Pay now</button>\
             </dialog>\
             <fieldset id=box aria-label=Login><legend>Login</legend>\
               <input id=secret type=password aria-label=Secret></fieldset>\
             <input id=plain aria-label=Plain>\
             <script>window.keys = 0; window.secretInputs = 0;\
               document.addEventListener('keydown', (e) => {{ window.keys += 1; \
                 window.keyAt = e.target.id || e.target.localName; }}, true);\
               document.getElementById('secret').addEventListener('input', () => {{ window.secretInputs += 1; }});\
               document.getElementById('dlg').showModal();</script>"
        ),
        // Row 16: a div dialog (not focusable itself) whose Pay button has autofocus.
        "/focus2" => format!(
            "<title>Focus 2</title>{style}\
             <div id=dlg2 role=dialog aria-modal=true aria-label=\"Confirm payment\"><p>Pay 10 EUR?</p>\
               <button id=pay2 autofocus onclick=\"this.dataset.n = +(this.dataset.n || 0) + 1\">Pay now</button>\
             </div>\
             <div id=box2 role=group aria-label=Login>\
               <input id=secret2 type=password aria-label=Secret></div>\
             <input id=plain2 aria-label=Plain>\
             <script>window.keys = 0; window.secretInputs = 0;\
               document.addEventListener('keydown', (e) => {{ window.keys += 1; \
                 window.keyAt = e.target.id || e.target.localName; }}, true);\
               document.getElementById('secret2').addEventListener('input', () => {{ window.secretInputs += 1; }});</script>"
        ),
        // Row 17: dialogs from timers between steps.
        "/timer" => format!(
            "<title>Timer</title>{style}\
             <button id=arm onclick=\"setTimeout(() => alert('from a timer'), 900)\">Arm</button>\
             <button id=arm0 onclick=\"setTimeout(() => alert('right away'), 0)\">Arm now</button>\
             <button id=hid style=\"display:none\">Hidden</button>\
             <button id=cnt onclick=\"this.dataset.n = +(this.dataset.n || 0) + 1\">Count</button>\
             <input id=ti aria-label=Later>"
        ),
        // Row 18: re-fetches that answer an error status; names with format characters.
        "/dls2" => format!(
            "<title>Downloads 2</title>{style}\
             <a id=e500 href=/dl-e500>e500</a>\
             <a id=e404 href=/dl-e404>e404</a>\
             <a id=rlo href=/dl-rlo>rlo</a>\
             <a id=zw href=/dl-zw>zw</a>\
             <a id=attrc download=\"pay&#x202E;fdp&#x200B;-{p}.exe\" href=/dl-attrc>attrc</a>"
        ),
        "/dl-e500" => return attach(format!("attachment; filename=\"err500-{p}.txt\"")),
        "/dl-e404" => return attach(format!("attachment; filename=\"err404-{p}.txt\"")),
        "/dl-rlo" => {
            return attach(format!(
                "attachment; filename*=UTF-8''inv%E2%80%AEtxt-{p}.exe"
            ))
        }
        "/dl-zw" => {
            return attach(format!(
                "attachment; filename*=UTF-8''re%E2%80%8Bpo%EF%BB%BFrt%E2%81%A6-{p}.txt"
            ))
        }
        "/dl-attrc" => {
            return Some((
                "text/plain".into(),
                String::new(),
                dl_body(path).into_bytes(),
            ))
        }
        // Row 10c: an iframe inside an iframe, both below the fold.
        "/nest" => "<title>Nest</title><style>body{margin:0}</style>\
             <button id=decoy style=\"position:fixed;left:0;top:0;width:190px;height:140px\" \
               onclick=\"document.getElementById('r').textContent = 'miss'\">Decoy</button>\
             <div style=\"height:1300px\"></div>\
             <iframe id=outer src=/nest-outer style=\"position:absolute;left:200px;top:1300px;\
               width:320px;height:240px;border:4px solid #000;padding:6px\"></iframe>\
             <div style=\"height:3000px\"></div>\
             <p id=r style=\"position:fixed;right:0;top:0\">none</p>"
            .to_string(),
        "/nest-outer" => "<title>Outer</title><style>body{margin:0}</style>\
             <div style=\"height:700px\">outer</div>\
             <iframe id=inner src=/nest-inner style=\"position:absolute;left:30px;top:700px;\
               width:200px;height:150px;border:3px solid #333;padding:5px\"></iframe>\
             <div style=\"height:1500px\"></div>"
            .to_string(),
        "/nest-inner" => "<title>Inner</title><style>body{margin:0}</style>\
             <button id=b style=\"position:absolute;left:10px;top:10px;width:8px;height:8px;\
               padding:0;border:0\" onclick=\"top.document.getElementById('r').textContent = 'hit'; \
               this.dataset.n = +(this.dataset.n || 0) + 1\">I</button>"
            .to_string(),
        "/framed" | "/framed-in" => "<title>Framed</title><style>body{margin:0}</style>\
             <button id=decoy style=\"position:fixed;left:0;top:0;width:190px;height:140px\" \
               onclick=\"document.getElementById('r').textContent = 'miss'\">Decoy</button>\
             <div style=\"height:1300px\"></div>\
             <iframe id=fr style=\"position:absolute;left:200px;TOPpx;width:300px;\
               height:200px;border:5px solid #000;padding:7px\" srcdoc=\"<body style='margin:0'>\
               <button id=b style='position:absolute;left:10px;top:10px;width:8px;height:8px;padding:0;border:0' \
               onclick='parent.document.getElementById(&quot;r&quot;).textContent = &quot;hit&quot;'>\
               I</button></body>\"></iframe>\
             <div style=\"height:3000px\"></div>\
             <p id=r style=\"position:fixed;right:0;top:0\">none</p>"
            .replace("TOP", if path == "/framed" { "top:1300" } else { "top:150" }),
        _ => return None,
    };
    Some(html(body))
}

/// What an error answer carries (row 18): it must never be saved.
fn error_body() -> String {
    format!("T21 tester ERROR PAGE {}", pid())
}

/// Requests per path (query stripped).
static HITS: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);

fn hits(path: &str) -> usize {
    HITS.lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(path).copied())
        .unwrap_or(0)
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
                let full = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let path = full.split('?').next().unwrap_or("/").to_string();
                *HITS
                    .lock()
                    .unwrap()
                    .get_or_insert_with(HashMap::new)
                    .entry(path.clone())
                    .or_default() += 1;
                // Row 18: the second request to these links answers an error status.
                let again = hits(&path) > 1;
                let error = match path.as_str() {
                    "/dl-e500" if again => Some("500 Internal Server Error"),
                    "/dl-e404" if again => Some("404 Not Found"),
                    _ => None,
                };
                let (status, ct, extra, body) = match page(&path) {
                    _ if error.is_some() => (
                        error.unwrap_or_default(),
                        "text/plain".into(),
                        String::new(),
                        error_body().into_bytes(),
                    ),
                    Some((ct, ex, b)) => ("200 OK", ct, ex, b),
                    None => ("404 Not Found", "text/plain".into(), String::new(), vec![]),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\n{extra}\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    port
}

struct Rig {
    d: Driver,
    _serial: tokio::sync::MutexGuard<'static, ()>,
    port: u16,
    home: tempfile::TempDir,
    harness: Harness,
    store: Arc<MemStore>,
    vault: Arc<Vault>,
}

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static RUNS: AtomicUsize = AtomicUsize::new(0);

impl Rig {
    fn url(&self, p: &str) -> String {
        format!("http://127.0.0.1:{}{p}", self.port)
    }

    fn run_dir(&self) -> PathBuf {
        self.home.path().join("runs").join("accept-t21-tester")
    }

    fn files(&self) -> PathBuf {
        self.run_dir().join("files")
    }

    fn ctx(&self, class: Class) -> ToolCtx {
        ToolCtx::new(
            RunId("accept-t21-tester".into()),
            self.run_dir(),
            class,
            &self.harness,
            ToolRegistry::new(vec![]),
        )
    }
}

async fn rig() -> Rig {
    let serial = SERIAL.lock().await;
    RUNS.fetch_add(1, Ordering::SeqCst);
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
    let vault = Arc::new(Vault::in_memory([
        ("pw".to_string(), canary(), EntryKind::Secret),
        ("long".to_string(), long_value(), EntryKind::Secret),
    ]));
    let store = Arc::new(MemStore::new());
    let harness = Harness {
        store: store.clone(),
        vault: vault.clone(),
        asker: Arc::new(ScriptedAsker::new()),
        runs_root: home.path().join("runs"),
        run: None,
    };
    std::fs::create_dir_all(home.path().join("runs").join("accept-t21-tester")).unwrap();
    Rig {
        d,
        _serial: serial,
        port,
        home,
        harness,
        store,
        vault,
    }
}

async fn sb_eval(d: &Driver, tab: TabId, frame: u32, decl: &str, args: &[Value]) -> Value {
    let ctx = d
        .wait_registry(CAP, |r| r.sb_context(tab, frame).cloned())
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
    assert!(v.get("exceptionDetails").is_none(), "sb_eval threw");
    v.pointer("/result/value").cloned().unwrap_or(Value::Null)
}

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

async fn id_of(d: &Driver, tab: TabId, frame: u32, sel: &str) -> u64 {
    let v = sb_eval(
        d,
        tab,
        frame,
        "function (sel) { return globalThis.__sb.id(document.querySelector(sel)); }",
        &[json!(sel)],
    )
    .await;
    v.as_u64().unwrap_or_else(|| panic!("id of {sel}: {v}"))
}

async fn r(d: &Driver, tab: TabId, sel: &str) -> String {
    format!("#{}", id_of(d, tab, 0, sel).await)
}

async fn act(d: &Driver, tab: TabId, program: &str, ctx: &ToolCtx) -> ActResult {
    d.act(tab, program, ctx)
        .await
        .unwrap_or_else(|e| panic!("act {program}: {e}"))
}

fn ok_steps(r: &ActResult) -> bool {
    r.steps.iter().all(|s| s.outcome == StepOutcome::Ok)
}

/// No ref (`#12`, `#f2.3`), link, or fence syntax survives, and it is capped.
fn escaped_and_capped(s: &str) {
    let c: Vec<char> = s.chars().collect();
    for (i, ch) in c.iter().enumerate() {
        assert!(!matches!(ch, '[' | ']' | '`'), "raw syntax char in {s:?}");
        if *ch == '#' {
            let n = c.get(i + 1).copied();
            let nn = c.get(i + 2).copied();
            let refy = n.is_some_and(|x| x.is_ascii_digit())
                || (n == Some('f') && nn.is_some_and(|x| x.is_ascii_digit()));
            assert!(!refy, "a ref survives in {s:?}");
        }
    }
    assert!(c.len() <= 123, "not capped: {} chars", c.len());
}

fn files_with(dir: &Path, body: &[u8]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        let ft = e.file_type();
        if ft.as_ref().is_ok_and(|t| t.is_symlink()) {
            continue;
        }
        if p.is_dir() {
            out.extend(files_with(&p, body));
        } else if std::fs::read(&p).is_ok_and(|b| b == body) {
            out.push(p);
        }
    }
    out
}

/// Files under `dir` whose bytes contain `needle` (symlinks and junctions not followed).
fn files_containing(dir: &Path, needle: &[u8]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            // A junction reports as a dir on Windows; read_link tells it apart.
            if std::fs::read_link(&p).is_ok() {
                continue;
            }
            out.extend(files_containing(&p, needle));
        } else if let Ok(b) = std::fs::read(&p) {
            if b.windows(needle.len()).any(|w| w == needle) {
                out.push(p);
            }
        }
    }
    out
}

async fn download_end(
    events: &mut tokio::sync::broadcast::Receiver<Arc<sb_cdp::Event>>,
    guid: &str,
) -> String {
    let wait = async {
        loop {
            let e = match events.recv().await {
                Ok(e) => e,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("events: {e}"),
            };
            if e.method == "Browser.downloadProgress" && e.params["guid"] == guid {
                let state = e.params["state"].as_str().unwrap_or_default().to_string();
                if state != "inProgress" {
                    return state;
                }
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), wait)
        .await
        .expect("the browser reports how the download ended")
}

fn download_of(res: &ActResult) -> (String, String) {
    match res.steps[0]
        .consequences
        .iter()
        .find(|c| matches!(c, Consequence::Download { .. }))
    {
        Some(Consequence::Download { name, guid }) => (name.clone(), guid.clone()),
        _ => panic!("no download: {res:?}"),
    }
}

fn record(seen: &mut Vec<String>, res: &ActResult) {
    seen.push(format!("{res:?}"));
    seen.push(serde_json::to_string(&res.steps).unwrap());
    seen.push(serde_json::to_string(&res.stopped).unwrap());
    seen.push(res.diff.clone().unwrap_or_default());
}

fn user_downloads_has(name: &str) -> bool {
    ["USERPROFILE", "HOME"].iter().any(|root| {
        std::env::var_os(root)
            .map(|h| PathBuf::from(h).join("Downloads").join(name).exists())
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------------------
// Row 1 and row 11 (title): target=_blank gives OpenedTab tab 2 in the same result.
// ---------------------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row01_blank_link_is_opened_tab_2_in_the_same_result() {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let tab = d.open(&rig.url("/links")).await.expect("open");
    assert_eq!(tab, TabId(1));
    let blank = r(d, tab, "#blank").await;
    let res = act(d, tab, &format!("c{blank}"), &ctx).await;
    assert_eq!(res.steps.len(), 1, "{res:?}");
    assert!(ok_steps(&res), "{res:?}");
    let opened: Vec<(TabId, String)> = res.steps[0]
        .consequences
        .iter()
        .filter_map(|c| match c {
            Consequence::OpenedTab { tab, title } => Some((*tab, title.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(opened.len(), 1, "{res:?}");
    assert_eq!(opened[0].0, TabId(2), "opened tab 2: {res:?}");
    println!("row1 opened title: {:?}", opened[0].1);
    // Row 11: the page-chosen title is escaped and capped.
    assert!(opened[0].1.starts_with("Two "), "{:?}", opened[0].1);
    escaped_and_capped(&opened[0].1);
    assert!(opened[0].1.ends_with("..."), "capped with ...");
    assert_eq!(d.with_registry(|r| r.opener(TabId(2))), Some(tab));
    // window.open from a click handler: tab 3, same result.
    let wo = r(d, tab, "#wo").await;
    let res = act(d, tab, &format!("c{wo}"), &ctx).await;
    assert!(
        res.steps[0].consequences.contains(&Consequence::OpenedTab {
            tab: TabId(3),
            title: "Plain two".into()
        }),
        "{res:?}"
    );
}

// ---------------------------------------------------------------------------------------
// Row 2 and row 11 (dialog text): alert pauses, later steps do not run; confirm; prompt.
// ---------------------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row02_dialogs_pause_and_resume_alert_confirm_prompt() {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let tab = d.open(&rig.url("/dialogs")).await.expect("open");
    let al = r(d, tab, "#al").await;
    let later = r(d, tab, "#later").await;
    let cf = r(d, tab, "#cf").await;
    let pr = r(d, tab, "#pr").await;
    let big = r(d, tab, "#big").await;

    // alert: Dialog, paused, the later step never ran.
    let res = act(d, tab, &format!("c{al}; c{later}"), &ctx).await;
    assert_eq!(res.steps.len(), 1, "stopped at the dialog: {res:?}");
    assert_eq!(
        res.steps[0].consequences.last(),
        Some(&Consequence::Dialog { text: "Hi".into() }),
        "{res:?}"
    );
    assert_eq!(res.diff, None);
    // Still paused: another act answers with the dialog and runs nothing.
    let again = act(d, tab, &format!("c{later}"), &ctx).await;
    assert!(
        matches!(&again.steps[0].outcome, StepOutcome::Failed { why } if why.contains("dialog")),
        "{again:?}"
    );
    d.dialog(tab, true, None).await.expect("accept alert");
    assert_eq!(
        page_js(d, tab, "document.getElementById('a').textContent").await,
        "continued"
    );
    assert_eq!(
        page_js(d, tab, "window.laterRan || 0").await,
        0,
        "no later step ran"
    );

    // confirm: dismiss gives false, accept gives true.
    let res = act(d, tab, &format!("c{cf}"), &ctx).await;
    assert_eq!(
        res.steps[0].consequences.last(),
        Some(&Consequence::Dialog {
            text: "Delete it?".into()
        }),
        "{res:?}"
    );
    d.dialog(tab, false, None).await.expect("dismiss confirm");
    assert_eq!(
        page_js(d, tab, "document.getElementById('c').textContent").await,
        "false"
    );
    act(d, tab, &format!("c{cf}"), &ctx).await;
    d.dialog(tab, true, None).await.expect("accept confirm");
    assert_eq!(
        page_js(d, tab, "document.getElementById('c').textContent").await,
        "true"
    );

    // prompt: the answer text reaches the page; dismiss gives null.
    let res = act(d, tab, &format!("c{pr}"), &ctx).await;
    assert_eq!(
        res.steps[0].consequences.last(),
        Some(&Consequence::Dialog {
            text: "Your name?".into()
        }),
        "{res:?}"
    );
    d.dialog(tab, true, Some("Lokesh K"))
        .await
        .expect("answer prompt");
    assert_eq!(
        page_js(d, tab, "document.getElementById('p').textContent").await,
        "Lokesh K"
    );
    act(d, tab, &format!("c{pr}"), &ctx).await;
    d.dialog(tab, false, None).await.expect("dismiss prompt");
    assert_eq!(
        page_js(d, tab, "document.getElementById('p').textContent").await,
        "null"
    );

    // Row 11: hostile, long dialog text is escaped and capped.
    let res = act(d, tab, &format!("c{big}"), &ctx).await;
    let Some(Consequence::Dialog { text }) = res.steps[0].consequences.last() else {
        panic!("no dialog: {res:?}");
    };
    println!("row11 dialog text: {text:?}");
    escaped_and_capped(text);
    assert!(text.ends_with("..."), "capped");
    d.dialog(tab, true, None).await.expect("accept big");
}

// ---------------------------------------------------------------------------------------
// Row 3 and row 11 (download names): page-supplied names land inside files/ or nowhere.
// ---------------------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row03_downloads_ask_first_and_land_only_inside_files() {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let tab = d.open(&rig.url("/dls")).await.expect("open");
    let reserved = |stem: &str| {
        let s = stem.trim().to_ascii_uppercase();
        matches!(s.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((s.starts_with("COM") || s.starts_with("LPT"))
                && s.len() == 4
                && s.as_bytes()[3].is_ascii_digit())
    };
    for (sel, path) in [
        ("#e1", "/dl-evil1"),
        ("#e2", "/dl-evil2"),
        ("#e3", "/dl-evil3"),
        ("#con", "/dl-con"),
        ("#ref", "/dl-ref"),
        ("#attr", "/dl-attr"),
    ] {
        let body = dl_body(path);
        let el = r(d, tab, sel).await;
        let mut events = d.cdp().events();
        let res = act(d, tab, &format!("c{el}"), &ctx).await;
        assert!(ok_steps(&res), "{path}: {res:?}");
        let (name, guid) = download_of(&res);
        println!("row3 {path}: reported name {name:?}");
        escaped_and_capped(&name);
        assert!(
            !res.steps[0]
                .consequences
                .iter()
                .any(|c| matches!(c, Consequence::Navigated { .. })),
            "{res:?}"
        );
        assert_eq!(download_end(&mut events, &guid).await, "canceled", "{path}");
        assert!(
            files_with(rig.home.path(), body.as_bytes()).is_empty(),
            "{path}: on disk before the answer"
        );
        assert!(
            !user_downloads_has(&name),
            "{path}: in the user's Downloads"
        );
        let saved = d
            .download(tab, &guid, true)
            .await
            .unwrap_or_else(|e| panic!("{path}: download allowed: {e}"))
            .expect("a path");
        println!("row3 {path}: saved as {:?}", saved.file_name());
        let files = std::fs::canonicalize(rig.files()).unwrap();
        let real = std::fs::canonicalize(&saved).unwrap();
        assert_eq!(real.parent(), Some(files.as_path()), "{path}: {saved:?}");
        assert_eq!(std::fs::read(&saved).unwrap(), body.as_bytes(), "{path}");
        let found: Vec<PathBuf> = files_with(rig.home.path(), body.as_bytes())
            .into_iter()
            .map(|p| std::fs::canonicalize(p).unwrap())
            .collect();
        assert_eq!(found, vec![real.clone()], "{path}: exactly one copy");
        let fname = saved.file_name().unwrap().to_string_lossy().to_string();
        assert!(!reserved(fname.split('.').next().unwrap_or("")), "{fname}");
        assert!(fname.chars().count() <= 120, "{fname}");
    }
    // allow=false: nothing on disk, not fetched again.
    let no = r(d, tab, "#no").await;
    let mut events = d.cdp().events();
    let res = act(d, tab, &format!("c{no}"), &ctx).await;
    let (name, guid) = download_of(&res);
    let before = hits("/dl-no");
    assert_eq!(d.download(tab, &guid, false).await.expect("refuse"), None);
    assert_eq!(download_end(&mut events, &guid).await, "canceled");
    assert!(files_with(rig.home.path(), dl_body("/dl-no").as_bytes()).is_empty());
    assert!(!user_downloads_has(&name));
    assert_eq!(hits("/dl-no"), before, "a refused download is not fetched");
    assert!(d.download(tab, &guid, true).await.is_err(), "answered once");
}

// ---------------------------------------------------------------------------------------
// Row 4: a no-op click. Row 5: a navigating click with a B-8 diff.
// ---------------------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row04_05_noop_click_and_navigating_click() {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let tab = d.open(&rig.url("/links")).await.expect("open");
    let noop = r(d, tab, "#noop").await;
    let res = act(d, tab, &format!("c{noop}"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(
        res.steps[0].consequences,
        [Consequence::NoVisibleChange],
        "{res:?}"
    );
    assert_eq!(res.diff, None);

    let nav = r(d, tab, "#nav").await;
    let res = act(d, tab, &format!("c{nav}"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    assert!(
        res.steps[0].consequences.contains(&Consequence::Navigated {
            url: rig.url("/welcome")
        }),
        "{res:?}"
    );
    let diff = res.diff.clone().expect("a diff");
    println!("row5 diff: {diff}");
    assert!(!diff.is_empty());
    for item in diff.split(" \u{b7} ") {
        assert!(
            item.starts_with("+ ")
                || item.starts_with("\u{2212} ")
                || item.starts_with("~ ")
                || (item.starts_with('+') && item.ends_with(" more")),
            "B-8 item form: {item:?} in {diff}"
        );
    }
    assert!(diff.contains("+ heading \"Welcome, Lokesh\""), "{diff}");
    assert!(diff.contains("\u{2212} form f1"), "{diff}");
}

// ---------------------------------------------------------------------------------------
// Row 6: typing, @vault into a plain input and a password field, canary sweep.
// ---------------------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row06_typing_and_the_vault_canary_never_leaves() {
    let rig = rig().await;
    let c = canary();
    let ctx = rig.ctx(Class::Submit);
    let mut seen: Vec<String> = Vec::new();
    let d = &rig.d;
    let tab = d.open(&rig.url("/type")).await.expect("open");
    let has = |sel: &'static str| async move {
        sb_eval(
            d,
            tab,
            0,
            "function (sel, c) { return document.querySelector(sel).value === c; }",
            &[json!(sel), json!(canary())],
        )
        .await
    };

    // Plain t#.
    let plain = r(d, tab, "#plain").await;
    let res = act(d, tab, &format!("t{plain}=hello t21"), &ctx).await;
    assert!(ok_steps(&res));
    assert_eq!(
        page_js(d, tab, "document.querySelector('#plain').value").await,
        "hello t21"
    );
    record(&mut seen, &res);

    // t#N=@vault(pw) into a plain text input.
    let vplain = r(d, tab, "#vplain").await;
    let res = act(d, tab, &format!("t{vplain}=@vault(pw)"), &ctx).await;
    assert!(ok_steps(&res), "vault into a text input failed");
    assert_eq!(has("#vplain").await, json!(true), "vault value landed");
    record(&mut seen, &res);

    // A password field.
    let pw = r(d, tab, "#pw").await;
    let res = act(d, tab, &format!("t{pw}=@vault(pw)"), &ctx).await;
    assert!(ok_steps(&res), "vault into a password field failed");
    assert_eq!(has("#pw").await, json!(true), "vault value landed");
    record(&mut seen, &res);

    // The page echoes the value into a dialog in the same program.
    let echo = r(d, tab, "#echoal").await;
    let res = act(d, tab, &format!("t{vplain}=@vault(pw); c{echo}"), &ctx).await;
    let dlg = res
        .steps
        .last()
        .and_then(|s| s.consequences.last())
        .cloned();
    record(&mut seen, &res);
    d.dialog(tab, true, None).await.expect("accept echo");
    assert!(
        matches!(&dlg, Some(Consequence::Dialog { text }) if text.contains("@vault(pw)")),
        "the dialog echo is shown as the literal"
    );

    // Errors: a gone target, a missing key, a malformed ref, a parse error.
    for prog in [
        "t#999999=@vault(pw)".to_string(),
        format!("t{vplain}=@vault(nokey)"),
        format!("t{vplain}=@vault(pw"),
    ] {
        match d.act(tab, &prog, &ctx).await {
            Ok(res) => {
                assert!(!ok_steps(&res), "{prog} should fail");
                record(&mut seen, &res);
            }
            Err(e) => seen.push(format!("{e} {e:?}")),
        }
    }
    if let Err(e) = d.act(tab, "t#=@vault(pw)", &ctx).await {
        seen.push(format!("{e} {e:?}"));
    }

    // The page echoes the value into the URL (GET form, Enter) in the same program.
    let q = r(d, tab, "#q").await;
    let res = act(d, tab, &format!("t{q}=@vault(pw); k{q}=Enter"), &ctx).await;
    let navigated = res.steps.iter().any(|s| {
        s.consequences
            .iter()
            .any(|c| matches!(c, Consequence::Navigated { .. }))
    });
    record(&mut seen, &res);
    assert!(navigated, "the GET form navigated");

    for (i, s) in seen.iter().enumerate() {
        assert!(!s.contains(&c), "canary in result string #{i}");
    }

    // A later program echoes it again (MCP-level: the process-wide scrubber is installed
    // as the server does at start).
    rig.vault.install_global();
    let t2 = d.open(&rig.url("/type")).await.expect("open again");
    let v2 = r(d, t2, "#vplain").await;
    act(d, t2, &format!("t{v2}=@vault(pw)"), &ctx).await;
    let e2 = r(d, t2, "#echoal").await;
    let res = act(d, t2, &format!("c{e2}"), &ctx).await;
    let leaked = format!("{res:?}").contains(&c);
    d.dialog(t2, true, None).await.expect("accept");
    assert!(
        !leaked,
        "a later program's dialog echo is not scrubbed even with install_global"
    );

    // Ledger and run dir (the driver writes no ledger events itself; the sweep proves
    // nothing it wrote holds the canary).
    let ledger = format!("{:?}", rig.store.ledger());
    assert!(!ledger.contains(&c), "canary in the ledger");
    let in_run = files_containing(&rig.run_dir(), c.as_bytes());
    assert!(
        in_run.is_empty(),
        "canary in {} run-dir files",
        in_run.len()
    );
    let tabs_leak = d
        .tabs()
        .await
        .unwrap()
        .iter()
        .any(|t| t.1.contains(&c) || t.2.contains(&c));
    println!("note: tabs() url/title holds the canary: {tabs_leak}");
    // Whole home after the browser exits (profile included): reported, the run dir is the
    // assertion above.
    let Rig { d, home, .. } = rig;
    d.shutdown(Duration::from_secs(10)).await;
    let all = files_containing(home.path(), c.as_bytes());
    println!(
        "note: files under the test home holding the canary after shutdown: {}",
        all.len()
    );
    for p in &all {
        println!(
            "note:   {}",
            p.strip_prefix(home.path()).unwrap_or(p).display()
        );
    }
}

// ---------------------------------------------------------------------------------------
// Row 7: select, check, key Enter on a form (Submit), scroll down, up and #N.
// ---------------------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row07_select_check_key_scroll() {
    let rig = rig().await;
    let d = &rig.d;
    let submit = rig.ctx(Class::Submit);
    let fill = rig.ctx(Class::Fill);
    let tab = d.open(&rig.url("/form")).await.expect("open");

    let plan = r(d, tab, "#plan").await;
    let res = act(d, tab, &format!("s{plan}=Plus"), &fill).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(
        page_js(d, tab, "document.querySelector('#plan').value").await,
        "p"
    );

    let agree = r(d, tab, "#agree").await;
    let res = act(d, tab, &format!("c{agree}"), &fill).await;
    assert!(ok_steps(&res), "check under Fill: {res:?}");
    assert_eq!(
        page_js(d, tab, "document.querySelector('#agree').checked").await,
        true
    );

    // Enter on a form field is Submit: under Fill it stops before the key.
    let k = r(d, tab, "#k").await;
    let res = act(d, tab, &format!("k{k}=Enter"), &fill).await;
    let stop = res
        .stopped
        .clone()
        .expect("Enter on a form stops under Fill");
    assert_eq!(stop.class, Class::Submit, "{res:?}");
    assert_eq!(page_js(d, tab, "window.keys").await, 0, "no key reached");
    assert_eq!(page_js(d, tab, "window.submits || 0").await, 0);
    // Under Submit it runs and submits.
    let res = act(d, tab, &format!("k{k}=Enter"), &submit).await;
    assert!(ok_steps(&res) && res.stopped.is_none(), "{res:?}");
    assert_eq!(page_js(d, tab, "window.submits || 0").await, 1);

    // Scroll is Navigate (docs/01 H-1 table), above Fill: stopped under Fill, runs under
    // Navigate.
    let res = act(d, tab, "sc(down)", &fill).await;
    assert_eq!(
        res.stopped.as_ref().map(|s| s.class),
        Some(Class::Navigate),
        "{res:?}"
    );
    assert_eq!(page_js(d, tab, "scrollY").await.as_f64(), Some(0.0));
    let nav = rig.ctx(Class::Navigate);
    let res = act(d, tab, "sc(down)", &nav).await;
    assert!(ok_steps(&res), "{res:?}");
    let y = page_js(d, tab, "scrollY").await.as_f64().unwrap();
    assert!(y > 100.0, "{y}");
    let res = act(d, tab, "sc(up); sc(up); sc(up); sc(up); sc(up)", &nav).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(page_js(d, tab, "scrollY").await.as_f64(), Some(0.0));
    let bottom = r(d, tab, "#bottom").await;
    let res = act(d, tab, &format!("sc({bottom})"), &nav).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(
        page_js(
            d,
            tab,
            "(() => { const b = document.getElementById('bottom').getBoundingClientRect(); \
             return b.top >= 0 && b.bottom <= innerHeight; })()"
        )
        .await,
        true
    );
}

// ---------------------------------------------------------------------------------------
// Row 8: upload inside files/ only; `..`, absolute, junction and symlink escapes refused.
// ---------------------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row08_upload_paths_cannot_escape_files() {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let files = rig.files();
    let outside = rig.run_dir().join("outside");
    std::fs::create_dir_all(&files).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(files.join("cv.txt"), b"cv").unwrap();
    std::fs::write(rig.run_dir().join("secret.txt"), b"s").unwrap();
    std::fs::write(outside.join("secret2.txt"), b"s2").unwrap();
    let tab = d.open(&rig.url("/up")).await.expect("open");
    let f = r(d, tab, "#f").await;
    let count = || page_js(d, tab, "document.querySelector('#f').files.length");

    let mut bad: Vec<String> = vec![
        "../secret.txt".into(),
        "..\\secret.txt".into(),
        "files/../../secret.txt".into(),
        "./../outside/secret2.txt".into(),
        rig.run_dir().join("secret.txt").display().to_string(),
        files.join("cv.txt").display().to_string(),
    ];
    // A junction (no admin needed on Windows) inside files/ pointing outside.
    let junction = files.join("jx");
    #[cfg(windows)]
    {
        let st = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output()
            .expect("mklink");
        assert!(st.status.success(), "junction not created");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &junction).expect("dir symlink");
    assert_eq!(
        std::fs::read(junction.join("secret2.txt")).unwrap(),
        b"s2",
        "the junction resolves outside"
    );
    bad.push("jx/secret2.txt".into());
    bad.push("files/jx/secret2.txt".into());
    // A file symlink, when the host allows creating one.
    #[cfg(windows)]
    let link =
        std::os::windows::fs::symlink_file(outside.join("secret2.txt"), files.join("ln.txt"));
    #[cfg(unix)]
    let link = std::os::unix::fs::symlink(outside.join("secret2.txt"), files.join("ln.txt"));
    match link {
        Ok(()) => bad.push("ln.txt".into()),
        Err(e) => println!("note: file symlink not created on this host: {e}"),
    }

    for p in &bad {
        let res = act(d, tab, &format!("u{f}={p}"), &ctx).await;
        assert!(
            matches!(&res.steps[0].outcome, StepOutcome::Failed { why } if why.starts_with("upload refused")),
            "{p}: {res:?}"
        );
        assert_eq!(count().await, 0, "{p} set a file");
    }
    println!("row8 refused: {bad:?}");
    let res = act(d, tab, &format!("u{f}=cv.txt"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(
        page_js(d, tab, "document.querySelector('#f').files[0].name").await,
        "cv.txt"
    );
}

// ---------------------------------------------------------------------------------------
// Row 9: the per-step gate, with in-page counters.
// ---------------------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row09_gate_stops_before_input_with_counters() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/gate")).await.expect("open");
    let user = r(d, tab, "#user").await;
    let sub = r(d, tab, "#sub").await;
    let cap = r(d, tab, "#cap").await;
    let fill = rig.ctx(Class::Fill);
    let pay = rig.ctx(Class::Pay);

    let res = act(d, tab, &format!("t{user}=me; c{sub}; t{user}=again"), &fill).await;
    assert_eq!(res.steps.len(), 2, "nothing after the stop: {res:?}");
    assert_eq!(res.steps[0].outcome, StepOutcome::Ok);
    assert!(matches!(res.steps[1].outcome, StepOutcome::Failed { .. }));
    let stop = res.stopped.clone().expect("stopped");
    assert_eq!(stop.class, Class::Submit);
    assert_eq!(stop.element.as_deref(), Some(sub.as_str()));
    assert_eq!(page_js(d, tab, "window.mouse").await, 0, "no mouse event");
    assert_eq!(page_js(d, tab, "window.subclicks || 0").await, 0);
    assert_eq!(page_js(d, tab, "window.submits || 0").await, 0);
    assert_eq!(
        page_js(d, tab, "document.querySelector('#user').value").await,
        "me",
        "the step after the stop did not run"
    );

    // CHALLENGE row, below the fold: no scroll, no mouse, no later step.
    let keys = page_js(d, tab, "window.keys").await;
    let res = act(d, tab, &format!("c{cap}; t{user}=after"), &pay).await;
    assert_eq!(res.steps.len(), 1, "{res:?}");
    assert_eq!(
        res.stopped.as_ref().map(|s| s.class),
        Some(Class::Challenge),
        "{res:?}"
    );
    assert_eq!(page_js(d, tab, "window.mouse").await, 0);
    assert_eq!(page_js(d, tab, "window.capclicks || 0").await, 0);
    assert_eq!(
        page_js(d, tab, "scrollY").await.as_f64(),
        Some(0.0),
        "no scroll"
    );
    assert_eq!(page_js(d, tab, "window.keys").await, keys, "no keys");
    assert_eq!(
        page_js(d, tab, "document.querySelector('#user').value").await,
        "me"
    );

    // Approved Submit: the same click runs once.
    let submit = rig.ctx(Class::Submit);
    let res = act(d, tab, &format!("c{sub}"), &submit).await;
    assert!(ok_steps(&res) && res.stopped.is_none(), "{res:?}");
    assert_eq!(page_js(d, tab, "window.subclicks || 0").await, 1);
    assert_eq!(page_js(d, tab, "window.submits || 0").await, 1);
}

// ---------------------------------------------------------------------------------------
// Row 10: a click inside a same-origin iframe (in view, and below the fold), 8 px target.
// ---------------------------------------------------------------------------------------
async fn frame_click(path: &str, scroll: bool) {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let tab = d.open(&rig.url(path)).await.expect("open");
    if scroll {
        page_js(d, tab, "scrollTo(0, 400)").await;
    }
    let frame = d
        .wait_registry(CAP, |r| {
            r.frames(tab)?
                .into_iter()
                .find(|f| {
                    f.frame > 0 && f.url == "about:srcdoc" && r.sb_context(tab, f.frame).is_some()
                })
                .map(|f| f.frame)
        })
        .await
        .expect("srcdoc frame");
    sb_eval(
        d,
        tab,
        frame,
        "function () { return globalThis.__sb.walk({ base: 1 }); }",
        &[],
    )
    .await;
    let b = id_of(d, tab, frame, "#b").await;
    // Where the mouse landed, for the failure message.
    page_js(
        d,
        tab,
        "document.addEventListener('mousedown', (e) => { window.pdown = e.clientX + ',' + \
         e.clientY + ',' + (e.target.id || e.target.tagName) + ',sy=' + scrollY; }, true)",
    )
    .await;
    sb_eval(
        d,
        tab,
        frame,
        "function () { document.addEventListener('mousedown', (e) => { \
         document.body.dataset.down = e.clientX + ',' + e.clientY + ',' + \
         (e.target.id || e.target.tagName); }, true); return true; }",
        &[],
    )
    .await;
    let res = act(d, tab, &format!("c#f{frame}.{b}"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    let got = page_js(d, tab, "document.getElementById('r').textContent").await;
    let where_ = page_js(
        d,
        tab,
        "JSON.stringify({ parent: window.pdown || null, \
         frame: document.getElementById('fr').contentDocument.body.dataset.down || null, \
         rect: document.getElementById('fr').getBoundingClientRect(), sy: scrollY, vh: innerHeight })",
    )
    .await;
    println!("row10 {path} landing: {where_}; step: {:?}", res.steps[0]);
    assert_eq!(got, "hit", "landed: {where_}");
}

/// Row 10a: the frame in view, an 8 px target: border and padding must both be added.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row10a_frame_click_in_view_hits_an_8px_target() {
    frame_click("/framed-in", false).await;
}

/// Row 10b: the frame below the fold of a scrolled page.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row10b_frame_click_below_the_fold_hits_the_target() {
    frame_click("/framed", true).await;
}
// ---------------------------------------------------------------------------------------
// Row 10c (round 2): an iframe inside an iframe, both below the fold, 8 px target.
// ---------------------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row10c_nested_frames_below_the_fold_hit_the_target() {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let tab = d.open(&rig.url("/nest")).await.expect("open");
    page_js(d, tab, "scrollTo(0, 400)").await;
    let inner = d
        .wait_registry(CAP, |r| {
            r.frames(tab)?
                .into_iter()
                .find(|f| f.url.ends_with("/nest-inner") && r.sb_context(tab, f.frame).is_some())
                .map(|f| f.frame)
        })
        .await
        .expect("the inner frame with its sb world");
    // Both frames start below their parent's fold.
    let below = page_js(
        d,
        tab,
        "(() => { const o = document.getElementById('outer'); \
         const i = o.contentDocument.getElementById('inner'); \
         return o.getBoundingClientRect().top > innerHeight && \
           i.getBoundingClientRect().top > o.contentWindow.innerHeight; })()",
    )
    .await;
    assert_eq!(
        below, true,
        "the outer and the inner frame start below the fold"
    );
    sb_eval(
        d,
        tab,
        inner,
        "function () { return globalThis.__sb.walk({ base: 1 }); }",
        &[],
    )
    .await;
    let b = id_of(d, tab, inner, "#b").await;
    let res = act(d, tab, &format!("c#f{inner}.{b}"), &ctx).await;
    let where_ = page_js(
        d,
        tab,
        "JSON.stringify({ r: document.getElementById('r').textContent, sy: scrollY, \
         vh: innerHeight, outer: document.getElementById('outer').getBoundingClientRect(), \
         outerSy: document.getElementById('outer').contentWindow.scrollY })",
    )
    .await;
    println!("row10c landing: {where_}; step: {:?}", res.steps[0]);
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(
        page_js(d, tab, "document.getElementById('r').textContent").await,
        "hit",
        "landed: {where_}"
    );
    let n = sb_eval(
        d,
        tab,
        inner,
        "function () { return +(document.getElementById('b').dataset.n || 0); }",
        &[],
    )
    .await;
    assert_eq!(n, 1, "the inner button's own counter");
}

// ---------------------------------------------------------------------------------------
// Row 15 (round 2, review Blocking 1): a typed value longer than every cap, echoed.
// ---------------------------------------------------------------------------------------

/// Every string form of a result that can leave the driver.
fn forms_of(res: &ActResult) -> Vec<String> {
    let mut out = vec![
        format!("{res:?}"),
        serde_json::to_string(&res.steps).unwrap(),
        serde_json::to_string(&res.stopped).unwrap(),
        res.diff.clone().unwrap_or_default(),
    ];
    for s in &res.steps {
        if let StepOutcome::Failed { why } = &s.outcome {
            out.push(why.clone());
        }
        for c in &s.consequences {
            out.push(serde_json::to_string(c).unwrap());
            out.push(format!("{c:?}"));
        }
    }
    out
}

/// Neither the canary nor the long value's first 8 characters (so no prefix of 8 or more)
/// is in any form, in any ASCII case. Never prints a form.
fn no_value(forms: &[String], what: &str) {
    let c = canary().to_ascii_lowercase();
    let p8: String = long_value()
        .chars()
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    for (i, f) in forms.iter().enumerate() {
        let f = f.to_ascii_lowercase();
        assert!(!f.contains(&c), "{what}: the canary is in form #{i}");
        assert!(
            !f.contains(&p8),
            "{what}: an 8-char prefix of the value is in form #{i}"
        );
    }
}

/// The process-wide scrubber of an earlier test (row 6) holds the canary; replace it with
/// one that knows nothing, so these checks see only what the driver itself scrubs.
fn neutral_global() {
    Vault::in_memory([(
        "unrelated".to_string(),
        "unrelated-value-t21r2".to_string(),
        EntryKind::Secret,
    )])
    .install_global();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row15_long_echo_never_leaves_whole_or_cut() {
    let rig = rig().await;
    neutral_global();
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let mut every: Vec<String> = Vec::new();

    // (1) Same document: six headings (two cut by the index at 150, three at the diff's
    // 60 with 9, 8 and 7 value characters before the cut), the title, the URL, and a new
    // tab whose title is 100 x then the value (cut at 120).
    let tab = d.open(&rig.url("/long")).await.expect("open /long");
    let e = r(d, tab, "#e").await;
    let open = r(d, tab, "#open").await;
    let res = act(d, tab, &format!("t{e}=@vault(long); c{open}"), &ctx).await;
    let forms = forms_of(&res);
    no_value(&forms, "same-document echo");
    every.extend(forms);
    let echoed = sb_eval(
        d,
        tab,
        0,
        "function (v) { return document.getElementById('h0').textContent === v && \
         document.getElementById('h3').textContent === 'p'.repeat(52) + v && \
         document.title === v && location.href.endsWith('?v=' + encodeURIComponent(v)); }",
        &[json!(long_value())],
    )
    .await;
    assert_eq!(echoed, json!(true), "the page echoed the value");
    println!("row15 (1) result: {res:?}");
    assert!(ok_steps(&res), "{res:?}");
    assert!(res.diff.is_some(), "a diff: {res:?}");
    assert!(
        res.steps[1]
            .consequences
            .iter()
            .any(|c| matches!(c, Consequence::OpenedTab { .. })),
        "{res:?}"
    );

    // (2) An alert with the value after 11 characters.
    let al = r(d, tab, "#al").await;
    let res = act(d, tab, &format!("t{e}=@vault(long); c{al}"), &ctx).await;
    let forms = forms_of(&res);
    no_value(&forms, "alert echo");
    every.extend(forms);
    println!("row15 (2) result: {res:?}");
    assert!(
        matches!(res.steps.last().and_then(|s| s.consequences.last()),
            Some(Consequence::Dialog { text }) if text.starts_with("You typed: ")),
        "{res:?}"
    );
    d.dialog(tab, true, None).await.expect("accept");

    // (3) A GET form: the value goes into the URL and the next page's headings and title.
    let t3 = d.open(&rig.url("/long-get")).await.expect("open /long-get");
    let q = r(d, t3, "#q").await;
    let res = act(d, t3, &format!("t{q}=@vault(long); k{q}=Enter"), &ctx).await;
    let forms = forms_of(&res);
    no_value(&forms, "GET form echo");
    every.extend(forms);
    let landed = sb_eval(
        d,
        t3,
        0,
        "function (v) { return document.title === v && \
         document.getElementById('h4').textContent === 'p'.repeat(53) + v; }",
        &[json!(long_value())],
    )
    .await;
    assert_eq!(landed, json!(true), "the landing page echoed the value");
    println!("row15 (3) result: {res:?}");
    assert!(ok_steps(&res), "{res:?}");
    assert!(
        res.steps[1]
            .consequences
            .iter()
            .any(|c| matches!(c, Consequence::Navigated { .. })),
        "{res:?}"
    );
    let diff = res.diff.clone().expect("a diff");
    assert!(diff.contains("+ heading"), "{diff}");

    // (4) tabs() and frames(). Without a process-wide vault the driver has nothing to
    // scrub them with (it keeps no vault outside act): reported, not asserted. With the
    // vault installed, as the MCP server does at start, they are clean.
    let raw_leak = d.tabs().await.unwrap().iter().any(|t| {
        let l = format!("{} {}", t.1, t.2).to_ascii_lowercase();
        l.contains(&canary().to_ascii_lowercase())
    });
    println!("note: tabs() before install_global holds the canary: {raw_leak}");
    rig.vault.install_global();
    let tabs = d.tabs().await.expect("tabs");
    assert!(tabs.len() >= 3, "three tabs");
    let mut shown = Vec::new();
    for (t, url, title) in &tabs {
        shown.push(format!("{t:?} {url} {title}"));
        for f in d.frames(*t).await.expect("frames") {
            shown.push(format!("{f:?}"));
        }
    }
    no_value(&shown, "tabs() and frames()");
    every.extend(shown.clone());
    println!("row15 (4) tabs and frames: {shown:?}");
    assert!(
        tabs.iter().any(|t| t.2.contains("@vault(long)")),
        "a title is shown as the literal"
    );

    // (5) The ledger and the run dir.
    let ledger = format!("{:?}", rig.store.ledger());
    no_value(&[ledger], "ledger");
    let in_run = files_containing(&rig.run_dir(), canary().as_bytes());
    assert!(
        in_run.is_empty(),
        "canary in {} run-dir files",
        in_run.len()
    );
    no_value(&every, "all forms together");
    let Rig { d, home, .. } = rig;
    d.shutdown(Duration::from_secs(10)).await;
    let all = files_containing(home.path(), canary().as_bytes());
    println!(
        "note: files under the test home holding the canary after shutdown: {}",
        all.len()
    );
    for p in &all {
        println!(
            "note:   {}",
            p.strip_prefix(home.path()).unwrap_or(p).display()
        );
    }
}

// ---------------------------------------------------------------------------------------
// Row 16 (round 2, review Blocking 2, D-615): input never reaches a focused descendant.
// ---------------------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row16_input_never_reaches_a_focused_descendant() {
    let rig = rig().await;
    let d = &rig.d;
    let nav = rig.ctx(Class::Navigate);

    // (i) A modal <dialog> whose Pay now button got autofocus from showModal().
    let tab = d.open(&rig.url("/focus")).await.expect("open /focus");
    assert_eq!(
        page_js(d, tab, "document.activeElement.id").await,
        "pay",
        "the modal dialog autofocused Pay now"
    );
    let dlg = r(d, tab, "#dlg").await;
    let res = act(d, tab, &format!("k{dlg}=Enter"), &nav).await;
    let key_at = page_js(d, tab, "window.keyAt || null").await;
    let active = page_js(d, tab, "document.activeElement.id").await;
    println!("row16 (i) Enter on a modal <dialog>: key went to {key_at}, active {active}: {res:?}");
    assert_eq!(
        page_js(d, tab, "+(document.getElementById('pay').dataset.n || 0)").await,
        0,
        "Pay now was pressed"
    );
    assert!(
        res.stopped.is_none(),
        "the gate let the dialog row run: {res:?}"
    );
    match &res.steps[0].outcome {
        // The <dialog> element itself took focus: the key may only have gone to it.
        StepOutcome::Ok => assert_eq!(key_at, "dlg", "the key reached a descendant"),
        StepOutcome::Failed { why } => {
            assert!(why.contains("did not take focus"), "{res:?}");
            assert_eq!(
                page_js(d, tab, "window.keys").await,
                0,
                "a key reached the page"
            );
        }
    }

    // (ii) A div dialog (not focusable) whose Pay now button has autofocus.
    let t2 = d.open(&rig.url("/focus2")).await.expect("open /focus2");
    assert_eq!(
        page_js(d, t2, "document.activeElement.id").await,
        "pay2",
        "Pay now has autofocus"
    );
    let pays = || page_js(d, t2, "+(document.getElementById('pay2').dataset.n || 0)");
    let dlg2 = r(d, t2, "#dlg2").await;
    let res = act(d, t2, &format!("k{dlg2}=Enter"), &nav).await;
    println!("row16 (ii) Enter on a div dialog: {res:?}");
    assert_eq!(pays().await, 0, "Pay now was pressed");
    assert_eq!(
        page_js(d, t2, "window.keys").await,
        0,
        "a key reached the page"
    );
    assert!(
        res.stopped.is_none(),
        "the gate let the dialog row run: {res:?}"
    );
    assert!(
        matches!(&res.steps[0].outcome, StepOutcome::Failed { why } if why.contains("did not take focus")),
        "{res:?}"
    );

    // (iii) A container whose password field holds focus: nothing is typed.
    let focused = page_js(
        d,
        t2,
        "document.getElementById('secret2').focus(); document.activeElement.id",
    )
    .await;
    assert_eq!(focused, "secret2");
    let boxed = r(d, t2, "#box2").await;
    // Pay: above any class the gate gives a container, so only the focus check can stop it.
    let res = act(d, t2, &format!("t{boxed}=x"), &rig.ctx(Class::Pay)).await;
    println!("row16 (iii) type into the container: {res:?}");
    assert_eq!(
        page_js(d, t2, "document.getElementById('secret2').value === ''").await,
        true,
        "something was typed into the password field"
    );
    assert_eq!(page_js(d, t2, "window.secretInputs").await, 0);
    assert_eq!(page_js(d, t2, "window.keys").await, 0);
    assert!(res.stopped.is_none(), "{res:?}");
    assert!(
        matches!(&res.steps[0].outcome, StepOutcome::Failed { why } if why.contains("did not take focus")),
        "{res:?}"
    );
    assert_eq!(pays().await, 0);

    // (iv) The fieldset on /focus, its password field focused: nothing is typed.
    let focused = page_js(
        d,
        tab,
        "document.getElementById('dlg').close(); document.getElementById('secret').focus(); \
         document.activeElement.id",
    )
    .await;
    assert_eq!(focused, "secret");
    let keys = page_js(d, tab, "window.keys").await;
    let boxed = r(d, tab, "#box").await;
    let res = act(d, tab, &format!("t{boxed}=x"), &rig.ctx(Class::Pay)).await;
    println!("row16 (iv) type into the fieldset: {res:?}");
    assert_eq!(
        page_js(d, tab, "document.getElementById('secret').value === ''").await,
        true,
        "something was typed into the password field"
    );
    assert_eq!(page_js(d, tab, "window.secretInputs").await, 0);
    assert_eq!(page_js(d, tab, "window.keys").await, keys);
    assert!(!matches!(res.steps[0].outcome, StepOutcome::Ok), "{res:?}");

    // Control: the element itself still takes focus and gets the text.
    let plain = r(d, t2, "#plain2").await;
    let res = act(d, t2, &format!("t{plain}=ok"), &rig.ctx(Class::Fill)).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(
        page_js(d, t2, "document.getElementById('plain2').value").await,
        "ok"
    );
}

// ---------------------------------------------------------------------------------------
// Row 17 (round 2, review Should-fix 1, D-618): a timer dialog between steps.
// ---------------------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row17_a_timer_dialog_between_steps_stops_at_once() {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let tab = d.open(&rig.url("/timer")).await.expect("open /timer");
    let arm = r(d, tab, "#arm").await;
    let arm0 = r(d, tab, "#arm0").await;
    let hid = r(d, tab, "#hid").await;
    let cnt = r(d, tab, "#cnt").await;
    let ti = r(d, tab, "#ti").await;
    let counted = || page_js(d, tab, "+(document.getElementById('cnt').dataset.n || 0)");

    for (prog, text) in [
        // The alert opens 900 ms later, while step 2 waits for a hidden button.
        (format!("c{arm}; c{hid}; c{cnt}"), "from a timer"),
        // The alert opens at once, before step 2 types.
        (format!("c{arm0}; t{ti}=later; c{cnt}"), "right away"),
    ] {
        let t0 = std::time::Instant::now();
        let res = act(d, tab, &prog, &ctx).await;
        let took = t0.elapsed();
        println!("row17 {prog}: {took:?} {res:?}");
        let at = res
            .steps
            .iter()
            .position(|s| {
                s.consequences
                    .contains(&Consequence::Dialog { text: text.into() })
            })
            .unwrap_or_else(|| panic!("no Dialog: {res:?}"));
        assert_eq!(
            at + 1,
            res.steps.len(),
            "a step ran after the dialog: {res:?}"
        );
        assert!(
            res.stopped.is_none(),
            "a dialog is not a gate stop: {res:?}"
        );
        assert!(
            !res.steps.iter().any(|s| matches!(&s.outcome,
                StepOutcome::Failed { why } if why.contains("not answering"))),
            "{res:?}"
        );
        assert!(took < Duration::from_millis(2500), "stalled: {took:?}");
        d.dialog(tab, true, None).await.expect("accept");
        assert_eq!(counted().await, 0, "the step after the dialog ran");
        assert_eq!(
            page_js(d, tab, "document.getElementById('ti').value").await,
            "",
            "the step after the dialog typed"
        );
    }
}

// ---------------------------------------------------------------------------------------
// Row 18 (round 2, review Should-fix 2 and 3, D-619): an error re-fetch is not saved;
// format characters leave download names.
// ---------------------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row18_error_refetch_not_saved_and_format_chars_stripped() {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let tab = d.open(&rig.url("/dls2")).await.expect("open /dls2");
    let listing = |dir: &Path| -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };

    for (sel, path, code) in [("#e500", "/dl-e500", "500"), ("#e404", "/dl-e404", "404")] {
        let el = r(d, tab, sel).await;
        let mut events = d.cdp().events();
        let res = act(d, tab, &format!("c{el}"), &ctx).await;
        let (name, guid) = download_of(&res);
        assert_eq!(download_end(&mut events, &guid).await, "canceled", "{path}");
        let before = hits(path);
        let err = d
            .download(tab, &guid, true)
            .await
            .expect_err("an error answer is not saved");
        let why = err.to_string();
        println!(
            "row18 {path}: download(true) -> Err({why}); hits {before} -> {}",
            hits(path)
        );
        assert_eq!(hits(path), before + 1, "{path} was fetched again");
        assert!(
            why.contains(code) || why.contains("RESPONSE_CODE"),
            "{path}: {why}"
        );
        assert!(files_with(rig.home.path(), error_body().as_bytes()).is_empty());
        assert!(files_with(rig.home.path(), dl_body(path).as_bytes()).is_empty());
        assert_eq!(
            listing(&rig.files()),
            Vec::<String>::new(),
            "{path}: files/ left over"
        );
        assert!(!user_downloads_has(&name));
    }

    for (sel, path) in [
        ("#rlo", "/dl-rlo"),
        ("#zw", "/dl-zw"),
        ("#attrc", "/dl-attrc"),
    ] {
        let el = r(d, tab, sel).await;
        let mut events = d.cdp().events();
        let res = act(d, tab, &format!("c{el}"), &ctx).await;
        let (name, guid) = download_of(&res);
        println!("row18 {path}: shown name {name:?}");
        assert!(
            !name.chars().any(is_cf),
            "{path}: a format char is shown: {name:?}"
        );
        assert_eq!(download_end(&mut events, &guid).await, "canceled", "{path}");
        let saved = d
            .download(tab, &guid, true)
            .await
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .expect("a path");
        let fname = saved.file_name().unwrap().to_string_lossy().to_string();
        println!("row18 {path}: saved as {fname:?}");
        assert!(
            !fname.chars().any(is_cf),
            "{path}: a format char on disk: {fname:?}"
        );
        assert!(fname.contains(&pid().to_string()), "{path}: {fname:?}");
        let files = std::fs::canonicalize(rig.files()).unwrap();
        let real = std::fs::canonicalize(&saved).unwrap();
        assert_eq!(real.parent(), Some(files.as_path()), "{path}");
        assert_eq!(std::fs::read(&saved).unwrap(), dl_body(path).as_bytes());
    }
    let left = listing(&rig.files());
    assert!(
        left.iter()
            .all(|n| !n.chars().any(is_cf) && !n.ends_with(".part")),
        "{left:?}"
    );
}

// ---------------------------------------------------------------------------------------
// Row 19 (round 2, review Should-fix 5, D-618): a stale target is a failed step, not a stop.
// ---------------------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row19_a_stale_target_after_navigation_is_a_failed_step() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/links")).await.expect("open /links");
    let noop = r(d, tab, "#noop").await;
    let blank = r(d, tab, "#blank").await;
    let search = r(d, tab, "input[aria-label=Search]").await;
    let nav = r(d, tab, "#nav").await;
    let res = act(d, tab, &format!("c{nav}"), &rig.ctx(Class::Navigate)).await;
    assert!(
        res.steps[0].consequences.contains(&Consequence::Navigated {
            url: rig.url("/welcome")
        }),
        "{res:?}"
    );
    for (prog, class) in [
        (format!("c{noop}"), Class::Fill),
        (format!("c{noop}"), Class::Read),
        (format!("c{blank}"), Class::Navigate),
        (format!("t{search}=x"), Class::Read),
        (format!("k{search}=Enter"), Class::Fill),
    ] {
        let res = act(d, tab, &prog, &rig.ctx(class)).await;
        println!("row19 {prog} under {class:?}: {res:?}");
        assert!(
            res.stopped.is_none(),
            "a stale target is not a gate stop: {res:?}"
        );
        assert_eq!(res.steps.len(), 1, "{res:?}");
        assert!(
            matches!(&res.steps[0].outcome, StepOutcome::Failed { why } if why == "stale" || why == "gone"),
            "{res:?}"
        );
    }
    // A removed element in the live document: gone, a failed step.
    let p = r(d, tab, "p").await;
    page_js(d, tab, "document.querySelector('p').remove()").await;
    let res = act(d, tab, &format!("c{p}"), &rig.ctx(Class::Fill)).await;
    println!("row19 removed element: {res:?}");
    assert!(res.stopped.is_none(), "{res:?}");
    assert!(
        matches!(&res.steps[0].outcome, StepOutcome::Failed { why } if why == "gone" || why == "stale"),
        "{res:?}"
    );
    assert_eq!(page_js(d, tab, "location.pathname").await, "/welcome");
}
