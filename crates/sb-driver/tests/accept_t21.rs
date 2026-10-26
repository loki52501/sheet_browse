//! T-21 against the real D-34 build (chromium-1248), through the public API: docs/05
//! acceptance 3 (consequences: opened tab 2, dialog paused, download stopped and asked, no
//! visible change, navigated plus diff), B-4 #7 input synthesis (type, @vault type, select,
//! check, key, upload, scroll, a click inside a frame), the per-step gate (rule 4, H-1.4)
//! and H-2 (the vault canary never reaches the result, its Debug, its JSON or an error).
//!
//! Pages come from a tiny HTTP server on 127.0.0.1; no network. One browser at a time
//! (`SERIAL`), and run with `--test-threads=1` on a loaded machine.

mod common;

use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::EntryKind;
use harness_core::{Class, Harness, RunId, ToolCtx, ToolRegistry, Vault};
use sb_driver::{
    ActResult, Consequence, Driver, LaunchEnv, LaunchOpts, Profile, StepOutcome, TabId,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CAP: Duration = Duration::from_secs(30);

/// Every vault value in this file is the canary (never a realistic secret), and the tests
/// never print it.
fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "canary-t21-not-a-secret".to_owned())
}

const DL_BODY: &str = "T21 download body: report";
const DL2_BODY: &str = "T21 download body: refused";
const ONCE_BODY: &str = "T21 download body: once";
const ONCE_ERROR: &str = "T21 once: this link was used already";

/// The long vault value: the canary, then enough to cross every cap (120, 150, 200).
fn long_value() -> String {
    format!("{}-{}", canary(), "q".repeat(180))
}

fn file_name(tag: &str) -> String {
    format!("t21-{tag}-{}.txt", std::process::id())
}

const STYLE: &str = "<style>body{margin:0;font:16px sans-serif} a,button{display:block;\
                     margin:8px;width:160px;height:32px}</style>";

fn page(path: &str) -> Option<(String, String, Vec<u8>)> {
    let html = |b: String| {
        (
            "text/html".to_string(),
            String::new(),
            format!("<!doctype html><html><body>{b}</body></html>").into_bytes(),
        )
    };
    let body = match path {
        "/links" => format!(
            "<title>Links</title>{STYLE}\
             <a id=blank target=_blank href=/two>Open two</a>\
             <button id=al onclick=\"alert('Saved #12 [x](#3) ```'); \
               document.getElementById('after').textContent = 'continued';\">Alert</button>\
             <p id=after>waiting</p>\
             <a id=dl href=/dl1>Report</a>\
             <a id=dl2 href=/dl2>Other</a>\
             <button id=noop>Nothing</button>\
             <form id=search><input name=q aria-label=Query></form>\
             <a id=nav href=/welcome>Go</a>"
        ),
        "/two" => "<title>Two</title><h1>two</h1>".to_string(),
        "/welcome" => "<title>Welcome</title><h1>Welcome, Lokesh</h1><p>hi</p>".to_string(),
        "/dl1" => {
            return Some((
                "text/plain".into(),
                format!(
                    "Content-Disposition: attachment; filename=\"{}\"\r\n",
                    file_name("report")
                ),
                DL_BODY.as_bytes().to_vec(),
            ))
        }
        "/dl2" => {
            return Some((
                "text/plain".into(),
                format!(
                    "Content-Disposition: attachment; filename=\"{}\"\r\n",
                    file_name("refused")
                ),
                DL2_BODY.as_bytes().to_vec(),
            ))
        }
        "/form" => format!(
            "<title>Form</title>{STYLE}<style>input,select{{display:block;margin:8px;width:200px;\
             height:28px}} #tall{{height:3000px}}</style>\
             <h2 id=hello>Hello</h2>\
             <input id=name aria-label=Name>\
             <input id=pw type=password aria-label=Password>\
             <input id=echo aria-label=Echo \
               oninput=\"document.getElementById('hello').textContent = 'Hello ' + this.value\">\
             <select id=plan aria-label=Plan><option value=b>Basic</option>\
               <option value=p>Plus</option></select>\
             <input type=checkbox id=agree><label for=agree>Agree</label>\
             <input id=k aria-label=Keys>\
             <input type=file id=f aria-label=CV>\
             <p id=log></p>\
             <div id=tall></div><p id=bottom>bottom</p>\
             <script>\
               const log = (s) => {{ document.getElementById('log').textContent += s + ';'; }};\
               document.getElementById('plan').addEventListener('change', () => log('change'));\
               document.getElementById('plan').addEventListener('input', () => log('input'));\
               document.getElementById('k').addEventListener('keydown', (e) => log('down:' + e.key));\
               document.getElementById('f').addEventListener('change', () => log('file'));\
             </script>"
        ),
        "/gate" => format!(
            "<title>Gate</title>{STYLE}<style>input{{display:block;margin:8px;width:200px;\
             height:28px}} .g-recaptcha{{width:300px;height:78px;background:#eee}}</style>\
             <form id=reg onsubmit=\"event.preventDefault(); window.submits = (window.submits||0) + 1;\
               document.title = 'submitted';\">\
             <input id=user aria-label=User>\
             <button id=sub type=submit>Register</button></form>\
             <div id=cap class=g-recaptcha data-sitekey=x \
               onclick=\"document.title = 'captcha clicked'\"></div>\
             <script>window.keys = 0; \
               document.addEventListener('keydown', () => {{ window.keys += 1; }}, true);</script>"
        ),
        // A page that echoes a typed value into a heading (the index cuts it at 150 with
        // `…`), the URL (history API), a new tab's title and an alert.
        "/echo" => format!(
            "<title>Echo</title>{STYLE}<style>input{{display:block;margin:8px;width:200px;\
             height:28px}}</style>\
             <h1 id=h>Hello</h1>\
             <input id=e aria-label=Echo oninput=\"document.getElementById('h').textContent = \
               this.value; history.replaceState(null, '', '/echo?pad=' + 'p'.repeat(40) + \
               '&v=' + encodeURIComponent(this.value));\">\
             <button id=open onclick=\"window.open('/titled')\">Open</button>\
             <button id=al onclick=\"alert(document.getElementById('e').value)\">Alert</button>"
        ),
        "/titled" => "<title>Titled</title><h1>t</h1><script>\
             document.title = 'Hi ' + opener.document.getElementById('e').value;</script>"
            .to_string(),
        // Focus already on an element inside the target (rule 4, D-615).
        "/focus" => format!(
            "<title>Focus</title>{STYLE}<style>input{{display:block;margin:8px;width:200px;\
             height:28px}}</style>\
             <div id=dlg role=dialog aria-modal=true aria-label=\"Confirm payment\">\
               <p>Pay 10 EUR?</p>\
               <button id=pay onclick=\"window.pays = (window.pays || 0) + 1\">Pay now</button>\
             </div>\
             <div id=box role=group aria-label=Login>\
               <input id=secret type=password aria-label=Secret></div>\
             <input id=plain aria-label=Plain>\
             <script>window.keys = 0; \
               document.addEventListener('keydown', () => {{ window.keys += 1; }}, true); \
               document.getElementById('pay').focus();</script>"
        ),
        // A dialog from a timer while the next step waits for a hidden element.
        "/timer" => format!(
            "<title>Timer</title>{STYLE}\
             <button id=t onclick=\"setTimeout(() => alert('timer'), 600)\">Arm</button>\
             <button id=hid style=\"display:none\">Hidden</button>"
        ),
        "/once" => format!("<title>Once</title>{STYLE}<a id=dl href=/dl-once>Export</a>"),
        "/dl-once" => {
            return Some((
                "text/plain".into(),
                format!(
                    "Content-Disposition: attachment; filename=\"{}\"\r\n",
                    file_name("once")
                ),
                ONCE_BODY.as_bytes().to_vec(),
            ))
        }
        "/get" => format!(
            "<title>Get</title>{STYLE}<style>input{{display:block;margin:8px;width:200px;\
             height:28px}}</style>\
             <form id=gf action=/welcome method=get><input id=q name=q aria-label=Query></form>"
        ),
        "/challenge" => format!("<title>Just a moment...</title>{STYLE}<button id=b>Go</button>"),
        "/framed" => "<title>Framed</title><style>body{margin:0}</style>\
             <button id=miss style=\"position:absolute;left:0;top:0;width:180px;height:120px\" \
               onclick=\"document.getElementById('r').textContent = 'miss'\">Parent</button>\
             <iframe id=fr style=\"position:absolute;left:200px;top:150px;width:300px;\
               height:200px;border:5px solid #000;padding:7px\" srcdoc=\"<body style='margin:0'>\
               <button id=b style='position:absolute;left:10px;top:10px;width:80px;height:30px' \
               onclick='parent.document.getElementById(&quot;r&quot;).textContent = &quot;hit&quot;'>\
               In</button></body>\"></iframe>\
             <p id=r style=\"position:absolute;top:400px\">none</p>"
            .to_string(),
        _ => return None,
    };
    Some(html(body))
}

/// Requests per path, to show a refused download is not fetched again.
static HITS: AtomicUsize = AtomicUsize::new(0);
/// Requests to the one-time download link.
static ONCE: AtomicUsize = AtomicUsize::new(0);

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
                let path = path.split('?').next().unwrap_or("/").to_string();
                if path == "/dl2" {
                    HITS.fetch_add(1, Ordering::SeqCst);
                }
                // A one-time link: the second request gets an error page.
                let used = path == "/dl-once" && ONCE.fetch_add(1, Ordering::SeqCst) > 0;
                let (status, ct, extra, body) = match page(&path) {
                    _ if used => (
                        "410 Gone",
                        "text/plain".into(),
                        String::new(),
                        ONCE_ERROR.as_bytes().to_vec(),
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
}

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl Rig {
    fn url(&self, p: &str) -> String {
        format!("http://127.0.0.1:{}{p}", self.port)
    }

    fn run_dir(&self) -> PathBuf {
        self.home.path().join("runs").join("accept-t21")
    }

    fn files(&self) -> PathBuf {
        self.run_dir().join("files")
    }

    fn ctx(&self, class: Class) -> ToolCtx {
        ToolCtx::new(
            RunId("accept-t21".into()),
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
    let port = serve().await;
    let vault = Vault::in_memory([
        ("pw".to_string(), canary(), EntryKind::Secret),
        ("long".to_string(), long_value(), EntryKind::Personal),
    ]);
    let harness = Harness {
        store: Arc::new(MemStore::new()),
        vault: Arc::new(vault),
        asker: Arc::new(ScriptedAsker::new()),
        runs_root: home.path().join("runs"),
        run: None,
    };
    Rig {
        d,
        _serial: serial,
        port,
        home,
        harness,
    }
}

/// `decl(args)` in the frame's `sb` world as the registry knows it now (no driver call).
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
    assert!(v.get("exceptionDetails").is_none(), "sb_eval threw: {v}");
    v.pointer("/result/value").cloned().unwrap_or(Value::Null)
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

/// `#N` in frame 0, `#fF.N` in frame F.
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

/// Every file under `dir` whose bytes are `body`.
fn files_with(dir: &Path, body: &[u8]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_with(&p, body));
        } else if std::fs::read(&p).is_ok_and(|b| b == body) {
            out.push(p);
        }
    }
    out
}

/// The terminal state the browser reports for download `guid` (`Browser.downloadProgress`):
/// `canceled` when it was stopped, `completed` when it landed. Capped at 10 s.
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

/// No copy of the download in the test's home (profile, runs) nor in the user's Downloads.
fn nowhere_on_disk(home: &Path, body: &str, name: &str) {
    let found = files_with(home, body.as_bytes());
    assert!(
        found.is_empty(),
        "the file landed before the answer: {found:?}"
    );
    for root in ["USERPROFILE", "HOME"] {
        if let Some(h) = std::env::var_os(root) {
            let p = PathBuf::from(h).join("Downloads").join(name);
            assert!(!p.exists(), "the file landed in {}", p.display());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acceptance_3_consequences() {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let tab = d.open(&rig.url("/links")).await.expect("open /links");
    assert_eq!(tab, TabId(1), "the first opened tab is tab 1 (D-610)");

    // (a) target=_blank: "opened tab 2" in the same result.
    let blank = r(d, tab, "#blank").await;
    let res = act(d, tab, &format!("c{blank}"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    // The popup loaded with its runtime (D-612): it has an sb world with __sb.
    let has_sb = sb_eval(
        d,
        TabId(2),
        0,
        "function () { return !!globalThis.__sb; }",
        &[],
    )
    .await;
    assert_eq!(has_sb, json!(true), "the popup's first document has __sb");
    let opened: Vec<&Consequence> = res.steps[0]
        .consequences
        .iter()
        .filter(|c| matches!(c, Consequence::OpenedTab { .. }))
        .collect();
    assert_eq!(
        opened,
        [&Consequence::OpenedTab {
            tab: TabId(2),
            title: "Two".into()
        }],
        "{res:?}"
    );
    assert_eq!(d.with_registry(|r| r.opener(TabId(2))), Some(tab));

    // (d) a click that does nothing: "no visible change", and no diff.
    let noop = r(d, tab, "#noop").await;
    let res = act(d, tab, &format!("c{noop}"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(
        res.steps[0].consequences,
        [Consequence::NoVisibleChange],
        "{res:?}"
    );
    assert_eq!(res.diff, None);

    // (b) alert(): "dialog", the page paused; later steps do not run; dialog() resumes it.
    let al = r(d, tab, "#al").await;
    let res = act(d, tab, &format!("c{al}; c{noop}"), &ctx).await;
    assert_eq!(
        res.steps.len(),
        1,
        "the program stops at the dialog: {res:?}"
    );
    assert_eq!(res.steps[0].outcome, StepOutcome::Ok);
    let Some(Consequence::Dialog { text }) = res.steps[0].consequences.last() else {
        panic!("no dialog: {res:?}");
    };
    // Page text is escaped: no link, ref or fence syntax survives.
    assert_eq!(text, "Saved ＃12 (x)(＃3) '''");
    // While it is open, an act answers with it instead of hanging.
    let blocked = act(d, tab, &format!("c{noop}"), &ctx).await;
    assert!(
        matches!(blocked.steps[0].outcome, StepOutcome::Failed { ref why } if why.contains("dialog")),
        "{blocked:?}"
    );
    d.dialog(tab, true, None).await.expect("accept the alert");
    let after = page_js(d, tab, "document.getElementById('after').textContent").await;
    assert_eq!(
        after, "continued",
        "the page continued after dialog(accept)"
    );
    assert!(
        d.dialog(tab, true, None).await.is_err(),
        "nothing left to answer"
    );

    // (c) a download: stopped and asked; nothing on disk before download(allow=true).
    let dl = r(d, tab, "#dl").await;
    let mut events = d.cdp().events();
    let res = act(d, tab, &format!("c{dl}"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    let Some(Consequence::Download { name, guid }) = res.steps[0]
        .consequences
        .iter()
        .find(|c| matches!(c, Consequence::Download { .. }))
        .cloned()
    else {
        panic!("no download: {res:?}");
    };
    assert_eq!(name, file_name("report"));
    assert!(
        !res.steps[0]
            .consequences
            .iter()
            .any(|c| matches!(c, Consequence::Navigated { .. })),
        "a download is not a navigation: {res:?}"
    );
    // The browser's own last word on it first (a condition, capped), then the disk.
    assert_eq!(download_end(&mut events, &guid).await, "canceled");
    nowhere_on_disk(rig.home.path(), DL_BODY, &name);
    let saved = d
        .download(tab, &guid, true)
        .await
        .expect("download allowed")
        .expect("a path");
    assert_eq!(saved.parent(), Some(rig.files().as_path()));
    assert_eq!(std::fs::read(&saved).unwrap(), DL_BODY.as_bytes());
    assert_eq!(saved.file_name().unwrap().to_str(), Some(name.as_str()));
    assert!(d.download(tab, &guid, true).await.is_err(), "answered once");
    // allow=false cancels: never on disk, never fetched again.
    let dl2 = r(d, tab, "#dl2").await;
    let mut events = d.cdp().events();
    let res = act(d, tab, &format!("c{dl2}"), &ctx).await;
    let Some(Consequence::Download { guid: g2, name: n2 }) = res.steps[0]
        .consequences
        .iter()
        .find(|c| matches!(c, Consequence::Download { .. }))
        .cloned()
    else {
        panic!("no download: {res:?}");
    };
    let hits = HITS.load(Ordering::SeqCst);
    assert_eq!(d.download(tab, &g2, false).await.expect("refuse"), None);
    assert_eq!(download_end(&mut events, &g2).await, "canceled");
    nowhere_on_disk(rig.home.path(), DL2_BODY, &n2);
    assert_eq!(
        HITS.load(Ordering::SeqCst),
        hits,
        "a refused download is not fetched"
    );

    // (e) a navigating click: "navigated to" and the diff.
    let nav = r(d, tab, "#nav").await;
    let res = act(d, tab, &format!("c{nav}"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    assert!(
        res.steps[0].consequences.contains(&Consequence::Navigated {
            url: rig.url("/welcome")
        }),
        "{res:?}"
    );
    let diff = res.diff.clone().expect("a diff after navigating");
    assert!(diff.starts_with("+ heading \"Welcome, Lokesh\""), "{diff}");
    assert!(diff.contains("\u{2212} form f1"), "{diff}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typing_select_check_key_upload_scroll() {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let tab = d.open(&rig.url("/form")).await.expect("open /form");
    let value = |sel: &str| format!("document.querySelector('{sel}').value");

    // Plain typing lands, and a second t# replaces (Ctrl+A, insertText).
    let name = r(d, tab, "#name").await;
    let res = act(d, tab, &format!("t{name}=hello world"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(page_js(d, tab, &value("#name")).await, "hello world");
    assert!(
        !res.steps[0]
            .consequences
            .contains(&Consequence::NoVisibleChange),
        "a typed value is a change: {res:?}"
    );
    let res = act(d, tab, &format!("t{name}=second"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(page_js(d, tab, &value("#name")).await, "second");

    // @vault(pw) lands in the password field; the canary is nowhere in the result.
    let pw = r(d, tab, "#pw").await;
    let res = act(d, tab, &format!("t{pw}=@vault(pw)"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    let same = sb_eval(
        d,
        tab,
        0,
        "function (c) { return document.querySelector('#pw').value === c; }",
        &[json!(canary())],
    )
    .await;
    assert_eq!(same, json!(true), "the vault value was typed");
    // A page that echoes the value (a greeting): the result is scrubbed to the literal.
    let echo = r(d, tab, "#echo").await;
    let res = act(d, tab, &format!("t{echo}=@vault(pw)"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    let echoed = sb_eval(
        d,
        tab,
        0,
        "function (c) { return document.querySelector('#hello').textContent === 'Hello ' + c; }",
        &[json!(canary())],
    )
    .await;
    assert_eq!(echoed, json!(true), "the page echoed the value");
    let dbg = format!("{res:?}");
    let json = serde_json::to_string(&res.steps).unwrap();
    assert!(!dbg.contains(&canary()), "canary in Debug");
    assert!(!json.contains(&canary()), "canary in JSON");
    assert!(
        res.diff
            .as_deref()
            .is_some_and(|d| d.contains("@vault(pw)")),
        "the echo is shown as the literal: {:?}",
        res.diff
    );
    // An unknown key types nothing and the error names the key only.
    let res = act(d, tab, &format!("t{name}=@vault(missing)"), &ctx).await;
    assert!(
        matches!(&res.steps[0].outcome, StepOutcome::Failed { why } if why.contains("missing")),
        "{res:?}"
    );
    assert_eq!(page_js(d, tab, &value("#name")).await, "second");

    // Select by label: value set, input and change dispatched.
    let plan = r(d, tab, "#plan").await;
    let res = act(d, tab, &format!("s{plan}=Plus"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(page_js(d, tab, &value("#plan")).await, "p");
    let res = act(d, tab, &format!("s{plan}=Gold"), &ctx).await;
    assert!(
        matches!(&res.steps[0].outcome, StepOutcome::Failed { why } if why.starts_with("no option")),
        "{res:?}"
    );

    // Check is a click; the diff shows it.
    let agree = r(d, tab, "#agree").await;
    let res = act(d, tab, &format!("c{agree}"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(
        page_js(d, tab, "document.querySelector('#agree').checked").await,
        true
    );
    assert!(
        res.diff.as_deref().is_some_and(|d| d.contains("checked")),
        "{res:?}"
    );

    // Key: Enter reaches the focused field.
    let k = r(d, tab, "#k").await;
    let res = act(d, tab, &format!("k{k}=Enter"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    let res2 = act(d, tab, &format!("k{k}=Unknown+Key"), &ctx).await;
    assert!(
        matches!(&res2.steps[0].outcome, StepOutcome::Failed { why } if why.starts_with("unknown key")),
        "{res2:?}"
    );

    // Upload: inside the run's files folder only.
    std::fs::create_dir_all(rig.files()).unwrap();
    std::fs::write(rig.files().join("cv.txt"), b"cv").unwrap();
    std::fs::write(rig.run_dir().join("secret.txt"), b"s").unwrap();
    let f = r(d, tab, "#f").await;
    let res = act(d, tab, &format!("u{f}=../secret.txt"), &ctx).await;
    assert!(
        matches!(&res.steps[0].outcome, StepOutcome::Failed { why } if why.starts_with("upload refused")),
        "{res:?}"
    );
    assert_eq!(
        page_js(d, tab, "document.querySelector('#f').files.length").await,
        0
    );
    let res = act(d, tab, &format!("u{f}=files/cv.txt"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(
        page_js(d, tab, "document.querySelector('#f').files[0].name").await,
        "cv.txt"
    );
    let log = page_js(d, tab, "document.getElementById('log').textContent").await;
    assert_eq!(log, "input;change;down:Enter;file;", "events in order");

    // Scroll: down, back up, and to an element.
    let res = act(d, tab, "sc(down)", &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    let y = page_js(d, tab, "scrollY").await.as_f64().unwrap();
    assert!(y > 100.0, "scrolled down: {y}");
    let res = act(d, tab, "sc(up); sc(up); sc(up); sc(up); sc(up)", &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(page_js(d, tab, "scrollY").await.as_f64(), Some(0.0));
    assert_eq!(
        res.steps[4].consequences,
        [Consequence::NoVisibleChange],
        "already at the top: {res:?}"
    );
    let bottom = r(d, tab, "#bottom").await;
    let res = act(d, tab, &format!("sc({bottom})"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    let seen = page_js(
        d,
        tab,
        "(() => { const b = document.getElementById('bottom').getBoundingClientRect(); \
         return b.top >= 0 && b.bottom <= innerHeight; })()",
    )
    .await;
    assert_eq!(seen, true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gate_stops_a_step_before_its_input() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/gate")).await.expect("open /gate");
    let user = r(d, tab, "#user").await;
    let sub = r(d, tab, "#sub").await;
    let cap = r(d, tab, "#cap").await;

    // ctx.class Fill: the Submit step stops before its click.
    let fill = rig.ctx(Class::Fill);
    let res = act(d, tab, &format!("t{user}=me; c{sub}; t{user}=again"), &fill).await;
    assert_eq!(res.steps.len(), 2, "nothing after the stop runs: {res:?}");
    assert_eq!(res.steps[0].outcome, StepOutcome::Ok);
    let stop = res.stopped.clone().expect("stopped");
    assert_eq!(stop.class, Class::Submit);
    assert_eq!(stop.element.as_deref(), Some(sub.as_str()));
    assert_eq!(page_js(d, tab, "window.submits || 0").await, 0, "no submit");
    assert_eq!(
        page_js(d, tab, "document.querySelector('#user').value").await,
        "me"
    );

    // A CHALLENGE row stops whatever the approved class.
    let pay = rig.ctx(Class::Pay);
    let res = act(d, tab, &format!("c{cap}"), &pay).await;
    let stop = res.stopped.clone().expect("stopped");
    assert_eq!(stop.class, Class::Challenge, "{res:?}");
    assert_eq!(page_js(d, tab, "document.title").await, "Gate", "no click");

    // With Submit approved the same click runs.
    let submit = rig.ctx(Class::Submit);
    let res = act(d, tab, &format!("c{sub}"), &submit).await;
    assert!(ok_steps(&res) && res.stopped.is_none(), "{res:?}");
    assert_eq!(page_js(d, tab, "window.submits || 0").await, 1);

    // A challenge page: stopped before any input.
    let ch = d
        .open(&rig.url("/challenge"))
        .await
        .expect("open /challenge");
    let res = act(d, ch, "sc(down)", &pay).await;
    assert_eq!(
        res.stopped.as_ref().map(|s| s.class),
        Some(Class::Challenge),
        "{res:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_click_inside_a_frame_lands_at_the_frame_offset() {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let tab = d.open(&rig.url("/framed")).await.expect("open /framed");
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
        .expect("the srcdoc frame with its sb world");
    // The driver walks a frame on its first call; the test needs an id first, so it walks
    // with the base the driver gives a frame's first document (D-604), which it then keeps.
    sb_eval(
        d,
        tab,
        frame,
        "function () { return globalThis.__sb.walk({ base: 1 }); }",
        &[],
    )
    .await;
    let b = id_of(d, tab, frame, "#b").await;
    let res = act(d, tab, &format!("c#f{frame}.{b}"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(
        page_js(d, tab, "document.getElementById('r').textContent").await,
        "hit"
    );
    // The step changed the parent only: a frame step never claims no change (D-617).
    assert!(
        !res.steps[0]
            .consequences
            .contains(&Consequence::NoVisibleChange),
        "{res:?}"
    );
}

/// Every string form of a result that can leave: Debug, the JSON of its steps and stop,
/// the diff and each consequence.
fn forms_of(res: &ActResult) -> Vec<String> {
    let mut out = vec![
        format!("{res:?}"),
        serde_json::to_string(&res.steps).unwrap(),
        serde_json::to_string(&res.stopped).unwrap(),
        res.diff.clone().unwrap_or_default(),
    ];
    for s in &res.steps {
        for c in &s.consequences {
            out.push(serde_json::to_string(c).unwrap());
        }
    }
    out
}

/// Neither the canary nor any 8-character prefix of the long value is in any form.
fn no_long_value(res: &ActResult, what: &str) {
    let v = long_value();
    let prefix: String = v.chars().take(8).collect();
    for f in forms_of(res) {
        assert!(
            !f.contains(&canary()),
            "{what}: the canary is in the result"
        );
        assert!(
            !f.contains(&prefix),
            "{what}: a prefix of the value is in the result"
        );
    }
}

/// Rule 2 (review T-21 Blocking 1): a typed value longer than every cap, echoed into a
/// heading (cut by the index at 150 with `…` and by the diff at 60), the URL (cut at 200),
/// a new tab's title and an alert (cut at 120), never reaches the result whole or cut.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_echo_never_leaves_whole_or_cut() {
    let rig = rig().await;
    let d = &rig.d;
    let ctx = rig.ctx(Class::Submit);
    let tab = d.open(&rig.url("/echo")).await.expect("open /echo");
    let e = r(d, tab, "#e").await;
    let open = r(d, tab, "#open").await;
    let res = act(d, tab, &format!("t{e}=@vault(long); c{open}"), &ctx).await;
    assert!(ok_steps(&res), "{res:?}");
    // The page did echo it: in the heading and the URL (the new tab's title is below).
    let echoed = sb_eval(
        d,
        tab,
        0,
        "function (v) { return document.getElementById('h').textContent === v && \
         location.href.endsWith('&v=' + encodeURIComponent(v)); }",
        &[json!(long_value())],
    )
    .await;
    assert_eq!(echoed, json!(true), "the page echoed the value");
    no_long_value(&res, "heading, URL, title");
    // Each echo is shown as the literal, so the model still reads what happened.
    let diff = res.diff.clone().unwrap_or_default();
    assert!(diff.contains("heading \"@vault(long)"), "diff: {diff}");
    let nav = res.steps[0].consequences.iter().find_map(|c| match c {
        Consequence::Navigated { url } => Some(url.clone()),
        _ => None,
    });
    assert!(
        nav.as_deref()
            .is_some_and(|u| u.ends_with("&v=@vault(long)")),
        "{nav:?}"
    );
    let title = res.steps[1].consequences.iter().find_map(|c| match c {
        Consequence::OpenedTab { title, .. } => Some(title.clone()),
        _ => None,
    });
    assert_eq!(title.as_deref(), Some("Hi @vault(long)"));

    // An alert with the value: the dialog text is cut at 120.
    let al = r(d, tab, "#al").await;
    let res = act(d, tab, &format!("t{e}=@vault(long); c{al}"), &ctx).await;
    let text = res.steps[1].consequences.iter().find_map(|c| match c {
        Consequence::Dialog { text } => Some(text.clone()),
        _ => None,
    });
    assert_eq!(text.as_deref(), Some("@vault(long)"), "{res:?}");
    no_long_value(&res, "alert");
    d.dialog(tab, true, None).await.expect("accept");
}

/// Rule 4 (review T-21 Blocking 2, D-615): a key or type step acts only on the element the
/// gate classified, never on a descendant that already holds focus.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_never_reaches_a_focused_descendant() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/focus")).await.expect("open /focus");
    assert_eq!(
        page_js(d, tab, "document.activeElement.id").await,
        "pay",
        "the dialog's Pay button holds focus"
    );
    // Enter on the dialog row: the gate classifies the dialog (Navigate at most), so the Pay
    // button inside it must not get the key.
    let dlg = r(d, tab, "#dlg").await;
    let res = act(d, tab, &format!("k{dlg}=Enter"), &rig.ctx(Class::Navigate)).await;
    assert!(
        res.stopped.is_none(),
        "the gate passed the dialog row: {res:?}"
    );
    assert!(
        matches!(&res.steps[0].outcome, StepOutcome::Failed { why } if why.contains("did not take focus")),
        "{res:?}"
    );
    assert_eq!(
        page_js(d, tab, "window.pays || 0").await,
        0,
        "Pay not pressed"
    );
    assert_eq!(
        page_js(d, tab, "window.keys").await,
        0,
        "no key event at all"
    );

    // Typing into a container while its password field holds focus types nothing.
    page_js(d, tab, "document.getElementById('secret').focus()").await;
    let boxed = r(d, tab, "#box").await;
    let res = act(d, tab, &format!("t{boxed}=x"), &rig.ctx(Class::Pay)).await;
    assert!(res.stopped.is_none(), "{res:?}");
    assert!(
        matches!(&res.steps[0].outcome, StepOutcome::Failed { why } if why.contains("did not take focus")),
        "{res:?}"
    );
    assert_eq!(
        page_js(d, tab, "document.getElementById('secret').value === ''").await,
        true,
        "nothing typed into the password field"
    );
    assert_eq!(page_js(d, tab, "window.keys").await, 0);

    // The element itself still takes focus and gets the input.
    let plain = r(d, tab, "#plain").await;
    let res = act(d, tab, &format!("t{plain}=ok"), &rig.ctx(Class::Fill)).await;
    assert!(ok_steps(&res), "{res:?}");
    assert_eq!(
        page_js(d, tab, "document.getElementById('plain').value").await,
        "ok"
    );
}

/// Review T-21 Should-fix 6: Enter in a form field under Fill and `u#` under Read stop at
/// the gate before any input; a stale id fails the step instead of asking for Submit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_for_enter_upload_and_stale_targets() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/gate")).await.expect("open /gate");
    let user = r(d, tab, "#user").await;
    let res = act(d, tab, &format!("k{user}=Enter"), &rig.ctx(Class::Fill)).await;
    let stop = res.stopped.clone().expect("stopped");
    assert_eq!(
        stop.class,
        Class::Submit,
        "Enter in a form is Submit: {res:?}"
    );
    assert_eq!(page_js(d, tab, "window.keys").await, 0, "no key event");
    assert_eq!(page_js(d, tab, "window.submits || 0").await, 0);
    // An upload on a CHALLENGE row stops whatever the approval.
    std::fs::create_dir_all(rig.files()).unwrap();
    std::fs::write(rig.files().join("cv.txt"), b"cv").unwrap();
    let cap = r(d, tab, "#cap").await;
    let res = act(d, tab, &format!("u{cap}=cv.txt"), &rig.ctx(Class::Pay)).await;
    assert_eq!(
        res.stopped.as_ref().map(|s| s.class),
        Some(Class::Challenge),
        "{res:?}"
    );

    let form = d.open(&rig.url("/form")).await.expect("open /form");
    let f = r(d, form, "#f").await;
    let res = act(d, form, &format!("u{f}=cv.txt"), &rig.ctx(Class::Read)).await;
    let stop = res.stopped.clone().expect("u# is gated");
    assert_eq!(stop.class, Class::Fill, "{res:?}");
    assert_eq!(
        page_js(d, form, "document.querySelector('#f').files.length").await,
        0
    );
    let res = act(d, form, &format!("u{f}=cv.txt"), &rig.ctx(Class::Fill)).await;
    assert!(ok_steps(&res), "{res:?}");

    // A stale id after a navigation: a failed step, not a Submit stop.
    let links = d.open(&rig.url("/links")).await.expect("open /links");
    let noop = r(d, links, "#noop").await;
    let nav = r(d, links, "#nav").await;
    let res = act(d, links, &format!("c{nav}"), &rig.ctx(Class::Navigate)).await;
    assert!(ok_steps(&res), "{res:?}");
    let res = act(d, links, &format!("c{noop}"), &rig.ctx(Class::Fill)).await;
    assert!(res.stopped.is_none(), "not a stop: {res:?}");
    assert_eq!(
        res.steps[0].outcome,
        StepOutcome::Failed {
            why: "stale".into()
        }
    );
}

/// Review T-21 Should-fix 1: a dialog a timer opens while a later step waits is reported at
/// once, not after the 5 s actionability budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dialog_between_steps_is_reported_at_once() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/timer")).await.expect("open /timer");
    let t = r(d, tab, "#t").await;
    let hid = r(d, tab, "#hid").await;
    let t0 = std::time::Instant::now();
    let res = act(d, tab, &format!("c{t}; c{hid}; c{t}"), &rig.ctx(Class::Pay)).await;
    let took = t0.elapsed();
    assert!(res.stopped.is_none(), "{res:?}");
    let at = res
        .steps
        .iter()
        .position(|s| {
            s.consequences.contains(&Consequence::Dialog {
                text: "timer".into(),
            })
        })
        .unwrap_or_else(|| panic!("no dialog: {res:?}"));
    assert_eq!(at + 1, res.steps.len(), "nothing runs after it: {res:?}");
    assert!(took < Duration::from_millis(2500), "{took:?}: {res:?}");
    d.dialog(tab, true, None).await.expect("accept");
}

/// Review T-21 Should-fix 2: a download whose second request answers an error status is
/// not saved under the download's name.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refetch_with_an_error_status_is_not_saved() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/once")).await.expect("open /once");
    let dl = r(d, tab, "#dl").await;
    let mut events = d.cdp().events();
    let res = act(d, tab, &format!("c{dl}"), &rig.ctx(Class::Submit)).await;
    let Some(Consequence::Download { guid, .. }) = res.steps[0]
        .consequences
        .iter()
        .find(|c| matches!(c, Consequence::Download { .. }))
        .cloned()
    else {
        panic!("no download: {res:?}");
    };
    assert_eq!(download_end(&mut events, &guid).await, "canceled");
    let err = d
        .download(tab, &guid, true)
        .await
        .expect_err("an error page is not saved");
    // chromium-1248 reports a non-2xx answer as a failed load itself
    // (ERR_HTTP_RESPONSE_CODE_FAILURE); the driver also checks the status (defence in depth).
    let why = err.to_string();
    assert!(
        why.contains("410") || why.contains("RESPONSE_CODE"),
        "{why}"
    );
    assert!(files_with(&rig.files(), ONCE_ERROR.as_bytes()).is_empty());
    assert!(files_with(rig.home.path(), ONCE_ERROR.as_bytes()).is_empty());
}

/// Tester T-21 note: `tabs()` scrubs a URL that echoes a typed value (H-2, rule 2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tabs_never_return_a_typed_value() {
    let rig = rig().await;
    let d = &rig.d;
    let tab = d.open(&rig.url("/get")).await.expect("open /get");
    let q = r(d, tab, "#q").await;
    let res = act(
        d,
        tab,
        &format!("t{q}=@vault(pw); k{q}=Enter"),
        &rig.ctx(Class::Submit),
    )
    .await;
    assert!(ok_steps(&res), "{res:?}");
    // The page put it in the URL (the registry's raw copy, once it has the new URL; never
    // printed).
    let echoed = d
        .wait_registry(CAP, |r| {
            r.tabs()
                .into_iter()
                .any(|t| t.0 == tab && t.1.contains("/welcome?q="))
                .then(|| r.tabs().iter().any(|t| t.1.contains(&canary())))
        })
        .await;
    assert_eq!(
        echoed,
        Some(true),
        "the GET form echoed the value into the URL"
    );
    rig.harness.vault.install_global();
    let tabs = d.tabs().await.expect("tabs");
    for (_, url, title) in &tabs {
        assert!(!url.contains(&canary()) && !title.contains(&canary()));
    }
    assert!(
        tabs.iter().any(|t| t.1.contains("@vault(pw)")),
        "shown as the literal"
    );
    let frames = d.frames(tab).await.expect("frames");
    assert!(frames.iter().all(|f| !f.url.contains(&canary())));
}
