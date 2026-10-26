//! T-06 acceptance (tester): run folder (H-4), ledger table + jsonl mirror (H-3 and the
//! docs/01 acceptance "ledger.jsonl matches the table row-for-row", MemStore half), vault
//! canary in ledger/report (H-2, D-102), report.md shape (docs/08 Q-4, Q-5).
//!
//! `SB_VAULT_CANARY=<unique> cargo test -p harness-core --features testing --test accept_t06`
//! Optional `SB_ACCEPT_RUNS_ROOT=<dir>` keeps the run folders there for an external grep.
//! No assertion message formats the canary.
#![cfg(feature = "testing")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once, OnceLock};

use chrono::{TimeZone, Utc};
use harness_core::class::Class;
use harness_core::ledger::{read_jsonl, write_report, Ledger, LedgerError};
use harness_core::memory::{ts_now, LedgerEvent, LedgerMode, MemoryStore};
use harness_core::run::{RunFolder, RunId};
use harness_core::testing::MemStore;
use harness_core::vault::{EntryKind, Vault};
use serde_json::json;

fn canary() -> &'static str {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| {
        std::env::var("SB_VAULT_CANARY")
            .ok()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| {
                format!(
                    "t06-accept-{}-{}",
                    std::process::id(),
                    Utc::now().timestamp_micros()
                )
            })
    })
}

fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        Vault::in_memory([("accept".to_owned(), canary().to_owned(), EntryKind::Secret)])
            .install_global();
    });
}

/// A runs root: `SB_ACCEPT_RUNS_ROOT/<name>` (kept) or a temp dir (removed on drop).
struct Root {
    path: PathBuf,
    keep: bool,
}
impl Root {
    fn new(name: &str) -> Root {
        static N: AtomicUsize = AtomicUsize::new(0);
        match std::env::var("SB_ACCEPT_RUNS_ROOT") {
            Ok(r) if !r.is_empty() => Root {
                path: PathBuf::from(r).join(name),
                keep: true,
            },
            _ => Root {
                path: std::env::temp_dir().join(format!(
                    "sb-accept-t06-{}-{}-{}-{}",
                    name,
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed),
                    Utc::now().timestamp_micros()
                )),
                keep: false,
            },
        }
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
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

fn all_files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
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

fn contains(hay: &[u8], needle: &str) -> bool {
    let n = needle.as_bytes();
    !n.is_empty() && hay.windows(n.len()).any(|w| w == n)
}

fn is_ts_id(id: &str, slug: &str) -> bool {
    // <yyyy-mm-ddThh-mm-ss>-<slug>
    if id.len() < 20 || !id.is_char_boundary(19) {
        return false;
    }
    let (ts, rest) = id.split_at(19);
    chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H-%M-%S").is_ok()
        && rest == format!("-{slug}")
}

// ---- H-4 run folder ----------------------------------------------------------------

#[test]
fn h4_run_folder_layout() {
    install();
    let root = Root::new("layout");
    let runs = root.path.join("runs");
    let before = Utc::now().timestamp();
    let r = RunFolder::create(
        &runs,
        "Find blog keywords",
        "find blog keywords\nand list posts",
    )
    .unwrap();
    let after = Utc::now().timestamp();
    assert!(is_ts_id(&r.id.0, "find-blog-keywords"), "id {}", r.id);
    let t = chrono::NaiveDateTime::parse_from_str(&r.id.0[..19], "%Y-%m-%dT%H-%M-%S")
        .unwrap()
        .and_utc()
        .timestamp();
    assert!(before <= t && t <= after, "id ts not now (UTC)");
    assert_eq!(r.dir, runs.join(&r.id.0));
    assert!(r.dir.is_dir());
    assert_eq!(
        std::fs::read_to_string(r.dir.join("prompt.md")).unwrap(),
        "find blog keywords\nand list posts"
    );
    for sub in ["files", "frames", "handoff"] {
        let d = r.dir.join(sub);
        assert!(d.is_dir(), "{sub}/ missing");
        assert_eq!(
            std::fs::read_dir(&d).unwrap().count(),
            0,
            "{sub}/ not empty"
        );
    }
    assert_eq!(r.ledger_jsonl(), r.dir.join("ledger.jsonl"));
    assert_eq!(r.report_md(), r.dir.join("report.md"));
}

#[test]
fn h4_same_second_same_slug_gets_suffix() {
    install();
    let root = Root::new("collide");
    let at = Utc.with_ymd_and_hms(2026, 10, 23, 14, 3, 9).unwrap();
    let a = RunFolder::create_at(&root.path, "Luma reg", "p1", at).unwrap();
    let b = RunFolder::create_at(&root.path, "luma reg", "p2", at).unwrap();
    let c = RunFolder::create_at(&root.path, "LUMA REG", "p3", at).unwrap();
    let other = RunFolder::create_at(&root.path, "other", "p4", at).unwrap();
    let next = RunFolder::create_at(
        &root.path,
        "luma reg",
        "p5",
        at + chrono::Duration::seconds(1),
    )
    .unwrap();
    assert_eq!(a.id.0, "2026-10-23T14-03-09-luma-reg");
    assert_eq!(b.id.0, "2026-10-23T14-03-09-luma-reg-2");
    assert_eq!(c.id.0, "2026-10-23T14-03-09-luma-reg-3");
    assert_eq!(other.id.0, "2026-10-23T14-03-09-other");
    assert_eq!(next.id.0, "2026-10-23T14-03-10-luma-reg");
    // Each one is a distinct, complete folder; earlier prompts are not overwritten.
    for (r, p) in [(&a, "p1"), (&b, "p2"), (&c, "p3")] {
        assert_eq!(std::fs::read_to_string(r.prompt_md()).unwrap(), p);
        for sub in ["files", "frames", "handoff"] {
            assert!(r.dir.join(sub).is_dir());
        }
    }
}

// ---- H-3 write: store first, then exactly one jsonl line ---------------------------

#[tokio::test]
async fn h3_write_inserts_then_appends_one_line() {
    install();
    let root = Root::new("order");
    let run = RunFolder::create(&root.path, "order", "order").unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());

    for i in 1..=5usize {
        let mut e = ev(&run.id, "srv-web", "fetch", Class::Read, LedgerMode::Allow);
        e.args = json!({ "url": format!("https://o.example/{i}") });
        let id = ledger.write(e).await.unwrap();
        let text = std::fs::read_to_string(run.ledger_jsonl()).unwrap();
        assert_eq!(text.lines().count(), i, "one line per write");
        assert!(text.ends_with('\n'));
        let rows = store.ledger_events(&run.id).await.unwrap();
        assert_eq!(rows.len(), i, "one row per write");
        let last: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(last["id"], json!(id.0), "line carries the store's id");
        assert_eq!(rows.last().unwrap().0, id);
    }

    // Store rejects the insert (MemStore: negative duration_ms) -> no jsonl line: the line
    // is written only after a successful insert.
    let mut bad = ev(&run.id, "srv-web", "fetch", Class::Read, LedgerMode::Allow);
    bad.duration_ms = -1;
    let err = ledger.write(bad).await.unwrap_err();
    assert!(matches!(err, LedgerError::Memory(_)), "{err}");
    let text = std::fs::read_to_string(run.ledger_jsonl()).unwrap();
    assert_eq!(text.lines().count(), 5, "line written for a failed insert");
    assert_eq!(store.ledger_events(&run.id).await.unwrap().len(), 5);
}

#[tokio::test]
async fn h3_insert_happens_before_jsonl_append() {
    install();
    let root = Root::new("order2");
    let run = RunFolder::create(&root.path, "order2", "order2").unwrap();
    // Make the jsonl path unwritable (a directory): the row must already be in the store.
    std::fs::create_dir(run.ledger_jsonl()).unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());
    let e = ev(&run.id, "srv-web", "fetch", Class::Read, LedgerMode::Allow);
    let err = ledger.write(e).await.unwrap_err();
    assert!(matches!(err, LedgerError::Io { .. }), "{err}");
    assert_eq!(store.ledger_events(&run.id).await.unwrap().len(), 1);
}

// ---- docs/01 acceptance: jsonl matches the table row-for-row (MemStore) ---------------

#[tokio::test]
async fn acceptance_jsonl_matches_table_row_for_row_including_ts() {
    install();
    let root = Root::new("rows");
    let run = RunFolder::create(&root.path, "rows", "rows").unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());
    let modes = [
        LedgerMode::Allow,
        LedgerMode::Ask,
        LedgerMode::Stop,
        LedgerMode::AnsweredProceed,
        LedgerMode::AnsweredSkip,
        LedgerMode::TimeoutProceed,
        LedgerMode::TimeoutSkip,
    ];
    let classes = [Class::Read, Class::Send, Class::Pay, Class::Challenge];
    let n = 40;
    for i in 0..n {
        let mut e = ev(
            &run.id,
            if i % 4 == 0 { "core" } else { "srv-web" },
            "fetch",
            classes[i % classes.len()],
            modes[i % modes.len()],
        );
        // Distinct instants with sub-microsecond parts (nanos) to exercise truncation.
        e.ts = Utc
            .timestamp_opt(1_793_000_000 + i as i64, 123_456_789 + i as u32)
            .unwrap();
        e.args = json!({ "url": format!("https://r.example/{i}"), "n": i, "f": 1.5 });
        e.result_summary = format!("summary {i} \"quoted\" \n newline");
        e.consequences = json!([{ "k": i }, "x"]);
        e.duration_ms = i as i32;
        e.ok = i % 5 != 0;
        e.why = (i % 2 == 0).then(|| format!("why {i}"));
        if i % 7 == 0 {
            e.class = None;
            e.mode = None;
            e.run_id = None; // filled with the ledger's run
        }
        ledger.write(e).await.unwrap();
    }
    // Noise from another run.
    store
        .append(ev(
            &RunId("x-other".into()),
            "core",
            "t",
            Class::Read,
            LedgerMode::Allow,
        ))
        .await
        .unwrap();

    let table = store.ledger_events(&run.id).await.unwrap();
    let jsonl = read_jsonl(&run.ledger_jsonl()).unwrap();
    assert_eq!(table.len(), n);
    assert_eq!(jsonl.len(), n);
    for (k, (j, t)) in jsonl.iter().zip(table.iter()).enumerate() {
        assert_eq!(j.0, t.0, "row {k} id");
        assert_eq!(j.1.ts, t.1.ts, "row {k} ts");
        assert_eq!(j.1, t.1, "row {k}");
        assert_eq!(t.1.run_id.as_ref(), Some(&run.id));
        assert_eq!(
            t.1.ts.timestamp_subsec_nanos() % 1000,
            0,
            "ts not micro precision"
        );
    }
    // Raw ts text in the jsonl parses to exactly the row's ts.
    let text = std::fs::read_to_string(run.ledger_jsonl()).unwrap();
    for (line, (_, t)) in text.lines().zip(table.iter()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let ts: chrono::DateTime<Utc> = serde_json::from_value(v["ts"].clone()).unwrap();
        assert_eq!(ts, t.ts);
    }
}

// ---- H-2 / D-102: canary in ledger and report --------------------------------------

#[tokio::test]
async fn vault_canary_absent_from_jsonl_rows_and_report() {
    install();
    let c = canary();
    let root = Root::new("canary");
    let run = RunFolder::create(&root.path, "canary", "send the code").unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());

    let mut e = ev(&run.id, "echo", "echo.send", Class::Send, LedgerMode::Allow);
    e.args = json!({
        "to": format!("{c}@x.com"),
        "body": format!("code {c}"),
        "list": [c, { "inner": format!("a{c}b") }],
        c: c,
    });
    e.result_summary = format!("sent {c}");
    e.consequences = json!([format!("message with {c}"), { "files": [format!("files/{c}.txt")] }]);
    e.why = Some(format!("because {c}"));
    ledger.write(e).await.unwrap();

    let mut ask = ev(
        &run.id,
        "core",
        "echo.send",
        Class::Send,
        LedgerMode::TimeoutProceed,
    );
    ask.why = Some(format!("send {c} to new contact"));
    ledger.write(ask).await.unwrap();

    let mut stop = ev(&run.id, "core", "act", Class::Challenge, LedgerMode::Stop);
    stop.ok = false;
    stop.why = Some(format!("captcha after typing {c}"));
    stop.result_summary = format!("typed {c}");
    stop.consequences = json!({ c: [c] });
    ledger.write(stop).await.unwrap();

    let rows = store.ledger_events(&run.id).await.unwrap();
    let rows_json = serde_json::to_string(&rows).unwrap();
    assert!(!rows_json.contains(c), "canary in store rows");
    assert!(rows_json.contains("@vault(accept)"));
    let jsonl = std::fs::read(run.ledger_jsonl()).unwrap();
    assert!(!contains(&jsonl, c), "canary in ledger.jsonl");

    let events: Vec<LedgerEvent> = rows.iter().map(|(_, e)| e.clone()).collect();
    let r = write_report(&run, &format!("Send {c} to Bob"), &events).unwrap();
    assert!(!r.contains(c), "canary in report text");
    let md = std::fs::read(run.report_md()).unwrap();
    assert!(!contains(&md, c), "canary in report.md");

    for (path, bytes) in all_files(&root.path) {
        assert!(!contains(&bytes, c), "canary in {}", path.display());
    }
    // print scrubbed output so the external stdout grep has something to scan
    println!("report:\n{r}");
}

// ---- Q-4 report shape and Result for done / partial / stopped ----------------------

/// The seven Q-4 lines, in order; returns them.
fn q4_lines(r: &str) -> Vec<String> {
    let lines: Vec<String> = r.lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 7, "Q-4 report has 7 lines");
    let prefixes = [
        "# ",
        "Result: ",
        "Read: ",
        "Asked: ",
        "Did: ",
        "Files: ",
        "Not done / needs you: ",
    ];
    for (l, p) in lines.iter().zip(prefixes) {
        assert!(l.starts_with(p), "line {l:?} lacks {p:?}");
    }
    // Read: n pages, n mails, n files
    let read = lines[2].strip_prefix("Read: ").unwrap();
    let parts: Vec<&str> = read.splitn(3, ", ").collect();
    assert_eq!(parts.len(), 3);
    for (part, word) in parts.iter().zip(["pages", "mails", "files"]) {
        let mut it = part.split_whitespace();
        assert!(it.next().unwrap().parse::<usize>().is_ok(), "{part}");
        assert_eq!(it.next(), Some(word));
    }
    // Asked: n (a answered / t timed out)
    let asked = lines[3].strip_prefix("Asked: ").unwrap();
    let (n, rest) = asked.split_once(" (").unwrap();
    let n: usize = n.parse().unwrap();
    let (a, rest) = rest.split_once(" answered / ").unwrap();
    let (t, _) = rest.split_once(" timed out)").unwrap();
    assert_eq!(n, a.parse::<usize>().unwrap() + t.parse::<usize>().unwrap());
    // Did: n actions
    let did = lines[4].strip_prefix("Did: ").unwrap();
    let mut it = did.split_whitespace();
    assert!(it.next().unwrap().parse::<usize>().is_ok());
    assert_eq!(it.next(), Some("actions"));
    lines
}

#[tokio::test]
async fn q4_report_done() {
    install();
    let root = Root::new("q4done");
    let run = RunFolder::create(&root.path, "say hi", "Say hi to Bob").unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());
    let mut fetch = ev(&run.id, "srv-web", "fetch", Class::Read, LedgerMode::Allow);
    fetch.args = json!({ "url": "https://a.example/" });
    ledger.write(fetch).await.unwrap();
    let mut ask = ev(
        &run.id,
        "core",
        "echo.send",
        Class::Send,
        LedgerMode::AnsweredProceed,
    );
    ask.why = Some("send hi to bob@x.com".into());
    ledger.write(ask).await.unwrap();
    let mut send = ev(
        &run.id,
        "echo",
        "echo.send",
        Class::Send,
        LedgerMode::AnsweredProceed,
    );
    send.args = json!({ "to": "bob@x.com", "body": "hi" });
    send.consequences = json!(["sent", { "saved": format!("runs/{}/files/sent.txt", run.id) }]);
    ledger.write(send).await.unwrap();
    std::fs::write(run.files().join("sent.txt"), "hi").unwrap();

    let events: Vec<LedgerEvent> = ledger
        .read_back()
        .unwrap()
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    let r = write_report(&run, "Say hi to Bob", &events).unwrap();
    let l = q4_lines(&r);
    assert_eq!(l[0], "# Say hi to Bob");
    assert_eq!(l[1], "Result: done");
    assert!(
        l[2].starts_with("Read: 1 pages, 0 mails, 0 files"),
        "{}",
        l[2]
    );
    assert!(l[2].contains("https://a.example/"));
    assert!(
        l[3].starts_with("Asked: 1 (1 answered / 0 timed out)"),
        "{}",
        l[3]
    );
    assert!(l[4].starts_with("Did: 1 actions"), "{}", l[4]);
    assert!(
        l[4].contains("Send") && l[4].contains("bob@x.com"),
        "{}",
        l[4]
    );
    assert_eq!(l[5], format!("Files: runs/{}/files/sent.txt", run.id));
    assert_eq!(l[6], "Not done / needs you: nothing");
    assert_eq!(std::fs::read_to_string(run.report_md()).unwrap(), r);
}

#[tokio::test]
async fn q4_report_partial() {
    install();
    let root = Root::new("q4partial");
    let run = RunFolder::create(&root.path, "buy", "Buy the ticket").unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());
    let mut fetch = ev(&run.id, "srv-web", "fetch", Class::Read, LedgerMode::Allow);
    fetch.args = json!({ "url": "https://shop.example/t" });
    ledger.write(fetch).await.unwrap();
    let mut skip = ev(
        &run.id,
        "core",
        "act.click",
        Class::Pay,
        LedgerMode::TimeoutSkip,
    );
    skip.why = Some("pay 20 EUR on shop.example".into());
    ledger.write(skip).await.unwrap();

    let events: Vec<LedgerEvent> = store
        .ledger_events(&run.id)
        .await
        .unwrap()
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    let r = write_report(&run, "Buy the ticket", &events).unwrap();
    let l = q4_lines(&r);
    assert_eq!(l[1], "Result: partial");
    assert!(
        l[3].starts_with("Asked: 1 (0 answered / 1 timed out)"),
        "{}",
        l[3]
    );
    assert!(l[4].starts_with("Did: 0 actions"), "{}", l[4]);
    assert_eq!(l[5], "Files: none");
    // Q-5: the step, its class, the why.
    assert!(l[6].contains("act.click"), "{}", l[6]);
    assert!(l[6].contains("Pay"), "{}", l[6]);
    assert!(l[6].contains("pay 20 EUR on shop.example"), "{}", l[6]);
}

#[tokio::test]
async fn q4_report_stopped_with_why() {
    install();
    let root = Root::new("q4stopped");
    let run = RunFolder::create(&root.path, "luma", "Register on Luma").unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());
    let mut fetch = ev(&run.id, "srv-web", "fetch", Class::Read, LedgerMode::Allow);
    fetch.args = json!({ "url": "https://lu.ma/e/x" });
    ledger.write(fetch).await.unwrap();
    let mut stop = ev(
        &run.id,
        "core",
        "act.click",
        Class::Challenge,
        LedgerMode::Stop,
    );
    stop.ok = false;
    stop.why = Some("Turnstile on lu.ma at #12".into());
    ledger.write(stop).await.unwrap();

    let events: Vec<LedgerEvent> = read_jsonl(&run.ledger_jsonl())
        .unwrap()
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    let r = write_report(&run, "Register on Luma", &events).unwrap();
    let l = q4_lines(&r);
    assert_eq!(l[0], "# Register on Luma");
    assert_eq!(l[1], "Result: stopped (Turnstile on lu.ma at #12)");
    assert!(l[2].starts_with("Read: 1 pages"), "{}", l[2]);
    assert!(l[4].starts_with("Did: 0 actions"), "{}", l[4]);
    assert!(
        l[6].contains("act.click") && l[6].contains("Challenge"),
        "{}",
        l[6]
    );
    assert!(l[6].contains("Turnstile on lu.ma at #12"), "{}", l[6]);
}
