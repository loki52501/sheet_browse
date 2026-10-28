//! T-28 (b): `tools.list`, `tools.run` (D-151, D-156) and `tools.save` (D-152) through
//! `harness_core::mcp::dispatch`, the in-process pipeline `serve` runs per call (gate, scrub,
//! ledger). `tools.list` and `tools.run` run over the repo's own `tools/` recipes (T-27);
//! `tools.save` over a temporary tools root and `browseros.jsonl` lines built at run time.
//!
//! The vault canary is built at run time (`SB_VAULT_CANARY` or a fixed test string) and the
//! vault is `Vault::in_memory`, never installed globally. No model is called (rule 1).

use super::*;
use crate::forge_save::{build_recipe, default_match, target_in_summary, SaveRequest};
use chrono::Utc;
use harness_core::ledger::BROWSEROS_FILE;
use harness_core::mcp::{dispatch, ToolRegistry};
use harness_core::memory::{MemoryStore, Run, RunStatus, ToolHealth};
use harness_core::run::RunFolder;
use harness_core::testing::{MemStore, ScriptedAsker};
use harness_core::vault::EntryKind;
use std::process::Command;
use toolstore::{parse_recipe, render_recipe};

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| c.trim().chars().count() >= 8)
        .unwrap_or_else(|| "sbcanary-forge-not-a-secret".to_owned())
}

struct Fx {
    _tmp: tempfile::TempDir,
    store: Arc<MemStore>,
    harness: Harness,
    reg: ToolRegistry,
    run: RunFolder,
    tools_root: PathBuf,
}

impl Fx {
    async fn call(&self, tool: &str, args: Value) -> Result<ToolOutput, ToolError> {
        dispatch(SERVER, &self.reg, &self.harness, tool, args).await
    }

    fn jsonl(&self, lines: &[String]) {
        let mut t = lines.join("\n");
        t.push('\n');
        std::fs::write(self.run.dir.join(BROWSEROS_FILE), t).unwrap();
    }

    /// Everything the ledger holds, as one string.
    fn ledger_text(&self) -> String {
        self.store
            .ledger()
            .iter()
            .map(|(_, e)| serde_json::to_string(e).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A run (row and folder) over `tools_root` (`None`: a fresh temporary root with a lu.ma
/// folder aliasing luma.com).
async fn fixture(tools_root: Option<PathBuf>) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let tools_root = tools_root.unwrap_or_else(|| {
        let root = tmp.path().join("tools");
        std::fs::create_dir_all(root.join("lu.ma")).unwrap();
        std::fs::write(root.join("lu.ma/_site.yaml"), "aliases: [luma.com]\n").unwrap();
        root
    });
    let runs = tmp.path().join("runs");
    let run = RunFolder::create_at(&runs, "forge", "forge test", Utc::now()).unwrap();
    let store = Arc::new(MemStore::new());
    store
        .run_start(Run {
            id: run.id.clone(),
            started: Utc::now(),
            ended: None,
            prompt: "forge test".into(),
            status: RunStatus::Running,
            folder: run.dir.to_string_lossy().into_owned(),
            parent_run: None,
            strict: false,
        })
        .await
        .unwrap();
    let vault = Vault::in_memory([("pw".to_owned(), canary(), EntryKind::Secret)]);
    let harness = Harness {
        store: store.clone(),
        vault: Arc::new(vault),
        asker: Arc::new(ScriptedAsker::new()),
        runs_root: runs,
        run: Some(run.id.clone()),
    };
    let reg = ToolRegistry::new(tools(&harness, &tools_root));
    Fx {
        _tmp: tmp,
        store,
        harness,
        reg,
        run,
        tools_root,
    }
}

fn repo_tools() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools")
}

fn steps_of(out: &ToolOutput) -> Vec<Value> {
    let (first, json) = out.text.split_once('\n').unwrap();
    assert_eq!(first, RUN_INSTRUCTION);
    serde_json::from_str(json).unwrap()
}

fn args_err(r: Result<ToolOutput, ToolError>) -> String {
    match r {
        Err(ToolError::Args(m)) => m,
        other => panic!("expected an Args refusal, got {other:?}"),
    }
}

// ---------------------------------------------------------------- tools.list / tools.run

/// The brief's acceptance: tools.list and tools.run load every recipe of
/// tools/mail.google.com, tools/calendar.google.com and tools/lu.ma (through luma.com).
#[tokio::test]
async fn list_and_run_load_the_repo_recipes() {
    let fx = fixture(Some(repo_tools())).await;
    for (domain, want) in [
        ("mail.google.com", vec!["draft", "inbox", "send", "thread"]),
        ("calendar.google.com", vec!["create", "events"]),
        ("luma.com", vec!["register"]),
        ("lu.ma", vec!["register"]),
    ] {
        let out = fx.call(LIST_TOOL, json!({"domain": domain})).await.unwrap();
        assert!(!out.text.contains("not usable"), "{}", out.text);
        for name in &want {
            assert!(
                out.text.contains(&format!("\n- {name}(")),
                "{domain}: {}",
                out.text
            );
        }
        let folder = if domain == "luma.com" {
            "lu.ma"
        } else {
            domain
        };
        let head = format!("{folder}: {} recipe", want.len());
        assert!(out.text.starts_with(&head), "{}", out.text);
    }
    let list = fx
        .call(
            LIST_TOOL,
            json!({"domain": "https://mail.google.com/mail/u/0/"}),
        )
        .await;
    assert!(list.is_ok(), "a URL resolves through folder_for too");
    let out = fx
        .call(LIST_TOOL, json!({"domain": "mail.google.com"}))
        .await
        .unwrap();
    assert!(
        out.text.contains("- inbox(count: int = 5) [new]:"),
        "{}",
        out.text
    );
    assert!(
        out.text
            .contains("- send(to: string, subject: string, body: string) [new]"),
        "{}",
        out.text
    );

    let run = |domain: &str, name: &str, args: Value| {
        let fx = &fx;
        let (domain, name) = (domain.to_owned(), name.to_owned());
        async move {
            fx.call(
                RUN_TOOL,
                json!({"domain": domain, "name": name, "args": args}),
            )
            .await
        }
    };
    // inbox: the whole-placeholder grep limit keeps the int type (D-156).
    let s = steps_of(&run("mail.google.com", "inbox", json!({})).await.unwrap());
    assert_eq!(s.len(), 3);
    assert_eq!(s[2]["args"]["limit"], json!(5));
    assert_eq!(s[2]["returns"], json!("threads"));
    let s = steps_of(
        &run("mail.google.com", "inbox", json!({"count": 12}))
            .await
            .unwrap(),
    );
    assert_eq!(s[2]["args"]["limit"], json!(12));
    // thread: the query is percent-encoded into the URL, nth comes from the int arg.
    let s = steps_of(
        &run(
            "mail.google.com",
            "thread",
            json!({"query": "subject:\"Q3 plan\"", "index": 2}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        s[0]["args"]["url"],
        json!("https://mail.google.com/mail/u/0/#search/subject%3A%22Q3%20plan%22")
    );
    assert_eq!(s[2]["target"], json!({"role": "row", "nth": 2}));
    let s = steps_of(&run("mail.google.com", "thread", json!({})).await.unwrap());
    assert_eq!(
        s[0]["args"]["url"],
        json!("https://mail.google.com/mail/u/0/#search/in%3Ainbox")
    );
    assert_eq!(s[2]["target"], json!({"role": "row", "nth": 1}));
    // send: act values filled, target names kept, expects as a final wait for=text.
    let send =
        json!({"to": "a@example.org", "subject": "Hello, there", "body": "Line one.\nLine two"});
    let out = run("mail.google.com", "send", send.clone()).await.unwrap();
    let s = steps_of(&out);
    assert_eq!(
        s[2]["target"],
        json!({"role": "combobox", "name": "To recipients"})
    );
    assert_eq!(
        s[2]["args"],
        json!({"kind": "fill", "value": "a@example.org"})
    );
    assert_eq!(s[5]["args"]["value"], json!("Line one.\nLine two"));
    assert_eq!(
        s.last().unwrap(),
        &json!({"tool": "wait", "args": {"for": "text", "value": "Message sent"}})
    );
    assert_eq!(s.len(), 9);
    assert!(!out.text.contains("[ref="), "no raw ref in a run");
    // draft: expects carries the filled subject.
    let s = steps_of(&run("mail.google.com", "draft", send.clone()).await.unwrap());
    assert_eq!(
        s.last().unwrap()["args"],
        json!({"for": "text", "value": "Hello, there"})
    );
    // calendar create: every value percent-encoded as a URL component, "" default kept.
    let s = steps_of(
        &run(
            "calendar.google.com",
            "create",
            json!({"title": "Team sync & lunch", "start": "20261028T100000", "end": "20261028T110000"}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        s[0]["args"]["url"],
        json!("https://calendar.google.com/calendar/u/0/r/eventedit?text=Team%20sync%20%26%20lunch&dates=20261028T100000/20261028T110000&details=")
    );
    let s = steps_of(
        &run("calendar.google.com", "events", json!({}))
            .await
            .unwrap(),
    );
    assert_eq!(s[2]["args"]["limit"], json!(10));
    // lu.ma register through the luma.com alias.
    let out = run("luma.com", "register", json!({"event": "cygp5fj9"}))
        .await
        .unwrap();
    let s = steps_of(&out);
    assert_eq!(s[0]["args"]["url"], json!("https://luma.com/cygp5fj9"));
    assert_eq!(
        s[2]["target"],
        json!({"role": "button", "name": "Register"})
    );

    // The run record reflect judges (D-153): server forge, tool tools.run, the step tools
    // and the filled expects.
    let ev = fx
        .store
        .ledger()
        .into_iter()
        .map(|(_, e)| e)
        .rfind(|e| e.server == SERVER && e.tool == RUN_TOOL && e.ok)
        .unwrap();
    assert_eq!(ev.args["name"], json!("register"));
    assert_eq!(ev.consequences["folder"], json!("lu.ma"));
    assert_eq!(
        ev.consequences["tools"],
        json!(["navigate", "wait", "act", "wait"])
    );
    assert_eq!(ev.consequences["expects"], json!("You're In"));
}

/// D-151, D-156: every refusal of tools.run.
#[tokio::test]
async fn run_refuses_bad_args_and_missing_or_retired_recipes() {
    let fx = fixture(Some(repo_tools())).await;
    let run = |name: &str, args: Value| {
        let fx = &fx;
        let name = name.to_owned();
        async move {
            fx.call(
                RUN_TOOL,
                json!({"domain": "mail.google.com", "name": name, "args": args}),
            )
            .await
        }
    };
    let ok = json!({"to": "a@example.org", "subject": "Hi", "body": "Hello"});
    let with = |k: &str, v: Value| {
        let mut o = ok.clone();
        o[k] = v;
        o
    };
    let cases: Vec<(&str, Value, &str)> = vec![
        ("send", with("cc", json!("x")), "unknown arg \"cc\""),
        (
            "send",
            json!({"to": "a@example.org", "subject": "Hi"}),
            "missing required arg body",
        ),
        (
            "send",
            with("subject", json!(3)),
            "arg subject must be of type string",
        ),
        (
            "inbox",
            json!({"count": "5"}),
            "arg count must be of type int",
        ),
        (
            "inbox",
            json!({"count": 2.5}),
            "arg count must be of type int",
        ),
        ("send", with("body", json!("pw: @vault(pw)")), "@vault"),
        ("send", with("body", json!("x @VAULT(pw)")), "@vault"),
        ("send", with("body", json!("one \u{2014} two")), "rule 7"),
        ("send", with("subject", json!("one \u{2013} two")), "rule 7"),
        ("send", with("body", json!("one -- two")), "rule 7"),
        ("thread", json!({"index": 0}), "1-based"),
        ("nope", json!({}), "no recipe mail.google.com/nope"),
        ("../x", json!({}), "is not [a-z0-9_-]"),
    ];
    for (name, args, want) in cases {
        let m = args_err(run(name, args.clone()).await);
        assert!(m.contains(want), "{name} {args}: {m}");
    }
    // A dash that is not rule 7's (hyphen, `--` without spaces) and an em dash in an arg
    // that only fills a URL or a wait are fine.
    run("send", with("body", json!("well-known --flag")))
        .await
        .unwrap();
    run("thread", json!({"query": "subject:a\u{2014}b"}))
        .await
        .unwrap();
    // A bad domain.
    let m = args_err(
        fx.call(RUN_TOOL, json!({"domain": "../etc", "name": "inbox"}))
            .await,
    );
    assert!(m.contains("not a host or a tool folder"), "{m}");
    // A retired row refuses the recipe even with its file in place, and tools.list hides it.
    fx.store
        .tool_health_set(ToolHealth {
            domain: "mail.google.com".into(),
            name: "inbox".into(),
            version: 2,
            status: ToolStatus::Retired,
            runs: 0,
            fails: 3,
            last_ok: None,
            last_fail: None,
            why: Some("retired after 3 failed runs".into()),
            pinned: false,
        })
        .await
        .unwrap();
    let m = args_err(run("inbox", json!({})).await);
    assert!(m.contains("retired (v2)"), "{m}");
    let out = fx
        .call(LIST_TOOL, json!({"domain": "mail.google.com"}))
        .await
        .unwrap();
    assert!(!out.text.contains("- inbox("), "{}", out.text);
    assert!(
        out.text.contains("mail.google.com: 3 recipes"),
        "{}",
        out.text
    );
    assert!(
        out.text
            .contains("not usable: inbox.recipe.yaml: retired (v2)"),
        "{}",
        out.text
    );
}

#[test]
fn encoders() {
    assert_eq!(
        percent_encode("a b&c/d?é~._-"),
        "a%20b%26c%2Fd%3F%C3%A9~._-"
    );
    assert_eq!(regex_escape("a.b*c (d)"), "a\\.b\\*c \\(d\\)");
}

// ---------------------------------------------------------------- tools.save

/// One browseros.jsonl line as scripts/hooks/browseros_ledger.py writes it.
fn line(tool: &str, args: Value, ok: bool, summary: &str) -> String {
    json!({
        "run_id": "r",
        "ts": "2026-10-27T17:10:05+0100",
        "server": "browseros",
        "tool": tool,
        "class": null,
        "mode": "external",
        "args": args,
        "result_summary": summary,
        "ok": ok,
        "why": if ok { Value::Null } else { json!(summary) },
        "session_id": "s",
    })
    .to_string()
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
    if git(root, &["init", "-q"]).is_none() {
        eprintln!("git not available: commit not checked");
        return false;
    }
    git(root, &["config", "user.email", "t@example.invalid"]).unwrap();
    git(root, &["config", "user.name", "forge test"]).unwrap();
    git(root, &["config", "commit.gpgsign", "false"]).unwrap();
    true
}

fn luma_lines() -> Vec<String> {
    vec![
        line(
            "navigate",
            json!({"url": "https://luma.com/cygp5fj9", "page": 3}),
            true,
            "Navigated",
        ),
        line(
            "snapshot",
            json!({"page": 3}),
            true,
            "- heading \"Ping Pong Night\" [ref=e3] - button \"Register\" [ref=e12]",
        ),
        line(
            "act",
            json!({"page": 3, "kind": "click", "ref": "e12", "diff": "summary"}),
            true,
            "clicked",
        ),
        line(
            "wait",
            json!({"page": 3, "for": "text", "value": "You're In"}),
            true,
            "matched",
        ),
        line("evaluate", json!({"page": 3, "expression": "1"}), true, "1"),
        line(
            "act",
            json!({"page": 3, "kind": "click", "ref": "e99"}),
            true,
            "clicked",
        ),
        line(
            "act",
            json!({"page": 3, "kind": "click", "ref": "e12"}),
            false,
            "stale ref",
        ),
        String::new(),
    ]
}

/// D-152: a recipe saved from jsonl lines loads, runs, is ephemeral and is committed.
#[tokio::test]
async fn save_writes_a_recipe_that_loads_and_runs() {
    let fx = fixture(None).await;
    let committed = git_init(&fx.tools_root);
    fx.jsonl(&luma_lines());
    let out = fx
        .call(
            SAVE_TOOL,
            json!({
                "domain": "luma.com",
                "name": "join",
                "steps": [{"line": 1}, {"line": 3}, {"line": 4}],
                "args": {"event": {"type": "string", "required": true, "value": "cygp5fj9"}},
                "expects": "You're In",
            }),
        )
        .await
        .unwrap();
    assert!(
        out.text
            .starts_with("saved tools/lu.ma/join.recipe.yaml: 3 steps, args (event), ephemeral v1"),
        "{}",
        out.text
    );
    let path = fx.tools_root.join("lu.ma/join.recipe.yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    let r = parse_recipe(&text, "lu.ma", "join").unwrap();
    assert_eq!(r.step_tools(), ["navigate", "act", "wait"]);
    assert_eq!(r.steps[0].args, json!({"url": "https://luma.com/{event}"}));
    assert_eq!(
        r.steps[1].target,
        Some(Target {
            role: "button".into(),
            name: "Register".into(),
            nth: None
        }),
        "target read from line 2's summary"
    );
    assert_eq!(
        r.steps[1].args,
        json!({"kind": "click"}),
        "ref, page, diff dropped"
    );
    assert_eq!(r.matches, ["/*"], "the path stops at the placeholder");
    assert_eq!(r.description, "lu.ma: navigate, act, wait");
    assert_eq!(r.expects.as_ref().unwrap().text, "You're In");
    for k in ["page", "ref", "e12", "diff", "cygp5fj9"] {
        assert!(!text.contains(k), "{k} in\n{text}");
    }
    let h = fx.store.tool_health("lu.ma").await.unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(
        (h[0].name.as_str(), h[0].status, h[0].version),
        ("join", ToolStatus::Ephemeral, 1)
    );
    if committed {
        let log = git(&fx.tools_root, &["log", "-1", "--format=%s", "--name-only"]).unwrap();
        assert!(
            log.starts_with(&format!(
                "tools: save lu.ma/join recipe (T-28; run {})",
                fx.run.id.0
            )),
            "{log}"
        );
        assert!(log.contains("lu.ma/join.recipe.yaml"), "{log}");
        assert!(
            !log.contains("_site.yaml"),
            "only the recipe is committed: {log}"
        );
        assert!(out.consequences["commit"].is_string());
    }
    // It runs.
    let run = fx
        .call(
            RUN_TOOL,
            json!({"domain": "lu.ma", "name": "join", "args": {"event": "abc"}}),
        )
        .await
        .unwrap();
    let s = steps_of(&run);
    assert_eq!(s[0]["args"]["url"], json!("https://luma.com/abc"));
    assert_eq!(s.len(), 4);
    // No overwrite, also not of a .js tool, also not of a live row without a file.
    let again = json!({"domain": "lu.ma", "name": "join", "steps": [{"line": 1}]});
    assert!(args_err(fx.call(SAVE_TOOL, again).await).contains("exists"));
    std::fs::write(fx.tools_root.join("lu.ma/tool.js"), "// x\n").unwrap();
    let js = json!({"domain": "lu.ma", "name": "tool", "steps": [{"line": 1}]});
    assert!(args_err(fx.call(SAVE_TOOL, js).await).contains("tool.js exists"));
    std::fs::remove_file(&path).unwrap();
    let row = json!({"domain": "lu.ma", "name": "join", "steps": [{"line": 1}]});
    assert!(args_err(fx.call(SAVE_TOOL, row).await).contains("registered in tools.health"));
}

/// D-152: each refusal names the line.
#[tokio::test]
async fn save_refuses_bad_lines() {
    let fx = fixture(None).await;
    let save = |steps: Value| {
        let fx = &fx;
        async move {
            fx.call(
                SAVE_TOOL,
                json!({"domain": "lu.ma", "name": "bad", "steps": steps}),
            )
            .await
        }
    };
    let m = args_err(save(json!([{"line": 1}])).await);
    assert!(m.contains("no browseros.jsonl"), "{m}");
    fx.jsonl(&luma_lines());
    for (steps, want) in [
        (
            json!([{"line": 5}]),
            "line 5: tool \"evaluate\" cannot be a recipe step",
        ),
        (json!([{"line": 6}]), "line 6: the call acted on ref e99"),
        (json!([{"line": 7}]), "line 7: the call failed"),
        (json!([{"line": 8}]), "line 8 is blank"),
        (json!([{"line": 9}]), "line 9: browseros.jsonl has 8 lines"),
        (json!([{"line": 0}]), "line 0"),
        (json!([]), "steps is empty"),
    ] {
        let m = args_err(save(steps.clone()).await);
        assert!(m.contains(want), "{steps}: {m}");
    }
    // A target the model gives fixes line 6; a placeholder it did not declare does not load.
    save(json!([{"line": 6, "target": {"role": "button", "name": "Go"}}]))
        .await
        .unwrap();
    let m = args_err(
        fx.call(
            SAVE_TOOL,
            json!({"domain": "lu.ma", "name": "bad2", "steps": [{"line": 1}], "expects": "{who}"}),
        )
        .await,
    );
    assert!(m.contains("would not load") && m.contains("{who}"), "{m}");
    assert!(!fx.tools_root.join("lu.ma/bad2.recipe.yaml").exists());
}

/// Rule 2: a vault value planted in a jsonl line comes out as @vault(name) in the saved file
/// (an act value, where D-150 allows it) and nowhere else: not in the reply, the ledger, the
/// health row or the commit. In a spot D-150 does not allow, the save is refused without
/// echoing it.
#[tokio::test]
async fn save_scrubs_a_planted_vault_value() {
    let fx = fixture(None).await;
    let committed = git_init(&fx.tools_root);
    let c = canary();
    fx.jsonl(&[
        line(
            "navigate",
            json!({"url": "https://luma.com/signin"}),
            true,
            "ok",
        ),
        line(
            "snapshot",
            json!({}),
            true,
            &format!("- textbox \"Secret word\" [ref=e5] {c}"),
        ),
        line(
            "act",
            json!({"kind": "fill", "ref": "e5", "value": c}),
            true,
            &format!("filled {c}"),
        ),
        line(
            "navigate",
            json!({"url": format!("https://luma.com/{c}")}),
            true,
            "ok",
        ),
    ]);
    let out = fx
        .call(
            SAVE_TOOL,
            json!({"domain": "luma.com", "name": "signin", "steps": [{"line": 1}, {"line": 3}]}),
        )
        .await
        .unwrap();
    let text = std::fs::read_to_string(fx.tools_root.join("lu.ma/signin.recipe.yaml")).unwrap();
    let saved = parse_recipe(&text, "lu.ma", "signin").unwrap();
    assert_eq!(
        saved.steps[1].args,
        json!({"kind": "fill", "value": "@vault(pw)"})
    );
    assert_eq!(text.matches("@vault(pw)").count(), 1, "{text}");
    assert_eq!(saved.steps[1].target.as_ref().unwrap().name, "Secret word");
    assert_eq!(saved.matches, ["/signin*"]);
    let refused = fx
        .call(
            SAVE_TOOL,
            json!({"domain": "luma.com", "name": "leak", "steps": [{"line": 4}]}),
        )
        .await;
    let m = args_err(refused);
    assert!(m.contains("@vault(k) is allowed only"), "{m}");
    assert!(!fx.tools_root.join("lu.ma/leak.recipe.yaml").exists());
    let mut seen = vec![out.text.clone(), m, text, fx.ledger_text()];
    seen.push(format!(
        "{:?}",
        fx.store.tool_health("lu.ma").await.unwrap()
    ));
    if committed {
        seen.push(git(&fx.tools_root, &["log", "-p", "--format=%B"]).unwrap());
    }
    for s in &seen {
        assert!(!s.contains(&c), "vault value leaked: {s}");
    }
}

#[test]
fn save_helpers() {
    assert_eq!(
        target_in_summary(
            "- row \"Inbox \\\"x\" [ref=e1] - button \"Send (Ctrl-Enter)\" [ref=e7]",
            "e7"
        ),
        Some(Target {
            role: "button".into(),
            name: "Send (Ctrl-Enter)".into(),
            nth: None
        })
    );
    assert_eq!(target_in_summary("button [ref=e7]", "e7"), None);
    assert_eq!(target_in_summary("- button \"A\" [ref=e70]", "e7"), None);
    assert_eq!(
        default_match("https://mail.google.com/mail/u/0/#inbox"),
        "/mail/u/0/*"
    );
    assert_eq!(default_match("https://luma.com/{event}"), "/*");
    assert_eq!(default_match("https://x.com"), "/*");
    assert_eq!(default_match("https://x.com/a/../b"), "/*");
    assert_eq!(default_match("https://x.com/a b"), "/a*");
}

/// D-152: declared values become placeholders: longest first, in one pass, in the forms
/// tools.run fills back (percent-encoded in a URL, regex-escaped in a grep pattern), ints
/// as whole placeholders; a 1-char value and keys are left alone.
#[test]
fn save_substitutes_declared_values() {
    let lines = [
        line(
            "navigate",
            json!({"url": "https://x.com/s?q=a%20b%26c"}),
            true,
            "",
        ),
        line(
            "grep",
            json!({"pattern": "^a b&c\\.", "limit": 15}),
            true,
            "",
        ),
        line(
            "act",
            json!({"kind": "fill", "ref": "e1", "value": "a b&c and a b"}),
            true,
            "",
        ),
        line("wait", json!({"for": "text", "value": "x"}), true, ""),
    ]
    .join("\n");
    let req: SaveRequest = serde_json::from_value(json!({
        "domain": "x.com",
        "name": "q",
        "steps": [
            {"line": 1},
            {"line": 2, "returns": "rows"},
            {"line": 3, "target": {"role": "textbox", "name": "Search a b&c"}},
            {"line": 4},
        ],
        "args": {
            "q": {"type": "string", "value": "a b&c", "default": "z"},
            "short": {"type": "string", "value": "a b"},
            "one": {"type": "string", "value": "x"},
            "n": {"type": "int", "value": 15, "default": 3},
        },
        "match": ["/s*"],
        "description": "search",
    }))
    .unwrap();
    let r = build_recipe("x.com", &req, &lines, &|s| s.to_owned()).unwrap();
    assert_eq!(r.steps[0].args, json!({"url": "https://x.com/s?q={q}"}));
    assert_eq!(
        r.steps[1].args,
        json!({"pattern": "^{q}\\.", "limit": "{n}"})
    );
    assert_eq!(r.steps[1].returns.as_deref(), Some("rows"));
    assert_eq!(
        r.steps[2].args,
        json!({"kind": "fill", "value": "{q} and {short}"})
    );
    assert_eq!(r.steps[2].target.as_ref().unwrap().name, "Search {q}");
    assert_eq!(r.steps[3].args, json!({"for": "text", "value": "x"}));
    let text = render_recipe(&r);
    let back = parse_recipe(&text, "x.com", "q").unwrap();
    assert_eq!(back, r, "{text}");
    // And tools.run gives the run's values back.
    let p = prepare_run(
        &back,
        &serde_json::from_value(json!({"q": "a b&c", "short": "a b", "one": "x", "n": 15}))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        p.steps[0]["args"]["url"],
        json!("https://x.com/s?q=a%20b%26c")
    );
    assert_eq!(
        p.steps[1]["args"],
        json!({"pattern": "^a b\\&c\\.", "limit": 15})
    );
    assert_eq!(p.steps[2]["args"]["value"], json!("a b&c and a b"));
}
