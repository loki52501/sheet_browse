//! T-28 (a), D-150: the recipe kind. The WAVE3 example parses verbatim; each D-150 rule has
//! a file that breaks only that rule; render -> parse round-trips; the `.js` scan ignores
//! recipe files; `recipes_live` skips a retired row (D-315).
//!
//! Key-shaped strings and the vault canary are built at run time, never written literally
//! (rule 1, rule 2). The canary test installs a process-wide vault holding only the canary
//! (no other test in this binary touches the global scrub).

use crate::recipe::{MAX_DESCRIPTION_CHARS, MAX_STEPS, RECIPE_SUFFIX};
use crate::*;
use harness_core::memory::{MemoryStore, ToolHealth, ToolStatus};
use harness_core::testing::MemStore;
use harness_core::vault::{EntryKind, Vault};
use serde_json::{json, Map};
use std::fs;
use std::path::Path;
use std::sync::Once;

/// The WAVE3 brief's example, verbatim ("Recipe format").
const WAVE3: &str = r#"name: inbox                      # [a-z0-9_]+, equals the file stem
domain: mail.google.com          # registrable host, same folder rules as other tools (toolstore::domain)
kind: recipe
description: "List the latest N inbox threads: sender, subject, date, snippet"
match: ["/mail/*"]               # @match path globs, same validation as other tools (no scheme, host, //, ..)
args:
  count: {type: int, default: 5}
  to: {type: string, required: true}
steps:                           # executed in order by the model through BrowserOS
  - tool: navigate
    args: {url: "https://mail.google.com/mail/u/0/#inbox"}
  - tool: wait
    args: {for: "selector", value: "[role=main]"}
  - tool: act
    target: {role: button, name: "Compose"}   # resolved to a ref from a fresh snapshot/grep at run time
    args: {kind: click}
  - tool: act
    target: {role: textbox, name: "To recipients"}
    args: {kind: type, text: "{to}"}           # {arg} placeholders are filled by tools.run
  - tool: read
    args: {format: markdown, selector: "[role=main]"}
    returns: threads                          # optional: name of the value this step yields
expects: {text: "Message sent"}               # optional success check after the last step
"#;

const FOLDER: &str = "mail.google.com";

fn parse(text: &str) -> Result<Recipe, StoreError> {
    parse_recipe(text, FOLDER, "inbox")
}

fn msg(r: Result<Recipe, StoreError>) -> String {
    match r {
        Err(StoreError::Recipe { msg, .. }) => msg,
        other => panic!("expected a recipe error, got {other:?}"),
    }
}

/// A minimal valid recipe whose single step is `step` (YAML, indented as a list item).
fn with_step(step: &str) -> String {
    format!(
        "name: inbox\ndomain: mail.google.com\nkind: recipe\ndescription: d\n\
         args:\n  to: {{type: string}}\nsteps:\n{step}"
    )
}

fn canary() -> String {
    std::env::var("SB_VAULT_CANARY")
        .ok()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "sbcanary-recipe-not-a-secret".to_owned())
}

fn install_canary_vault() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        Vault::in_memory([("test".to_owned(), canary(), EntryKind::Secret)]).install_global();
    });
}

#[test]
fn wave3_example_parses_verbatim() {
    let r = parse(WAVE3).unwrap();
    assert_eq!(r.name, "inbox");
    assert_eq!(r.domain, FOLDER);
    assert_eq!(r.matches, vec!["/mail/*"]);
    assert_eq!(
        r.args,
        vec![
            RecipeArg {
                name: "count".into(),
                ty: ArgType::Int,
                default: Some(json!(5)),
                required: false,
            },
            RecipeArg {
                name: "to".into(),
                ty: ArgType::String,
                default: None,
                required: true,
            },
        ]
    );
    assert_eq!(r.step_tools(), ["navigate", "wait", "act", "act", "read"]);
    assert_eq!(
        r.steps[2].target,
        Some(Target {
            role: "button".into(),
            name: "Compose".into(),
            nth: None
        })
    );
    assert_eq!(r.steps[3].args, json!({"kind": "type", "text": "{to}"}));
    assert_eq!(r.steps[4].returns.as_deref(), Some("threads"));
    assert_eq!(
        r.expects.as_ref().map(|e| e.text.as_str()),
        Some("Message sent")
    );
}

#[test]
fn render_parse_round_trips() {
    let r = parse(WAVE3).unwrap();
    let text = render_recipe(&r);
    assert_eq!(
        parse_recipe(&text, &r.domain, &r.name).unwrap(),
        r,
        "{text}"
    );
    // Defaults: no match, no args, no expects, a step without args.
    let small = parse("name: inbox\ndomain: mail.google.com\nkind: recipe\ndescription: d\nsteps:\n  - tool: snapshot\n").unwrap();
    assert_eq!(small.matches, vec!["/*"]);
    assert_eq!(small.steps[0].args, json!({}));
    let text = render_recipe(&small);
    assert_eq!(parse(&text).unwrap(), small, "{text}");
    // Strings YAML would otherwise read as other types or syntax survive.
    let mut odd = r.clone();
    odd.description = "yes: {to} # not a comment".into();
    odd.steps[3].args = json!({"kind": "type", "text": "@vault(pw)"});
    odd.steps[1].args = json!({"for": "text", "value": "123", "x": "null", "y": "- a"});
    let text = render_recipe(&odd);
    assert_eq!(parse(&text).unwrap(), odd, "{text}");
}

#[test]
fn fill_and_placeholders() {
    assert_eq!(
        placeholders("a {to} b {n_1} {} {x y} {to}"),
        ["to", "n_1", "to"]
    );
    let mut v = Map::new();
    v.insert("to".into(), json!("me@example.org"));
    v.insert("n".into(), json!(5));
    assert_eq!(
        fill_placeholders("To {to}, {n} of {other} {to}", &v),
        "To me@example.org, 5 of {other} me@example.org"
    );
    // One pass: a value holding a placeholder is not filled again.
    v.insert("to".into(), json!("{n}"));
    assert_eq!(fill_placeholders("{to}", &v), "{n}");
}

/// Each D-150 rule: a file that breaks only that rule is refused with a message naming it.
#[test]
fn each_rule_is_refused() {
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "raw ref key",
            with_step("  - tool: act\n    target: {role: button, name: Go}\n    args: {kind: click, ref: e12}\n"),
            "key `ref`",
        ),
        (
            "targetRef key",
            with_step("  - tool: act\n    target: {role: button, name: Go}\n    args: {kind: drag, targetRef: e3}\n"),
            "key `targetRef`",
        ),
        (
            "page key",
            with_step("  - tool: navigate\n    args: {url: \"https://x.y/\", page: 3}\n"),
            "key `page`",
        ),
        (
            "nested agentName key",
            with_step("  - tool: read\n    args: {opts: [{agentName: claude}]}\n"),
            "key `agentName`",
        ),
        (
            "undeclared placeholder",
            with_step("  - tool: navigate\n    args: {url: \"https://x.y/{who}\"}\n"),
            "placeholder {who}",
        ),
        (
            "undeclared placeholder in a target name",
            with_step("  - tool: act\n    target: {role: link, name: \"{who}\"}\n    args: {kind: click}\n"),
            "placeholder {who}",
        ),
        (
            "wait for=time",
            with_step("  - tool: wait\n    args: {for: time, value: 2000}\n"),
            "fixed sleep",
        ),
        (
            "wait without for",
            with_step("  - tool: wait\n    args: {value: 2000}\n"),
            "wait needs args.for",
        ),
        (
            "act click without target",
            with_step("  - tool: act\n    args: {kind: click}\n"),
            "act click needs a target",
        ),
        (
            "act without kind",
            with_step("  - tool: act\n    target: {role: button, name: Go}\n    args: {}\n"),
            "act needs args.kind",
        ),
        (
            "act *_at kind",
            with_step("  - tool: act\n    args: {kind: click_at, x: 10, y: 20}\n"),
            "coordinates",
        ),
        (
            "fill fields without targets",
            with_step("  - tool: act\n    args: {kind: fill, fields: [{value: a}, {value: b}]}\n"),
            "one per field",
        ),
        (
            "fill fields with too few targets",
            with_step("  - tool: act\n    targets: [{role: textbox, name: A}]\n    args: {kind: fill, fields: [{value: a}, {value: b}]}\n"),
            "one per field",
        ),
        (
            "disallowed tool evaluate",
            with_step("  - tool: evaluate\n    args: {expression: \"1+1\"}\n"),
            "\"evaluate\" is not allowed",
        ),
        (
            "disallowed tool upload",
            with_step("  - tool: upload\n    args: {}\n"),
            "\"upload\" is not allowed",
        ),
        (
            "@vault outside act text",
            with_step("  - tool: navigate\n    args: {url: \"https://x.y/?k=@vault(pw)\"}\n"),
            "@vault(k) is allowed only",
        ),
        (
            "@vault in an act arg other than text/value",
            with_step("  - tool: act\n    target: {role: textbox, name: A}\n    args: {kind: type, text: hi, note: \"@vault(pw)\"}\n"),
            "@vault(k) is allowed only",
        ),
        (
            "@vault in a target name",
            with_step("  - tool: act\n    target: {role: textbox, name: \"@vault(pw)\"}\n    args: {kind: click}\n"),
            "@vault(k) is allowed only",
        ),
        (
            "raw ref in a string",
            with_step("  - tool: grep\n    args: {pattern: \"button [ref=e5]\"}\n"),
            "raw ref",
        ),
        (
            "literal under a secret-looking key",
            with_step("  - tool: act\n    targets: [{role: textbox, name: P}]\n    args: {kind: fill, fields: [{value: \"{to}\"}], password: hunter2}\n"),
            "secret-looking key",
        ),
        (
            "a number under a secret-looking key",
            with_step("  - tool: read\n    args: {otp: 123456}\n"),
            "secret-looking key",
        ),
        (
            "name != stem",
            WAVE3.replacen("name: inbox ", "name: outbox", 1),
            "does not equal the file stem",
        ),
        (
            "upper-case name",
            WAVE3.replacen("name: inbox ", "name: Inbox ", 1),
            "is not [a-z0-9_]+",
        ),
        (
            "domain != folder",
            WAVE3.replacen("domain: mail.google.com", "domain: google.com", 1),
            "does not equal the folder",
        ),
        (
            "kind != recipe",
            WAVE3.replacen("kind: recipe", "kind: reader", 1),
            "kind must be recipe",
        ),
        (
            "match with a scheme",
            WAVE3.replacen("[\"/mail/*\"]", "[\"https://evil.com/*\"]", 1),
            "has a scheme",
        ),
        (
            "match with ..",
            WAVE3.replacen("[\"/mail/*\"]", "[\"/mail/../x\"]", 1),
            "contains ..",
        ),
        (
            "match with a host",
            WAVE3.replacen("[\"/mail/*\"]", "[\"evil.com/*\"]", 1),
            "does not start with /",
        ),
        (
            "empty match",
            WAVE3.replacen("[\"/mail/*\"]", "[]", 1),
            "match is empty",
        ),
        (
            "***",
            with_step("  - tool: act\n    target: {role: textbox, name: A}\n    args: {kind: type, text: \"***\"}\n"),
            "`***`",
        ),
        (
            "unknown top-level key",
            format!("{WAVE3}notes: hi\n"),
            "unknown field `notes`",
        ),
        (
            "unknown step key",
            with_step("  - tool: snapshot\n    wait_ms: 500\n"),
            "unknown field `wait_ms`",
        ),
        (
            "arg default of the wrong type",
            WAVE3.replacen("{type: int, default: 5}", "{type: int, default: five}", 1),
            "default is not a int value",
        ),
        (
            "arg type outside string|int|bool",
            WAVE3.replacen("{type: int, default: 5}", "{type: float}", 1),
            "unknown variant `float`",
        ),
        (
            "required arg with a default (B-210)",
            WAVE3.replacen("{type: string, required: true}", "{type: string, required: true, default: x}", 1),
            "required and default exclude each other",
        ),
        (
            "bad arg name",
            WAVE3.replacen("  count:", "  Count:", 1),
            "is not [a-z_][a-z0-9_]*",
        ),
        (
            "two-line description",
            WAVE3.replacen(
                "\"List the latest N inbox threads: sender, subject, date, snippet\"",
                "\"a\\nb\"",
                1,
            ),
            "description must be one line",
        ),
        (
            "description over 300 chars (B-214)",
            WAVE3.replacen(
                "\"List the latest N inbox threads: sender, subject, date, snippet\"",
                &"x".repeat(MAX_DESCRIPTION_CHARS + 1),
                1,
            ),
            "description must be one line",
        ),
        (
            "no steps",
            "name: inbox\ndomain: mail.google.com\nkind: recipe\ndescription: d\nsteps: []\n".into(),
            "1..=50 steps",
        ),
        (
            "YAML tag",
            with_step("  - tool: read\n    args: {format: !secret x}\n"),
            "YAML tags",
        ),
    ];
    let mut failed = Vec::new();
    for (rule, text, want) in &cases {
        match parse(text) {
            Err(StoreError::Recipe { msg, .. }) if msg.contains(want) => {}
            other => failed.push(format!("{rule}: want {want:?}, got {other:?}")),
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[test]
fn too_many_steps_is_refused() {
    let steps = "  - tool: snapshot\n".repeat(MAX_STEPS + 1);
    assert!(msg(parse(&with_step(&steps))).contains("1..=50 steps"));
    let steps = "  - tool: snapshot\n".repeat(MAX_STEPS);
    assert_eq!(parse(&with_step(&steps)).unwrap().steps.len(), MAX_STEPS);
}

/// `@vault(k)` is fine in act text, act value and fill fields[].value; a well-formed
/// literal only.
#[test]
fn vault_literals_where_allowed() {
    let ok = with_step(
        "  - tool: act\n    target: {role: textbox, name: P}\n    args: {kind: type, text: \"@vault(pw)\"}\n\
         \x20 - tool: act\n    target: {role: textbox, name: P}\n    args: {kind: fill, value: \"@vault(pw)\"}\n\
         \x20 - tool: act\n    targets: [{role: textbox, name: P}, {role: textbox, name: Q}]\n    args: {kind: fill, fields: [{value: \"@vault(pw)\"}, {value: \"{to}\"}]}\n\
         \x20 - tool: act\n    targets: [{role: textbox, name: P}]\n    args: {kind: fill, fields: [{password: \"@vault(pw)\"}]}\n",
    );
    // fields[].password is not fields[].value: refused.
    assert!(msg(parse(&ok)).contains("steps[4].args.fields[1].password: @vault(k)"));
    let ok = ok
        .rsplit_once("  - tool: act\n    targets: [{role: textbox, name: P}]\n")
        .unwrap()
        .0;
    let r = parse(ok).unwrap();
    assert_eq!(r.steps.len(), 3);
    let bad = with_step("  - tool: act\n    target: {role: textbox, name: P}\n    args: {kind: type, text: \"@vault(pw\"}\n");
    assert!(msg(parse(&bad)).contains("malformed @vault"));
}

/// A key-shaped string anywhere (value or key) is refused, and the message does not echo it.
#[test]
fn key_shaped_strings_are_refused_without_echo() {
    let key = format!("{}{}", "sk-", "Ab3".repeat(8));
    assert!(key_shaped(&key));
    let aws = format!("{}{}", "AK", "IA0123456789ABCDEF");
    assert!(key_shaped(&aws));
    assert!(!key_shaped("sk-short"));
    assert!(!key_shaped("ask-me-anything-about-skating"));
    for text in [
        with_step(&format!(
            "  - tool: act\n    target: {{role: textbox, name: K}}\n    args: {{kind: type, text: \"{key}\"}}\n"
        )),
        with_step(&format!("  - tool: read\n    args: {{{aws}: 1}}\n")),
        WAVE3.replacen("Message sent", &key, 1),
    ] {
        let m = msg(parse(&text));
        assert!(m.contains("key-shaped"), "{m}");
        assert!(!m.contains(&key) && !m.contains(&aws), "message echoes the key");
    }
}

/// A vault value (the planted canary) anywhere in the file is refused by the scrub check and
/// never echoed, including through serde's own messages (an unknown key named after it).
#[test]
fn vault_value_is_refused_without_echo() {
    install_canary_vault();
    let c = canary();
    for text in [
        with_step(&format!(
            "  - tool: act\n    target: {{role: textbox, name: K}}\n    args: {{kind: type, text: \"{c}\"}}\n"
        )),
        format!("{WAVE3}{c}: 1\n"),
        with_step(&format!("  - tool: {c}\n")),
    ] {
        let e = parse(&text).unwrap_err();
        let shown = format!("{e} {e:?}");
        assert!(shown.contains("vault value"), "{}", shown.replace(&c, "<canary>"));
        assert!(!shown.contains(&c), "error echoes the canary");
    }
}

#[test]
fn folder_and_stem_are_checked_before_parsing() {
    assert!(msg(parse_recipe(WAVE3, "../x", "inbox")).contains("folder"));
    assert!(msg(parse_recipe(WAVE3, FOLDER, "In-box")).contains("file stem"));
    match parse_recipe(WAVE3, "../x", "In-box") {
        Err(StoreError::Recipe { path, .. }) => {
            assert_eq!(path, Path::new("?").join(format!("?{RECIPE_SUFFIX}")))
        }
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------- store readers

fn put(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn js(name: &str) -> String {
    format!("// @name {name}\n// @kind reader\n// @gate Read\n// @match /*\nreturn 1;\n")
}

fn mail_root() -> (tempfile::TempDir, ToolStore) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    put(root, "mail.google.com/inbox.recipe.yaml", WAVE3);
    let draft = WAVE3.replacen("name: inbox ", "name: draft ", 1);
    put(root, "mail.google.com/draft.recipe.yaml", &draft);
    // Collides with a .js tool of the folder.
    let unread = WAVE3.replacen("name: inbox ", "name: unread ", 1);
    put(root, "mail.google.com/unread.recipe.yaml", &unread);
    put(root, "mail.google.com/unread.js", &js("unread"));
    // Breaks a rule.
    let bad = WAVE3.replacen("name: inbox ", "name: broken ", 1).replacen(
        "tool: read",
        "tool: evaluate",
        1,
    );
    put(root, "mail.google.com/broken.recipe.yaml", &bad);
    let ts = ToolStore::new(root);
    (dir, ts)
}

#[test]
fn recipes_lists_valid_and_reports_skips() {
    let (_dir, ts) = mail_root();
    let r = ts.recipes("https://mail.google.com/mail/u/0/");
    let names: Vec<&str> = r.recipes.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["draft", "inbox"]);
    let skipped: Vec<(String, &str)> = r
        .skipped
        .iter()
        .map(|(p, why)| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                why.as_str(),
            )
        })
        .collect();
    assert_eq!(skipped.len(), 2, "{skipped:?}");
    assert_eq!(skipped[0].0, "broken.recipe.yaml");
    assert!(
        skipped[0].1.contains("\"evaluate\" is not allowed"),
        "{skipped:?}"
    );
    assert_eq!(skipped[1].0, "unread.recipe.yaml");
    assert!(
        skipped[1].1.contains("collides with the tool unread.js"),
        "{skipped:?}"
    );
}

#[test]
fn recipe_by_name() {
    let (_dir, ts) = mail_root();
    assert_eq!(
        ts.recipe(FOLDER, "inbox").unwrap().unwrap(),
        parse(WAVE3).unwrap()
    );
    assert!(ts.recipe(FOLDER, "missing").unwrap().is_none());
    assert!(matches!(
        ts.recipe(FOLDER, "unread"),
        Err(StoreError::Recipe { .. })
    ));
    assert!(matches!(
        ts.recipe(FOLDER, "broken"),
        Err(StoreError::Recipe { .. })
    ));
    assert!(matches!(
        ts.recipe(FOLDER, "../x"),
        Err(StoreError::BadPath(_))
    ));
    assert!(matches!(
        ts.recipe("..", "inbox"),
        Err(StoreError::BadPath(_))
    ));
    assert_eq!(
        ts.try_recipe_path(FOLDER, "inbox").unwrap(),
        ts.root.join(FOLDER).join("inbox.recipe.yaml")
    );
    assert!(ts.try_recipe_path(FOLDER, "in-box").is_err());
}

/// D-150: the `.js` scan ignores `*.recipe.yaml` silently (not loaded, not skipped).
#[test]
fn js_scan_ignores_recipe_files() {
    let (_dir, ts) = mail_root();
    let r = ts.scan(FOLDER, "/mail/");
    let names: Vec<&str> = r.tools.iter().map(|t| t.namespaced.as_str()).collect();
    assert_eq!(names, ["mail.unread"]);
    assert!(r.skipped.is_empty(), "{:?}", r.skipped);
}

fn row(name: &str, status: ToolStatus, version: i32) -> ToolHealth {
    ToolHealth {
        domain: FOLDER.into(),
        name: name.into(),
        version,
        status,
        runs: 0,
        fails: 3,
        last_ok: None,
        last_fail: None,
        why: Some("failed".into()),
        pinned: false,
    }
}

/// D-315 for recipes: a retired row hides its recipe even with the file in place; other
/// statuses (and no row) list it; `recipes` (files only) still lists it.
#[tokio::test]
async fn recipes_live_skips_a_retired_row() {
    let (_dir, ts) = mail_root();
    let store = MemStore::new();
    store
        .tool_health_set(row("inbox", ToolStatus::Retired, 2))
        .await
        .unwrap();
    store
        .tool_health_set(row("draft", ToolStatus::NeedsRepair, 1))
        .await
        .unwrap();
    let r = ts.recipes_live(&store, "mail.google.com").await.unwrap();
    let names: Vec<&str> = r.recipes.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["draft"]);
    let inbox = ts.try_recipe_path(FOLDER, "inbox").unwrap();
    assert!(inbox.is_file());
    assert!(
        r.skipped.contains(&(inbox, "retired (v2)".to_string())),
        "{:?}",
        r.skipped
    );
    assert_eq!(r.skipped.len(), 3);
    assert_eq!(ts.recipes(FOLDER).recipes.len(), 2);
    // A host that is not a folder lists nothing.
    let none = ts.recipes_live(&store, "..").await.unwrap();
    assert_eq!(none, RecipeReport::default());
}

/// B-596 (1), D-156: a target's name may be left out (any name) and `nth` is 1-based, or a
/// whole placeholder of an int arg; both round-trip.
#[test]
fn target_name_optional_and_nth_rules() {
    let base = "name: inbox\ndomain: mail.google.com\nkind: recipe\ndescription: d\n\
                args:\n  index: {type: int, default: 1}\n  who: {type: string, default: x}\n\
                steps:\n";
    let step =
        |t: &str| format!("{base}  - tool: act\n    target: {t}\n    args: {{kind: click}}\n");
    let r = parse(&step("{role: row, nth: \"{index}\"}")).unwrap();
    assert_eq!(
        r.steps[0].target,
        Some(Target {
            role: "row".into(),
            name: String::new(),
            nth: Some(Nth::Arg("{index}".into())),
        })
    );
    assert_eq!(parse(&render_recipe(&r)).unwrap(), r);
    let r = parse(&step("{role: row, name: Inbox, nth: 2}")).unwrap();
    assert_eq!(r.steps[0].target.as_ref().unwrap().nth, Some(Nth::Index(2)));
    assert_eq!(parse(&render_recipe(&r)).unwrap(), r);
    for (bad, why) in [
        ("{role: row, nth: 0}", "1-based"),
        ("{role: row, nth: \"{who}\"}", "int arg"),
        ("{role: row, nth: \"x{index}\"}", "int arg"),
        ("{role: row, nth: \"{nope}\"}", "int arg"),
        ("{role: \"\", name: x}", "role is empty"),
    ] {
        let m = msg(parse(&step(bad)));
        assert!(m.contains(why), "{bad}: {m}");
    }
}

/// D-1500: the merged T-27 recipes (and any other `tools/**/*.recipe.yaml` of the repo) load
/// with toolstore's parser, each through its folder.
#[test]
fn repo_recipes_parse() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools");
    let ts = ToolStore::new(&root);
    let mut seen = 0;
    for e in fs::read_dir(&root).unwrap().flatten() {
        let folder = e.file_name().into_string().unwrap();
        if !e.path().is_dir() || !valid_domain(&folder) {
            continue;
        }
        let report = ts.recipes(&folder);
        assert!(report.skipped.is_empty(), "{folder}: {:?}", report.skipped);
        for r in &report.recipes {
            assert_eq!(
                parse_recipe(&render_recipe(r), &r.domain, &r.name).unwrap(),
                *r
            );
        }
        seen += report.recipes.len();
    }
    assert!(seen >= 7, "mail 4, calendar 2, lu.ma 1; saw {seen}");
    assert_eq!(
        ts.recipes("luma.com").recipes.len(),
        1,
        "luma.com is an alias of lu.ma"
    );
}

fn result(name: &str, outcome: RunOutcome) -> ToolRunResult {
    ToolRunResult {
        domain: FOLDER.into(),
        name: name.into(),
        outcome,
        at: harness_core::memory::ts_now(),
    }
}

fn failed(name: &str) -> ToolRunResult {
    result(
        name,
        RunOutcome::Failed {
            why: "step 3 (act) not seen".into(),
        },
    )
}

/// D-153, B-212: a recipe whose version reaches RECIPE_FAILS_TO_RETIRE failed runs is retired
/// and its file archived as `_archive/<domain>/<name>.<version>.recipe.yaml`; an ok run only
/// counts; a pinned recipe and a `.js` tool with the same failures stay.
#[tokio::test]
async fn three_failed_runs_retire_and_archive_a_recipe() {
    let (_dir, ts) = mail_root();
    let store = MemStore::new();
    let ok = ts
        .record(&store, result("inbox", RunOutcome::Ok))
        .await
        .unwrap();
    assert_eq!(
        (ok.status, ok.runs, ok.fails),
        (ToolStatus::Ephemeral, 1, 0)
    );
    for i in 1..RECIPE_FAILS_TO_RETIRE {
        let h = ts.record(&store, failed("inbox")).await.unwrap();
        assert_eq!((h.status, h.fails), (ToolStatus::Ephemeral, i));
    }
    let file = ts.try_recipe_path(FOLDER, "inbox").unwrap();
    assert!(file.is_file());
    let h = ts.record(&store, failed("inbox")).await.unwrap();
    assert_eq!(h.status, ToolStatus::Retired);
    assert!(
        h.why
            .as_deref()
            .unwrap()
            .starts_with("recipe retired after 3 failed runs"),
        "{:?}",
        h.why
    );
    let archived = ts.root.join("_archive/mail.google.com/inbox.1.recipe.yaml");
    assert!(!file.exists());
    assert_eq!(fs::read_to_string(&archived).unwrap(), WAVE3);
    assert_eq!(
        ts.archived_recipe(FOLDER, "inbox", 1),
        Some(archived.clone())
    );
    assert_eq!(ts.archived_recipe(FOLDER, "inbox", 2), None);
    let retire = store
        .ledger()
        .into_iter()
        .map(|(_, e)| e)
        .find(|e| e.tool == "tools.retire")
        .unwrap();
    assert_eq!(
        retire.consequences,
        json!({"archived": "_archive/mail.google.com/inbox.1.recipe.yaml"})
    );
    // A later failure of the retired row archives nothing more.
    let h = ts.record(&store, failed("inbox")).await.unwrap();
    assert_eq!(h.status, ToolStatus::Retired);
    // The same version archived twice takes the next free name.
    put(&ts.root, "mail.google.com/inbox.recipe.yaml", WAVE3);
    let again = ts.archive(FOLDER, "inbox", 1).unwrap().unwrap();
    assert_eq!(
        again,
        ts.root
            .join("_archive/mail.google.com/inbox.1.2.recipe.yaml")
    );
    assert_eq!(ts.archived_recipe(FOLDER, "inbox", 1), Some(again));

    // Pinned (T-7): counted, never retired here (reflect proposes it instead).
    ts.set_pinned(&store, FOLDER, "draft", true).await.unwrap();
    for _ in 0..RECIPE_FAILS_TO_RETIRE + 1 {
        ts.record(&store, failed("draft")).await.unwrap();
    }
    let h = ts.health(&store, FOLDER, "draft").await.unwrap().unwrap();
    assert_eq!((h.status, h.fails), (ToolStatus::Ephemeral, 4));
    assert!(ts.try_recipe_path(FOLDER, "draft").unwrap().is_file());
    // A .js tool keeps the T-7 rule: plain failures do not retire it.
    for _ in 0..RECIPE_FAILS_TO_RETIRE {
        ts.record(&store, failed("unread")).await.unwrap();
    }
    let h = ts.health(&store, FOLDER, "unread").await.unwrap().unwrap();
    assert_ne!(h.status, ToolStatus::Retired);
    assert!(ts.root.join("mail.google.com/unread.js").is_file());
}

/// D-303 for recipes: a retired recipe whose file is still in place is archived by the sweep.
#[tokio::test]
async fn sweep_archives_a_retired_recipe_left_in_place() {
    let (_dir, ts) = mail_root();
    let store = MemStore::new();
    store
        .tool_health_set(row("draft", ToolStatus::Retired, 2))
        .await
        .unwrap();
    let done = ts
        .sweep(&store, harness_core::memory::ts_now(), &|_| 0)
        .await
        .unwrap();
    let to = ts.root.join("_archive/mail.google.com/draft.2.recipe.yaml");
    assert!(
        done.contains(&SweepAction::Archived {
            domain: FOLDER.into(),
            name: "draft".into(),
            version: 2,
            to: to.clone(),
        }),
        "{done:?}"
    );
    assert!(to.is_file());
    assert!(!ts.try_recipe_path(FOLDER, "draft").unwrap().exists());
    assert_eq!(
        ts.tool_file(FOLDER, "unread").unwrap(),
        ts.root.join("mail.google.com/unread.js"),
        "a .js tool wins over a recipe of the same name"
    );
}
