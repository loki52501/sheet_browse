//! Runner tests: T-9 invariant 2 (no document/window/fetch), D-52 quoting, D-53 page API,
//! D-305 sandbox and caps. No network: hosts are fakes.

use super::*;
use crate::{ArgSpec, ToolHeader};
use harness_core::Class;
use serde_json::json;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

struct FakePage {
    refs: HashMap<&'static str, &'static str>,
}

impl FakePage {
    fn new() -> FakePage {
        let refs = HashMap::from([
            ("input \"Full name\"", "12"),
            ("input \"Email\"", "13"),
            ("button \"Register\" SUBMIT", "14"),
            ("link \"Job\"", "f2.15"),
        ]);
        FakePage { refs }
    }
}

impl PageHost for FakePage {
    fn find(&self, q: &str) -> Result<String, String> {
        self.refs
            .get(q)
            .map(|r| r.to_string())
            .ok_or_else(|| format!("find({q:?}): no match"))
    }
    fn find_all(&self, q: &str) -> Vec<String> {
        self.refs
            .iter()
            .filter(|(k, _)| k.starts_with(q))
            .map(|(_, v)| v.to_string())
            .collect()
    }
    fn text(&self, region: Option<&str>) -> String {
        format!("text of {}", region.unwrap_or("page"))
    }
    fn table(&self, id: &str) -> serde_json::Value {
        if id == "1" {
            json!({"header": ["Role"], "rows": [["Engineer"]]})
        } else {
            serde_json::Value::Null
        }
    }
    fn url(&self) -> String {
        "https://luma.example/e/abc".into()
    }
    fn title(&self) -> String {
        "Meetup".into()
    }
}

#[derive(Default)]
struct FakeApi {
    calls: RefCell<Vec<String>>,
    budgets: RefCell<Vec<Duration>>,
}

impl ApiFetch for FakeApi {
    fn api(&self, url: &str, remaining: Duration) -> Result<serde_json::Value, String> {
        self.calls.borrow_mut().push(url.to_string());
        self.budgets.borrow_mut().push(remaining);
        if url.contains("/api/jobs") {
            Ok(json!({"jobs": [{"title": "A"}, {"title": "B"}]}))
        } else {
            Err(format!("api {url}: 404"))
        }
    }
}

fn tool(kind: ToolKind, phase: u8, args: &[(&str, bool)], src: &str) -> Tool {
    Tool {
        domain: "luma.example".into(),
        namespaced: "luma.t".into(),
        path: PathBuf::from("tools/luma.example/t.js"),
        header: ToolHeader {
            name: "t".into(),
            kind,
            matches: vec!["/*".into()],
            args: args
                .iter()
                .map(|(n, optional)| ArgSpec {
                    name: n.to_string(),
                    ty: "string".into(),
                    optional: *optional,
                })
                .collect(),
            gate: Class::Read,
            fingerprint: vec![],
            expects: None,
            forged: None,
            phase,
        },
        source: src.into(),
    }
}

fn reader(phase: u8, body: &str) -> Tool {
    tool(
        ToolKind::Reader,
        phase,
        &[],
        &format!("export default function (page, args) {{ {body} }}"),
    )
}

fn read(body: &str) -> Result<serde_json::Value, RunError> {
    run_reader(&reader(2, body), &FakePage::new(), &json!({}), None)
}

fn register() -> Tool {
    tool(
        ToolKind::Planner,
        2,
        &[("name", false), ("email", true)],
        r#"// @name register
// @kind planner
export default function (page, args) {
  const name = page.find('input "Full name"');
  const btn = page.find('button "Register" SUBMIT');
  return `t#${name}=${q(args.name)}; c#${btn}; ?s`;
}
"#,
    )
}

/// Splits an act program on `;` outside quoted values (D-52 escapes), for counting steps.
fn steps(program: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted, mut esc) = (Vec::new(), String::new(), false, false);
    for c in program.chars() {
        match c {
            _ if esc => esc = false,
            '\\' if quoted => esc = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur.trim().to_string());
    out
}

#[test]
fn runner_globals_are_exactly_the_allowlist() {
    let v =
        read(r#"return Reflect.ownKeys(globalThis).filter(k => typeof k === "string");"#).unwrap();
    let mut got: Vec<String> = serde_json::from_value(v).unwrap();
    got.sort();
    let mut want: Vec<String> = ALLOWED_GLOBALS.iter().map(|s| s.to_string()).collect();
    want.push("q".into());
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn runner_prelude_fails_on_a_global_it_cannot_remove() {
    let rt = Runtime::new().unwrap();
    let ctx = Context::custom::<Intrinsics>(&rt).unwrap();
    ctx.with(|ctx| {
        ctx.eval::<(), _>(
            "Object.defineProperty(globalThis, 'stuck', { value: 1, configurable: false });",
        )
        .unwrap();
        match install_prelude(&ctx) {
            Err(RunError::Load(msg)) => assert!(msg.contains("`stuck`"), "{msg}"),
            r => panic!("{r:?}"),
        }
    });
}

#[test]
fn runner_globals_absent() {
    let v = read(
        r#"const t = {};
        for (const k of ["document", "window", "fetch", "eval", "require", "XMLHttpRequest",
                         "setTimeout", "setInterval", "queueMicrotask", "navigator", "process",
                         "importScripts", "WebAssembly", "performance"]) {
          t[k] = eval_free_typeof(k);
        }
        function eval_free_typeof(k) { return typeof globalThis[k]; }
        t.direct = [typeof document, typeof window, typeof fetch, typeof eval, typeof require];
        t.pageKeys = Object.keys(page).sort();
        t.frozen = Object.isFrozen(page);
        t.q = typeof q;
        return t;"#,
    )
    .unwrap();
    for (k, val) in v.as_object().unwrap() {
        if k == "direct" {
            assert_eq!(val, &json!(vec!["undefined"; 5]));
        } else if k == "pageKeys" {
            assert_eq!(
                val,
                &json!(["find", "findAll", "table", "text", "title", "url"])
            );
        } else if k == "frozen" {
            assert_eq!(val, &json!(true));
        } else if k == "q" {
            assert_eq!(val, &json!("function"));
        } else {
            assert_eq!(val, &json!("undefined"), "global {k}");
        }
    }
}

#[test]
fn runner_forbidden_calls_throw() {
    for call in [
        "document.title",
        "window.location",
        "fetch('https://x.example/')",
        "eval('1 + 1')",
        "require('fs')",
        "new XMLHttpRequest()",
        "setTimeout(() => 0, 1)",
        "Function('return 1')()",
        "new Function('return 1')()",
        "(function () {}).constructor('return 1')()",
        "(() => 0).constructor('return 1')()",
        "(async function () {}).constructor('return 1')",
        "(function* () {}).constructor('yield 1')",
        "page.document.body",
    ] {
        let err = read(&format!("return {call};")).unwrap_err();
        assert!(matches!(err, RunError::Threw { .. }), "{call}: {err:?}");
    }
    // instanceof still works with the poisoned constructor.
    assert_eq!(
        read("return (() => 0) instanceof Function;").unwrap(),
        json!(true)
    );
}

#[test]
fn runner_no_import() {
    let t = tool(
        ToolKind::Reader,
        2,
        &[],
        "import fs from 'fs';\nexport default function () { return 1; }",
    );
    let err = run_reader(&t, &FakePage::new(), &json!({}), None).unwrap_err();
    assert!(matches!(err, RunError::Load(_)), "{err:?}");

    let t = tool(
        ToolKind::Reader,
        2,
        &[],
        "export default async function () { const m = await import('fs'); return 1; }",
    );
    match run_reader(&t, &FakePage::new(), &json!({}), None).unwrap_err() {
        RunError::Threw { msg } => assert!(msg.contains("could not load module"), "{msg}"),
        e => panic!("{e:?}"),
    }
}

#[test]
fn runner_reader_returns_json() {
    let t = tool(
        ToolKind::Reader,
        2,
        &[("n", false)],
        r#"export default function (page, args) {
  return { url: page.url, title: page.title, n: args.n + 1, main: page.text("main"),
           all: page.text(), tbl: page.table("1"), none: page.table("9"),
           jobs: page.findAll('link "Job"') };
}"#,
    );
    let v = run_reader(&t, &FakePage::new(), &json!({"n": 41}), None).unwrap();
    assert_eq!(
        v,
        json!({
            "url": "https://luma.example/e/abc", "title": "Meetup", "n": 42,
            "main": "text of main", "all": "text of page",
            "tbl": {"header": ["Role"], "rows": [["Engineer"]]}, "none": null,
            "jobs": ["f2.15"]
        })
    );
}

#[test]
fn runner_async_reader_settles() {
    assert_eq!(read("return Promise.resolve(7);").unwrap(), json!(7));
    let t = tool(
        ToolKind::Reader,
        2,
        &[],
        "export default async function (page) { return await Promise.resolve(page.title); }",
    );
    assert_eq!(
        run_reader(&t, &FakePage::new(), &json!(null), None).unwrap(),
        json!("Meetup")
    );
}

#[test]
fn runner_planner_builds_program_with_q() {
    let p = run_planner(&register(), &FakePage::new(), &json!({"name": "Ada"})).unwrap();
    assert_eq!(p, r#"t#12="Ada"; c#14; ?s"#);
}

#[test]
fn runner_quoting_cannot_inject_a_step() {
    let evil = "Ada\"; c#99";
    let p = run_planner(&register(), &FakePage::new(), &json!({ "name": evil })).unwrap();
    assert_eq!(p, format!("t#12={}; c#14; ?s", sb_act::quote(evil)));
    assert_eq!(p, r#"t#12="Ada\"; c#99"; c#14; ?s"#);
    let s = steps(&p);
    assert_eq!(s.len(), 3, "{s:?}");
    assert_eq!(s[1], "c#14");

    for evil in ["x; c#99", "a\\\"; c#99; \"", "line\n; c#99", "\\", "\"\""] {
        let p = run_planner(&register(), &FakePage::new(), &json!({ "name": evil })).unwrap();
        assert_eq!(steps(&p).len(), 3, "{evil:?} -> {p}");
        assert!(!steps(&p).iter().any(|s| s == "c#99"), "{evil:?} -> {p}");
    }
}

#[test]
fn runner_q_rejects_undefined_and_null() {
    let v = read(
        r#"const out = [];
        for (const f of [() => q(undefined), () => q(null), () => q(), () => q(args.missing)]) {
          try { out.push(f()); } catch (e) { out.push(e.name + ": " + e.message); }
        }
        return out;"#,
    )
    .unwrap();
    assert_eq!(
        v,
        json!([
            "TypeError: q: value is undefined",
            "TypeError: q: value is null",
            "TypeError: q: value is undefined",
            "TypeError: q: value is undefined"
        ])
    );
    // A planner that quotes a missing optional arg fails instead of typing "undefined".
    let t = tool(
        ToolKind::Planner,
        2,
        &[("email", true)],
        "export default function (page, args) { return `t#13=${q(args.email)}`; }",
    );
    match run_planner(&t, &FakePage::new(), &json!({})).unwrap_err() {
        RunError::Threw { msg } => {
            assert!(msg.starts_with("TypeError: q: value is undefined"), "{msg}")
        }
        e => panic!("{e:?}"),
    }
    // Falsy values that are present still quote.
    assert_eq!(
        read("return [q(0), q(false), q('')];").unwrap(),
        json!([
            sb_act::quote("0"),
            sb_act::quote("false"),
            sb_act::quote("")
        ])
    );
}

#[test]
fn runner_q_is_sb_act_quote() {
    let t = tool(
        ToolKind::Reader,
        2,
        &[],
        r#"export default function () { return [q('a"b\\c\nd'), q(5), q("")]; }"#,
    );
    let v = run_reader(&t, &FakePage::new(), &json!({}), None).unwrap();
    assert_eq!(
        v,
        json!([
            sb_act::quote("a\"b\\c\nd"),
            sb_act::quote("5"),
            sb_act::quote("")
        ])
    );
}

#[test]
fn runner_planner_has_no_api() {
    let t = tool(
        ToolKind::Planner,
        1,
        &[],
        "export default function (page) { return typeof page.api; }",
    );
    assert_eq!(
        run_planner(&t, &FakePage::new(), &json!({})).unwrap(),
        "undefined"
    );
    // A @phase 2 reader does not get api either, even when the caller offers one.
    let api = FakeApi::default();
    let v = run_reader(
        &reader(2, "return typeof page.api;"),
        &FakePage::new(),
        &json!({}),
        Some(&api),
    )
    .unwrap();
    assert_eq!(v, json!("undefined"));
    assert!(api.calls.borrow().is_empty());
}

#[test]
fn runner_api_reaches_fetch_for_phase1_reader() {
    let api = FakeApi::default();
    let t = reader(
        1,
        r#"const r = page.api("https://jobs.example/api/jobs?board=x");
        let err = null;
        try { page.api("https://jobs.example/nope"); } catch (e) { err = e.message; }
        return { titles: r.jobs.map(j => j.title), err };"#,
    );
    let v = run_reader(&t, &FakePage::new(), &json!({}), Some(&api)).unwrap();
    assert_eq!(
        v,
        json!({"titles": ["A", "B"], "err": "api https://jobs.example/nope: 404"})
    );
    assert_eq!(
        *api.calls.borrow(),
        vec![
            "https://jobs.example/api/jobs?board=x".to_string(),
            "https://jobs.example/nope".to_string()
        ]
    );
    // Each call is told what is left of the default 2 s cap.
    for b in api.budgets.borrow().iter() {
        assert!(*b > Duration::ZERO && *b <= Duration::from_secs(2), "{b:?}");
    }
}

/// Overruns whatever budget it is given, then answers.
#[derive(Default)]
struct SlowApi {
    budgets: RefCell<Vec<Duration>>,
}

impl ApiFetch for SlowApi {
    fn api(&self, _url: &str, remaining: Duration) -> Result<serde_json::Value, String> {
        self.budgets.borrow_mut().push(remaining);
        // Spins on the clock (no sleep): the point of this fake is to come back late.
        let until = Instant::now() + remaining + Duration::from_millis(20);
        while Instant::now() < until {
            std::hint::spin_loop();
        }
        Ok(json!({"late": true}))
    }
}

#[test]
fn runner_api_past_the_deadline_is_timeout() {
    let opts = RunOpts {
        timeout: Duration::from_millis(150),
        ..RunOpts::default()
    };
    for body in [
        // The late Ok is used as the result.
        "return page.api('https://jobs.example/api/jobs');",
        // The tool swallows the error and returns right away.
        "try { page.api('https://jobs.example/api/jobs'); } catch (e) {} return 1;",
        // A second call after the budget is spent never reaches the host.
        "try { page.api('https://a.example/1'); } catch (e) {}
         try { page.api('https://a.example/2'); } catch (e) {} return 2;",
    ] {
        let api = SlowApi::default();
        let start = Instant::now();
        let t = reader(1, body);
        let err = run_reader_with(&t, &FakePage::new(), &json!({}), Some(&api), &opts).unwrap_err();
        assert_eq!(err, RunError::Timeout(Duration::from_millis(150)), "{body}");
        assert!(start.elapsed() < Duration::from_secs(2), "{body}");
        let budgets = api.budgets.borrow();
        assert_eq!(budgets.len(), 1, "{body}");
        assert!(
            budgets[0] > Duration::ZERO && budgets[0] <= Duration::from_millis(150),
            "{body}: {budgets:?}"
        );
    }
}

#[test]
fn runner_find_errors_are_catchable_and_uncaught_ones_surface() {
    assert_eq!(
        read(r#"try { page.find("nope"); return "no"; } catch (e) { return e.message; }"#).unwrap(),
        json!("find(\"nope\"): no match")
    );
    match read(r#"return page.find("nope");"#).unwrap_err() {
        RunError::Threw { msg } => assert!(msg.contains("no match"), "{msg}"),
        e => panic!("{e:?}"),
    }
}

#[test]
fn runner_infinite_loop_hits_timeout() {
    let opts = RunOpts {
        timeout: Duration::from_millis(200),
        ..RunOpts::default()
    };
    for body in [
        "while (true) {}",
        "for (;;) { try { while (true) {} } catch (e) {} }",
    ] {
        let start = Instant::now();
        let err = run_reader_with(&reader(2, body), &FakePage::new(), &json!({}), None, &opts)
            .unwrap_err();
        assert_eq!(err, RunError::Timeout(Duration::from_millis(200)), "{body}");
        assert!(start.elapsed() < Duration::from_secs(2), "{body}");
    }
    // A loop at module top level is capped too.
    let t = tool(
        ToolKind::Reader,
        2,
        &[],
        "while (true) {}\nexport default function () { return 1; }",
    );
    let err = run_reader_with(&t, &FakePage::new(), &json!({}), None, &opts).unwrap_err();
    assert_eq!(err, RunError::Timeout(Duration::from_millis(200)));
    assert_eq!(RunOpts::default().timeout, Duration::from_secs(2));
}

#[test]
fn runner_memory_and_stack_caps() {
    let opts = RunOpts {
        memory_limit: 8 * 1024 * 1024,
        ..RunOpts::default()
    };
    let err = run_reader_with(
        &reader(
            2,
            "const a = []; for (;;) a.push('x'.repeat(1 << 20) + a.length);",
        ),
        &FakePage::new(),
        &json!({}),
        None,
        &opts,
    )
    .unwrap_err();
    assert_eq!(err, RunError::OutOfMemory);

    match read("function f(n) { return f(n + 1) + 1; } return f(0);").unwrap_err() {
        RunError::Threw { msg } => assert!(msg.starts_with("RangeError"), "{msg}"),
        e => panic!("{e:?}"),
    }
}

#[test]
fn runner_oom_in_json_stringify_is_out_of_memory() {
    let opts = RunOpts {
        memory_limit: 8 * 1024 * 1024,
        max_output: usize::MAX,
        ..RunOpts::default()
    };
    // 16 references to one 1 MiB string: cheap to build, 16 MiB to serialize.
    let t = reader(
        2,
        "const s = 'x'.repeat(1 << 20); return new Array(16).fill(s);",
    );
    let err = run_reader_with(&t, &FakePage::new(), &json!({}), None, &opts).unwrap_err();
    assert_eq!(err, RunError::OutOfMemory);
}

#[test]
fn runner_thrown_out_of_memory_text_stays_threw() {
    for body in [
        "throw new Error('out of memory');",
        "throw new InternalError('out of memory');",
        "throw 'out of memory';",
    ] {
        match read(body).unwrap_err() {
            RunError::Threw { msg } => assert!(msg.contains("out of memory"), "{msg}"),
            e => panic!("{body}: {e:?}"),
        }
    }
    // The same text thrown while serializing the result is a bad return, not OutOfMemory.
    match read("return { toJSON() { throw new InternalError('out of memory'); } };").unwrap_err() {
        RunError::BadReturn(msg) => assert!(msg.contains("out of memory"), "{msg}"),
        e => panic!("{e:?}"),
    }
}

#[test]
fn runner_output_cap() {
    assert_eq!(RunOpts::default().max_output, 64 * 1024);
    let small = RunOpts {
        max_output: 12,
        ..RunOpts::default()
    };
    // The JSON text of "0123456789" is 12 bytes: at the cap is fine, one more is not.
    let at = reader(2, "return '0123456789';");
    assert_eq!(
        run_reader_with(&at, &FakePage::new(), &json!({}), None, &small).unwrap(),
        json!("0123456789")
    );
    let over = reader(2, "return '0123456789a';");
    assert_eq!(
        run_reader_with(&over, &FakePage::new(), &json!({}), None, &small),
        Err(RunError::OutputTooLarge { max: 12, got: 13 })
    );
    // Default cap: a 70,000-char string is refused, and a bigger cap lets it through.
    let big = reader(2, "return 'x'.repeat(70000);");
    assert_eq!(
        run_reader(&big, &FakePage::new(), &json!({}), None),
        Err(RunError::OutputTooLarge {
            max: 64 * 1024,
            got: 70002
        })
    );
    let roomy = RunOpts {
        max_output: 1 << 20,
        ..RunOpts::default()
    };
    assert!(run_reader_with(&big, &FakePage::new(), &json!({}), None, &roomy).is_ok());
    // Planners: the program text.
    let t = tool(
        ToolKind::Planner,
        2,
        &[],
        "export default function () { return 'c#1; '.repeat(20000); }",
    );
    assert_eq!(
        run_planner(&t, &FakePage::new(), &json!({})),
        Err(RunError::OutputTooLarge {
            max: 64 * 1024,
            got: 100000
        })
    );
}

#[test]
fn runner_bad_returns_and_args() {
    assert!(matches!(read("return;"), Err(RunError::BadReturn(_))));
    assert!(matches!(
        read("return () => 1;"),
        Err(RunError::BadReturn(_))
    ));
    let t = tool(
        ToolKind::Planner,
        2,
        &[],
        "export default function () { return 5; }",
    );
    assert!(matches!(
        run_planner(&t, &FakePage::new(), &json!({})),
        Err(RunError::BadReturn(_))
    ));
    assert_eq!(
        run_planner(&register(), &FakePage::new(), &json!({"email": "a@b"})),
        Err(RunError::Args("missing required arg `name`".into()))
    );
    assert!(matches!(
        run_planner(&register(), &FakePage::new(), &json!(["Ada"])),
        Err(RunError::Args(_))
    ));
    assert_eq!(
        run_reader(&register(), &FakePage::new(), &json!({"name": "Ada"}), None),
        Err(RunError::WrongKind {
            expected: ToolKind::Reader,
            got: ToolKind::Planner
        })
    );
    let t = tool(ToolKind::Reader, 2, &[], "export const x = 1;");
    assert!(matches!(
        run_reader(&t, &FakePage::new(), &json!({}), None),
        Err(RunError::Load(_))
    ));
    let t = tool(ToolKind::Reader, 2, &[], "export default function ( {");
    assert!(matches!(
        run_reader(&t, &FakePage::new(), &json!({}), None),
        Err(RunError::Load(_))
    ));
}

#[test]
fn runner_fresh_context_per_run() {
    let body = "const seen = typeof globalThis.leak; globalThis.leak = 1; \
                Object.prototype.polluted = 1; return seen;";
    assert_eq!(read(body).unwrap(), json!("undefined"));
    assert_eq!(read(body).unwrap(), json!("undefined"));
    assert_eq!(
        read("return typeof ({}).polluted;").unwrap(),
        json!("undefined")
    );
}

#[test]
fn runner_error_text_names_the_line() {
    match read("\n  throw new TypeError('boom');").unwrap_err() {
        RunError::Threw { msg } => {
            assert!(msg.starts_with("TypeError: boom"), "{msg}");
            assert!(msg.contains("luma.t.js:2"), "{msg}");
        }
        e => panic!("{e:?}"),
    }
}

#[test]
fn runner_sheet_page_table_and_construction() {
    use sb_sheet::{ElemRef, Span};
    let row = |order, table, header, cells: &[(&str, Option<u32>)]| TextBlock {
        order,
        frame: 0,
        kind: BlockKind::TableRow,
        level: None,
        landmark: None,
        heading: None,
        table: Some(table),
        header,
        spans: cells
            .iter()
            .map(|(t, e)| Span {
                text: t.to_string(),
                elem: e.map(ElemRef::main),
            })
            .collect(),
    };
    let blocks = vec![
        row(3, 1, false, &[("Engineer", Some(40)), (" Remote ", None)]),
        row(1, 1, true, &[("Role", None), ("Where", None)]),
        row(2, 2, false, &[("other", None)]),
    ];
    let page = SheetPage::new(&[], &blocks, "https://x.example/", "X");
    assert_eq!(page.url(), "https://x.example/");
    assert_eq!(page.title(), "X");
    assert_eq!(
        page.table("1"),
        json!({"header": ["Role", "Where"], "rows": [["Engineer", "Remote"]],
               "refs": [["40", null]]})
    );
    assert_eq!(page.table("#2"), json!({"header": [], "rows": [["other"]]}));
    assert_eq!(page.table("3"), serde_json::Value::Null);
    assert_eq!(page.table("f1.1"), serde_json::Value::Null);
    assert_eq!(page.table("junk"), serde_json::Value::Null);
}
