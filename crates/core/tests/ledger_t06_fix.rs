//! T-06 fix round regressions (H-2, H-3, H-4, Q-4, D-102): numbers in ledger events are
//! scrubbed and the serialized event is checked before the insert (fail closed); report.md
//! scrubs before it clips or collapses whitespace; a slug-shaped vault value cannot reach a
//! run id; several Ledgers on one run still match the table keyed by id.
//!
//! The vault here: the ledger canary (`SB_VAULT_CANARY` or `canary-ledger-not-a-secret`),
//! a numeric canary, a canary with a double space, and the literal ledger canary under its
//! own name. Installed once as the process-wide scrub. No assertion message formats a
//! canary.
//! `cargo test -p harness-core --test ledger_t06_fix`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};

use harness_core::class::Class;
use harness_core::ledger::{read_jsonl, report, write_report, Ledger, LedgerError};
use harness_core::memory::{ts_now, LedgerEvent, LedgerMode, MemoryStore};
use harness_core::run::{RunFolder, RunId};
use harness_core::testing::MemStore;
use harness_core::vault::{EntryKind, Vault};
use serde_json::json;

/// The default ledger canary; also installed under its own name for the slug test.
const LEDGER_CANARY: &str = "canary-ledger-not-a-secret";
/// A numeric canary (an OTP, PIN or card-number shaped vault value).
const NUM: &str = "48291377";
/// A canary whose spelling a whitespace collapse changes.
const SPACED: &str = "canary-ledger  not-a-secret";

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| LEDGER_CANARY.to_owned())
}

fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let mut entries = vec![
            ("test".to_owned(), canary(), EntryKind::Secret),
            ("otp".to_owned(), NUM.to_owned(), EntryKind::Secret),
            ("spaced".to_owned(), SPACED.to_owned(), EntryKind::Secret),
        ];
        if canary() != LEDGER_CANARY {
            entries.push((
                "slug".to_owned(),
                LEDGER_CANARY.to_owned(),
                EntryKind::Secret,
            ));
        }
        Vault::in_memory(entries).install_global();
    });
}

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        Tmp(std::env::temp_dir().join(format!(
            "sb-ledger-t06fix-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
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

/// Item 1: a vault value sent as a JSON number is scrubbed in the store and the jsonl;
/// a matching number becomes its scrubbed text as a string.
#[tokio::test]
async fn numeric_vault_value_is_scrubbed_in_store_and_jsonl() {
    install();
    let tmp = Tmp::new();
    let run = RunFolder::create(&tmp.0, "otp", "enter the code").unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());
    let n: u64 = NUM.parse().unwrap();
    let mut e = event(
        &run.id,
        "sheetbrowse",
        "act.type",
        Class::Fill,
        LedgerMode::Allow,
    );
    e.args = json!({
        "otp": n,
        "list": [n, 7, { "deep": n }],
        "longer": n * 10,
        "float": n as f64,
        "negative": -(n as i64),
        "plain": 1234,
    });
    e.consequences = json!([n, { "typed": n }]);
    ledger.write(e).await.unwrap();

    let text = std::fs::read_to_string(run.ledger_jsonl()).unwrap();
    assert!(!text.contains(NUM), "numeric canary in ledger.jsonl");
    let rows = store.ledger_events(&run.id).await.unwrap();
    assert!(
        !serde_json::to_string(&rows).unwrap().contains(NUM),
        "numeric canary in table"
    );
    let row = &rows[0].1;
    assert_eq!(row.args["otp"], json!("@vault(otp)"));
    assert_eq!(row.args["list"][0], json!("@vault(otp)"));
    assert_eq!(row.args["list"][1], json!(7));
    assert_eq!(row.args["list"][2]["deep"], json!("@vault(otp)"));
    assert_eq!(row.args["longer"], json!("@vault(otp)0"));
    assert_eq!(row.args["negative"], json!("-@vault(otp)"));
    assert!(row.args["float"]
        .as_str()
        .unwrap()
        .starts_with("@vault(otp)"));
    assert_eq!(row.args["plain"], json!(1234));
    assert_eq!(
        row.consequences,
        json!(["@vault(otp)", { "typed": "@vault(otp)" }])
    );
    assert_eq!(read_jsonl(&run.ledger_jsonl()).unwrap(), rows);
}

/// Item 1 (fail closed): a field scrub cannot rewrite (`duration_ms`) still matches a
/// vault value: nothing is inserted, no line is written, the error names the field only.
#[tokio::test]
async fn unscrubbable_event_is_refused_before_insert() {
    install();
    let tmp = Tmp::new();
    let run = RunFolder::create(&tmp.0, "refuse", "p").unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());
    let mut e = event(&run.id, "web", "fetch", Class::Read, LedgerMode::Allow);
    e.duration_ms = NUM.parse().unwrap();
    let err = ledger.write(e).await.unwrap_err();
    let msg = err.to_string();
    assert!(!msg.contains(NUM), "numeric canary in error");
    match err {
        LedgerError::Unscrubbable { fields } => assert_eq!(fields, "duration_ms"),
        _ => panic!("expected Unscrubbable"),
    }
    assert!(store.ledger_events(&run.id).await.unwrap().is_empty());
    assert!(!run.ledger_jsonl().exists());

    // The same ledger keeps working for clean events.
    let ok = event(&run.id, "web", "fetch", Class::Read, LedgerMode::Allow);
    ledger.write(ok).await.unwrap();
    assert_eq!(read_jsonl(&run.ledger_jsonl()).unwrap().len(), 1);
}

/// Item 2: every report input is scrubbed before clip/collapse. A value crossing the
/// 117-char (task) or 97-char (item) cut, or one whose spaces a collapse would change,
/// leaves no part behind.
#[tokio::test]
async fn report_scrubs_before_clipping_and_collapsing() {
    install();
    let c = canary();
    let tmp = Tmp::new();
    let run = RunFolder::create(&tmp.0, "clip", "p").unwrap();
    let k = c.chars().count() / 2;
    let head: String = c.chars().take(k).collect();
    assert!(!head.is_empty());
    let pad = |n: usize| "x".repeat(n);

    // The task line is cut at 117 chars; the value starts `k` chars before the cut.
    let prompt = format!("{}{c} and more", pad(117 - k));
    let mut send = event(&run.id, "web", "post", Class::Send, LedgerMode::Allow);
    // The target and the consequence are cut at 97 chars.
    send.args = json!({ "to": format!("{}{c}", pad(97 - k)) });
    send.consequences = json!([format!("{}{c}", pad(97 - k))]);
    let mut ask = event(
        &run.id,
        "core",
        "post",
        Class::Send,
        LedgerMode::AnsweredProceed,
    );
    ask.why = Some(format!("{}{c}", pad(97 - k)));
    let mut fail = event(&run.id, "web", "post2", Class::Send, LedgerMode::Allow);
    fail.ok = false;
    fail.why = Some(format!("typed {SPACED} then failed"));
    fail.result_summary = format!("summary\t{SPACED}");
    let mut stop = event(&run.id, "core", "act", Class::Challenge, LedgerMode::Stop);
    stop.ok = false;
    stop.why = Some(format!("{}{SPACED}", pad(90)));
    let events = vec![ask, send, fail, stop];

    let cut_prefix = format!("x{head}");
    let collapsed = SPACED.split_whitespace().collect::<Vec<_>>().join(" ");
    let first_word = SPACED.split_whitespace().next().unwrap();
    for r in [
        report(&prompt, &events),
        report(&format!("{SPACED} please"), &events),
        write_report(&run, &prompt, &events).unwrap(),
    ] {
        assert!(!r.contains(&c), "canary in report");
        assert!(!r.contains(&cut_prefix), "clipped canary prefix in report");
        assert!(!r.contains(&collapsed), "collapsed spaced canary in report");
        assert!(!r.contains(first_word), "part of spaced canary in report");
        assert!(r.contains("@vault(test)"));
        assert!(r.contains("@vault(spaced)"));
    }
    let md = std::fs::read_to_string(run.report_md()).unwrap();
    assert!(!md.contains(&cut_prefix) && !md.contains(&collapsed));
}

/// Item 7: a slug-shaped vault value written with other case or punctuation only matches
/// after cleaning; it must not become the run id.
#[test]
fn slug_shaped_value_never_reaches_run_id() {
    install();
    let tmp = Tmp::new();
    let spelled = LEDGER_CANARY.replace('-', "_").to_uppercase();
    let run = RunFolder::create(&tmp.0, &format!("login {spelled}"), "p").unwrap();
    assert!(!run.id.0.contains(LEDGER_CANARY), "canary in run id");
    assert!(
        !run.id.0.to_lowercase().contains("ledger-not"),
        "canary part in run id"
    );
    assert!(run.id.0.contains("-login-vault-"), "{}", run.id);
    assert_eq!(RunFolder::open(&tmp.0, &run.id).unwrap(), run);
    // Nothing but the value: still never the value.
    let only = harness_core::run::slugify(&spelled);
    assert!(!only.contains("ledger"), "canary part in slug");
}

/// Item 4: two Ledgers (two tool server processes) on one run; the jsonl read back
/// equals the table keyed by id even when appends land out of id order.
#[tokio::test]
async fn two_ledgers_on_one_run_match_table_by_id() {
    install();
    let tmp = Tmp::new();
    let run = RunFolder::create(&tmp.0, "two", "p").unwrap();
    let store = Arc::new(MemStore::new());
    let a = Arc::new(Ledger::new(store.clone(), run.clone()));
    let b = Arc::new(Ledger::new(
        store.clone(),
        RunFolder::open(&tmp.0, &run.id).unwrap(),
    ));
    let mut tasks = Vec::new();
    for (t, ledger) in [a.clone(), b.clone(), a, b].into_iter().enumerate() {
        let run = run.id.clone();
        tasks.push(tokio::spawn(async move {
            for i in 0..15 {
                let mut e = event(&run, "web", "fetch", Class::Read, LedgerMode::Allow);
                e.args = json!({ "url": format!("https://e.example/{t}/{i}") });
                ledger.write(e).await.unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let table = store.ledger_events(&run.id).await.unwrap();
    assert_eq!(table.len(), 60);
    assert_eq!(read_jsonl(&run.ledger_jsonl()).unwrap(), table);
}
