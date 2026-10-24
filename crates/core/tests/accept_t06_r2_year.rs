//! T-06 acceptance, round 2 (tester): a short, year-shaped vault value (`2026`) must not
//! make every `Ledger::write` fail. Its own test binary because it installs a different
//! process-wide vault than `accept_t06_r2`. docs/01 H-2, H-3; D-102.
//!
//! `cargo test -p harness-core --test accept_t06_r2_year`
//! Expected to fail on 5bf1f37: the `ts` of every event serializes as `2026-...`, which the
//! whole-line check reports as `Unscrubbable { fields: "ts" }`.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use harness_core::class::Class;
use harness_core::ledger::{read_jsonl, Ledger};
use harness_core::memory::{ts_now, LedgerEvent, LedgerMode, MemoryStore};
use harness_core::run::RunFolder;
use harness_core::testing::MemStore;
use harness_core::vault::{EntryKind, Vault};
use serde_json::json;

const YEAR: &str = "2026";

struct Tmp(PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
#[ignore = "red-first T-06 round 2"]
async fn r2_year_shaped_vault_value_does_not_make_write_fail() {
    Vault::in_memory([("pin".to_owned(), YEAR.to_owned(), EntryKind::Secret)]).install_global();
    let tmp = Tmp(std::env::temp_dir().join(format!(
        "sb-accept-t06r2-year-{}-{}",
        std::process::id(),
        Utc::now().timestamp_micros()
    )));
    let run = RunFolder::create(&tmp.0.join("runs"), "year", "year").unwrap();
    let store = Arc::new(MemStore::new());
    let ledger = Ledger::new(store.clone(), run.clone());
    let e = LedgerEvent {
        run_id: Some(run.id.clone()),
        ts: ts_now(),
        server: "web".into(),
        tool: "fetch".into(),
        class: Some(Class::Read),
        mode: Some(LedgerMode::Allow),
        args: json!({ "url": "https://a.example/", "pin": YEAR }),
        result_summary: "read".into(),
        consequences: json!([]),
        duration_ms: 2,
        ok: true,
        why: None,
    };
    let res = ledger.write(e).await;
    if let Err(err) = &res {
        // The error names fields only, never the value.
        println!("Ledger::write error: {err}");
    }
    assert!(
        res.is_ok(),
        "Ledger::write failed for a year-shaped vault value"
    );
    let rows = store.ledger_events(&run.id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(read_jsonl(&run.ledger_jsonl()).unwrap(), rows);
    // The value in args is still scrubbed.
    assert_eq!(rows[0].1.args["pin"], json!("@vault(pin)"));
}
