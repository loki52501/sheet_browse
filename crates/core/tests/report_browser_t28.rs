//! T-28 (c), D-154: the vault canary in a browseros.jsonl line (in the tool name, an arg key,
//! an arg value, the result summary and the why) never reaches report.md; it shows as
//! `@vault(test)`. A separate test binary because it installs the process-wide scrub, which
//! harness-core's unit tests must leave empty (vault::tests::global_scrub_before_and_after_install).
//! No assertion message formats the canary.
//! `cargo test -p harness-core --test report_browser_t28`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use harness_core::ledger::write_report;
use harness_core::run::RunFolder;
use harness_core::vault::{EntryKind, Vault};
use serde_json::json;

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "canary-report-browser-not-a-secret".to_owned())
}

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        Tmp(std::env::temp_dir().join(format!(
            "sb-report-bos-{}-{}-{}",
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

#[test]
fn canary_in_browseros_jsonl_never_reaches_report() {
    let c = canary();
    Vault::in_memory([("test".to_owned(), c.clone(), EntryKind::Secret)]).install_global();
    let tmp = Tmp::new();
    let run = RunFolder::create(&tmp.0, "bos-canary", "Log in").unwrap();
    let line = |tool: &str, args: serde_json::Value, ok: bool, summary: &str, why: &str| {
        json!({
            "run_id": run.id.0, "ts": "2026-10-27T09:00:00+0000", "server": "browseros",
            "tool": tool, "class": null, "mode": "external", "args": args,
            "result_summary": summary, "ok": ok, "why": if ok { None } else { Some(why) },
        })
        .to_string()
    };
    // A canary straddling the 120-char summary cut: scrubbed before the cut, so no prefix
    // of it is left.
    let pad = "y".repeat(110);
    let body = [
        line(
            "act",
            json!({"kind": "type", "text": c}),
            true,
            &format!("typed {c}"),
            "",
        ),
        line(&c, json!({ c.as_str(): 1 }), false, "", &format!("bad {c}")),
        line("read", json!({}), true, &format!("{pad}{c} after"), ""),
    ]
    .join("\n");
    std::fs::write(run.browseros_jsonl(), body + "\n").unwrap();
    let text = write_report(&run, "Log in", &[]).unwrap();
    let on_disk = std::fs::read_to_string(run.report_md()).unwrap();
    assert!(!on_disk.contains(&c), "report.md holds the canary");
    assert!(!text.contains(&c), "the returned report holds the canary");
    assert!(text.contains("Browser: 3 calls"));
    assert!(text.contains("1. act kind=type text=@vault(test) -> ok: typed @vault(test)"));
    assert!(text.contains("2. @vault(test) @vault(test)=1 -> failed: bad @vault(test)"));
    assert!(text.contains(&format!("3. read -> ok: {pad}@vault(...")));
    assert!(!text.contains(&c[..8]), "a canary prefix survived the cut");
}
