//! Tool runner (D-53, D-305): runs a tool file's `export default function (page, args)` in
//! an embedded JS engine (rquickjs 0.14, quickjs-ng) and hands back JSON (readers) or an act
//! program string (planners). Shared by srv-web (fetched index) and sb-mcp (live index).
//!
//! Sandbox, per run:
//! - A fresh `Runtime` and `Context`; nothing survives between runs.
//! - Intrinsics: base objects, Date, RegExp, JSON, Map/Set, Promise, and the compiler that
//!   Rust itself needs to load the module. A prelude then deletes every global outside a
//!   fixed allowlist ([`ALLOWED_GLOBALS`]) and replaces the `Function`, `AsyncFunction`,
//!   `GeneratorFunction` and `AsyncGeneratorFunction` constructors with one that throws, so
//!   there is no `document`, `window`, `fetch`, `XMLHttpRequest`, `setTimeout`, `eval`,
//!   string-to-code `Function`, `require` or `queueMicrotask`.
//! - No module loader is installed: a static `import` fails to load and a dynamic
//!   `import()` rejects.
//! - The tool file is loaded as an ES module named `<namespaced>.js` (no source rewriting);
//!   its `default` export must be a function. If it returns a promise the runner drives the
//!   job queue until it settles (still under the wall cap).
//! - Wall-time cap through the interrupt handler (no sleeps, no threads), a memory cap
//!   enforced by a counting allocator, and an output cap, all from [`RunOpts`]. The wall cap
//!   includes host calls: `api` gets the remaining budget and a call that returns after the
//!   deadline turns the run into [`RunError::Timeout`] (D-308).
//!
//! Page API (the `page` argument, frozen): `find(q)`, `findAll(q)`, `text(region?)`,
//! `table(id)`, `url`, `title`, and `api(url)` only for `@phase 1` readers that were given
//! an [`ApiFetch`]. Host errors (`find` with no or a weak match, `api` failures) are thrown
//! as JS `Error`s the tool can catch. The global `q(s)` is `sb_act::quote` (D-52).

use crate::{Tool, ToolKind};
use rquickjs::allocator::{Allocator, RustAllocator};
use rquickjs::context::intrinsic;
use rquickjs::prelude::Opt;
use rquickjs::{
    Coerced, Context, Ctx, Error as JsError, Exception, Function, Module, Object, Runtime, Value,
};
use sb_sheet::{BlockKind, FindOpts, HitReason, IndexRow, ReadOpts, TextBlock};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// What the tool sees as `page` (D-53). Implemented by [`SheetPage`] over an index, and by
/// test fakes. Refs are returned without the `#` (`12`, `f2.15`) so planners write `c#${id}`.
pub trait PageHost {
    /// Best element for `q`; `Err` (thrown into JS) when nothing matches or the match is weak.
    fn find(&self, q: &str) -> Result<String, String>;
    /// Every strong match for `q`, document order.
    fn find_all(&self, q: &str) -> Vec<String>;
    /// Short text of a region (landmark, form id or heading), or of the page for `None`.
    fn text(&self, region: Option<&str>) -> String;
    /// Table `id` (`3` or `f2.3`) as JSON, `null` when absent.
    fn table(&self, id: &str) -> serde_json::Value;
    fn url(&self) -> String;
    fn title(&self) -> String;
}

/// `page.api(url)` for `@phase 1` readers. srv-web wires this to its `api` tool, which goes
/// through the gate as Read (D-53). The runner never touches the network itself.
pub trait ApiFetch {
    /// `remaining` is what is left of the run's wall cap (D-308). The call must return within
    /// it; one that returns later (`Ok` or `Err`) makes the whole run [`RunError::Timeout`].
    fn api(&self, url: &str, remaining: Duration) -> Result<serde_json::Value, String>;
}

/// Run limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOpts {
    /// Wall-time cap for load + call (+ promise settling), host calls included.
    pub timeout: Duration,
    /// QuickJS heap cap in bytes.
    pub memory_limit: usize,
    /// QuickJS stack cap in bytes (kept well under a 1 MiB native thread stack).
    pub max_stack: usize,
    /// Output cap in bytes: the JSON text of a reader's result, or a planner's program text.
    /// Readers return short text (CLAUDE.md rule 5).
    pub max_output: usize,
}

impl Default for RunOpts {
    fn default() -> Self {
        RunOpts {
            timeout: Duration::from_secs(2),
            memory_limit: 32 * 1024 * 1024,
            max_stack: 512 * 1024,
            max_output: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    #[error("tool is a {got:?}, expected a {expected:?}")]
    WrongKind { expected: ToolKind, got: ToolKind },
    #[error("bad args: {0}")]
    Args(String),
    /// The module did not compile, link or evaluate, or has no default-export function.
    #[error("tool failed to load: {0}")]
    Load(String),
    /// The tool's function threw (including host errors it did not catch).
    #[error("tool threw: {msg}")]
    Threw { msg: String },
    #[error("tool ran past its {0:?} cap")]
    Timeout(Duration),
    /// The engine refused an allocation past [`RunOpts::memory_limit`] and the run failed.
    #[error("tool ran out of memory")]
    OutOfMemory,
    /// The result was longer than [`RunOpts::max_output`] bytes.
    #[error("tool output is {got} bytes, over the {max} byte cap")]
    OutputTooLarge { max: usize, got: usize },
    /// Reader returned something that is not JSON, or a planner returned a non-string.
    #[error("bad return: {0}")]
    BadReturn(String),
    #[error("js engine: {0}")]
    Engine(String),
}

/// Runs a reader with [`RunOpts::default`]. `api` is exposed only when the tool is `@phase 1`.
pub fn run_reader(
    tool: &Tool,
    host: &dyn PageHost,
    args: &serde_json::Value,
    api: Option<&dyn ApiFetch>,
) -> Result<serde_json::Value, RunError> {
    run_reader_with(tool, host, args, api, &RunOpts::default())
}

pub fn run_reader_with(
    tool: &Tool,
    host: &dyn PageHost,
    args: &serde_json::Value,
    api: Option<&dyn ApiFetch>,
    opts: &RunOpts,
) -> Result<serde_json::Value, RunError> {
    expect_kind(tool, ToolKind::Reader)?;
    let api = if tool.header.phase == 1 { api } else { None };
    match run(tool, host, args, api, ToolKind::Reader, opts)? {
        Out::Json(v) => Ok(v),
        Out::Program(_) => Err(RunError::Engine("reader produced a program".into())),
    }
}

/// Runs a planner with [`RunOpts::default`] and returns its act program. Planners never get
/// `api`. The caller parses, checks (`sb_act::check_planner`) and classifies the program.
pub fn run_planner(
    tool: &Tool,
    host: &dyn PageHost,
    args: &serde_json::Value,
) -> Result<String, RunError> {
    run_planner_with(tool, host, args, &RunOpts::default())
}

pub fn run_planner_with(
    tool: &Tool,
    host: &dyn PageHost,
    args: &serde_json::Value,
    opts: &RunOpts,
) -> Result<String, RunError> {
    expect_kind(tool, ToolKind::Planner)?;
    match run(tool, host, args, None, ToolKind::Planner, opts)? {
        Out::Program(p) => Ok(p),
        Out::Json(_) => Err(RunError::Engine("planner produced JSON".into())),
    }
}

fn expect_kind(tool: &Tool, expected: ToolKind) -> Result<(), RunError> {
    if tool.header.kind == expected {
        Ok(())
    } else {
        Err(RunError::WrongKind {
            expected,
            got: tool.header.kind,
        })
    }
}

/// `null` counts as `{}`. Every non-optional `@args` entry must be present and not null.
/// Types are not checked: `q()` coerces, and readers decide.
fn check_args(tool: &Tool, args: &serde_json::Value) -> Result<serde_json::Value, RunError> {
    let obj = match args {
        serde_json::Value::Null => serde_json::Map::new(),
        serde_json::Value::Object(m) => m.clone(),
        _ => return Err(RunError::Args("args must be a JSON object".into())),
    };
    for spec in tool.header.args.iter().filter(|s| !s.optional) {
        if obj.get(&spec.name).is_none_or(|v| v.is_null()) {
            return Err(RunError::Args(format!(
                "missing required arg `{}`",
                spec.name
            )));
        }
    }
    Ok(serde_json::Value::Object(obj))
}

enum Out {
    Json(serde_json::Value),
    Program(String),
}

/// Globals a tool may see; everything else is deleted by the prelude.
pub const ALLOWED_GLOBALS: &[&str] = &[
    "globalThis",
    "undefined",
    "NaN",
    "Infinity",
    "Object",
    "Function",
    "Array",
    "Number",
    "Boolean",
    "String",
    "Symbol",
    "BigInt",
    "Math",
    "JSON",
    "Date",
    "RegExp",
    "Map",
    "Set",
    "WeakMap",
    "WeakSet",
    "Promise",
    "Reflect",
    "Iterator",
    "Error",
    "EvalError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "TypeError",
    "URIError",
    "AggregateError",
    "InternalError",
    "parseInt",
    "parseFloat",
    "isNaN",
    "isFinite",
    "encodeURIComponent",
    "decodeURIComponent",
    "encodeURI",
    "decodeURI",
];

/// Runs once per context before the tool loads. Takes `q` and the allowlist.
const PRELUDE: &str = r#"(function (q, keep) {
  "use strict";
  const g = globalThis;
  const allow = new Set(keep);
  for (const k of Reflect.ownKeys(g)) {
    if (typeof k !== "string" || allow.has(k)) continue;
    let gone = false;
    try { gone = delete g[k]; } catch (_) {}
    if (!gone || Reflect.getOwnPropertyDescriptor(g, k) !== undefined) {
      throw new TypeError("cannot remove global `" + k + "`");
    }
  }
  const deny = function () {
    throw new EvalError("code generation from strings is disabled in tools");
  };
  const protos = [
    Function.prototype,
    Object.getPrototypeOf(function* () {}),
    Object.getPrototypeOf(async function () {}),
    Object.getPrototypeOf(async function* () {}),
  ];
  for (const p of protos) {
    Object.defineProperty(p, "constructor",
      { value: deny, writable: false, enumerable: false, configurable: false });
  }
  Object.defineProperty(deny, "prototype", { value: Function.prototype });
  Object.defineProperty(g, "Function",
    { value: deny, writable: false, enumerable: false, configurable: false });
  Object.defineProperty(g, "q",
    { value: q, writable: false, enumerable: false, configurable: false });
})"#;

type Intrinsics = (
    intrinsic::Date,
    intrinsic::Eval,
    intrinsic::RegExpCompiler,
    intrinsic::RegExp,
    intrinsic::Json,
    intrinsic::MapSet,
    intrinsic::Promise,
);

#[derive(Clone, Copy)]
struct Hosts {
    page: &'static dyn PageHost,
    api: Option<&'static dyn ApiFetch>,
}

/// Where the JS host functions find the caller's borrowed host objects.
type HostSlot = Rc<Cell<Option<Hosts>>>;

/// Empties the slot when the run ends, so a host function that somehow outlived the run
/// would see `None` and throw instead of touching a dead borrow.
struct SlotGuard(HostSlot);

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.0.set(None);
    }
}

/// rquickjs host closures must be `'js`, which a caller's `&'a dyn PageHost` cannot prove.
///
/// # Safety
/// The returned [`Hosts`] must not be dereferenced after `'a` ends. `run` upholds this; see
/// the SAFETY note at its call site.
unsafe fn erase<'a>(page: &'a dyn PageHost, api: Option<&'a dyn ApiFetch>) -> Hosts {
    Hosts {
        page: std::mem::transmute::<&'a dyn PageHost, &'static dyn PageHost>(page),
        api: api.map(|a| std::mem::transmute::<&'a dyn ApiFetch, &'static dyn ApiFetch>(a)),
    }
}

/// The run's wall clock, shared with host functions that block (`api`).
#[derive(Clone)]
struct Clock {
    deadline: Instant,
    /// Set by the interrupt handler, or by a host call that came back after `deadline`.
    fired: Rc<Cell<bool>>,
}

/// The engine's allocator: Rust's global allocator with a byte cap. A refused allocation
/// sets `hit`, which is how the runner tells an engine out-of-memory from a tool that throws
/// an error whose text says "out of memory".
struct CappedAlloc {
    used: usize,
    limit: usize,
    hit: Rc<Cell<bool>>,
}

impl CappedAlloc {
    fn admit(&mut self, extra: usize) -> bool {
        if self.used.saturating_add(extra) > self.limit {
            self.hit.set(true);
            false
        } else {
            true
        }
    }
}

// SAFETY: every pointer comes from `RustAllocator` and goes back to it, so its contract holds
// unchanged; this wrapper only refuses (returns null, which QuickJS handles as out of memory)
// and keeps a byte count.
unsafe impl Allocator for CappedAlloc {
    fn alloc(&mut self, size: usize) -> *mut u8 {
        if !self.admit(size) {
            return std::ptr::null_mut();
        }
        let p = RustAllocator.alloc(size);
        if !p.is_null() {
            // SAFETY: `p` was just returned by `RustAllocator`.
            self.used += unsafe { RustAllocator::usable_size(p) };
        }
        p
    }

    fn calloc(&mut self, count: usize, size: usize) -> *mut u8 {
        let Some(n) = count.checked_mul(size) else {
            return std::ptr::null_mut();
        };
        if !self.admit(n) {
            return std::ptr::null_mut();
        }
        let p = RustAllocator.calloc(count, size);
        if !p.is_null() {
            // SAFETY: `p` was just returned by `RustAllocator`.
            self.used += unsafe { RustAllocator::usable_size(p) };
        }
        p
    }

    unsafe fn dealloc(&mut self, ptr: *mut u8) {
        if ptr.is_null() {
            return;
        }
        self.used = self.used.saturating_sub(RustAllocator::usable_size(ptr));
        RustAllocator.dealloc(ptr);
    }

    unsafe fn realloc(&mut self, ptr: *mut u8, new_size: usize) -> *mut u8 {
        if ptr.is_null() {
            return self.alloc(new_size);
        }
        let old = RustAllocator::usable_size(ptr);
        if new_size > old && !self.admit(new_size - old) {
            return std::ptr::null_mut();
        }
        let p = RustAllocator.realloc(ptr, new_size);
        if !p.is_null() {
            self.used = self.used.saturating_sub(old) + RustAllocator::usable_size(p);
        }
        p
    }

    unsafe fn usable_size(ptr: *mut u8) -> usize {
        RustAllocator::usable_size(ptr)
    }
}

fn engine(e: JsError) -> RunError {
    RunError::Engine(e.to_string())
}

fn run(
    tool: &Tool,
    host: &dyn PageHost,
    args: &serde_json::Value,
    api: Option<&dyn ApiFetch>,
    want: ToolKind,
    opts: &RunOpts,
) -> Result<Out, RunError> {
    let args = check_args(tool, args)?;
    let oom = Rc::new(Cell::new(false));
    let rt = Runtime::new_with_alloc(CappedAlloc {
        used: 0,
        limit: opts.memory_limit,
        hit: oom.clone(),
    })
    .map_err(engine)?;
    rt.set_max_stack_size(opts.max_stack);
    let clock = Clock {
        deadline: Instant::now() + opts.timeout,
        fired: Rc::new(Cell::new(false)),
    };
    {
        let clock = clock.clone();
        rt.set_interrupt_handler(Some(Box::new(move || {
            if Instant::now() >= clock.deadline {
                clock.fired.set(true);
                true
            } else {
                false
            }
        })));
    }
    let ctx = Context::custom::<Intrinsics>(&rt).map_err(engine)?;
    // SAFETY: `erase` hands out `'static` views of `host` and `api`, which outlive this call.
    // They are stored only in `slot`, whose clones live in host closures owned by `ctx`/`rt`.
    // JS (and so every host closure) runs only inside `ctx.with` below, while the borrows are
    // live. `_guard` is declared after `rt` and `ctx`, so it drops first and empties the slot
    // before `ctx`/`rt` free the closures (which hold only `Rc`s); a panic in a host call
    // unwinds through the guard too. Leftover promise jobs are freed without running,
    // FinalizationRegistry/WeakRef are not reachable from tool code, and no JS value leaves
    // this function (results become serde_json / String inside `run_in`).
    let slot: HostSlot = Rc::new(Cell::new(Some(unsafe { erase(host, api) })));
    let _guard = SlotGuard(slot.clone());
    let res = ctx.with(|ctx| run_in(ctx, tool, &args, &slot, &clock, want, opts));
    if clock.fired.get() {
        // The wall cap wins even over `Ok`: a host call may have overrun it and the tool
        // swallowed the error.
        return Err(RunError::Timeout(opts.timeout));
    }
    match res {
        Err(_) if oom.get() => Err(RunError::OutOfMemory),
        r => r,
    }
}

fn run_in<'js>(
    ctx: Ctx<'js>,
    tool: &Tool,
    args: &serde_json::Value,
    slot: &HostSlot,
    clock: &Clock,
    want: ToolKind,
    opts: &RunOpts,
) -> Result<Out, RunError> {
    let has_api = slot.get().is_some_and(|h| h.api.is_some());
    install_prelude(&ctx)?;
    let page = page_object(&ctx, slot, clock, has_api)?;
    let args_js = to_js(&ctx, args)?;

    let name = format!("{}.js", tool.namespaced);
    let module = Module::declare(ctx.clone(), name, tool.source.as_bytes())
        .map_err(|e| load_err(&ctx, e))?;
    let (module, done) = module.eval().map_err(|e| load_err(&ctx, e))?;
    done.finish::<()>().map_err(|e| load_err(&ctx, e))?;
    let default: Value = module.get("default").map_err(|e| load_err(&ctx, e))?;
    let func = default
        .into_function()
        .ok_or_else(|| RunError::Load("no `export default function (page, args)`".into()))?;

    let mut ret: Value = func.call((page, args_js)).map_err(|e| threw(&ctx, e))?;
    if let Some(p) = ret.as_promise().cloned() {
        ret = p.finish::<Value>().map_err(|e| match e {
            JsError::WouldBlock => {
                RunError::BadReturn("returned a promise that never settles".into())
            }
            e => threw(&ctx, e),
        })?;
    }

    let fits = |got: usize| {
        if got > opts.max_output {
            Err(RunError::OutputTooLarge {
                max: opts.max_output,
                got,
            })
        } else {
            Ok(())
        }
    };
    match want {
        ToolKind::Planner => match ret.as_string() {
            Some(s) => {
                let program = s.to_string().map_err(engine)?;
                fits(program.len())?;
                Ok(Out::Program(program))
            }
            None => Err(RunError::BadReturn(format!(
                "a planner must return an act program string, got {}",
                ret.type_name()
            ))),
        },
        ToolKind::Reader => {
            let type_name = ret.type_name();
            let json = ctx
                .json_stringify(ret)
                .map_err(|e| js_err(&ctx, e, RunError::BadReturn))?
                .ok_or_else(|| {
                    RunError::BadReturn(format!("a reader must return JSON, got {type_name}"))
                })?
                .to_string()
                .map_err(engine)?;
            fits(json.len())?;
            serde_json::from_str(&json)
                .map(Out::Json)
                .map_err(|e| RunError::BadReturn(e.to_string()))
        }
    }
}

/// Installs `q` and prunes the globals. A global the prelude cannot remove is a load error:
/// the tool must not run in a context wider than [`ALLOWED_GLOBALS`].
fn install_prelude<'js>(ctx: &Ctx<'js>) -> Result<(), RunError> {
    let q = Function::new(ctx.clone(), |ctx: Ctx<'js>, v: Opt<Value<'js>>| {
        let v = v.0.unwrap_or_else(|| Value::new_undefined(ctx.clone()));
        if v.is_undefined() {
            return Err(Exception::throw_type(&ctx, "q: value is undefined"));
        }
        if v.is_null() {
            return Err(Exception::throw_type(&ctx, "q: value is null"));
        }
        let s: Coerced<String> = v.get()?;
        Ok(sb_act::quote(&s.0))
    })
    .map_err(engine)?;
    let prelude: Function = ctx.eval(PRELUDE).map_err(|e| engine_js(ctx, e))?;
    let keep: Vec<String> = ALLOWED_GLOBALS.iter().map(|s| s.to_string()).collect();
    prelude
        .call::<_, ()>((q, keep))
        .map_err(|e| RunError::Load(format!("sandbox prelude: {}", describe(ctx, e))))
}

fn page_object<'js>(
    ctx: &Ctx<'js>,
    slot: &HostSlot,
    clock: &Clock,
    has_api: bool,
) -> Result<Object<'js>, RunError> {
    fn hosts(ctx: &Ctx<'_>, slot: &HostSlot) -> rquickjs::Result<Hosts> {
        slot.get()
            .ok_or_else(|| Exception::throw_internal(ctx, "page is no longer available"))
    }
    let (url, title) = {
        let h = slot
            .get()
            .ok_or_else(|| RunError::Engine("no page host".into()))?;
        (h.page.url(), h.page.title())
    };
    let page = Object::new(ctx.clone()).map_err(engine)?;

    let s = slot.clone();
    let find = Function::new(ctx.clone(), move |ctx: Ctx<'js>, q: Coerced<String>| {
        let h = hosts(&ctx, &s)?;
        h.page
            .find(&q.0)
            .map_err(|m| Exception::throw_message(&ctx, &m))
    })
    .map_err(engine)?;

    let s = slot.clone();
    let find_all = Function::new(ctx.clone(), move |ctx: Ctx<'js>, q: Coerced<String>| {
        let h = hosts(&ctx, &s)?;
        Ok::<_, JsError>(h.page.find_all(&q.0))
    })
    .map_err(engine)?;

    let s = slot.clone();
    let text = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, region: Opt<Coerced<String>>| {
            let h = hosts(&ctx, &s)?;
            let region = region.0.map(|r| r.0).filter(|r| !r.is_empty());
            Ok::<_, JsError>(h.page.text(region.as_deref()))
        },
    )
    .map_err(engine)?;

    let s = slot.clone();
    let table = Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: Coerced<String>| {
        let h = hosts(&ctx, &s)?;
        let v = h.page.table(&id.0);
        ctx.json_parse(v.to_string())
    })
    .map_err(engine)?;

    page.set("find", find).map_err(engine)?;
    page.set("findAll", find_all).map_err(engine)?;
    page.set("text", text).map_err(engine)?;
    page.set("table", table).map_err(engine)?;
    page.set("url", url).map_err(engine)?;
    page.set("title", title).map_err(engine)?;

    if has_api {
        let s = slot.clone();
        let clock = clock.clone();
        let api = Function::new(ctx.clone(), move |ctx: Ctx<'js>, url: Coerced<String>| {
            let h = hosts(&ctx, &s)?;
            let fetch = h
                .api
                .ok_or_else(|| Exception::throw_internal(&ctx, "api is not available"))?;
            let overrun = |ctx: &Ctx<'js>| {
                clock.fired.set(true);
                Exception::throw_internal(ctx, "api: the run's wall cap is spent")
            };
            let remaining = clock.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(overrun(&ctx));
            }
            let res = fetch.api(&url.0, remaining);
            if Instant::now() >= clock.deadline {
                return Err(overrun(&ctx));
            }
            match res {
                Ok(v) => ctx.json_parse(v.to_string()),
                Err(m) => Err(Exception::throw_message(&ctx, &m)),
            }
        })
        .map_err(engine)?;
        page.set("api", api).map_err(engine)?;
    }

    let freeze: Function = ctx
        .globals()
        .get::<_, Object>("Object")
        .and_then(|o| o.get("freeze"))
        .map_err(engine)?;
    freeze
        .call::<_, ()>((page.clone(),))
        .map_err(|e| engine_js(ctx, e))?;
    Ok(page)
}

fn to_js<'js>(ctx: &Ctx<'js>, v: &serde_json::Value) -> Result<Value<'js>, RunError> {
    ctx.json_parse(v.to_string()).map_err(|e| engine_js(ctx, e))
}

/// Longest error text handed back (errors reach the model; keep them short).
const MAX_MSG: usize = 400;

/// Turns a JS error into text: `Name: message (file:line:col)`, or the thrown value.
fn describe(ctx: &Ctx<'_>, e: JsError) -> String {
    let msg = match e {
        JsError::Exception => {
            let v = ctx.catch();
            if let Some(ex) = v.as_exception() {
                let name: String = ex.get("name").unwrap_or_else(|_| "Error".into());
                let mut s = format!("{name}: {}", ex.message().unwrap_or_default());
                if let Some(at) = ex.stack().as_deref().and_then(first_frame) {
                    s.push_str(&format!(" ({at})"));
                }
                s
            } else {
                match <Coerced<String> as rquickjs::FromJs>::from_js(ctx, v) {
                    Ok(s) => format!("uncaught {}", s.0),
                    Err(_) => "uncaught non-error value".into(),
                }
            }
        }
        e => e.to_string(),
    };
    truncate(msg)
}

/// `    at f (luma.register.js:3:9)` -> `luma.register.js:3:9`.
fn first_frame(stack: &str) -> Option<String> {
    let line = stack
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("at "))?;
    let inner = line
        .rsplit_once('(')
        .map(|(_, r)| r.trim_end_matches(')'))
        .unwrap_or(&line[3..]);
    Some(inner.to_string())
}

fn truncate(mut s: String) -> String {
    if s.len() > MAX_MSG {
        let mut cut = MAX_MSG;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("...");
    }
    s
}

/// rquickjs's own allocation failure is out of memory; anything else is wrapped. An engine
/// out-of-memory exception is recognised in `run` by the allocator's flag, never by its text,
/// so a tool that throws "out of memory" stays [`RunError::Threw`].
fn js_err(ctx: &Ctx<'_>, e: JsError, wrap: fn(String) -> RunError) -> RunError {
    if matches!(e, JsError::Allocation) {
        return RunError::OutOfMemory;
    }
    wrap(describe(ctx, e))
}

fn load_err(ctx: &Ctx<'_>, e: JsError) -> RunError {
    js_err(ctx, e, RunError::Load)
}

fn threw(ctx: &Ctx<'_>, e: JsError) -> RunError {
    js_err(ctx, e, |msg| RunError::Threw { msg })
}

fn engine_js(ctx: &Ctx<'_>, e: JsError) -> RunError {
    RunError::Engine(describe(ctx, e))
}

/// Default [`PageHost`] over an index and its text blocks (live or fetched), through the
/// one scorer in sb-sheet (D-51).
#[derive(Debug, Clone, Copy)]
pub struct SheetPage<'a> {
    pub index: &'a [IndexRow],
    pub blocks: &'a [TextBlock],
    pub url: &'a str,
    pub title: &'a str,
}

/// Hits per query for `findAll`.
pub const FIND_ALL_K: usize = 50;

impl<'a> SheetPage<'a> {
    pub fn new(
        index: &'a [IndexRow],
        blocks: &'a [TextBlock],
        url: &'a str,
        title: &'a str,
    ) -> SheetPage<'a> {
        SheetPage {
            index,
            blocks,
            url,
            title,
        }
    }

    fn hits(&self, q: &str, k: usize) -> Vec<sb_sheet::FindHit> {
        let opts = FindOpts {
            query: q.to_string(),
            k,
        };
        sb_sheet::find(self.index, self.blocks, &opts)
            .into_iter()
            .filter(|h| matches!(h.reason, HitReason::Match { .. }))
            .collect()
    }
}

/// `#12` -> `12`, `#f2.15` -> `f2.15`.
fn bare(r: &sb_sheet::ElemRef) -> String {
    r.to_string().trim_start_matches('#').to_string()
}

impl PageHost for SheetPage<'_> {
    fn find(&self, q: &str) -> Result<String, String> {
        match self.hits(q, 1).into_iter().next() {
            None => Err(format!("find({q:?}): no match")),
            Some(h) if h.weak => Err(format!(
                "find({q:?}): only a weak match ({}); refine the query",
                bare(&h.elem)
            )),
            Some(h) => Ok(bare(&h.elem)),
        }
    }

    fn find_all(&self, q: &str) -> Vec<String> {
        self.hits(q, FIND_ALL_K)
            .iter()
            .filter(|h| !h.weak)
            .map(|h| bare(&h.elem))
            .collect()
    }

    fn text(&self, region: Option<&str>) -> String {
        let opts = ReadOpts {
            region: region.map(str::to_string),
            cursor: None,
            max_tokens: None,
            chunk_tokens: 4000,
        };
        sb_sheet::read(self.index, self.blocks, &opts).markdown
    }

    fn table(&self, id: &str) -> serde_json::Value {
        table_json(self.blocks, id)
    }

    fn url(&self) -> String {
        self.url.to_string()
    }

    fn title(&self) -> String {
        self.title.to_string()
    }
}

/// `3` -> (frame 0, table 3); `f2.3` -> (frame 2, table 3); a leading `#` is ignored.
fn parse_table_id(id: &str) -> Option<(sb_sheet::FrameId, u32)> {
    let id = id.trim().trim_start_matches('#');
    match id.strip_prefix('f') {
        Some(rest) => {
            let (f, t) = rest.split_once('.')?;
            Some((f.parse().ok()?, t.parse().ok()?))
        }
        None => Some((0, id.parse().ok()?)),
    }
}

/// `{"header": [..], "rows": [[..]]}`, plus `"refs": [[ref|null]]` (refs without `#`, one per
/// cell) when any cell links an element. `null` when the table has no rows.
pub fn table_json(blocks: &[TextBlock], id: &str) -> serde_json::Value {
    use serde_json::{json, Value as J};
    let Some((frame, table)) = parse_table_id(id) else {
        return J::Null;
    };
    let mut rows: Vec<&TextBlock> = blocks
        .iter()
        .filter(|b| b.kind == BlockKind::TableRow && b.frame == frame && b.table == Some(table))
        .collect();
    if rows.is_empty() {
        return J::Null;
    }
    rows.sort_by_key(|b| b.order);
    let cells = |b: &TextBlock| -> Vec<String> {
        b.spans.iter().map(|s| s.text.trim().to_string()).collect()
    };
    let header: Vec<String> = rows
        .iter()
        .find(|b| b.header)
        .map(|b| cells(b))
        .unwrap_or_default();
    let body: Vec<&&TextBlock> = rows.iter().filter(|b| !b.header).collect();
    let data: Vec<Vec<String>> = body.iter().map(|b| cells(b)).collect();
    let mut out = json!({ "header": header, "rows": data });
    if body
        .iter()
        .any(|b| b.spans.iter().any(|s| s.elem.is_some()))
    {
        let refs: Vec<Vec<Option<String>>> = body
            .iter()
            .map(|b| b.spans.iter().map(|s| s.elem.as_ref().map(bare)).collect())
            .collect();
        out["refs"] = json!(refs);
    }
    out
}

#[cfg(test)]
mod tests;
