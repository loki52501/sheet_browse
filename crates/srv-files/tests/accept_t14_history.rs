//! T-14 acceptance round 1 (tester), F-6 `history.search`: the temp copy is removed after
//! the call, and a source locked the way Chrome locks it (SQLite exclusive locking, the
//! file shared for read) still works. Its own test binary because it points the process
//! temp folder (TMP/TEMP/TMPDIR) at a folder it then checks is empty.

use harness_core::memory::{ts_now, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::Vault;
use harness_core::{dispatch, Harness, ToolRegistry};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn fixture(path: &Path) {
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute_batch(
        "CREATE TABLE urls(id INTEGER PRIMARY KEY AUTOINCREMENT, url LONGVARCHAR, title LONGVARCHAR,
            visit_count INTEGER DEFAULT 0 NOT NULL, typed_count INTEGER DEFAULT 0 NOT NULL,
            last_visit_time INTEGER NOT NULL, hidden INTEGER DEFAULT 0 NOT NULL);
         CREATE TABLE visits(id INTEGER PRIMARY KEY, url INTEGER NOT NULL, visit_time INTEGER NOT NULL,
            from_visit INTEGER, transition INTEGER DEFAULT 0 NOT NULL, segment_id INTEGER,
            visit_duration INTEGER DEFAULT 0 NOT NULL);",
    )
    .unwrap();
    for (url, title, when) in [
        (
            "https://docs.rs/chromiumoxide",
            "chromiumoxide - Rust CDP client",
            "2026-10-20T14:03:00Z",
        ),
        (
            "https://github.com/mattsse/chromiumoxide",
            "Chrome DevTools Protocol (cdp) for rust",
            "2026-10-22T09:00:00Z",
        ),
        (
            "https://example.com/other",
            "unrelated",
            "2026-10-24T09:00:00Z",
        ),
    ] {
        let t = chrono::DateTime::parse_from_rfc3339(when)
            .unwrap()
            .with_timezone(&chrono::Utc);
        let ct = srv_files::history::to_chrome(t);
        c.execute(
            "INSERT INTO urls(url, title, visit_count, last_visit_time) VALUES (?1, ?2, 1, ?3)",
            rusqlite::params![url, title, ct],
        )
        .unwrap();
        let id = c.last_insert_rowid();
        c.execute(
            "INSERT INTO visits(url, visit_time, transition) VALUES (?1, ?2, 805306368)",
            rusqlite::params![id, ct],
        )
        .unwrap();
    }
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect()
}

#[tokio::test]
async fn r1_history_copy_removed_and_locked_source_works() {
    // Everything this test owns lives under `work`; the process temp folder is `tmp`.
    let work = tempfile::tempdir().unwrap();
    let tmp = work.path().join("proc-tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    for k in ["TMP", "TEMP", "TMPDIR"] {
        std::env::set_var(k, &tmp);
    }
    // Control: temp folders now go to `tmp`, so a leftover copy would be seen there.
    {
        let probe = tempfile::tempdir().unwrap();
        assert!(probe.path().starts_with(&tmp), "control: TMP is honoured");
    }
    assert!(entries(&tmp).is_empty());

    let runs_root = work.path().join("runs");
    let run = RunFolder::create(&runs_root, "t14h", "history acceptance").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "history acceptance".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    let harness = Harness {
        store: store.clone(),
        vault: Arc::new(Vault::in_memory([])),
        asker: Arc::new(ScriptedAsker::new()),
        runs_root: runs_root.clone(),
        run: Some(run.id.clone()),
    };
    let src = work.path().join("chrome").join("History");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    fixture(&src);
    let registry = ToolRegistry::new(srv_files::history::tools_with(Some(src.clone())));
    let call =
        |args: serde_json::Value| dispatch("files", &registry, &harness, "history.search", args);

    // 1. Plain call: rows, and no copy left behind.
    let out = call(json!({"q": "rust cdp"})).await.unwrap();
    assert!(
        out.text.starts_with("2 results for \"rust cdp\""),
        "{}",
        out.text
    );
    assert!(out
        .text
        .contains("https://github.com/mattsse/chromiumoxide"));
    assert!(
        entries(&tmp).is_empty(),
        "the copy is removed after: {:?}",
        entries(&tmp)
    );

    // 2. Locked the way Chrome locks it: an open connection holding an EXCLUSIVE lock with
    // an uncommitted write (a hot -journal beside the file).
    {
        let holder = rusqlite::Connection::open(&src).unwrap();
        holder
            .execute_batch(
                "PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE;
                 UPDATE urls SET title = 'uncommitted rust cdp' WHERE id = 3;",
            )
            .unwrap();
        // Control: the lock is real (another connection cannot read or write).
        let other = rusqlite::Connection::open(&src).unwrap();
        assert!(
            other
                .query_row("SELECT count(*) FROM urls", [], |r| r.get::<_, i64>(0))
                .is_err(),
            "control: the source is locked"
        );
        drop(other);
        let out = call(json!({"q": "rust cdp"})).await.unwrap();
        assert!(
            out.text.starts_with("2 results for \"rust cdp\""),
            "locked source: {}",
            out.text
        );
        assert!(!out.text.contains("uncommitted"), "{}", out.text);
        assert!(entries(&tmp).is_empty(), "copy removed (locked case)");
        holder.execute_batch("ROLLBACK;").unwrap();
    }

    // 3. Windows: a handle that shares nothing (stricter than Chrome). Reading is
    // impossible for any process; the tool must fail with a clear message, not hang or
    // leave a copy.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let h = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&src)
            .unwrap();
        assert!(
            std::fs::read(&src).is_err(),
            "control: share-none handle held"
        );
        let err = call(json!({"q": "rust cdp"})).await.unwrap_err();
        let m = err.to_string();
        assert!(m.contains("could not copy the History file"), "{m}");
        assert!(m.contains("close Chrome"), "{m}");
        assert!(entries(&tmp).is_empty(), "no copy left (share-none case)");
        drop(h);
    }

    // The source was never changed.
    let c = rusqlite::Connection::open(&src).unwrap();
    let t: String = c
        .query_row("SELECT title FROM urls WHERE id = 3", [], |r| r.get(0))
        .unwrap();
    assert_eq!(t, "unrelated");
}
