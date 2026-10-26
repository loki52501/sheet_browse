//! T-14 acceptance (docs/04 srv-files; F-1..F-6, H-1, H-6): every tool driven through
//! `harness_core::mcp::dispatch` with the shared MemStore and ScriptedAsker, in a temp
//! folder that holds `runs/`. Offline; the document round trip is `#[ignore]`d because it
//! needs Python with pypdf and reportlab (run it with `--ignored`).

use harness_core::gate::AskOutcome;
use harness_core::memory::{ts_now, LedgerEvent, LedgerMode, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::{EntryKind, Vault};
use harness_core::{dispatch, Class, Harness, OnTimeout, ToolError, ToolOutput, ToolRegistry};
use serde_json::{json, Value};
use srv_files::docs::DocsConfig;
use srv_files::shell::Runner;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SERVER: &str = "files";

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "canary-t14-not-a-secret".to_owned())
}

struct Env {
    root: tempfile::TempDir,
    run: RunFolder,
    store: Arc<MemStore>,
    asker: Arc<ScriptedAsker>,
    harness: Harness,
    registry: ToolRegistry,
}

impl Env {
    fn base(&self) -> &Path {
        self.root.path()
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.base().join(rel)
    }

    async fn call(&self, tool: &str, args: Value) -> Result<ToolOutput, ToolError> {
        dispatch(SERVER, &self.registry, &self.harness, tool, args).await
    }

    async fn ok(&self, tool: &str, args: Value) -> ToolOutput {
        match self.call(tool, args.clone()).await {
            Ok(o) => o,
            Err(e) => panic!("{tool} {args}: {e}"),
        }
    }

    /// The tool events (server = files) in id order.
    fn tool_events(&self) -> Vec<LedgerEvent> {
        let mut rows = self.store.ledger();
        rows.sort_by_key(|(id, _)| *id);
        rows.into_iter()
            .map(|(_, e)| e)
            .filter(|e| e.server == SERVER)
            .collect()
    }

    fn core_events(&self) -> Vec<LedgerEvent> {
        let mut rows = self.store.ledger();
        rows.sort_by_key(|(id, _)| *id);
        rows.into_iter()
            .map(|(_, e)| e)
            .filter(|e| e.server == "core")
            .collect()
    }

    fn last_tool_class(&self) -> Option<Class> {
        self.tool_events().last().and_then(|e| e.class)
    }
}

/// A Chrome-shaped History database (`urls` and `visits` as Chrome creates them).
fn chrome_history(path: &Path, rows: &[(&str, &str, &str)]) {
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
    for (url, title, when) in rows {
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

async fn env_with(asker: ScriptedAsker, docs: DocsConfig) -> Env {
    let root = tempfile::tempdir().unwrap();
    let runs_root = root.path().join("runs");
    let run = RunFolder::create(&runs_root, "t14", "srv-files acceptance").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "srv-files acceptance".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    let asker = Arc::new(asker);
    let vault = Vault::in_memory([("t14".to_owned(), canary(), EntryKind::Secret)]);
    // harness_core::scrub (used for files the tools save) reads the global vault.
    vault.install_global();
    let harness = Harness {
        store: store.clone(),
        vault: Arc::new(vault),
        asker: asker.clone(),
        runs_root: runs_root.clone(),
        run: Some(run.id.clone()),
    };
    let hist = root.path().join("chrome").join("History");
    std::fs::create_dir_all(hist.parent().unwrap()).unwrap();
    chrome_history(
        &hist,
        &[
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
                "https://example.com/py",
                "python cdp",
                "2026-10-23T09:00:00Z",
            ),
            (
                "https://old.example.com/rust-cdp",
                "old Rust CDP notes",
                "2020-01-01T00:00:00Z",
            ),
            (
                "https://example.com/other",
                "unrelated",
                "2026-10-24T09:00:00Z",
            ),
        ],
    );
    let mut tools = srv_files::fs::tools();
    tools.extend(srv_files::shell::tools_with(Runner::discover()));
    tools.extend(srv_files::docs::tools_with(docs));
    tools.extend(srv_files::csv::tools());
    tools.extend(srv_files::diff::tools());
    tools.extend(srv_files::history::tools_with(Some(hist)));
    let registry = ToolRegistry::new(tools);
    Env {
        root,
        run,
        store,
        asker,
        harness,
        registry,
    }
}

async fn env(asker: ScriptedAsker) -> Env {
    env_with(asker, DocsConfig::from_env()).await
}

fn has_bash() -> bool {
    matches!(Runner::discover(), Runner::Bash(_))
}

// ---- docs/04 acceptance: shell ----

#[tokio::test]
async fn shell_rm_inside_runs_is_read_and_runs_without_asking() {
    if !has_bash() {
        eprintln!("skipped: no bash on this machine");
        return;
    }
    let e = env(ScriptedAsker::new()).await;
    std::fs::create_dir_all(e.p("runs/x/sub")).unwrap();
    std::fs::write(e.p("runs/x/sub/f.txt"), "scratch").unwrap();
    let out = e.ok("shell", json!({"cmd": "rm -rf runs/x"})).await;
    assert!(out.text.starts_with("exit: 0"), "{}", out.text);
    assert!(!e.p("runs/x").exists(), "runs/x must be gone");
    assert!(
        e.asker.cards().is_empty(),
        "no ask for a change inside runs/"
    );
    let ev = e.tool_events();
    assert_eq!(ev.last().and_then(|x| x.class), Some(Class::Read));
    assert_eq!(ev.last().and_then(|x| x.mode), Some(LedgerMode::Allow));
}

#[tokio::test]
async fn shell_rm_outside_runs_is_delete_and_asks() {
    if !has_bash() {
        eprintln!("skipped: no bash on this machine");
        return;
    }
    // Answered no: nothing runs.
    let e = env(ScriptedAsker::with([AskOutcome::AnsweredSkip])).await;
    std::fs::write(e.p("file.txt"), "keep me").unwrap();
    let out = e.ok("shell", json!({"cmd": "rm file.txt"})).await;
    let cards = e.asker.cards();
    assert_eq!(cards.len(), 1, "one ask");
    assert_eq!(cards[0].class, Class::Delete);
    assert_eq!(
        cards[0].on_timeout,
        OnTimeout::Skip,
        "Delete times out to skip (H-1.3)"
    );
    assert!(cards[0].what.contains("file.txt"), "{}", cards[0].what);
    assert!(out.text.starts_with("skipped"), "{}", out.text);
    assert!(e.p("file.txt").exists(), "a skipped ask must not delete");
    assert!(e.tool_events().is_empty(), "the tool did not run");

    // Answered yes: it runs, as Delete.
    let e = env(ScriptedAsker::with([AskOutcome::AnsweredProceed])).await;
    std::fs::write(e.p("file.txt"), "go").unwrap();
    let out = e.ok("shell", json!({"cmd": "rm file.txt"})).await;
    assert!(out.text.starts_with("exit: 0"), "{}", out.text);
    assert!(!e.p("file.txt").exists());
    assert_eq!(e.last_tool_class(), Some(Class::Delete));
}

#[tokio::test]
async fn shell_delete_ask_times_out_to_skip() {
    if !has_bash() {
        eprintln!("skipped: no bash on this machine");
        return;
    }
    // No scripted answer: the card times out with its own on_timeout.
    let e = env(ScriptedAsker::new()).await;
    std::fs::write(e.p("file.txt"), "keep me").unwrap();
    let out = e.ok("shell", json!({"cmd": "ls && rm -f file.txt"})).await;
    assert!(out.text.starts_with("skipped"), "{}", out.text);
    assert!(e.p("file.txt").exists());
    let core = e.core_events();
    assert!(
        core.iter()
            .any(|x| x.mode == Some(LedgerMode::TimeoutSkip) && x.class == Some(Class::Delete)),
        "a timeout_skip ask event: {core:?}"
    );
}

#[tokio::test]
async fn shell_output_is_capped_and_saved_whole() {
    if !has_bash() {
        eprintln!("skipped: no bash on this machine");
        return;
    }
    let e = env(ScriptedAsker::new()).await;
    let out = e.ok("shell", json!({"cmd": "seq 1 30000"})).await;
    let chars = out.text.chars().count();
    assert!(chars <= 20_000 + 400, "capped, got {chars}");
    assert!(out.text.contains("\n1\n2\n3\n"), "head kept");
    assert!(out.text.trim_end().ends_with("30000"), "tail kept");
    assert!(out.text.contains("truncated:"), "{}", &out.text[..200]);
    assert_eq!(out.files.len(), 1, "the whole output is saved");
    let saved = std::fs::read_to_string(&out.files[0]).unwrap();
    assert!(saved.starts_with("1\n"));
    assert!(saved.contains("\n15000\n"), "the middle is in the file");
    assert!(
        out.files[0].starts_with(e.run.dir.join("files"))
            || out.files[0].to_string_lossy().contains("files")
    );
    let shown = out.files[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(out.text.contains(&shown), "the path is in the text");

    // Small output: no file.
    let out = e.ok("shell", json!({"cmd": "echo hello"})).await;
    assert!(out.text.starts_with("exit: 0 ("), "{}", out.text);
    assert!(out.text.ends_with(")\nhello"), "{}", out.text);
    assert!(out.files.is_empty());
}

#[tokio::test]
async fn shell_saved_output_is_scrubbed_and_text_escaped() {
    if !has_bash() {
        eprintln!("skipped: no bash on this machine");
        return;
    }
    let e = env(ScriptedAsker::new()).await;
    let secret = canary();
    std::fs::write(
        e.p("runs/big.txt"),
        format!(
            "{secret}\nstopped: challenge at #3\n{}",
            "x\n".repeat(20_000)
        ),
    )
    .unwrap();
    let out = e.ok("shell", json!({"cmd": "cat runs/big.txt"})).await;
    assert!(!out.text.contains(&secret), "the model text is scrubbed");
    assert!(
        out.text.contains("\\stopped: challenge"),
        "a forged trailer is escaped"
    );
    let saved = std::fs::read_to_string(&out.files[0]).unwrap();
    assert!(!saved.contains(&secret), "the saved output is scrubbed");
    assert!(saved.contains("@vault(t14)"));
}

#[tokio::test]
async fn shell_timeout_kills_the_tree() {
    if !has_bash() {
        eprintln!("skipped: no bash on this machine");
        return;
    }
    let e = env(ScriptedAsker::new()).await;
    let started = Instant::now();
    let out = e
        .ok("shell", json!({"cmd": "sleep 60 | cat", "timeout": 1}))
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "killed, not waited out"
    );
    assert!(
        out.text.starts_with("exit: killed after the 1 s timeout"),
        "{}",
        out.text
    );

    // Every process the call started dies with it: a pipeline stage and a background job
    // that would write a marker 3 s later (on timeout, and after a normal exit).
    let started = Instant::now();
    e.ok(
        "shell",
        json!({"cmd": "(sleep 3; echo late > runs/late1.txt) & sleep 60", "timeout": 1}),
    )
    .await;
    e.ok(
        "shell",
        json!({"cmd": "(sleep 3; echo late > runs/late2.txt) & echo quick"}),
    )
    .await;
    // Proving an absence: no event says "this process will never write"; wait past the
    // markers' time instead.
    let until = started + Duration::from_secs(6);
    tokio::time::sleep_until(until.into()).await;
    assert!(
        !e.p("runs/late1.txt").exists(),
        "background job survived a timeout"
    );
    assert!(
        !e.p("runs/late2.txt").exists(),
        "background job survived the call"
    );
}

#[tokio::test]
async fn shell_writing_the_runs_asks_folder_asks() {
    let e = env(ScriptedAsker::new()).await;
    let ask = format!("runs/{}/asks/1.answer", e.run.id);
    let out = e
        .ok("shell", json!({"cmd": format!("echo yes > {ask}")}))
        .await;
    assert!(out.text.starts_with("skipped"), "{}", out.text);
    assert_eq!(e.asker.cards()[0].class, Class::Delete);
    assert!(!e.p(&ask).exists());
}

// ---- F-1 fs ----

#[tokio::test]
async fn fs_delete_inside_runs_runs_outside_asks() {
    let e = env(ScriptedAsker::new()).await;
    std::fs::create_dir_all(e.p("runs/tmp")).unwrap();
    std::fs::write(e.p("runs/tmp/a.txt"), "a").unwrap();
    std::fs::write(e.p("notes.txt"), "keep").unwrap();

    let out = e.ok("fs.delete", json!({"path": "runs/tmp/a.txt"})).await;
    assert!(out.text.starts_with("deleted"), "{}", out.text);
    assert!(!e.p("runs/tmp/a.txt").exists());
    assert!(e.asker.cards().is_empty());
    assert_eq!(e.last_tool_class(), Some(Class::Fill));

    let out = e.ok("fs.delete", json!({"path": "notes.txt"})).await;
    assert!(out.text.starts_with("skipped"), "{}", out.text);
    assert!(
        e.p("notes.txt").exists(),
        "outside runs/ is not deleted on timeout"
    );
    let cards = e.asker.cards();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].class, Class::Delete);
}

#[tokio::test]
async fn fs_path_escapes_are_caught() {
    let e = env(ScriptedAsker::new()).await;
    std::fs::write(e.p("notes.txt"), "keep").unwrap();
    std::fs::create_dir_all(e.p("runs/x")).unwrap();
    let id = e.run.id.to_string();
    let cases = [
        json!({"path": "runs/../notes.txt"}),
        json!({"path": "runs/x/../../notes.txt"}),
        json!({"path": format!("runs/{id}/ledger.jsonl")}),
        json!({"path": format!("runs/{id}")}),
        json!({"path": "runs"}),
    ];
    for (i, args) in cases.iter().enumerate() {
        let out = e.ok("fs.delete", args.clone()).await;
        assert!(
            out.text.starts_with("skipped"),
            "case {i} {args}: {}",
            out.text
        );
        assert_eq!(e.asker.cards().len(), i + 1, "case {i} asked");
        assert_eq!(e.asker.cards()[i].class, Class::Delete);
    }
    assert!(e.p("notes.txt").exists());
    assert!(e.run.ledger_jsonl().exists());

    // Answering the run's own ask by writing the file is Delete too.
    let out = e
        .ok(
            "fs.write",
            json!({"path": format!("runs/{id}/asks/9.answer"), "content": "yes"}),
        )
        .await;
    assert!(out.text.starts_with("skipped"), "{}", out.text);
    #[cfg(windows)]
    {
        let out = e
            .ok(
                "fs.write",
                json!({"path": format!("RUNS/{id}/ASKS./9.answer"), "content": "yes"}),
            )
            .await;
        assert!(out.text.starts_with("skipped"), "{}", out.text);
    }
    // Moving a file from outside into runs/ (where it could be deleted freely) is Delete.
    let out = e
        .ok("fs.move", json!({"from": "notes.txt", "to": "runs/x/"}))
        .await;
    assert!(out.text.starts_with("skipped"), "{}", out.text);
    assert!(e.p("notes.txt").exists());
}

#[tokio::test]
async fn fs_junction_out_of_runs_is_outside() {
    let e = env(ScriptedAsker::new()).await;
    let precious = e.p("precious");
    std::fs::create_dir_all(&precious).unwrap();
    std::fs::write(precious.join("p.txt"), "keep").unwrap();
    let link = e.p("runs/link");
    #[cfg(windows)]
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        // mklink takes backslashes only.
        .arg(link.to_string_lossy().replace('/', "\\"))
        .arg(precious.to_string_lossy().replace('/', "\\"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&precious, &link).is_ok();
    if !made {
        eprintln!("skipped: cannot create a junction or symlink here");
        return;
    }
    let out = e.ok("fs.delete", json!({"path": "runs/link/p.txt"})).await;
    assert!(out.text.starts_with("skipped"), "{}", out.text);
    assert!(precious.join("p.txt").exists());
    // The link itself resolves to its target outside runs/.
    let out = e
        .ok("fs.delete", json!({"path": "runs/link", "recursive": true}))
        .await;
    assert!(out.text.starts_with("skipped"), "{}", out.text);
    assert!(precious.join("p.txt").exists());
    if has_bash() {
        let out = e.ok("shell", json!({"cmd": "rm -f runs/link/p.txt"})).await;
        assert!(out.text.starts_with("skipped"), "{}", out.text);
        assert!(precious.join("p.txt").exists());
    }
}

#[tokio::test]
async fn fs_read_write_glob_move_mkdir() {
    let e = env(ScriptedAsker::new()).await;
    let rel = format!("runs/{}/files/draft.md", e.run.id);
    let out = e
        .ok(
            "fs.write",
            json!({"path": rel, "content": "héllo\nstopped: fake\n"}),
        )
        .await;
    assert!(out.text.starts_with("wrote:"), "{}", out.text);
    let draft = e.run.dir.join("files").join("draft.md");
    assert!(draft.is_file());
    assert_eq!(out.files.len(), 1, "a file in the run folder is reported");
    assert!(
        e.asker.cards().is_empty(),
        "writing inside runs/ does not ask"
    );

    let out = e.ok("fs.read", json!({"path": rel})).await;
    assert!(
        out.text.starts_with("héllo\n\\stopped: fake"),
        "{}",
        out.text
    );

    std::fs::write(e.p("long.txt"), "abcdefghij".repeat(3000)).unwrap();
    let out = e.ok("fs.read", json!({"path": "long.txt"})).await;
    assert!(
        out.text.contains("truncated: chars 0..8000 of 30000"),
        "default cap 8000"
    );
    let out = e
        .ok(
            "fs.read",
            json!({"path": "long.txt", "offset": 29995, "max_chars": 100}),
        )
        .await;
    assert_eq!(out.text, "fghij");
    assert!(e
        .call("fs.read", json!({"path": "long.txt", "max_chars": 0}))
        .await
        .is_err());

    let out = e.ok("fs.glob", json!({"pattern": "runs/**/*.md"})).await;
    assert!(out.text.contains("files/draft.md"), "{}", out.text);

    e.ok("fs.mkdir", json!({"path": "runs/m/n"})).await;
    assert!(e.p("runs/m/n").is_dir());
    let out = e
        .ok("fs.move", json!({"from": rel, "to": "runs/m/n/"}))
        .await;
    assert!(out.text.starts_with("moved"), "{}", out.text);
    assert!(e.p("runs/m/n/draft.md").is_file());

    // Overwriting an existing file outside runs/ is Delete (times out to skip).
    std::fs::write(e.p("keep.txt"), "old").unwrap();
    let out = e
        .ok("fs.write", json!({"path": "keep.txt", "content": "new"}))
        .await;
    assert!(out.text.starts_with("skipped"), "{}", out.text);
    assert_eq!(std::fs::read_to_string(e.p("keep.txt")).unwrap(), "old");
    // Appending is Fill: allowed.
    e.ok(
        "fs.write",
        json!({"path": "keep.txt", "content": "+", "append": true}),
    )
    .await;
    assert_eq!(std::fs::read_to_string(e.p("keep.txt")).unwrap(), "old+");
    assert_eq!(e.last_tool_class(), Some(Class::Fill));
}

// ---- F-4, F-5 ----

#[tokio::test]
async fn csv_clean_dedupes_normalises_and_flags() {
    let e = env(ScriptedAsker::new()).await;
    std::fs::write(
        e.p("contacts.csv"),
        "\u{FEFF}name,email,city\n  Ada   Lovelace ,ADA@x.org,london\nada lovelace,ada@x.org ,London\nBob,,paris\n,,\nCy,cy@x.org,  rome \n",
    )
    .unwrap();
    let out = e
        .ok(
            "csv.clean",
            json!({"path": "contacts.csv", "rules": {"key": ["email"], "case": {"city": "title"}}}),
        )
        .await;
    assert!(
        out.text
            .contains("rows: 5 in, 3 out; 1 duplicates dropped (key: email); 1 empty rows dropped"),
        "{}",
        out.text
    );
    assert!(
        out.text.contains("missing required values: email 1"),
        "{}",
        out.text
    );
    assert!(
        out.text.contains("duplicate: line 3 repeats line 2"),
        "{}",
        out.text
    );
    let written = e.run.dir.join("files").join("contacts.clean.csv");
    let body = std::fs::read_to_string(&written).unwrap();
    assert_eq!(
        body,
        "name,email,city,_missing\nAda Lovelace,ADA@x.org,London,\nBob,,Paris,email\nCy,cy@x.org,Rome,\n"
    );
    assert!(e
        .call(
            "csv.clean",
            json!({"path": "contacts.csv", "rules": {"key": ["nope"]}})
        )
        .await
        .is_err());
}

#[tokio::test]
async fn diff_text_and_table() {
    let e = env(ScriptedAsker::new()).await;
    std::fs::write(e.p("a.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(e.p("b.txt"), "one\n2\nthree\nfour\n").unwrap();
    let out = e.ok("diff", json!({"a": "a.txt", "b": "b.txt"})).await;
    assert!(
        out.text.starts_with("diff a.txt vs b.txt: +2 -1 lines"),
        "{}",
        out.text
    );
    assert!(out.text.contains("\n-two\n+2\n"), "{}", out.text);
    let out = e.ok("diff", json!({"a": "a.txt", "b": "a.txt"})).await;
    assert!(out.text.ends_with("(identical)"), "{}", out.text);

    std::fs::write(
        e.p("old.csv"),
        "id,name,price\n1,apple,10\n2,pear,5\n3,fig,7\n",
    )
    .unwrap();
    std::fs::write(
        e.p("new.csv"),
        "id,name,price,stock\n1,apple,12,4\n3,fig,7,1\n4,kiwi,3,9\n",
    )
    .unwrap();
    let out = e
        .ok(
            "diff",
            json!({"a": "old.csv", "b": "new.csv", "mode": "table"}),
        )
        .await;
    assert!(
        out.text.contains("1 only in a, 1 only in b, 1 changed"),
        "{}",
        out.text
    );
    assert!(
        out.text.contains("columns only in b: stock"),
        "{}",
        out.text
    );
    assert!(out.text.contains("only in a: id=2"), "{}", out.text);
    assert!(out.text.contains("only in b: id=4"), "{}", out.text);
    assert!(
        out.text.contains("- id=1: price \"10\" -> \"12\""),
        "{}",
        out.text
    );
    assert!(!out.text.contains("id=3:"), "unchanged rows are not listed");
}

// ---- F-6 ----

#[tokio::test]
async fn history_search_reads_a_copied_history() {
    let e = env(ScriptedAsker::new()).await;
    let src = e.p("chrome/History");
    let before = std::fs::read(&src).unwrap();
    let out = e.ok("history.search", json!({"q": "rust cdp"})).await;
    let lines: Vec<&str> = out.text.lines().collect();
    assert!(
        lines[0].starts_with("3 results for \"rust cdp\""),
        "{}",
        out.text
    );
    let rows: Vec<Value> = lines[1..]
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows[0]["url"], "https://github.com/mattsse/chromiumoxide");
    assert_eq!(rows[0]["visited"], "2026-10-22T09:00:00Z");
    assert_eq!(rows[1]["title"], "chromiumoxide - Rust CDP client");
    assert_eq!(rows[2]["url"], "https://old.example.com/rust-cdp");
    assert!(rows
        .iter()
        .all(|r| r.get("url").is_some() && r.get("title").is_some() && r.get("visited").is_some()));

    let out = e
        .ok(
            "history.search",
            json!({"q": "rust cdp", "since": "2026-10-21", "k": 5}),
        )
        .await;
    assert!(out.text.starts_with("1 results"), "{}", out.text);
    assert_eq!(
        std::fs::read(&src).unwrap(),
        before,
        "the source is never written"
    );
    assert_eq!(e.last_tool_class(), Some(Class::Read));
}

// ---- F-3 docs ----

#[tokio::test]
async fn docs_missing_helper_is_a_clear_error() {
    let e = env_with(
        ScriptedAsker::new(),
        DocsConfig {
            python: Some(PathBuf::from("Z:/no/such/python.exe")),
            pandoc: Some(PathBuf::from("Z:/no/such/pandoc.exe")),
            skills: None,
        },
    )
    .await;
    std::fs::write(e.p("a.pdf"), "%PDF-1.4").unwrap();
    let err = e
        .call("pdf.read", json!({"path": "a.pdf"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("needs Python 3"), "{err}");
    let err = e
        .call("docx.write", json!({"markdown": "# hi"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("needs pandoc"), "{err}");
    let err = e
        .call("pdf.write", json!({"text": "x", "out": "x.docx"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains(".pdf"), "{err}");
}

/// Works on any machine: either the workbook is written and read back, or the error names
/// what is missing (Python or openpyxl), never a panic or a bare failure.
#[tokio::test]
async fn docs_xlsx_round_trip_or_names_what_is_missing() {
    let e = env(ScriptedAsker::new()).await;
    let res = e
        .call(
            "xlsx.write",
            json!({"sheets": [{"name": "Tools", "rows": [["tool", "price"], ["a", 3], ["b", 4]]}]}),
        )
        .await;
    match res {
        Ok(out) => {
            assert!(out.text.starts_with("wrote: "), "{}", out.text);
            let rel = format!("runs/{}/files/workbook.xlsx", e.run.id);
            let out = e.ok("xlsx.read", json!({"path": rel})).await;
            assert!(out.text.contains("## Tools"), "{}", out.text);
            assert!(out.text.contains("a | 3"), "{}", out.text);
        }
        Err(err) => {
            let m = err.to_string();
            assert!(
                m.contains("needs Python 3") || m.contains("needs the Python package openpyxl"),
                "{m}"
            );
        }
    }
}

#[tokio::test]
#[ignore = "needs Python 3 with pypdf and reportlab (the pdf skill's libraries); run with --ignored"]
async fn docs_pdf_write_then_read() {
    let e = env(ScriptedAsker::new()).await;
    let out = e
        .ok(
            "pdf.write",
            json!({"text": "# Report\n\nThe **total** is 42.\n\n- first\n- second", "title": "T14"}),
        )
        .await;
    assert!(out.text.starts_with("wrote: "), "{}", out.text);
    let pdf = e.run.dir.join("files").join("document.pdf");
    assert!(pdf.is_file());
    let rel = format!("runs/{}/files/document.pdf", e.run.id);
    let out = e.ok("pdf.read", json!({"path": rel})).await;
    assert!(out.text.contains("1 pages"), "{}", out.text);
    assert!(out.text.contains("total is 42"), "{}", out.text);
    std::fs::write(e.p("notes.md"), "# Notes\n\nbody text").unwrap();
    let out = e.ok("md.to_pdf", json!({"path": "notes.md"})).await;
    assert!(out.text.contains("notes.pdf"), "{}", out.text);
}

#[tokio::test]
#[ignore = "needs pandoc on PATH (the docx skill's reader and writer); run with --ignored"]
async fn docs_docx_write_then_read() {
    let e = env(ScriptedAsker::new()).await;
    e.ok("docx.write", json!({"markdown": "# Title\n\nSome *text*."}))
        .await;
    let rel = format!("runs/{}/files/document.docx", e.run.id);
    let out = e.ok("docx.read", json!({"path": rel})).await;
    assert!(out.text.contains("Some *text*."), "{}", out.text);
}

#[tokio::test]
async fn vault_canary_never_leaves_through_fs_read() {
    let e = env(ScriptedAsker::new()).await;
    let secret = canary();
    std::fs::write(e.p("runs/secret.txt"), format!("token={secret}")).unwrap();
    let out = e.ok("fs.read", json!({"path": "runs/secret.txt"})).await;
    assert!(!out.text.contains(&secret));
    assert!(out.text.contains("@vault(t14)"));
    let ledger = std::fs::read_to_string(e.run.ledger_jsonl()).unwrap();
    assert!(!ledger.contains(&secret));
}
