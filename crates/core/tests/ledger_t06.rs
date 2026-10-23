//! T-06: ledger table + jsonl mirror, run folder, report.md (docs/01 H-3, H-4 and its
//! acceptance "ledger.jsonl matches the table row-for-row"; docs/08 Q-4; D-102).
//!
//! Every test here installs the same canary vault as the process-wide scrub (idempotent),
//! so this binary is the only one whose global scrub carries the ledger canary. No
//! assertion message formats the canary.
//! `cargo test -p harness-core --features testing --test ledger_t06`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};

use harness_core::class::Class;
use harness_core::ledger::{read_jsonl, report, write_report, Ledger};
use harness_core::memory::{ts_now, LedgerEvent, LedgerMode, MemoryStore};
use harness_core::run::{RunFolder, RunId};
use harness_core::testing::MemStore;
use harness_core::vault::{EntryKind, Vault};
use serde_json::json;

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "canary-ledger-not-a-secret".to_owned())
}

fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        Vault::in_memory([("test".to_owned(), canary(), EntryKind::Secret)]).install_global();
    });
}

/// A unique folder under the OS temp dir, removed on drop.
struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        Tmp(std::env::temp_dir().join(format!(
            "sb-ledger-t06-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            chrono::Utc::now().timestamp_micros()
        )))
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn event(run: &RunId, server: &str, tool: &str, class: Class, mode: LedgerMode) -> LedgerEvent {
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
        duration_ms: 1,
        ok: true,
        why: None,
    }
}

/// Every file under `dir`, recursively, as (path, contents).
fn all_files(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            out.extend(all_files(&p));
        } else {
            let text = String::from_utf8_lossy(&std::fs::read(&p).unwrap()).into_owned();
            out.push((p, text));
        }
    }
    out
}

#[test]
fn run_folder_layout_and_collision() {
    install();
    let tmp = Tmp::new();
    let root = tmp.0.join("runs");
    let a = RunFolder::create(&root, "Say HI!!  to Bob", "say hi to bob").unwrap();
    assert!(a.id.0.ends_with("-say-hi-to-bob"), "{}", a.id);
    let ts = &a.id.0[..19];
    assert!(
        chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H-%M-%S").is_ok(),
        "{ts}"
    );
    for sub in ["files", "frames", "handoff"] {
        assert!(a.dir.join(sub).is_dir(), "{sub}");
    }
    assert_eq!(
        std::fs::read_to_string(a.dir.join("prompt.md")).unwrap(),
        "say hi to bob"
    );
    // Same second, same slug: suffixed. Created via create_at with a's own second.
    let at = chrono::DateTime::parse_from_rfc3339(&format!(
        "{}:{}:{}Z",
        &ts[..13],
        &ts[14..16],
        &ts[17..19]
    ))
    .unwrap()
    .with_timezone(&chrono::Utc);
    let b = RunFolder::create_at(&root, "say hi to bob", "again", at).unwrap();
    assert_eq!(b.id.0, format!("{}-2", a.id));
    assert_eq!(RunFolder::open(&root, &b.id).unwrap(), b);
}

#[test]
fn prompt_and_slug_are_scrubbed() {
    install();
    let tmp = Tmp::new();
    let c = canary();
    let run = RunFolder::create(&tmp.0, &format!("login {c}"), &format!("use {c}")).unwrap();
    assert!(!run.id.0.contains(&c));
    assert!(run.id.0.ends_with("-login-vault-test"), "{}", run.id);
    let prompt = std::fs::read_to_string(run.prompt_md()).unwrap();
    assert!(!prompt.contains(&c));
    assert_eq!(prompt, "use @vault(test)");
}

#[tokio::test]
async fn jsonl_matches_table_row_for_row() {
    install();
    let tmp = Tmp::new();
    let run = RunFolder::create(&tmp.0, "rows", "rows").unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());
    let mut ids = Vec::new();
    for i in 0..25 {
        let mut e = event(&run.id, "srv-web", "fetch", Class::Read, LedgerMode::Allow);
        e.args = json!({ "url": format!("https://e.example/{i}"), "n": i });
        e.result_summary = format!("page {i}");
        e.consequences = json!([{ "navigated": format!("https://e.example/{i}") }]);
        e.duration_ms = i;
        e.why = (i % 3 == 0).then(|| format!("why {i}"));
        ids.push(ledger.write(e).await.unwrap());
    }
    // An event for another run in the same store does not show up in this run's rows.
    store
        .append(event(
            &RunId("other".into()),
            "core",
            "x",
            Class::Read,
            LedgerMode::Allow,
        ))
        .await
        .unwrap();

    let table = store.ledger_events(&run.id).await.unwrap();
    let jsonl = read_jsonl(&run.ledger_jsonl()).unwrap();
    assert_eq!(table.len(), 25);
    assert_eq!(jsonl, table);
    assert_eq!(jsonl.iter().map(|(id, _)| *id).collect::<Vec<_>>(), ids);
    assert!(ids.windows(2).all(|w| w[0] < w[1]));
    let text = std::fs::read_to_string(run.ledger_jsonl()).unwrap();
    assert_eq!(text.lines().count(), 25);
    assert!(text.ends_with('\n'));
    let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(first["id"], json!(ids[0].0));
    assert_eq!(first["server"], json!("srv-web"));
}

#[tokio::test]
async fn concurrent_writers_keep_lines_whole_and_ordered() {
    install();
    let tmp = Tmp::new();
    let run = RunFolder::create(&tmp.0, "concurrent", "c").unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Arc::new(Ledger::new(store.clone(), run.clone()));
    let mut tasks = Vec::new();
    for t in 0..8 {
        let ledger = ledger.clone();
        let run = run.id.clone();
        tasks.push(tokio::spawn(async move {
            for i in 0..10 {
                let mut e = event(&run, "srv-web", "fetch", Class::Read, LedgerMode::Allow);
                e.args = json!({ "t": t, "i": i, "pad": "x".repeat(500) });
                ledger.write(e).await.unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let jsonl = read_jsonl(&run.ledger_jsonl()).unwrap();
    assert_eq!(jsonl.len(), 80);
    assert_eq!(jsonl, store.ledger_events(&run.id).await.unwrap());
}

#[tokio::test]
async fn canary_never_reaches_jsonl_or_table() {
    install();
    let c = canary();
    let tmp = Tmp::new();
    let run = RunFolder::create(&tmp.0, "canary", "log in").unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());

    let mut e = event(&run.id, "echo", "echo.send", Class::Send, LedgerMode::Allow);
    e.args = json!({
        "to": "a@x.com",
        "body": format!("pw {c}"),
        "nested": [{ "deep": c.clone() }, format!("pre{c}post")],
        c.clone(): "as a key",
    });
    e.result_summary = format!("typed {c}");
    e.consequences = json!([format!("filled {c}")]);
    e.why = Some(format!("because {c}"));
    e.tool = format!("echo.send{c}");
    ledger.write(e).await.unwrap();

    let text = std::fs::read_to_string(run.ledger_jsonl()).unwrap();
    assert!(!text.contains(&c), "canary in ledger.jsonl");
    assert!(text.contains("@vault(test)"));
    let rows = store.ledger_events(&run.id).await.unwrap();
    let rows_json = serde_json::to_string(&rows).unwrap();
    assert!(!rows_json.contains(&c), "canary in ledger table");
    let (_, row) = &rows[0];
    assert_eq!(row.args["body"], json!("pw @vault(test)"));
    assert_eq!(row.args["nested"][0]["deep"], json!("@vault(test)"));
    assert_eq!(row.args["@vault(test)"], json!("as a key"));
    assert_eq!(row.result_summary, "typed @vault(test)");
    assert_eq!(row.why.as_deref(), Some("because @vault(test)"));
    assert_eq!(read_jsonl(&run.ledger_jsonl()).unwrap(), rows);

    let r = write_report(&run, &format!("log in with {c}"), std::slice::from_ref(row)).unwrap();
    assert!(!r.contains(&c), "canary in report");
    for (path, contents) in all_files(&run.dir) {
        assert!(!contents.contains(&c), "canary in {}", path.display());
    }
}

#[tokio::test]
async fn report_done_partial_stopped_from_written_events() {
    install();
    let c = canary();
    let tmp = Tmp::new();
    let store = Arc::new(MemStore::new());

    // (a) all ok → done
    let run = RunFolder::create(&tmp.0, "a", "a").unwrap();
    let ledger = Ledger::new(store.clone(), run.clone());
    let mut fetch = event(&run.id, "srv-web", "fetch", Class::Read, LedgerMode::Allow);
    fetch.args = json!({ "url": "https://a.example/" });
    ledger.write(fetch.clone()).await.unwrap();
    let mut send = event(&run.id, "echo", "echo.send", Class::Send, LedgerMode::Allow);
    send.args = json!({ "to": "bob@x.com", "body": c.clone() });
    send.consequences = json!([format!("sent {c}")]);
    ledger.write(send.clone()).await.unwrap();
    let events: Vec<LedgerEvent> = ledger
        .read_back()
        .unwrap()
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    let r = report(&format!("Say hi to Bob ({c})"), &events);
    assert!(
        r.starts_with("# Say hi to Bob (@vault(test))\nResult: done\n"),
        "{r}"
    );
    assert!(r.contains("Read: 1 pages, 0 mails, 0 files (https://a.example/)"));
    assert!(r.contains("Did: 1 actions (Send echo.send bob@x.com: sent @vault(test))"));
    assert!(!r.contains(&c));

    // (b) a timeout_skip → partial
    let run = RunFolder::create(&tmp.0, "b", "b").unwrap();
    let ledger = Ledger::new(store.clone(), run.clone());
    ledger.write(fetch.clone()).await.ok();
    let mut skip = event(&run.id, "core", "act", Class::Pay, LedgerMode::TimeoutSkip);
    skip.why = Some(format!("pay with card {c}"));
    ledger.write(skip).await.unwrap();
    let events: Vec<LedgerEvent> = store
        .ledger_events(&run.id)
        .await
        .unwrap()
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    let r = write_report(&run, "buy", &events).unwrap();
    assert!(r.contains("\nResult: partial\n"), "{r}");
    assert!(r.contains("Asked: 1 (0 answered / 1 timed out)"), "{r}");
    assert!(!r.contains(&c));

    // (c) a challenge stop → stopped (why)
    let run = RunFolder::create(&tmp.0, "c", "c").unwrap();
    let ledger = Ledger::new(store.clone(), run.clone());
    let mut stop = event(&run.id, "core", "act", Class::Challenge, LedgerMode::Stop);
    stop.ok = false;
    stop.why = Some("hCaptcha on luma.com at #31".into());
    ledger.write(stop).await.unwrap();
    let events: Vec<LedgerEvent> = read_jsonl(&run.ledger_jsonl())
        .unwrap()
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    let r = write_report(&run, "Register on Luma", &events).unwrap();
    assert!(
        r.contains("\nResult: stopped (hCaptcha on luma.com at #31)\n"),
        "{r}"
    );
    assert!(r.contains("handed back to you"), "{r}");
    assert_eq!(std::fs::read_to_string(run.report_md()).unwrap(), r);
}
