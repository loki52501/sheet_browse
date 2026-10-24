//! T-06 acceptance, round 2 (tester): numeric vault value in args, a value straddling the
//! report clip point, two `Ledger`s on one run, a torn last line, `RunFolder::open` alias
//! rejection, and `report.md` built from the events the real T-07 wrapper
//! (`mcp::dispatch`) writes. docs/01 H-2, H-3, H-4, H-6 + Acceptance; docs/08 Q-4, Q-5;
//! D-41, D-102.
//!
//! `SB_VAULT_CANARY=<unique> cargo test -p harness-core --test accept_t06_r2`
//! Optional `SB_ACCEPT_RUNS_ROOT=<dir>` keeps the run folders there for an external grep.
//! No assertion message formats a vault value.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once, OnceLock};

use async_trait::async_trait;
use chrono::Utc;
use harness_core::class::Class;
use harness_core::gate::{AskCard, AskOutcome, OnTimeout, StopReason};
use harness_core::ledger::{read_jsonl, read_jsonl_checked, report, write_report, Ledger};
use harness_core::mcp::{
    dispatch, Classified, Harness, ToolCtx, ToolError, ToolHandler, ToolOutput, ToolRegistry,
    ToolSpec,
};
use harness_core::memory::{ts_now, LedgerEvent, LedgerMode, MemoryStore, Run, RunStatus};
use harness_core::run::{RunFolder, RunId};
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::{EntryKind, Vault};
use serde_json::{json, Value};

/// The numeric canary the brief names.
const PIN: &str = "48291377";
const PIN_N: u64 = 48_291_377;

fn canary() -> &'static str {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| {
        std::env::var("SB_VAULT_CANARY")
            .ok()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| {
                format!(
                    "r2canary-{}-{}",
                    std::process::id(),
                    Utc::now().timestamp_micros()
                )
            })
    })
}

fn vault() -> Arc<Vault> {
    static V: OnceLock<Arc<Vault>> = OnceLock::new();
    V.get_or_init(|| {
        Arc::new(Vault::in_memory([
            ("accept".to_owned(), canary().to_owned(), EntryKind::Secret),
            ("pin".to_owned(), PIN.to_owned(), EntryKind::Secret),
        ]))
    })
    .clone()
}

fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| vault().install_global());
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
                base: PathBuf::from(r).join(format!("r2-{name}")),
                keep: true,
            },
            _ => Root {
                base: std::env::temp_dir().join(format!(
                    "sb-accept-t06r2-{}-{}-{}-{}",
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

/// True if any `min`-long window of `secret` occurs in `hay` (ASCII case-insensitive):
/// a partial value, such as the half left by a clip that cut before scrubbing.
fn partial_hit(hay: &[u8], secret: &str, min: usize) -> bool {
    let s = secret.as_bytes();
    if s.len() < min {
        return contains_ci(hay, secret);
    }
    s.windows(min)
        .any(|w| hay.windows(min).any(|h| h.eq_ignore_ascii_case(w)))
}

fn all_files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd {
        let p = entry.unwrap().path();
        if p.is_dir() {
            out.extend(all_files(&p));
        } else {
            let bytes = std::fs::read(&p).unwrap();
            out.push((p, bytes));
        }
    }
    out
}

// ---- numeric canary in args ----------------------------------------------------------

#[tokio::test]
async fn r2_numeric_canary_in_args_is_scrubbed() {
    install();
    let root = Root::new("pin");
    let run = RunFolder::create(&root.runs(), "pin", "enter the pin").unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());

    let mut e = ev(
        &run.id,
        "sheetbrowse",
        "act.type",
        Class::Fill,
        LedgerMode::Allow,
    );
    e.args = json!({
        "pin": PIN_N,
        "pin_str": PIN,
        "nested": [PIN_N, { "deep": PIN_N, "f": 48_291_377.0_f64 }],
        "neg": -(PIN_N as i64),
        "bigger": 1_482_913_770_u64,
        PIN: "key",
    });
    e.result_summary = format!("typed {PIN}");
    e.consequences = json!([PIN_N, { "n": PIN_N }]);
    e.why = Some(format!("pin {PIN}"));
    let id = ledger.write(e).await.unwrap();

    let rows = store.ledger_events(&run.id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, id);
    let rows_json = serde_json::to_string(&rows).unwrap();
    assert!(!rows_json.contains(PIN), "numeric value in store row");
    assert!(rows_json.contains("@vault(pin)"), "positive control");
    let jsonl = std::fs::read(run.ledger_jsonl()).unwrap();
    assert!(!contains_ci(&jsonl, PIN), "numeric value in ledger.jsonl");
    // The mirror still matches the table after numbers became strings.
    assert_eq!(read_jsonl(&run.ledger_jsonl()).unwrap(), rows);
    let a = &rows[0].1.args;
    assert_eq!(a["pin"], json!("@vault(pin)"));
    assert_eq!(a["nested"][1]["deep"], json!("@vault(pin)"));

    let events: Vec<LedgerEvent> = rows.into_iter().map(|(_, e)| e).collect();
    let r = write_report(&run, &format!("enter {PIN}"), &events).unwrap();
    assert!(!r.contains(PIN), "numeric value in report");
    for (p, bytes) in all_files(&root.base) {
        assert!(
            !contains_ci(&bytes, PIN),
            "numeric value in {}",
            p.display()
        );
    }
}

// ---- a value straddling the clip point -------------------------------------------------

#[tokio::test]
async fn r2_canary_straddling_clip_point_not_partially_in_report() {
    install();
    let root = Root::new("clip");
    let run = RunFolder::create(&root.runs(), "clip", "clip").unwrap();
    let c = canary();
    // ITEM_MAX = 100 (cut keeps 97 chars + "..."), task line 120 (117 + "...").
    for secret in [c, PIN] {
        for k in 80..=118usize {
            let pad = "a".repeat(k);
            let mut fetch = ev(&run.id, "web", "fetch", Class::Read, LedgerMode::Allow);
            fetch.args = json!({ "url": format!("https://x.example/{pad}{secret}tail") });
            let mut send = ev(&run.id, "echo", "echo.send", Class::Send, LedgerMode::Allow);
            send.args = json!({ "to": format!("{pad}{secret}@x.com") });
            send.consequences = json!([format!("{pad}{secret} sent")]);
            let mut ask = ev(
                &run.id,
                "core",
                "shop.pay",
                Class::Pay,
                LedgerMode::TimeoutSkip,
            );
            ask.why = Some(format!("{pad} {secret} more"));
            let mut fail = ev(&run.id, "web", "fetch", Class::Read, LedgerMode::Allow);
            fail.ok = false;
            fail.args = json!({ "url": format!("https://y.example/{pad}{secret}") });
            fail.why = Some(format!("{}{secret}", "w".repeat(k)));
            fail.result_summary = format!("{pad}\n{secret}");
            let events = vec![fetch, send, ask, fail];
            let prompt = format!("{}{secret} rest\nsecond line", "p".repeat(k));
            let r = report(&prompt, &events);
            // 6 chars of an ASCII value never occur in the padding or the fixed text.
            assert!(
                !partial_hit(r.as_bytes(), secret, 6),
                "partial value in report (pad {k})"
            );
            if k == 95 {
                let w = write_report(&run, &prompt, &events).unwrap();
                assert_eq!(w, std::fs::read_to_string(run.report_md()).unwrap());
                let md = std::fs::read(run.report_md()).unwrap();
                assert!(!partial_hit(&md, secret, 6), "partial value in report.md");
                assert!(w.contains("@vault("), "positive control");
            }
        }
    }
}

// ---- two Ledgers on one run ----------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r2_two_ledgers_on_one_run_compare_equal_by_id() {
    install();
    let root = Root::new("two");
    let run = RunFolder::create(&root.runs(), "two", "two").unwrap();
    let store: Arc<MemStore> = Arc::new(MemStore::new());
    // A second tool server process attaches to the same run via RunFolder::open.
    let attached = RunFolder::open(&root.runs(), &run.id).unwrap();
    let a = Arc::new(Ledger::new(store.clone(), run.clone()));
    let b = Arc::new(Ledger::new(store.clone(), attached));
    let mut tasks = Vec::new();
    for (k, l) in [a.clone(), b.clone(), a.clone(), b.clone()]
        .into_iter()
        .enumerate()
    {
        let rid = run.id.clone();
        tasks.push(tokio::spawn(async move {
            for i in 0..25 {
                let mut e = ev(&rid, "web", "fetch", Class::Read, LedgerMode::Allow);
                e.args = json!({ "url": format!("https://t.example/{k}/{i}") });
                l.write(e).await.unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let table = store.ledger_events(&run.id).await.unwrap();
    let checked = read_jsonl_checked(&run.ledger_jsonl()).unwrap();
    assert!(checked.torn.is_empty(), "torn lines {:?}", checked.torn);
    assert_eq!(table.len(), 100);
    assert_eq!(checked.events, table, "jsonl == table keyed by id");
    assert_eq!(a.read_back().unwrap(), table);
    assert_eq!(b.read_back().unwrap(), table);
    let text = std::fs::read_to_string(run.ledger_jsonl()).unwrap();
    assert_eq!(text.lines().count(), 100);
    for l in text.lines() {
        serde_json::from_str::<Value>(l).expect("whole line");
    }
}

// ---- torn last line ---------------------------------------------------------------------

#[tokio::test]
async fn r2_torn_last_line_is_skipped_and_next_append_is_whole() {
    install();
    let root = Root::new("torn");
    for cut_in_char in [false, true] {
        let run = RunFolder::create(&root.runs(), "torn", "torn").unwrap();
        let store = Arc::new(MemStore::new());
        let ledger = Ledger::new(store.clone(), run.clone());
        for i in 0..3 {
            let mut e = ev(&run.id, "web", "fetch", Class::Read, LedgerMode::Allow);
            e.args = json!({ "url": format!("https://torn.example/{i}") });
            e.result_summary = "\u{00e9}t\u{00e9} r\u{00e9}sum\u{00e9}".into();
            ledger.write(e).await.unwrap();
        }
        // A crashed writer left half a line (no '\n').
        let text = std::fs::read(run.ledger_jsonl()).unwrap();
        let first_len = text.iter().position(|b| *b == b'\n').unwrap() + 1;
        let line = &text[..first_len];
        let cut = if cut_in_char {
            // inside the 2-byte UTF-8 of the first 'é'
            line.iter().position(|b| *b == 0xC3).unwrap() + 1
        } else {
            line.len() / 2
        };
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(run.ledger_jsonl())
                .unwrap();
            f.write_all(&line[..cut]).unwrap();
        }
        let before = read_jsonl_checked(&run.ledger_jsonl()).unwrap();
        assert_eq!(before.events.len(), 3);
        assert_eq!(before.torn, vec![4]);

        let mut next = ev(&run.id, "web", "fetch", Class::Read, LedgerMode::Allow);
        next.args = json!({ "url": "https://torn.example/next" });
        let id = ledger.write(next).await.unwrap();

        let bytes = std::fs::read(run.ledger_jsonl()).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let last_line = bytes[..bytes.len() - 1]
            .rsplit(|b| *b == b'\n')
            .next()
            .unwrap();
        let v: Value = serde_json::from_slice(last_line).expect("next append is a whole line");
        assert_eq!(v["id"], json!(id.0));
        let after = read_jsonl_checked(&run.ledger_jsonl()).unwrap();
        assert_eq!(
            after.torn,
            vec![4],
            "the torn line stays skipped, nothing else"
        );
        assert_eq!(after.events.len(), 4);
        assert_eq!(after.events, store.ledger_events(&run.id).await.unwrap());
        // One more append after that is still whole.
        ledger
            .write(ev(&run.id, "web", "fetch", Class::Read, LedgerMode::Allow))
            .await
            .unwrap();
        let again = read_jsonl_checked(&run.ledger_jsonl()).unwrap();
        assert_eq!(again.events.len(), 5);
        assert_eq!(again.torn, vec![4]);
    }
}

// ---- RunFolder::open aliases -----------------------------------------------------------

#[test]
fn r2_open_rejects_trailing_dot_and_other_aliases() {
    install();
    let root = Root::new("open");
    let run = RunFolder::create(&root.runs(), "Open me", "p").unwrap();
    let ok = RunFolder::open(&root.runs(), &run.id).unwrap();
    assert_eq!(ok.dir, run.dir);
    let id = run.id.0.clone();
    for alias in [
        format!("{id}."),
        format!("{id}.."),
        format!("{id} "),
        format!("{id}. "),
        id.to_uppercase(),
        format!("{id}/."),
        format!("{id}\\."),
        format!("../runs/{id}"),
        format!("{id}::$DATA"),
    ] {
        let r = RunFolder::open(&root.runs(), &RunId(alias.clone()));
        assert!(r.is_err(), "alias {alias:?} opened");
    }
    // A missing well-formed id is NotFound.
    let missing = RunFolder::open(&root.runs(), &RunId(format!("{id}-x")));
    assert_eq!(missing.unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

// ---- report() over the events the T-07 wrapper writes -------------------------------

#[derive(Default, Clone)]
struct Beh {
    classify: Classified,
    mid_ask: Option<AskCard>,
    stop: Option<StopReason>,
    fail: Option<String>,
    write_file: bool,
}

struct H(Beh);

#[async_trait]
impl ToolHandler for H {
    async fn classify(&self, _ctx: &ToolCtx, _args: &Value) -> Result<Classified, ToolError> {
        Ok(self.0.classify.clone())
    }
    async fn call(&self, ctx: &ToolCtx, _args: Value) -> Result<ToolOutput, ToolError> {
        if let Some(card) = &self.0.mid_ask {
            let _ = ctx.ask(card.clone()).await;
        }
        if let Some(msg) = &self.0.fail {
            return Err(ToolError::Failed(msg.clone()));
        }
        let mut files = Vec::new();
        if self.0.write_file {
            let f = ctx.run_dir.join("files").join("receipt.txt");
            std::fs::write(&f, "receipt").unwrap();
            files.push(f);
        }
        Ok(ToolOutput {
            text: "done\nmore".into(),
            summary: None,
            consequences: json!(["order placed"]),
            files,
            stopped: self.0.stop.clone(),
        })
    }
}

struct Env {
    _root: Root,
    run: RunFolder,
    store: Arc<MemStore>,
    harness: Harness,
}

async fn env(name: &str, asker: ScriptedAsker) -> Env {
    let root = Root::new(name);
    let run = RunFolder::create(&root.runs(), name, "wrapper run").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "wrapper run".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    store.add_contact("friend@example.com");
    let harness = Harness {
        store: store.clone(),
        vault: vault(),
        asker: Arc::new(asker),
        runs_root: root.runs(),
        run: Some(run.id.clone()),
    };
    Env {
        _root: root,
        run,
        store,
        harness,
    }
}

fn spec(name: &str, class: Class, b: Beh) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: format!("{name} test tool"),
        class,
        args: json!({"type": "object"}),
        handler: Arc::new(H(b)),
    }
}

fn lines_of(env: &Env, prompt: &str) -> (Vec<LedgerEvent>, Vec<String>) {
    let mut rows = env.store.ledger();
    rows.sort_by_key(|(id, _)| *id);
    assert_eq!(read_jsonl(&env.run.ledger_jsonl()).unwrap(), rows);
    let events: Vec<LedgerEvent> = rows.into_iter().map(|(_, e)| e).collect();
    let r = write_report(&env.run, prompt, &events).unwrap();
    println!("--- {}\n{r}", env.run.id);
    let lines: Vec<String> = r.lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 7, "Q-4 has 7 lines");
    (events, lines)
}

#[tokio::test]
async fn r2_wrapper_ask_proceed_with_file_reports_done() {
    install();
    let e = env(
        "wrap-pay",
        ScriptedAsker::with([AskOutcome::AnsweredProceed]),
    )
    .await;
    let b = Beh {
        write_file: true,
        ..Beh::default()
    };
    let reg = ToolRegistry::new(vec![spec("shop.pay", Class::Pay, b)]);
    dispatch(
        "shop",
        &reg,
        &e.harness,
        "shop.pay",
        json!({"id": "order-7"}),
    )
    .await
    .unwrap();
    let (_ev, l) = lines_of(&e, "Pay order 7");
    assert_eq!(l[1], "Result: done");
    assert!(
        l[3].starts_with("Asked: 1 (1 answered / 0 timed out)"),
        "{}",
        l[3]
    );
    assert!(
        l[4].starts_with("Did: 1 actions (Pay shop.pay order-7"),
        "{}",
        l[4]
    );
    assert_eq!(l[5], format!("Files: runs/{}/files/receipt.txt", e.run.id));
    assert_eq!(l[6], "Not done / needs you: nothing");
}

#[tokio::test]
async fn r2_wrapper_ask_skip_reports_partial() {
    install();
    let e = env("wrap-skip", ScriptedAsker::with([AskOutcome::AnsweredSkip])).await;
    let reg = ToolRegistry::new(vec![spec("shop.pay", Class::Pay, Beh::default())]);
    dispatch(
        "shop",
        &reg,
        &e.harness,
        "shop.pay",
        json!({"id": "order-8"}),
    )
    .await
    .unwrap();
    let (_ev, l) = lines_of(&e, "Pay order 8");
    assert_eq!(l[1], "Result: partial");
    assert!(
        l[3].starts_with("Asked: 1 (1 answered / 0 timed out)"),
        "{}",
        l[3]
    );
    assert!(l[4].starts_with("Did: 0 actions"), "{}", l[4]);
    assert!(
        l[6].contains("shop.pay (Pay)") && l[6].contains("you said skip"),
        "{}",
        l[6]
    );
}

#[tokio::test]
async fn r2_wrapper_handler_error_reports_q5() {
    install();
    let e = env("wrap-err", ScriptedAsker::new()).await;
    let b = Beh {
        fail: Some("form field #email not found".into()),
        ..Beh::default()
    };
    let reg = ToolRegistry::new(vec![spec("act.fill", Class::Fill, b)]);
    let _ = dispatch(
        "sheetbrowse",
        &reg,
        &e.harness,
        "act.fill",
        json!({"url": "https://f.example/"}),
    )
    .await;
    let (_ev, l) = lines_of(&e, "Fill the form");
    assert_eq!(l[1], "Result: partial");
    // Q-5: step, class, why, consequence captured, fallback.
    assert!(
        l[6].contains("failed act.fill https://f.example/ (Fill)"),
        "{}",
        l[6]
    );
    assert!(l[6].contains("form field #email not found"), "{}", l[6]);
    assert!(l[6].contains("consequence: "), "{}", l[6]);
    assert!(l[6].contains("fallback: none"), "{}", l[6]);
}

#[tokio::test]
async fn r2_wrapper_handler_stop_listed_once() {
    install();
    let e = env("wrap-stop", ScriptedAsker::new()).await;
    let b = Beh {
        stop: Some(StopReason {
            class: Class::Challenge,
            why: "Turnstile on the register form".into(),
            element: Some("#44".into()),
        }),
        ..Beh::default()
    };
    let reg = ToolRegistry::new(vec![spec("act.click", Class::Submit, b)]);
    dispatch("sheetbrowse", &reg, &e.harness, "act.click", json!({}))
        .await
        .unwrap();
    let (ev, l) = lines_of(&e, "Register");
    assert_eq!(ev.len(), 2);
    assert_eq!(l[1], "Result: stopped (Turnstile on the register form)");
    assert!(l[4].starts_with("Did: 0 actions"), "{}", l[4]);
    assert_eq!(l[6].matches("stopped at").count(), 1, "{}", l[6]);
    assert!(l[6].contains("act.click (Challenge)"), "{}", l[6]);
}

/// The documented mid-call challenge path: `ToolCtx::ask` with a Challenge card logs a core
/// stop and returns TimeoutSkip; the handler then returns `ToolOutput::stopped`, which the
/// wrapper logs as a tool stop event plus a second core stop event. One stop, one listing.
#[tokio::test]
#[ignore = "red-first T-06 round 2"]
async fn r2_wrapper_mid_call_challenge_listed_once() {
    install();
    let e = env("wrap-mid", ScriptedAsker::new()).await;
    let b = Beh {
        mid_ask: Some(AskCard {
            class: Class::Challenge,
            what: "hCaptcha appeared".into(),
            site: Some("lu.ma".into()),
            values: vec![],
            timeout: std::time::Duration::from_millis(1),
            on_timeout: OnTimeout::Skip,
        }),
        stop: Some(StopReason {
            class: Class::Challenge,
            why: "hCaptcha on lu.ma".into(),
            element: Some("#9".into()),
        }),
        ..Beh::default()
    };
    let reg = ToolRegistry::new(vec![spec("act.click", Class::Submit, b)]);
    dispatch("sheetbrowse", &reg, &e.harness, "act.click", json!({}))
        .await
        .unwrap();
    let (ev, l) = lines_of(&e, "Register on Luma");
    let stops = ev
        .iter()
        .filter(|x| x.mode == Some(LedgerMode::Stop))
        .count();
    println!("events: {} ({} stop)", ev.len(), stops);
    assert!(l[1].starts_with("Result: stopped ("), "{}", l[1]);
    assert!(l[4].starts_with("Did: 0 actions"), "{}", l[4]);
    assert_eq!(
        l[6].matches("stopped at").count(),
        1,
        "one challenge stop listed more than once: {}",
        l[6]
    );
}

#[tokio::test]
async fn r2_wrapper_canary_absent_from_report_and_run_folder() {
    install();
    let c = canary();
    let e = env("wrap-canary", ScriptedAsker::new()).await;
    let b = Beh {
        fail: Some(format!("boom {c} {PIN}")),
        ..Beh::default()
    };
    let reg = ToolRegistry::new(vec![spec("act.fill", Class::Fill, b)]);
    let _ = dispatch(
        "sheetbrowse",
        &reg,
        &e.harness,
        "act.fill",
        json!({"url": format!("https://f.example/{c}"), "code": PIN_N, "text": c}),
    )
    .await;
    let (_ev, l) = lines_of(&e, &format!("Fill with {c}"));
    let joined = l.join("\n");
    assert!(!partial_hit(joined.as_bytes(), c, 8), "canary in report");
    assert!(!joined.contains(PIN), "pin in report");
    for (p, bytes) in all_files(&e.run.dir) {
        assert!(!contains_ci(&bytes, c), "canary in {}", p.display());
        assert!(!contains_ci(&bytes, PIN), "pin in {}", p.display());
    }
}
