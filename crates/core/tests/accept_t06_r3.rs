//! T-06 acceptance, round 3 (tester): D-117. A vault value that coincides with a field
//! the harness generates (the year in `ts` and `run_id`, `duration_ms`, a `mode`, a
//! `class`, a key name, `ok`, `null`) does not block `Ledger::write`; the same values
//! inside a data field are still scrubbed; a value that survives the per-string scrub in
//! `server`, `tool`, `args`, `result_summary`, `consequences` or `why` (it spans JSON
//! glue or a JSON escape) still fails closed with nothing inserted and no jsonl line.
//! Plus the vault canary through a full ledger + report flow (docs/01 Acceptance) and the
//! Q-4/Q-5 report shape with a mid-call challenge stop. docs/01 H-2, H-3, H-4; docs/08
//! Q-4, Q-5; D-102, D-117.
//!
//! `SB_VAULT_CANARY=<unique> cargo test -p harness-core --test accept_t06_r3`
//! Optional `SB_ACCEPT_RUNS_ROOT=<dir>` keeps the run folders there for an external grep.
//! The process-wide vault differs per test, so every test holds `LOCK` while it runs.
//! No assertion message formats a vault value.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use chrono::Utc;
use harness_core::class::Class;
use harness_core::ledger::{read_jsonl, write_report, Ledger, LedgerError};
use harness_core::memory::{ts_now, LedgerEvent, LedgerMode, MemoryStore};
use harness_core::run::{RunFolder, RunId};
use harness_core::testing::MemStore;
use harness_core::vault::{EntryKind, Vault};
use serde_json::{json, Value};

/// Serialises the tests: each installs its own process-wide vault.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const YEAR: &str = "2026";
const DUR: &str = "48213";
const DUR_N: i32 = 48_213;
/// Spans an object key and its value in `args`: `{"r3ka":"r3va"}`.
const SPAN_ARGS: &str = "r3ka\":\"r3va";
/// Spans two array elements in `consequences`: `["r3ca","r3cb"]`.
const SPAN_CONS: &str = "r3ca\",\"r3cb";
/// A literal backslash-n: the raw string `r3e<LF>r3e` does not contain it, its JSON
/// serialization does.
const ESC: &str = "r3e\\nr3e";
const ESC_RAW: &str = "r3e\nr3e";

fn canary() -> &'static str {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| {
        std::env::var("SB_VAULT_CANARY")
            .ok()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| {
                format!(
                    "r3canary-{}-{}",
                    std::process::id(),
                    Utc::now().timestamp_micros()
                )
            })
    })
}

fn install(extra: &[(&str, &str)]) {
    let mut entries = vec![("accept".to_owned(), canary().to_owned(), EntryKind::Secret)];
    for (k, v) in extra {
        entries.push(((*k).to_owned(), (*v).to_owned(), EntryKind::Secret));
    }
    Vault::in_memory(entries).install_global();
}

/// The coincidence values D-117 names (plus `ok`, `null`, a class).
fn coincidence_vault() {
    install(&[
        ("year", YEAR),
        ("dur", DUR),
        ("mode", "allow"),
        ("mode2", "timeout_proceed"),
        ("cls", "Navigate"),
        ("key", "result_summary"),
        ("okv", "true"),
        ("nul", "null"),
    ]);
}

fn span_vault() {
    install(&[("sargs", SPAN_ARGS), ("scons", SPAN_CONS), ("esc", ESC)]);
}

/// `<root>/runs`: under `SB_ACCEPT_RUNS_ROOT/<name>` (kept) or a temp dir (removed).
struct Root {
    base: PathBuf,
    keep: bool,
}
impl Root {
    fn new(name: &str) -> Root {
        static N: AtomicUsize = AtomicUsize::new(0);
        match std::env::var("SB_ACCEPT_RUNS_ROOT") {
            Ok(r) if !r.is_empty() => Root {
                base: PathBuf::from(r).join(format!("r3-{name}")),
                keep: true,
            },
            _ => Root {
                base: std::env::temp_dir().join(format!(
                    "sb-accept-t06r3-{}-{}-{}-{}",
                    name,
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed),
                    Utc::now().timestamp_micros()
                )),
                keep: false,
            },
        }
    }
    fn runs(&self) -> PathBuf {
        self.base.join("runs")
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }
}

fn ev(run: &RunId, server: &str, tool: &str, class: Class, mode: LedgerMode) -> LedgerEvent {
    LedgerEvent {
        run_id: Some(run.clone()),
        ts: ts_now(),
        server: server.into(),
        tool: tool.into(),
        class: Some(class),
        mode: Some(mode),
        args: json!({}),
        result_summary: String::new(),
        consequences: json!([]),
        duration_ms: 2,
        ok: true,
        why: None,
    }
}

fn contains_ci(hay: &[u8], needle: &str) -> bool {
    let n = needle.as_bytes();
    !n.is_empty() && hay.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

fn all_files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(b) = std::fs::read(&p) {
                out.push((p, b));
            }
        }
    }
    out
}

fn jsonl_bytes(run: &RunFolder) -> Vec<u8> {
    std::fs::read(run.ledger_jsonl()).unwrap_or_default()
}

fn setup(name: &str) -> (Root, RunFolder, Arc<MemStore>, Ledger) {
    let root = Root::new(name);
    let run = RunFolder::create(&root.runs(), name, name).unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());
    (root, run, store, ledger)
}

// ---------------------------------------------------------------------------------------
// D-117: coincidences in harness-generated fields do not block writes

/// Year in `ts` and `run_id`, a duration equal to `duration_ms`, modes `allow` and
/// `timeout_proceed`, class `Navigate`, the key name `result_summary`, `ok: true` and a
/// `null` field: every write succeeds and the jsonl matches the table row-for-row.
#[tokio::test]
async fn r3_coincidence_in_harness_fields_does_not_block_write() {
    let _g = LOCK.lock().await;
    coincidence_vault();
    let (_root, run, store, ledger) = setup("coincide");
    assert!(run.id.0.contains(YEAR), "run id carries the year");

    let mut events = Vec::new();
    let mut a = ev(&run.id, "web", "fetch", Class::Read, LedgerMode::Allow);
    a.args = json!({ "url": "https://a.example/" });
    a.result_summary = "read 1 page".into();
    a.duration_ms = DUR_N;
    events.push(a);
    let mut b = ev(
        &run.id,
        "sheetbrowse",
        "nav.go",
        Class::Navigate,
        LedgerMode::TimeoutProceed,
    );
    b.args = json!({ "url": "https://b.example/" });
    b.consequences = Value::Null;
    b.duration_ms = DUR_N;
    events.push(b);
    let mut c = ev(
        &run.id,
        "core",
        "nav.go",
        Class::Navigate,
        LedgerMode::Allow,
    );
    c.args = Value::Null;
    c.consequences = Value::Null;
    events.push(c);
    // run_id None is filled with the ledger's own run (which contains the year).
    let mut d = ev(&run.id, "web", "fetch", Class::Read, LedgerMode::Allow);
    d.run_id = None;
    events.push(d);

    for (i, e) in events.into_iter().enumerate() {
        let res = ledger.write(e).await;
        if let Err(err) = &res {
            println!("event {i}: Ledger::write error: {err}");
        }
        assert!(res.is_ok(), "write {i} refused for a coincidence value");
    }
    let rows = store.ledger_events(&run.id).await.unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(read_jsonl(&run.ledger_jsonl()).unwrap(), rows);
    assert_eq!(rows[0].1.duration_ms, DUR_N);
    assert_eq!(rows[1].1.mode, Some(LedgerMode::TimeoutProceed));
    assert_eq!(rows[1].1.class, Some(Class::Navigate));
    assert_eq!(rows[3].1.run_id.as_ref(), Some(&run.id));
    // The coincidences are on the line, where D-117 does not check them.
    let raw = String::from_utf8(jsonl_bytes(&run)).unwrap();
    assert_eq!(raw.lines().count(), 4);
    for (what, v) in [
        ("year", YEAR),
        ("dur", DUR),
        ("mode", "\"allow\""),
        ("mode2", "\"timeout_proceed\""),
        ("cls", "\"Navigate\""),
        ("key", "\"result_summary\""),
        ("okv", "\"ok\":true"),
        ("nul", "null"),
    ] {
        assert!(raw.contains(v), "coincidence {what} not on the line");
    }
}

/// The same values inside data fields are still scrubbed (a number in args becomes the
/// `@vault(name)` string), and the write succeeds.
#[tokio::test]
async fn r3_coincidence_value_inside_data_field_is_still_scrubbed() {
    let _g = LOCK.lock().await;
    coincidence_vault();
    let (_root, run, store, ledger) = setup("coincide-data");
    let mut e = ev(&run.id, "web", "fetch", Class::Read, LedgerMode::Allow);
    e.args = json!({ "url": "https://a.example/", "y": 2026, "t": "took 48213 ms" });
    e.result_summary = "page says allow".into();
    e.consequences = json!(["year 2026"]);
    e.why = Some("Navigate then read".into());
    e.duration_ms = DUR_N;
    let res = ledger.write(e).await;
    if let Err(err) = &res {
        println!("Ledger::write error: {err}");
    }
    assert!(res.is_ok(), "write refused");
    let rows = store.ledger_events(&run.id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(read_jsonl(&run.ledger_jsonl()).unwrap(), rows);
    let r = &rows[0].1;
    assert_eq!(r.args["y"], json!("@vault(year)"));
    assert_eq!(r.args["t"], json!("took @vault(dur) ms"));
    assert_eq!(r.result_summary, "page says @vault(mode)");
    assert_eq!(r.consequences, json!(["year @vault(year)"]));
    assert_eq!(r.why.as_deref(), Some("@vault(cls) then read"));
}

// ---------------------------------------------------------------------------------------
// D-117: a value left in a data field fails closed, nothing written

/// Writes `build(run)` on a fresh run and checks: `Unscrubbable` naming exactly `fields`,
/// the error text free of every planted value, no row, no jsonl line; then a clean write
/// on the same ledger succeeds, the mirror matches the table, and no file under the run
/// root holds a planted value.
async fn fail_closed_case(name: &str, build: impl Fn(&RunId) -> LedgerEvent, fields: &str) {
    let (root, run, store, ledger) = setup(name);
    let res = ledger.write(build(&run.id)).await;
    match &res {
        Err(LedgerError::Unscrubbable { fields: got }) => {
            assert_eq!(got, fields, "{name}: wrong fields named");
        }
        Err(other) => panic!("{name}: expected Unscrubbable, got another error: {other}"),
        Ok(_) => panic!("{name}: write succeeded, expected Unscrubbable"),
    }
    let msg = format!("{}", res.as_ref().unwrap_err());
    for v in [SPAN_ARGS, SPAN_CONS, ESC, ESC_RAW, canary()] {
        assert!(!contains_ci(msg.as_bytes(), v), "{name}: value in error");
    }
    assert!(
        store.ledger_events(&run.id).await.unwrap().is_empty(),
        "{name}: row inserted"
    );
    assert!(jsonl_bytes(&run).is_empty(), "{name}: jsonl line written");

    let mut ok = ev(&run.id, "web", "fetch", Class::Read, LedgerMode::Allow);
    ok.args = json!({ "url": "https://ok.example/" });
    ledger.write(ok).await.unwrap();
    let rows = store.ledger_events(&run.id).await.unwrap();
    assert_eq!(rows.len(), 1, "{name}");
    assert_eq!(read_jsonl(&run.ledger_jsonl()).unwrap(), rows, "{name}");
    for (p, b) in all_files(&root.runs()) {
        for v in [SPAN_ARGS, SPAN_CONS, ESC, ESC_RAW, canary()] {
            assert!(!contains_ci(&b, v), "{name}: value in {}", p.display());
        }
    }
}

#[tokio::test]
async fn r3_fail_closed_args_key_value_span() {
    let _g = LOCK.lock().await;
    span_vault();
    fail_closed_case(
        "fc-args",
        |r| {
            let mut e = ev(r, "web", "form.fill", Class::Fill, LedgerMode::Allow);
            e.args = json!({ "r3ka": "r3va" });
            e
        },
        "args",
    )
    .await;
}

#[tokio::test]
async fn r3_fail_closed_consequences_element_span() {
    let _g = LOCK.lock().await;
    span_vault();
    fail_closed_case(
        "fc-cons",
        |r| {
            let mut e = ev(r, "files", "fs.write", Class::Fill, LedgerMode::Allow);
            e.consequences = json!(["r3ca", "r3cb"]);
            e
        },
        "consequences",
    )
    .await;
}

#[tokio::test]
async fn r3_fail_closed_result_summary_escape() {
    let _g = LOCK.lock().await;
    span_vault();
    fail_closed_case(
        "fc-summary",
        |r| {
            let mut e = ev(r, "web", "fetch", Class::Read, LedgerMode::Allow);
            e.result_summary = ESC_RAW.into();
            e
        },
        "result_summary",
    )
    .await;
}

#[tokio::test]
async fn r3_fail_closed_why_escape() {
    let _g = LOCK.lock().await;
    span_vault();
    fail_closed_case(
        "fc-why",
        |r| {
            let mut e = ev(
                r,
                "core",
                "act.click",
                Class::Send,
                LedgerMode::TimeoutProceed,
            );
            e.why = Some(ESC_RAW.into());
            e
        },
        "why",
    )
    .await;
}

#[tokio::test]
async fn r3_fail_closed_server_and_tool_escape() {
    let _g = LOCK.lock().await;
    span_vault();
    fail_closed_case(
        "fc-server-tool",
        |r| ev(r, ESC_RAW, ESC_RAW, Class::Read, LedgerMode::Allow),
        "server, tool",
    )
    .await;
}

/// Several data fields at once: all named, in field order.
#[tokio::test]
async fn r3_fail_closed_names_every_field() {
    let _g = LOCK.lock().await;
    span_vault();
    fail_closed_case(
        "fc-all",
        |r| {
            let mut e = ev(r, "web", "form.fill", Class::Fill, LedgerMode::Allow);
            e.args = json!({ "r3ka": "r3va" });
            e.result_summary = ESC_RAW.into();
            e.consequences = json!(["r3ca", "r3cb"]);
            e.why = Some(ESC_RAW.into());
            e
        },
        "args, result_summary, consequences, why",
    )
    .await;
}

/// D-117: `run_id` must be the ledger's own run; another run (even one carrying the
/// canary) is `WrongRun`, nothing written, the error text scrubbed.
#[tokio::test]
async fn r3_wrong_run_refused_and_scrubbed() {
    let _g = LOCK.lock().await;
    install(&[]);
    let (_root, run, store, ledger) = setup("wrong-run");
    let mut e = ev(&run.id, "web", "fetch", Class::Read, LedgerMode::Allow);
    e.run_id = Some(RunId(format!("2026-01-01T00-00-00-{}", canary())));
    let res = ledger.write(e).await;
    assert!(
        matches!(res, Err(LedgerError::WrongRun { .. })),
        "not WrongRun"
    );
    let msg = format!("{}", res.unwrap_err());
    assert!(!contains_ci(msg.as_bytes(), canary()), "canary in error");
    assert!(
        msg.contains("@vault(accept)"),
        "error not scrubbed to the literal"
    );
    assert!(store.ledger_events(&run.id).await.unwrap().is_empty());
    assert!(jsonl_bytes(&run).is_empty());
}

// ---------------------------------------------------------------------------------------
// Canary through a full flow + Q-4/Q-5 report shape

/// Canary in the prompt, args (value, nested, key), result_summary, consequences, why and
/// a stop: absent from the rows, `ledger.jsonl`, `report.md`, every file under the run
/// root and stdout (the report is printed for the external grep). The report has the Q-4
/// seven lines and a mid-call challenge is listed once (Q-5).
#[tokio::test]
async fn r3_canary_flow_and_report_shape() {
    let _g = LOCK.lock().await;
    install(&[]);
    let c = canary();
    let prompt = format!("Register on Luma for {c}\nmore");
    let root = Root::new("canary-flow");
    let run = RunFolder::create(&root.runs(), "register", &prompt).unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());

    let mut read = ev(&run.id, "web", "fetch", Class::Read, LedgerMode::Allow);
    read.args = json!({ "url": format!("https://a.example/?k={c}"), c: { "n": [c] } });
    read.result_summary = format!("page mentions {c}");
    let mut ask = ev(
        &run.id,
        "core",
        "echo.send",
        Class::Send,
        LedgerMode::TimeoutProceed,
    );
    ask.why = Some(format!("send to {c}@x.com"));
    let mut send = ev(
        &run.id,
        "echo",
        "echo.send",
        Class::Send,
        LedgerMode::TimeoutProceed,
    );
    send.args = json!({ "to": "bob@x.com", "body": format!("hi {c}") });
    send.consequences = json!(["files/sent.txt"]);
    std::fs::write(run.files().join("sent.txt"), "sent").unwrap();
    let stop = |server: &str, why: String| {
        let mut e = ev(
            &run.id,
            server,
            "act.click",
            Class::Challenge,
            LedgerMode::Stop,
        );
        e.ok = false;
        e.args = json!({ "ref": "9", "note": c });
        e.why = Some(why);
        e
    };
    let events = vec![
        read,
        ask,
        send,
        stop("core", format!("challenge during act.click: hCaptcha {c}")),
        stop("sheetbrowse", "hCaptcha on lu.ma".into()),
        stop("core", "hCaptcha on lu.ma".into()),
    ];
    for e in events {
        ledger.write(e).await.unwrap();
    }
    let rows = store.ledger_events(&run.id).await.unwrap();
    assert_eq!(rows.len(), 6);
    assert_eq!(read_jsonl(&run.ledger_jsonl()).unwrap(), rows);
    let evs: Vec<LedgerEvent> = rows.iter().map(|(_, e)| e.clone()).collect();
    let text = write_report(&run, &prompt, &evs).unwrap();
    println!("--- report.md ---\n{text}--- end ---");

    // Canary absent everywhere.
    let rows_json = serde_json::to_string(&evs).unwrap();
    assert!(
        !contains_ci(rows_json.as_bytes(), c),
        "canary in store rows"
    );
    assert!(
        !contains_ci(&jsonl_bytes(&run), c),
        "canary in ledger.jsonl"
    );
    assert!(!contains_ci(text.as_bytes(), c), "canary in report text");
    let files = all_files(&root.runs());
    assert!(
        files.len() >= 4,
        "prompt.md, ledger.jsonl, report.md, sent.txt"
    );
    for (p, b) in &files {
        assert!(!contains_ci(b, c), "canary in {}", p.display());
    }
    // Positive control: the literal is there.
    assert!(contains_ci(&jsonl_bytes(&run), "@vault(accept)"));
    assert!(text.contains("@vault(accept)"));

    // Q-4 shape, Q-5 stop listed once.
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 7, "report has 7 lines");
    assert_eq!(lines[0], "# Register on Luma for @vault(accept)");
    assert_eq!(
        lines[1],
        "Result: stopped (challenge during act.click: hCaptcha @vault(accept))"
    );
    assert_eq!(
        lines[2],
        "Read: 1 pages, 0 mails, 0 files (https://a.example/?k=@vault(accept))"
    );
    assert!(
        lines[3]
            .starts_with("Asked: 1 (0 answered / 1 timed out) (send to @vault(accept)@x.com at "),
        "Asked line"
    );
    assert!(
        lines[3].ends_with(": timed out, proceeded)"),
        "Asked line end"
    );
    assert_eq!(
        lines[4],
        "Did: 1 actions (Send echo.send bob@x.com: files/sent.txt)"
    );
    assert_eq!(lines[5], format!("Files: runs/{}/files/sent.txt", run.id));
    assert_eq!(
        lines[6],
        "Not done / needs you: stopped at act.click (Challenge): challenge during act.click: \
         hCaptcha @vault(accept); handed back to you"
    );
    assert_eq!(text.matches("stopped at").count(), 1);
    assert_eq!(std::fs::read_to_string(run.report_md()).unwrap(), text);
}
