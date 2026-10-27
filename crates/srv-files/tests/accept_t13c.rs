//! T-13c (B-564; docs/04 F-1, F-3, F-4, F-5): a bare file name an output-taking tool is
//! given lands in this run's `files/` (D-570), a bare name a reader is given falls back to
//! `files/<name>` when the base folder has no such file, the base winning when both exist
//! (D-571), classify and call act on the same resolved path (D-572), and a move's bare
//! destination follows D-573. Driven through `harness_core::mcp::dispatch` with the shared
//! MemStore and ScriptedAsker, in a temp folder that holds `runs/`. Offline.

use harness_core::memory::{ts_now, LedgerMode, MemoryStore, Run, RunStatus};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::Vault;
use harness_core::{dispatch, Class, Harness, ToolCtx, ToolError, ToolOutput, ToolRegistry};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SERVER: &str = "files";

struct Env {
    root: tempfile::TempDir,
    run: RunFolder,
    store: Arc<MemStore>,
    asker: Arc<ScriptedAsker>,
    harness: Harness,
    registry: ToolRegistry,
}

fn tools() -> Vec<harness_core::ToolSpec> {
    let mut t = srv_files::fs::tools();
    t.extend(srv_files::csv::tools());
    t.extend(srv_files::diff::tools());
    t
}

async fn env() -> Env {
    let root = tempfile::tempdir().unwrap();
    let runs_root = root.path().join("runs");
    let run = RunFolder::create(&runs_root, "t13c", "srv-files bare names").unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: ts_now(),
            ended: None,
            prompt: "srv-files bare names".into(),
            status: RunStatus::Running,
            folder: format!("runs/{}", run.id),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    let asker = Arc::new(ScriptedAsker::new());
    let harness = Harness {
        store: store.clone(),
        vault: Arc::new(Vault::in_memory([])),
        asker: asker.clone(),
        runs_root,
        run: Some(run.id.clone()),
    };
    Env {
        root,
        run,
        store,
        asker,
        harness,
        registry: ToolRegistry::new(tools()),
    }
}

impl Env {
    fn p(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn files(&self, name: &str) -> PathBuf {
        self.run.dir.join("files").join(name)
    }

    /// `runs/<id>/files/<name>`, the way the tools show a path in this run's files/.
    fn shown(&self, name: &str) -> String {
        format!("runs/{}/files/{name}", self.run.id)
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

    /// The last tool event's class and mode.
    fn last(&self) -> (Option<Class>, Option<LedgerMode>) {
        let mut rows = self.store.ledger();
        rows.sort_by_key(|(id, _)| *id);
        rows.into_iter()
            .map(|(_, e)| e)
            .rfind(|e| e.server == SERVER)
            .map(|e| (e.class, e.mode))
            .unwrap_or((None, None))
    }

    /// What the tool's classify shows for `key` (the path the gate decides on).
    async fn classified(&self, tool: &str, args: &Value, key: &str) -> (Option<Class>, String) {
        let spec = tools().into_iter().find(|t| t.name == tool).unwrap();
        let ctx = ToolCtx::new(
            self.run.id.clone(),
            self.run.dir.clone(),
            spec.class,
            &self.harness,
            self.registry.clone(),
        );
        let c = spec.handler.classify(&ctx, args).await.unwrap();
        let v = c
            .values
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        (c.class, v)
    }
}

fn slash(p: &Path) -> String {
    srv_files::paths::plain(p).replace('\\', "/")
}

#[tokio::test]
async fn bare_write_lands_in_files_and_is_reported() {
    let e = env().await;
    let out = e
        .ok(
            "fs.write",
            json!({"path": "roles.csv", "content": "title\nengineer\n"}),
        )
        .await;
    assert_eq!(
        std::fs::read_to_string(e.files("roles.csv")).unwrap(),
        "title\nengineer\n"
    );
    assert!(!e.p("roles.csv").exists(), "not in the base folder");
    assert!(
        out.text
            .starts_with(&format!("wrote: {} (", e.shown("roles.csv"))),
        "{}",
        out.text
    );
    assert_eq!(out.consequences["wrote"], json!(e.shown("roles.csv")));
    assert_eq!(out.files.len(), 1, "listed for the run report");
    assert!(
        e.asker.cards().is_empty(),
        "a write into files/ does not ask"
    );
    assert_eq!(e.last(), (Some(Class::Fill), Some(LedgerMode::Allow)));

    // Append to the same bare name appends to the same file.
    e.ok(
        "fs.write",
        json!({"path": "roles.csv", "content": "designer\n", "append": true}),
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(e.files("roles.csv")).unwrap(),
        "title\nengineer\ndesigner\n"
    );
    // A bare name equal to one in the base folder still goes to files/: the base file is
    // not replaced and nothing asks.
    std::fs::write(e.p("notes.txt"), "base").unwrap();
    e.ok("fs.write", json!({"path": "notes.txt", "content": "run"}))
        .await;
    assert_eq!(std::fs::read_to_string(e.p("notes.txt")).unwrap(), "base");
    assert_eq!(
        std::fs::read_to_string(e.files("notes.txt")).unwrap(),
        "run"
    );
    assert!(e.asker.cards().is_empty());
    assert_eq!(e.last(), (Some(Class::Fill), Some(LedgerMode::Allow)));
}

#[tokio::test]
async fn explicit_paths_keep_their_meaning() {
    let e = env().await;
    e.ok("fs.write", json!({"path": "./here.txt", "content": "a"}))
        .await;
    assert!(e.p("here.txt").is_file());
    e.ok("fs.write", json!({"path": "out/there.txt", "content": "b"}))
        .await;
    assert!(e.p("out/there.txt").is_file());
    let abs = slash(&e.p("abs.txt"));
    e.ok("fs.write", json!({"path": abs, "content": "c"})).await;
    assert!(e.p("abs.txt").is_file());
    let rel = format!("runs/{}/files/deep/x.txt", e.run.id);
    e.ok("fs.write", json!({"path": rel, "content": "d"})).await;
    assert!(e.files("deep").join("x.txt").is_file());
    for n in ["here.txt", "there.txt", "abs.txt", "x.txt"] {
        assert!(!e.files(n).exists(), "{n} did not go to files/");
    }
    // A bare folder name for fs.mkdir stays in the base folder (D-570).
    e.ok("fs.mkdir", json!({"path": "made"})).await;
    assert!(e.p("made").is_dir());
    // Outside runs/ still classes as before: replacing an existing file asks.
    let out = e
        .ok("fs.write", json!({"path": "./here.txt", "content": "z"}))
        .await;
    assert!(out.text.starts_with("skipped"), "{}", out.text);
    assert_eq!(e.asker.cards().len(), 1);
    assert_eq!(e.asker.cards()[0].class, Class::Delete);
}

#[tokio::test]
async fn bare_read_falls_back_to_files_and_the_base_wins() {
    let e = env().await;
    e.ok(
        "fs.write",
        json!({"path": "draft.md", "content": "from run"}),
    )
    .await;
    let out = e.ok("fs.read", json!({"path": "draft.md"})).await;
    assert_eq!(out.text, "from run");
    assert_eq!(out.consequences["path"], json!(e.shown("draft.md")));

    // Both exist: the base file is read, and the text names the other one.
    std::fs::write(e.p("draft.md"), "from base").unwrap();
    let out = e.ok("fs.read", json!({"path": "draft.md"})).await;
    assert!(out.text.starts_with("from base\n"), "{}", out.text);
    assert!(
        out.text.contains(&format!(
            "note: this is draft.md; this run's {} has the same name",
            e.shown("draft.md")
        )),
        "{}",
        out.text
    );
    // Neither exists: the error names the base path.
    let err = e
        .call("fs.read", json!({"path": "nowhere.md"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("nowhere.md"), "{err}");
    // An explicit relative path never falls back.
    std::fs::write(e.files("only.md"), "x").unwrap();
    assert!(e
        .call("fs.read", json!({"path": "./only.md"}))
        .await
        .is_err());
    assert_eq!(e.ok("fs.read", json!({"path": "only.md"})).await.text, "x");
}

#[tokio::test]
async fn bare_delete_and_move_use_files() {
    let e = env().await;
    e.ok("fs.write", json!({"path": "a.txt", "content": "a"}))
        .await;
    // Source falls back to files/; a bare destination for a file in runs/ is files/ too.
    let out = e
        .ok("fs.move", json!({"from": "a.txt", "to": "b.txt"}))
        .await;
    assert!(out.text.starts_with("moved"), "{}", out.text);
    assert!(e.files("b.txt").is_file());
    assert!(!e.p("b.txt").exists());
    assert_eq!(e.last(), (Some(Class::Fill), Some(LedgerMode::Allow)));
    // Into a base folder of that name, as before.
    std::fs::create_dir_all(e.p("archive")).unwrap();
    e.ok("fs.move", json!({"from": "b.txt", "to": "archive"}))
        .await;
    assert!(e.p("archive/b.txt").is_file());
    // A bare delete of what the run wrote is inside runs/: Read, no ask.
    e.ok("fs.write", json!({"path": "gone.txt", "content": "g"}))
        .await;
    let out = e.ok("fs.delete", json!({"path": "gone.txt"})).await;
    assert!(out.text.starts_with("deleted"), "{}", out.text);
    assert!(!e.files("gone.txt").exists());
    assert_eq!(e.last(), (Some(Class::Read), Some(LedgerMode::Allow)));
    assert!(e.asker.cards().is_empty());
    // A base file moved to a bare name stays a rename in the base folder and is Delete.
    std::fs::write(e.p("mine.txt"), "m").unwrap();
    let out = e
        .ok("fs.move", json!({"from": "mine.txt", "to": "yours.txt"}))
        .await;
    assert!(out.text.starts_with("skipped"), "{}", out.text);
    let cards = e.asker.cards();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].class, Class::Delete);
    assert!(cards[0].what.contains("mine.txt"), "{}", cards[0].what);
    let (class, to) = e
        .classified(
            "fs.move",
            &json!({"from": "mine.txt", "to": "yours.txt"}),
            "to",
        )
        .await;
    assert_eq!(class, Some(Class::Delete));
    assert_eq!(to, "yours.txt");
}

#[tokio::test]
async fn csv_clean_bare_in_and_out_round_trip() {
    let e = env().await;
    e.ok(
        "fs.write",
        json!({"path": "jobs.csv", "content": "title,url\nEngineer,https://x/1\n engineer ,https://x/1\nDesigner,https://x/2\n"}),
    )
    .await;
    let out = e
        .ok(
            "csv.clean",
            json!({"path": "jobs.csv", "rules": {"key": ["url"]}, "out": "jobs-deduped.csv"}),
        )
        .await;
    assert!(
        out.text
            .starts_with(&format!("wrote: {}\n", e.shown("jobs-deduped.csv"))),
        "{}",
        out.text
    );
    assert!(
        out.text
            .contains("rows: 3 in, 2 out; 1 duplicates dropped (key: url)"),
        "{}",
        out.text
    );
    assert_eq!(
        std::fs::read_to_string(e.files("jobs-deduped.csv")).unwrap(),
        "title,url,_missing\nEngineer,https://x/1,\nDesigner,https://x/2,\n"
    );
    assert!(!e.p("jobs-deduped.csv").exists());
    assert_eq!(out.files.len(), 1);
    assert_eq!(e.last(), (Some(Class::Fill), Some(LedgerMode::Allow)));
    // Default out next to it in files/.
    e.ok("csv.clean", json!({"path": "jobs.csv"})).await;
    assert!(e.files("jobs.clean.csv").is_file());
    // A bare out equal to the bare input (both files/jobs.csv) is refused.
    let err = e
        .call("csv.clean", json!({"path": "jobs.csv", "out": "jobs.csv"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("out must differ"), "{err}");
    // diff reads both bare names back from files/.
    let out = e
        .ok(
            "diff",
            json!({"a": "jobs.csv", "b": "jobs-deduped.csv", "mode": "table", "key": ["url"]}),
        )
        .await;
    assert!(
        out.text.contains("columns only in b: _missing"),
        "{}",
        out.text
    );
}

/// D-572: the path classify shows (and the gate decides on) is the one call writes.
#[tokio::test]
async fn classify_and_call_agree_on_the_path() {
    let e = env().await;
    std::fs::write(e.p("same.txt"), "base").unwrap();
    for (tool, args, key, name) in [
        (
            "fs.write",
            json!({"path": "same.txt", "content": "w"}),
            "path",
            "same.txt",
        ),
        (
            "csv.clean",
            json!({"path": "same.txt", "out": "same-clean.csv"}),
            "out",
            "same-clean.csv",
        ),
    ] {
        let (class, shown) = e.classified(tool, &args, key).await;
        assert_eq!(class, Some(Class::Fill), "{tool}");
        assert_eq!(shown, e.shown(name), "{tool}");
        let out = e.ok(tool, args).await;
        assert_eq!(out.consequences["wrote"], json!(shown), "{tool}");
        assert!(e.files(name).is_file(), "{tool}");
    }
    assert_eq!(std::fs::read_to_string(e.p("same.txt")).unwrap(), "base");
    // Readers: the base wins in classify and call alike.
    let (_, shown) = e
        .classified("fs.read", &json!({"path": "same.txt"}), "path")
        .await;
    assert_eq!(shown, "same.txt");
    let (_, shown) = e
        .classified("fs.delete", &json!({"path": "same.txt"}), "path")
        .await;
    assert_eq!(shown, "same.txt");
    assert!(e.asker.cards().is_empty());
}
