//! T-28 acceptance (tester-1; WAVE3 core T-28 items a-e, D-150..D-156).
//!
//! Written by the tester, independent of the coders' unit tests: every forge call goes through
//! `harness_core::mcp::dispatch` (gate, scrub, ledger) as `harness-forge` serves it, and the
//! judge reads run records produced by real `tools.run` dispatches, not hand-made events.
//!
//! * a: every merged recipe under tools/mail.google.com, tools/calendar.google.com and
//!   tools/lu.ma parses with 0 skipped; parse_recipe refuses raw refs, fixed waits, undeclared
//!   placeholders, @vault outside act text, key-shaped strings, bad names and bad @match.
//! * b: tools.list / tools.run over the repo's ./tools; run refusals, defaults, URL encoding,
//!   regex escape; tools.save from bench/fixtures/t28/browseros.jsonl.
//! * c: report.md's Browser: section from the same fixture, canary scrubbed.
//! * d: reflect judges recipe runs: 3 failed windows retire and archive, Ok records ok, a
//!   second pass judges nothing new, a running run is skipped.
//! * e: HARNESS_ALLOWED_TOOLS, the readers-only set and mcp.example.json.
//!
//! The vault canary is `SB_VAULT_CANARY` (or a fixed test string), planted as vault entry `pw`
//! at run time, never written into a fixture file, never formatted into an assertion message.
//! `SB_T28_KEEP=<dir>` keeps every file the tests write (run folders, report.md, recipes,
//! ledger dumps) under `<dir>/<test>/` for the tester's canary sweep. No model, no BrowserOS,
//! no Postgres (rule 1).

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use harness::forge::{self, LIST_TOOL, RUN_INSTRUCTION, RUN_TOOL, SAVE_TOOL, SERVER};
use harness::launcher::{ClaudeLauncher, HARNESS_ALLOWED_TOOLS};
use harness::reflect::{ReflectLauncher, Reflected, READERS_ALLOWED_TOOLS};
use harness_core::ledger::write_report;
use harness_core::mcp::{dispatch, ToolRegistry};
use harness_core::memory::{MemoryStore, Run, RunStatus, ToolStatus, Ts};
use harness_core::run::{RunFolder, RunId};
use harness_core::sched::{Clock, LaunchError, LaunchExit, LaunchRequest, Launcher};
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::{EntryKind, Vault};
use harness_core::{Harness, ToolError, ToolOutput};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use toolstore::{parse_recipe, RunOutcome, StoreError, ToolStore};

const VAULT_KEY: &str = "pw";

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| c.trim().chars().count() >= 8)
        .unwrap_or_else(|| "sbcanary-t28-accept-not-a-secret".to_owned())
}

/// The process-wide scrubber (core::scrub, used by write_report and parse_recipe) knows the
/// canary, as `core_pg::bootstrap` installs the real vault in harness-forge.
fn install_vault() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        Vault::in_memory([(VAULT_KEY.to_owned(), canary(), EntryKind::Secret)]).install_global();
    });
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A work folder: `SB_T28_KEEP/<name>` (kept, recreated) or a temp dir (dropped).
struct Work {
    _tmp: Option<tempfile::TempDir>,
    dir: PathBuf,
}

fn work(name: &str) -> Work {
    match std::env::var_os("SB_T28_KEEP").filter(|v| !v.is_empty()) {
        Some(root) => {
            let dir = PathBuf::from(root).join(name);
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Work { _tmp: None, dir }
        }
        None => {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().to_path_buf();
            Work {
                _tmp: Some(tmp),
                dir,
            }
        }
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn git_init(root: &Path) -> bool {
    git(root, &["init", "-q"]).is_some()
        && git(root, &["config", "user.email", "t28@example.invalid"]).is_some()
        && git(root, &["config", "user.name", "t28 tester"]).is_some()
        && git(root, &["config", "commit.gpgsign", "false"]).is_some()
        && git(root, &["add", "-A"]).is_some()
        && git(
            root,
            &["commit", "-q", "--allow-empty", "-m", "seed (T-28)"],
        )
        .is_some()
}

/// A forge server over `tools_root` in a run of its own (row `running`), on `store`.
struct Forge {
    store: Arc<MemStore>,
    harness: Harness,
    reg: ToolRegistry,
    run: RunFolder,
}

impl Forge {
    async fn new(store: Arc<MemStore>, runs: &Path, tools_root: &Path, slug: &str) -> Forge {
        install_vault();
        let run = RunFolder::create_at(runs, slug, "t28 accept", Utc::now()).unwrap();
        store
            .run_start(Run {
                id: run.id.clone(),
                started: Utc::now(),
                ended: None,
                prompt: "t28 accept".into(),
                status: RunStatus::Running,
                folder: run.dir.to_string_lossy().into_owned(),
                parent_run: None,
                strict: false,
            })
            .await
            .unwrap();
        let harness = Harness {
            store: store.clone(),
            vault: Arc::new(Vault::in_memory([(
                VAULT_KEY.to_owned(),
                canary(),
                EntryKind::Secret,
            )])),
            asker: Arc::new(ScriptedAsker::new()),
            runs_root: runs.to_path_buf(),
            run: Some(run.id.clone()),
        };
        let reg = ToolRegistry::new(forge::tools(&harness, tools_root));
        Forge {
            store,
            harness,
            reg,
            run,
        }
    }

    async fn call(&self, tool: &str, args: Value) -> Result<ToolOutput, ToolError> {
        dispatch(SERVER, &self.reg, &self.harness, tool, args).await
    }

    /// Every ledger row of the store, one JSON line each.
    fn ledger_text(&self) -> String {
        ledger_text(&self.store)
    }
}

fn ledger_text(store: &MemStore) -> String {
    store
        .ledger()
        .iter()
        .map(|(_, e)| serde_json::to_string(e).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

fn steps_of(out: &ToolOutput) -> Vec<Value> {
    let (first, rest) = out.text.split_once('\n').expect("instruction line");
    assert_eq!(first, RUN_INSTRUCTION);
    serde_json::from_str(rest).expect("steps are compact JSON")
}

fn args_err(r: Result<ToolOutput, ToolError>) -> String {
    match r {
        Err(ToolError::Args(m)) => m,
        Err(other) => panic!("expected an Args refusal, got {other}"),
        Ok(o) => panic!("expected an Args refusal, got ok: {}", o.text),
    }
}

/// The fixture jsonl with the canary and run id filled in.
fn fixture_jsonl(run: &RunId) -> String {
    std::fs::read_to_string(repo().join("bench/fixtures/t28/browseros.jsonl"))
        .unwrap()
        .replace("\r\n", "\n")
        .replace("__CANARY__", &canary())
        .replace("__RUN__", &run.0)
}

// ------------------------------------------------------------------------------------ (a)

#[test]
fn a1_merged_recipes_parse_with_zero_skipped() {
    install_vault();
    let ts = ToolStore::new(repo().join("tools"));
    let mut total = 0;
    for (folder, want) in [
        ("mail.google.com", vec!["draft", "inbox", "send", "thread"]),
        ("calendar.google.com", vec!["create", "events"]),
        ("lu.ma", vec!["register"]),
    ] {
        let rep = ts.recipes(folder);
        assert!(
            rep.skipped.is_empty(),
            "{folder} skipped: {:?}",
            rep.skipped
        );
        let mut names: Vec<&str> = rep.recipes.iter().map(|r| r.name.as_str()).collect();
        names.sort();
        assert_eq!(names, want, "{folder}");
        // Every *.recipe.yaml file of the folder is one of them (none silently ignored).
        let files = std::fs::read_dir(repo().join("tools").join(folder))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".recipe.yaml"))
            .count();
        assert_eq!(files, want.len(), "{folder}: files vs parsed");
        for name in &want {
            let text =
                std::fs::read_to_string(repo().join(format!("tools/{folder}/{name}.recipe.yaml")))
                    .unwrap();
            let r = parse_recipe(&text, folder, name).unwrap();
            // render round-trips (D-150).
            let back = parse_recipe(&toolstore::render_recipe(&r), folder, name).unwrap();
            assert_eq!(back, r, "{folder}/{name} round trip");
            total += 1;
        }
    }
    assert_eq!(total, 7);
}

/// A valid recipe with `steps` and `args` swapped in.
fn recipe_text(name: &str, matches: &str, args: &str, steps: &str) -> String {
    format!(
        "name: {name}\ndomain: example.com\nkind: recipe\ndescription: \"t28 refusal case\"\n\
         match: {matches}\nargs:\n{args}\nsteps:\n{steps}"
    )
}

const ARGS: &str = "  to: {type: string, required: true}\n";
const GOOD_STEPS: &str = "  - tool: navigate\n    args: {url: \"https://example.com/a\"}\n  \
     - tool: act\n    target: {role: textbox, name: \"Password\"}\n    \
     args: {kind: type, text: \"@vault(pw)\"}\n  \
     - tool: act\n    target: {role: textbox, name: \"To\"}\n    args: {kind: fill, value: \"{to}\"}\n";

fn refused(text: &str, stem: &str) -> String {
    match parse_recipe(text, "example.com", stem) {
        Err(StoreError::Recipe { msg, .. }) => msg,
        Err(other) => panic!("expected a Recipe refusal, got {other}"),
        Ok(_) => panic!("accepted a recipe that must be refused:\n{text}"),
    }
}

#[test]
fn a2_parse_refusals() {
    install_vault();
    // Control: the base recipe (with @vault in act text and a declared placeholder) loads.
    let good = recipe_text("ok_case", "[\"/a*\"]", ARGS, GOOD_STEPS);
    parse_recipe(&good, "example.com", "ok_case").unwrap();

    let step = |s: &str| recipe_text("bad", "[\"/*\"]", ARGS, &format!("{GOOD_STEPS}{s}"));
    let key = format!("{}{}", "sk-", "Ab1".repeat(10));
    let cases: Vec<(&str, String)> = vec![
        (
            "raw ref",
            step("  - tool: act\n    args: {kind: click, ref: e12}\n"),
        ),
        (
            "raw targetRef",
            step(
                "  - tool: act\n    target: {role: button, name: Go}\n    args: {kind: click, targetRef: e12}\n",
            ),
        ),
        (
            "fixed wait",
            step("  - tool: wait\n    args: {for: time, value: 2000}\n"),
        ),
        (
            "undeclared placeholder",
            step("  - tool: navigate\n    args: {url: \"https://example.com/{nope}\"}\n"),
        ),
        (
            "@vault in a navigate url",
            step("  - tool: navigate\n    args: {url: \"https://example.com/@vault(pw)\"}\n"),
        ),
        (
            "@vault in a target name",
            step(
                "  - tool: act\n    target: {role: button, name: \"@vault(pw)\"}\n    args: {kind: click}\n",
            ),
        ),
        (
            "@vault in a wait value",
            step("  - tool: wait\n    args: {for: text, value: \"@vault(pw)\"}\n"),
        ),
        (
            "key-shaped string",
            step(&format!(
                "  - tool: wait\n    args: {{for: text, value: \"{key}\"}}\n"
            )),
        ),
        (
            "literal under a secret-looking key",
            step("  - tool: act\n    target: {role: textbox, name: Pw}\n    args: {kind: fill, value: \"x\", password: \"hunter22\"}\n"),
        ),
        (
            "evaluate step",
            step("  - tool: evaluate\n    args: {expression: \"1\"}\n"),
        ),
        (
            "click without target",
            step("  - tool: act\n    args: {kind: click}\n"),
        ),
    ];
    for (what, text) in &cases {
        let m = refused(text, "bad");
        assert!(!m.is_empty(), "{what}");
        assert!(
            !m.contains(&key),
            "{what}: the refusal echoes the key-shaped value"
        );
    }
    // Names and match.
    for (what, text, stem) in [
        (
            "upper-case name",
            recipe_text("Inbox", "[\"/*\"]", ARGS, GOOD_STEPS),
            "Inbox",
        ),
        (
            "name is not the stem",
            recipe_text("inbox", "[\"/*\"]", ARGS, GOOD_STEPS),
            "other",
        ),
        (
            "name with a dash",
            recipe_text("in-box", "[\"/*\"]", ARGS, GOOD_STEPS),
            "in-box",
        ),
        (
            "match with a scheme and host",
            recipe_text("m1", "[\"https://example.com/*\"]", ARGS, GOOD_STEPS),
            "m1",
        ),
        (
            "match with //",
            recipe_text("m2", "[\"//evil.com/*\"]", ARGS, GOOD_STEPS),
            "m2",
        ),
        (
            "match with ..",
            recipe_text("m3", "[\"/a/../b\"]", ARGS, GOOD_STEPS),
            "m3",
        ),
        (
            "empty match",
            recipe_text("m4", "[]", ARGS, GOOD_STEPS),
            "m4",
        ),
    ] {
        let m = refused(&text, stem);
        assert!(!m.is_empty(), "{what}");
    }
    // Domain must equal the folder; kind must be recipe.
    assert!(parse_recipe(&good, "other.com", "ok_case").is_err());
    let kind = good.replace("kind: recipe", "kind: reader");
    refused(&kind, "ok_case");
}

// ------------------------------------------------------------------------------------ (b)

#[tokio::test]
async fn b1_list_and_run_over_the_repo_recipes() {
    let w = work("b1_list_run");
    let store = Arc::new(MemStore::new());
    let fx = Forge::new(store, &w.dir.join("runs"), &repo().join("tools"), "b1").await;

    for (domain, folder, want) in [
        (
            "mail.google.com",
            "mail.google.com",
            vec!["draft", "inbox", "send", "thread"],
        ),
        (
            "calendar.google.com",
            "calendar.google.com",
            vec!["create", "events"],
        ),
        ("lu.ma", "lu.ma", vec!["register"]),
        ("luma.com", "lu.ma", vec!["register"]),
    ] {
        let out = fx.call(LIST_TOOL, json!({"domain": domain})).await.unwrap();
        assert!(
            out.text
                .starts_with(&format!("{folder}: {} recipe", want.len())),
            "{domain}: {}",
            out.text
        );
        assert!(!out.text.contains("not usable"), "{}", out.text);
        for n in &want {
            assert!(
                out.text.contains(&format!("\n- {n}(")),
                "{domain}/{n}: {}",
                out.text
            );
        }
    }
    assert!(
        out_contains(&fx, "mail.google.com", "inbox(count: int = 5) [new]").await,
        "signature and status"
    );

    // Defaults applied; a lone placeholder takes the arg's JSON type.
    let out = fx
        .call(
            RUN_TOOL,
            json!({"domain": "mail.google.com", "name": "inbox"}),
        )
        .await
        .unwrap();
    let s = steps_of(&out);
    assert_eq!(s.len(), 3);
    assert_eq!(s[2]["tool"], "grep");
    assert_eq!(
        s[2]["args"]["limit"],
        json!(5),
        "default count, as a number"
    );
    assert_eq!(s[2]["returns"], "threads");
    let out = fx
        .call(
            RUN_TOOL,
            json!({"domain": "mail.google.com", "name": "inbox", "args": {"count": 9}}),
        )
        .await
        .unwrap();
    assert_eq!(steps_of(&out)[2]["args"]["limit"], json!(9));

    // URL percent-encoding (navigate url) and defaults in thread.
    let out = fx
        .call(
            RUN_TOOL,
            json!({"domain": "mail.google.com", "name": "thread",
                   "args": {"query": "subject:\"Q3 plan\" & more", "index": 2}}),
        )
        .await
        .unwrap();
    let s = steps_of(&out);
    assert_eq!(
        s[0]["args"]["url"],
        json!("https://mail.google.com/mail/u/0/#search/subject%3A%22Q3%20plan%22%20%26%20more")
    );
    assert_eq!(
        s[2]["target"],
        json!({"role": "row", "nth": 2}),
        "nth from an int arg"
    );
    let out = fx
        .call(
            RUN_TOOL,
            json!({"domain": "mail.google.com", "name": "thread"}),
        )
        .await
        .unwrap();
    assert_eq!(
        steps_of(&out)[0]["args"]["url"],
        json!("https://mail.google.com/mail/u/0/#search/in%3Ainbox")
    );

    // Calendar: encoded title, "" default description, expects as a final wait.
    let out = fx
        .call(
            RUN_TOOL,
            json!({"domain": "calendar.google.com", "name": "create",
                   "args": {"title": "Team sync & plan", "start": "20261028T100000",
                            "end": "20261028T110000"}}),
        )
        .await
        .unwrap();
    let s = steps_of(&out);
    assert_eq!(
        s[0]["args"]["url"],
        json!("https://calendar.google.com/calendar/u/0/r/eventedit?text=Team%20sync%20%26%20plan&dates=20261028T100000/20261028T110000&details=")
    );
    assert_eq!(
        s.last().unwrap(),
        &json!({"tool": "wait", "args": {"for": "text", "value": "Event saved"}})
    );

    // lu.ma through its luma.com alias.
    let out = fx
        .call(
            RUN_TOOL,
            json!({"domain": "luma.com", "name": "register", "args": {"event": "cygp5fj9"}}),
        )
        .await
        .unwrap();
    let s = steps_of(&out);
    assert_eq!(s[0]["args"]["url"], json!("https://luma.com/cygp5fj9"));
    assert_eq!(
        s[2]["target"],
        json!({"role": "button", "name": "Register"})
    );

    // Send fills act values as given.
    let send = |to: &str, subject: &str, body: &str| {
        json!({"domain": "mail.google.com", "name": "send",
               "args": {"to": to, "subject": subject, "body": body}})
    };
    let out = fx
        .call(
            RUN_TOOL,
            send("bob@example.com", "Lunch", "See you at noon, Bob."),
        )
        .await
        .unwrap();
    let s = steps_of(&out);
    assert_eq!(
        s[2]["args"],
        json!({"kind": "fill", "value": "bob@example.com"})
    );
    assert_eq!(s[6]["target"], json!({"role": "button", "name": "Send"}));

    // Refusals.
    let unknown = args_err(
        fx.call(
            RUN_TOOL,
            json!({"domain": "mail.google.com", "name": "inbox", "args": {"cnt": 3}}),
        )
        .await,
    );
    assert!(unknown.contains("unknown arg"), "{unknown}");
    let missing = args_err(
        fx.call(
            RUN_TOOL,
            json!({"domain": "mail.google.com", "name": "send",
                   "args": {"to": "bob@example.com", "subject": "x"}}),
        )
        .await,
    );
    assert!(missing.contains("missing required arg body"), "{missing}");
    let ty = args_err(
        fx.call(
            RUN_TOOL,
            json!({"domain": "mail.google.com", "name": "inbox", "args": {"count": "five"}}),
        )
        .await,
    );
    assert!(ty.contains("must be of type int"), "{ty}");
    let ty2 = args_err(
        fx.call(
            RUN_TOOL,
            json!({"domain": "mail.google.com", "name": "thread", "args": {"index": 1.5}}),
        )
        .await,
    );
    assert!(ty2.contains("must be of type int"), "{ty2}");
    for v in ["@vault(gmail_pw)", "x @VAULT(k) y"] {
        let m = args_err(fx.call(RUN_TOOL, send(v, "s", "b")).await);
        assert!(m.contains("@vault"), "{m}");
        assert!(!m.contains(v), "the refusal repeats the value");
    }
    let em = "\u{2014}";
    let en = "\u{2013}";
    for (subject, body) in [
        ("Lunch", format!("See you {em} Bob")),
        ("Lunch", "See you -- Bob".to_owned()),
        (&*format!("Lunch {en} Friday"), "ok".to_owned()),
    ] {
        let m = args_err(
            fx.call(RUN_TOOL, send("bob@example.com", subject, &body))
                .await,
        );
        assert!(m.contains("rule 7"), "{m}");
    }
    // An em dash in an arg that only fills a URL is not text sent as the user.
    fx.call(
        RUN_TOOL,
        json!({"domain": "mail.google.com", "name": "thread",
               "args": {"query": format!("subject:a {em} b")}}),
    )
    .await
    .unwrap();
    let none = args_err(
        fx.call(
            RUN_TOOL,
            json!({"domain": "mail.google.com", "name": "nope"}),
        )
        .await,
    );
    assert!(none.contains("no recipe mail.google.com/nope"), "{none}");
    assert!(fx
        .call(
            RUN_TOOL,
            json!({"domain": "mail.google.com", "name": "inbox", "args": [1]})
        )
        .await
        .is_err());

    // Every call is in the ledger as a forge event; tools.run carries the run record.
    let rows: Vec<_> = fx
        .store
        .ledger()
        .into_iter()
        .map(|(_, e)| e)
        .filter(|e| e.server == SERVER)
        .collect();
    let runs_ok = rows.iter().filter(|e| e.tool == RUN_TOOL && e.ok).count();
    assert_eq!(runs_ok, 8, "ok tools.run events");
    assert!(rows.iter().filter(|e| e.tool == LIST_TOOL).count() >= 4);
    assert!(
        rows.iter().any(|e| e.tool == RUN_TOOL && !e.ok),
        "refusals are ledgered"
    );
    let rec = rows.iter().find(|e| e.tool == RUN_TOOL && e.ok).unwrap();
    assert_eq!(
        rec.consequences["tools"],
        json!(["navigate", "wait", "grep"])
    );
    let jsonl = std::fs::read_to_string(fx.run.ledger_jsonl()).unwrap();
    assert_eq!(
        jsonl.lines().filter(|l| l.contains("\"forge\"")).count(),
        rows.len(),
        "jsonl mirror has every forge row"
    );
    std::fs::write(w.dir.join("memstore-ledger.jsonl"), fx.ledger_text()).unwrap();
}

async fn out_contains(fx: &Forge, domain: &str, needle: &str) -> bool {
    fx.call(LIST_TOOL, json!({"domain": domain}))
        .await
        .unwrap()
        .text
        .contains(needle)
}

#[tokio::test]
async fn b2_grep_pattern_is_regex_escaped_and_url_encoded() {
    let w = work("b2_grep");
    let root = w.dir.join("tools");
    std::fs::create_dir_all(root.join("example.com")).unwrap();
    std::fs::write(
        root.join("example.com/find.recipe.yaml"),
        "name: find\ndomain: example.com\nkind: recipe\ndescription: \"Find rows naming who\"\n\
         match: [\"/*\"]\nargs:\n  who: {type: string, required: true}\nsteps:\n  \
         - tool: navigate\n    args: {url: \"https://example.com/search?q={who}\"}\n  \
         - tool: grep\n    args: {over: ax, pattern: 'row \"{who}'}\n",
    )
    .unwrap();
    let fx = Forge::new(Arc::new(MemStore::new()), &w.dir.join("runs"), &root, "b2").await;
    let out = fx
        .call(
            RUN_TOOL,
            json!({"domain": "example.com", "name": "find", "args": {"who": "a.b (c)*|d"}}),
        )
        .await
        .unwrap();
    let s = steps_of(&out);
    assert_eq!(
        s[0]["args"]["url"],
        json!("https://example.com/search?q=a.b%20%28c%29%2A%7Cd")
    );
    assert_eq!(s[1]["args"]["pattern"], json!("row \"a\\.b \\(c\\)\\*\\|d"));
}

#[tokio::test]
async fn b3_save_from_fixture_jsonl() {
    let w = work("b3_save");
    let c = canary();
    let root = w.dir.join("tools");
    std::fs::create_dir_all(root.join("mail.google.com")).unwrap();
    std::fs::write(root.join("mail.google.com/.keep"), "").unwrap();
    let committed = git_init(&root);
    let store = Arc::new(MemStore::new());
    let fx = Forge::new(store.clone(), &w.dir.join("runs"), &root, "b3").await;

    // No browseros.jsonl in the run: refused.
    let m = args_err(
        fx.call(
            SAVE_TOOL,
            json!({"domain": "mail.google.com", "name": "compose_t28", "steps": [{"line": 1}]}),
        )
        .await,
    );
    assert!(m.contains("no browseros.jsonl"), "{m}");

    std::fs::write(fx.run.browseros_jsonl(), fixture_jsonl(&fx.run.id)).unwrap();
    let save = |name: &str, steps: Value| {
        json!({"domain": "mail.google.com", "name": name, "steps": steps,
               "args": {"to": {"type": "string", "required": true, "value": "bob@example.com"}},
               "expects": "Message sent"})
    };
    // Target required: line 5 acted on ref e30, which no summary names.
    let m = args_err(fx.call(SAVE_TOOL, save("t1", json!([{"line": 5}]))).await);
    assert!(m.contains("line 5") && m.contains("e30"), "{m}");
    // Not a recipe tool, a failed call, @vault outside act text.
    let m = args_err(fx.call(SAVE_TOOL, save("t2", json!([{"line": 7}]))).await);
    assert!(m.contains("line 7"), "{m}");
    let m = args_err(fx.call(SAVE_TOOL, save("t3", json!([{"line": 8}]))).await);
    assert!(m.contains("line 8") && m.contains("failed"), "{m}");
    let m = args_err(fx.call(SAVE_TOOL, save("t4", json!([{"line": 9}]))).await);
    assert!(m.contains("@vault"), "{m}");
    assert!(!m.contains(&c), "refusal echoes the vault value");
    for n in ["t1", "t2", "t3", "t4"] {
        assert!(!root
            .join(format!("mail.google.com/{n}.recipe.yaml"))
            .exists());
    }

    let out = fx
        .call(
            SAVE_TOOL,
            save(
                "compose_t28",
                json!([
                    {"line": 1},
                    {"line": 3},
                    {"line": 4},
                    {"line": 5, "target": {"role": "button", "name": "Send"}},
                    {"line": 6}
                ]),
            ),
        )
        .await
        .unwrap();
    assert!(
        out.text
            .starts_with("saved tools/mail.google.com/compose_t28.recipe.yaml: 5 steps, args (to)"),
        "{}",
        out.text
    );
    let path = root.join("mail.google.com/compose_t28.recipe.yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    let r = parse_recipe(&text, "mail.google.com", "compose_t28").unwrap();
    assert_eq!(r.step_tools(), ["navigate", "act", "act", "act", "wait"]);
    // Refs, page ids and diff dropped; targets from the snapshot summary and the model.
    for k in [
        "ref",
        "e7",
        "e9",
        "e30",
        "page",
        "diff",
        "bob@example.com",
        "s1",
    ] {
        assert!(!text.contains(k), "{k} in the saved recipe");
    }
    let tgt = |i: usize| {
        let t = r.steps[i].target.as_ref().unwrap();
        (t.role.clone(), t.name.clone())
    };
    assert_eq!(tgt(1), ("combobox".into(), "To recipients".into()));
    assert_eq!(tgt(2), ("textbox".into(), "Subject".into()));
    assert_eq!(tgt(3), ("button".into(), "Send".into()));
    assert_eq!(r.steps[1].args, json!({"kind": "fill", "value": "{to}"}));
    assert_eq!(
        r.steps[2].args,
        json!({"kind": "fill", "value": format!("Code @vault({VAULT_KEY})")}),
        "the canary is saved as @vault(name) only"
    );
    assert!(!text.contains(&c), "vault value in the recipe file");
    assert!(
        r.matches[0].starts_with("/mail/"),
        "default match from the first navigate"
    );
    assert_eq!(r.expects.as_ref().unwrap().text, "Message sent");

    // Registered ephemeral.
    let h = ToolStore::new(&root)
        .health(&*store, "mail.google.com", "compose_t28")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((h.status, h.version), (ToolStatus::Ephemeral, 1));

    // Git commit with the task id, only the recipe file.
    let mut seen = vec![out.text.clone(), text.clone(), fx.ledger_text()];
    if committed {
        let log = git(&root, &["log", "-1", "--format=%s", "--name-only"]).unwrap();
        assert!(
            log.starts_with(&format!(
                "tools: save mail.google.com/compose_t28 recipe (T-28; run {})",
                fx.run.id.0
            )),
            "{log}"
        );
        let files: Vec<&str> = log.lines().skip(1).filter(|l| !l.is_empty()).collect();
        assert_eq!(files, ["mail.google.com/compose_t28.recipe.yaml"]);
        seen.push(git(&root, &["log", "-p", "--format=%B"]).unwrap());
    } else {
        panic!("git is not available: the commit item cannot be checked");
    }

    // Existing file refused, no overwrite.
    let before = std::fs::read_to_string(&path).unwrap();
    let m = args_err(
        fx.call(SAVE_TOOL, save("compose_t28", json!([{"line": 1}])))
            .await,
    );
    assert!(m.contains("exists"), "{m}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    // The saved recipe runs.
    let run = fx
        .call(
            RUN_TOOL,
            json!({"domain": "mail.google.com", "name": "compose_t28",
                   "args": {"to": "amy@example.com"}}),
        )
        .await
        .unwrap();
    let s = steps_of(&run);
    assert_eq!(s[1]["args"]["value"], json!("amy@example.com"));
    seen.push(run.text.clone());

    // A server with no run refuses every call.
    let mut no_run = fx.harness.clone_for_test();
    no_run.run = None;
    let r = dispatch(
        SERVER,
        &fx.reg,
        &no_run,
        SAVE_TOOL,
        save("t5", json!([{"line": 1}])),
    )
    .await;
    assert!(matches!(r, Err(ToolError::Failed(ref m)) if m.contains("no run")));

    for s in &seen {
        assert!(!s.contains(&c), "vault value leaked from tools.save");
    }
    std::fs::write(w.dir.join("memstore-ledger.jsonl"), fx.ledger_text()).unwrap();
}

trait CloneForTest {
    fn clone_for_test(&self) -> Harness;
}

impl CloneForTest for Harness {
    fn clone_for_test(&self) -> Harness {
        Harness {
            store: self.store.clone(),
            vault: self.vault.clone(),
            asker: self.asker.clone(),
            runs_root: self.runs_root.clone(),
            run: self.run.clone(),
        }
    }
}

// ------------------------------------------------------------------------------------ (c)

#[test]
fn c1_report_browser_section_from_jsonl_without_the_canary() {
    install_vault();
    let w = work("c1_report");
    let c = canary();
    let run = RunFolder::create_at(&w.dir.join("runs"), "c1", "report", Utc::now()).unwrap();
    std::fs::write(run.browseros_jsonl(), fixture_jsonl(&run.id)).unwrap();
    let text = write_report(&run, "Send Bob a note", &[]).unwrap();
    let disk = std::fs::read_to_string(run.report_md()).unwrap();
    assert_eq!(text, disk);
    assert!(!disk.contains(&c), "vault value in report.md");
    assert!(disk.contains("Browser: 9 calls"), "{disk}");
    let lines: Vec<&str> = disk
        .lines()
        .skip_while(|l| !l.starts_with("Browser:"))
        .skip(1)
        .take(9)
        .collect();
    let tools = [
        "navigate", "snapshot", "act", "act", "act", "wait", "evaluate", "act", "navigate",
    ];
    for (i, (l, t)) in lines.iter().zip(tools).enumerate() {
        assert!(
            l.starts_with(&format!("{}. {t}", i + 1)),
            "line {}: {l}",
            i + 1
        );
    }
    assert!(lines[0].contains("-> ok"), "{}", lines[0]);
    assert!(lines[7].contains("-> failed"), "{}", lines[7]);
    assert!(
        lines[3].contains(&format!("@vault({VAULT_KEY})")),
        "the canary shows as @vault(name)"
    );
    assert!(!disk.contains("page=4"), "per-session ids dropped");
    // No browseros.jsonl, no section.
    let bare = RunFolder::create_at(&w.dir.join("runs"), "c1bare", "report", Utc::now()).unwrap();
    let t = write_report(&bare, "x", &[]).unwrap();
    assert!(!t.contains("Browser:"));
}

// ------------------------------------------------------------------------------------ (d)

struct Fixed(Ts);

#[async_trait]
impl Clock for Fixed {
    fn now(&self) -> Ts {
        self.0
    }
    async fn sleep_until(&self, _t: Ts) {}
}

struct Nop;

#[async_trait]
impl Launcher for Nop {
    async fn launch(&self, _req: LaunchRequest<'_>) -> Result<LaunchExit, LaunchError> {
        Ok(LaunchExit::OK)
    }
}

/// One browseros.jsonl line `secs` after `at`, in the hook's format.
fn bline(at: Ts, secs: i64, tool: &str, ok: bool, summary: &str) -> String {
    let ts = (at + Duration::seconds(secs))
        .with_timezone(&chrono::FixedOffset::east_opt(3600).unwrap())
        .format("%Y-%m-%dT%H:%M:%S%z")
        .to_string();
    json!({"run_id": "x", "ts": ts, "server": "browseros", "tool": tool, "class": null,
           "mode": "external", "args": {"page": 1}, "result_summary": summary, "ok": ok,
           "why": if ok { Value::Null } else { json!(summary) }})
    .to_string()
}

/// A work run that called tools.run once through the forge; returns it and the event time.
async fn recipe_run(
    store: &Arc<MemStore>,
    runs: &Path,
    tools: &Path,
    slug: &str,
    domain: &str,
    name: &str,
    args: Value,
) -> (RunFolder, Ts) {
    let fx = Forge::new(store.clone(), runs, tools, slug).await;
    fx.call(
        RUN_TOOL,
        json!({"domain": domain, "name": name, "args": args}),
    )
    .await
    .unwrap();
    let ts = store
        .ledger()
        .into_iter()
        .map(|(_, e)| e)
        .rev()
        .find(|e| e.server == SERVER && e.tool == RUN_TOOL)
        .unwrap()
        .ts;
    (fx.run, ts)
}

async fn reflect(store: &Arc<MemStore>, tools: &Path, runs: &Path, at: Ts) -> Reflected {
    let launcher = ReflectLauncher {
        store: store.clone(),
        tools_root: tools.to_path_buf(),
        proposals_file: None,
        clock: Arc::new(Fixed(at)),
        commit: false,
        inner: Arc::new(Nop),
        readers: Arc::new(Nop),
    };
    let me = RunFolder::create_at(runs, "reflect", "reflect", at).unwrap();
    launcher
        .reflect(&LaunchRequest {
            run: &me,
            runs_root: runs,
            prompt: "reflect",
            schedule: None,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn d1_reflect_judges_recipe_runs() {
    let w = work("d1_judge");
    let tools = w.dir.join("tools");
    copy_dir(
        &repo().join("tools/mail.google.com"),
        &tools.join("mail.google.com"),
    );
    copy_dir(&repo().join("tools/lu.ma"), &tools.join("lu.ma"));
    let runs = w.dir.join("runs");
    let store = Arc::new(MemStore::new());
    let ts = ToolStore::new(&tools);

    // Three inbox runs whose grep failed; the third is still running at the first pass.
    let mut fails = Vec::new();
    for i in 1..=3 {
        let (run, at) = recipe_run(
            &store,
            &runs,
            &tools,
            &format!("inbox{i}"),
            "mail.google.com",
            "inbox",
            json!({}),
        )
        .await;
        let lines = [
            bline(at, 0, "navigate", true, "Navigated"),
            bline(at, 1, "wait", true, "matched"),
            bline(at, 2, "grep", false, "No matches"),
        ];
        std::fs::write(run.browseros_jsonl(), lines.join("\n") + "\n").unwrap();
        fails.push(run);
    }
    // One lu.ma register run that worked (expects "You're In" seen).
    let (ok_run, at) = recipe_run(
        &store,
        &runs,
        &tools,
        "register",
        "luma.com",
        "register",
        json!({"event": "cygp5fj9"}),
    )
    .await;
    let lines = [
        bline(at, 0, "navigate", true, "Navigated"),
        bline(at, 1, "wait", true, "matched Registration"),
        bline(at, 1, "snapshot", true, "- button \"Register\" [ref=e4]"),
        bline(at, 2, "act", true, "clicked"),
        bline(at, 3, "wait", true, "matched You're In"),
    ];
    std::fs::write(ok_run.browseros_jsonl(), lines.join("\n") + "\n").unwrap();
    let done = |r: &RunFolder| {
        let store = store.clone();
        let id = r.id.clone();
        async move {
            store
                .run_end(&id, RunStatus::Done, Utc::now())
                .await
                .unwrap()
        }
    };
    done(&fails[0]).await;
    done(&fails[1]).await;
    done(&ok_run).await;

    let t0: DateTime<Utc> = Utc::now() + Duration::hours(1);
    // Pass 1: the two finished inbox runs fail, register is ok, the running run is skipped.
    let p1 = reflect(&store, &tools, &runs, t0).await;
    assert_eq!(p1.judged.judged(), 3, "{:?}", p1.judged.verdicts);
    assert_eq!(p1.judged.failed(), 2);
    assert!(
        p1.judged.verdicts.iter().all(|v| v.run != fails[2].id),
        "running run judged"
    );
    let ok = p1
        .judged
        .verdicts
        .iter()
        .find(|v| v.run == ok_run.id)
        .unwrap();
    assert_eq!(ok.outcome, Some(RunOutcome::Ok));
    let reg = ts
        .health(&*store, "lu.ma", "register")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((reg.runs, reg.fails), (1, 0), "Ok recorded");
    assert!(reg.last_ok.is_some());
    let inbox = ts
        .health(&*store, "mail.google.com", "inbox")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(inbox.fails, 2);
    assert_ne!(inbox.status, ToolStatus::Retired);
    assert!(tools.join("mail.google.com/inbox.recipe.yaml").is_file());

    // Pass 2: nothing new.
    let p2 = reflect(&store, &tools, &runs, t0 + Duration::minutes(10)).await;
    assert!(p2.judged.verdicts.is_empty(), "{:?}", p2.judged.verdicts);

    // The third run ends: pass 3 judges it, the third failure retires and archives inbox.
    done(&fails[2]).await;
    let p3 = reflect(&store, &tools, &runs, t0 + Duration::minutes(20)).await;
    assert_eq!(p3.judged.judged(), 1);
    assert_eq!(p3.judged.failed(), 1);
    assert!(p3.judged.verdicts[0].retired);
    let inbox = ts
        .health(&*store, "mail.google.com", "inbox")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((inbox.status, inbox.fails), (ToolStatus::Retired, 3));
    assert!(!tools.join("mail.google.com/inbox.recipe.yaml").exists());
    assert!(tools
        .join("_archive/mail.google.com/inbox.1.recipe.yaml")
        .is_file());
    assert!(store.ledger().iter().any(|(_, e)| e.tool == "tools.retire"));
    let marks = store
        .ledger()
        .into_iter()
        .filter(|(_, e)| e.tool == harness::judge::JUDGE_TOOL)
        .count();
    assert_eq!(marks, 4, "one tools.judge event per verdict");

    // The retired recipe is gone from tools.list and tools.run refuses it.
    let fx = Forge::new(store.clone(), &runs, &tools, "after").await;
    let list = fx
        .call(LIST_TOOL, json!({"domain": "mail.google.com"}))
        .await
        .unwrap();
    assert!(!list.text.contains("\n- inbox("), "{}", list.text);
    assert!(fx
        .call(
            RUN_TOOL,
            json!({"domain": "mail.google.com", "name": "inbox"})
        )
        .await
        .is_err());

    // Pass 4: still nothing new.
    let p4 = reflect(&store, &tools, &runs, t0 + Duration::minutes(30)).await;
    assert!(p4.judged.verdicts.is_empty(), "{:?}", p4.judged.verdicts);
    std::fs::write(w.dir.join("memstore-ledger.jsonl"), ledger_text(&store)).unwrap();
}

/// A browseros.jsonl line in the shape BrowserOS neo 0.0.69 really returns (checked against the
/// T-27 live runs, F:/sb-swarm/wt/servers/runs/t27-accept-3/browseros.jsonl lines 15-16): a
/// `wait for=text` that matched says `matched (text)` and does not repeat the text.
fn real_line(at: Ts, secs: i64, tool: &str, args: Value, text: &str) -> String {
    let ts = (at + Duration::seconds(secs))
        .with_timezone(&chrono::FixedOffset::east_opt(-7 * 3600).unwrap())
        .format("%Y-%m-%dT%H:%M:%S%z")
        .to_string();
    let summary = json!([{"type": "text", "text": text}]).to_string();
    json!({"run_id": "x", "ts": ts, "server": "browseros", "tool": tool, "class": null,
           "mode": "external", "args": args, "result_summary": summary, "ok": true,
           "why": null})
    .to_string()
}

/// WAVE3 (d) / D-153 "Ok records ok" on the real line shape: a send run whose every step
/// worked and whose final `wait for=text value="Message sent"` matched is judged Ok.
#[tokio::test]
async fn d2_successful_send_with_real_wait_lines_is_judged_ok() {
    let w = work("d2_judge_real_shape");
    let tools = w.dir.join("tools");
    copy_dir(
        &repo().join("tools/mail.google.com"),
        &tools.join("mail.google.com"),
    );
    let runs = w.dir.join("runs");
    let store = Arc::new(MemStore::new());
    let (run, at) = recipe_run(
        &store,
        &runs,
        &tools,
        "send",
        "mail.google.com",
        "send",
        json!({"to": "bob@example.com", "subject": "Lunch", "body": "See you at noon."}),
    )
    .await;
    let p = json!(22);
    let lines = [
        real_line(
            at,
            1,
            "navigate",
            json!({"page": p, "url": "https://mail.google.com/mail/u/0/#inbox?compose=new"}),
            "Navigated to https://mail.google.com/mail/u/0/#inbox?compose=new",
        ),
        real_line(
            at,
            3,
            "wait",
            json!({"page": p, "for": "selector", "value": "[role=dialog] [aria-label=\"To recipients\"]"}),
            "matched (selector)",
        ),
        real_line(
            at,
            4,
            "snapshot",
            json!({"page": p}),
            "- combobox \"To recipients\" [ref=e7]",
        ),
        real_line(
            at,
            5,
            "act",
            json!({"page": p, "kind": "fill", "ref": "e7", "value": "bob@example.com"}),
            "filled",
        ),
        real_line(
            at,
            6,
            "act",
            json!({"page": p, "kind": "press", "key": "Tab"}),
            "pressed Tab",
        ),
        real_line(
            at,
            7,
            "act",
            json!({"page": p, "kind": "fill", "ref": "e9", "value": "Lunch"}),
            "filled",
        ),
        real_line(
            at,
            8,
            "act",
            json!({"page": p, "kind": "fill", "ref": "e11", "value": "See you at noon."}),
            "filled",
        ),
        real_line(
            at,
            9,
            "act",
            json!({"page": p, "kind": "click", "ref": "e13"}),
            "clicked",
        ),
        real_line(
            at,
            11,
            "wait",
            json!({"page": p, "for": "text", "value": "Message sent"}),
            "matched (text)",
        ),
        real_line(
            at,
            12,
            "wait",
            json!({"page": p, "for": "text", "value": "Message sent"}),
            "matched (text)",
        ),
    ];
    std::fs::write(
        run.browseros_jsonl(),
        lines.join(
            "
",
        ) + "
",
    )
    .unwrap();
    store
        .run_end(&run.id, RunStatus::Done, Utc::now())
        .await
        .unwrap();
    let p1 = reflect(&store, &tools, &runs, Utc::now() + Duration::hours(1)).await;
    assert_eq!(p1.judged.judged(), 1, "{:?}", p1.judged.verdicts);
    assert_eq!(
        p1.judged.verdicts[0].outcome,
        Some(RunOutcome::Ok),
        "a successful send is judged failed (reviewer B1): {:?}",
        p1.judged.verdicts[0]
    );
}

/// WAVE3 (d) "existing lifecycle rules": a hand-written tool's first tools.health row is
/// `saved` (G-5, toolstore new_row); only forged tools start `ephemeral`. A hand-written
/// recipe judged once must get the same row, or the T-7 idle sweep archives it.
#[tokio::test]
async fn d3_hand_written_recipe_row_is_saved_after_a_judged_run() {
    let w = work("d3_judge_status");
    let tools = w.dir.join("tools");
    copy_dir(
        &repo().join("tools/mail.google.com"),
        &tools.join("mail.google.com"),
    );
    let runs = w.dir.join("runs");
    let store = Arc::new(MemStore::new());
    let (run, at) = recipe_run(
        &store,
        &runs,
        &tools,
        "inbox",
        "mail.google.com",
        "inbox",
        json!({}),
    )
    .await;
    let lines = [
        bline(at, 0, "navigate", true, "Navigated"),
        bline(at, 1, "wait", true, "matched (selector)"),
        bline(at, 2, "grep", true, "5 matches"),
    ];
    std::fs::write(
        run.browseros_jsonl(),
        lines.join(
            "
",
        ) + "
",
    )
    .unwrap();
    store
        .run_end(&run.id, RunStatus::Done, Utc::now())
        .await
        .unwrap();
    let p1 = reflect(&store, &tools, &runs, Utc::now() + Duration::hours(1)).await;
    assert_eq!(p1.judged.verdicts[0].outcome, Some(RunOutcome::Ok));
    let row = ToolStore::new(&tools)
        .health(&*store, "mail.google.com", "inbox")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.status,
        ToolStatus::Saved,
        "hand-written recipe registered as {:?} (reviewer B2)",
        row.status
    );
}

// ------------------------------------------------------------------------------------ (e)

#[test]
fn e1_allowed_tools_and_mcp_example() {
    for t in ["mcp__browseros", "mcp__harness-forge"] {
        assert!(HARNESS_ALLOWED_TOOLS.contains(&t), "{t}");
        assert!(
            ClaudeLauncher::print("claude")
                .allowed_tools
                .iter()
                .any(|a| a == t),
            "-p launcher: {t}"
        );
        assert!(!READERS_ALLOWED_TOOLS.contains(&t), "readers-only: {t}");
    }
    let ro = ClaudeLauncher::print("claude").readers_only("srv-web".into(), "tools".into());
    assert_eq!(ro.allowed_tools, ["mcp__harness-web"]);
    let tmp = tempfile::tempdir().unwrap();
    let cfg = harness::launcher::write_readers_config(tmp.path(), Path::new("srv-web")).unwrap();
    let v: Value = serde_json::from_str(&std::fs::read_to_string(cfg).unwrap()).unwrap();
    let names: Vec<&String> = v["mcpServers"].as_object().unwrap().keys().collect();
    assert_eq!(names, ["harness-web"], "replay MCP config");

    let text = std::fs::read_to_string(repo().join("mcp.example.json")).unwrap();
    let v: Value = serde_json::from_str(&text).expect("mcp.example.json is valid JSON");
    let s = &v["mcpServers"];
    assert_eq!(
        s["harness-forge"]["command"],
        json!("target/release/harness-forge")
    );
    assert_eq!(
        s["browseros"],
        json!({"type": "http", "url": "http://127.0.0.1:9010/mcp"})
    );
    for k in ["harness-web", "harness-files", "harness-cli", "harness-mem"] {
        assert!(s.get(k).is_some(), "{k}");
    }
}
