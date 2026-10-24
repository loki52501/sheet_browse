# Review: T-11 tool runner (commit e5305f7)

Scope: `git show e5305f7`. Files: crates/toolstore/src/runner.rs, crates/toolstore/src/runner/tests.rs, plus the runner lines in crates/toolstore/src/lib.rs and crates/toolstore/Cargo.toml. Claimed ids: T-2, T-9, D-53, D-305 (q() per D-52). Interfaces: I-7, I-12 (draft).

I checked it in a detached throwaway worktree at e5305f7:
- `cargo test -p toolstore runner`: 17 passed, exit 0.
- `cargo clippy -p toolstore --all-targets -- -D warnings`: clean.
- `cargo fmt -p toolstore --check`: clean.

I also ran an out-of-tree probe crate against the public API, with a 300 ms cap and a 24 MiB heap:
- `Reflect.ownKeys(globalThis)` is exactly ALLOWED_GLOBALS + `q` (+ `Symbol.toStringTag`).
- FinalizationRegistry, WeakRef, Proxy, ArrayBuffer, Atomics, SharedArrayBuffer, structuredClone, escape, print and console are all undefined.
- A bound `Function.prototype.constructor` still throws.
- `import('./other.js')` gives "could not load module".
- `Array.prototype.indexOf/includes/lastIndexOf.call({length: 2**53-1})` and the ReDoS `/(a+)+$/` both return Timeout at about 300 ms, so quickjs-ng polls interrupts inside these C loops.
- Re-entrant `page.find({toString(){return page.find(o)}})` throws RangeError and does not crash the native stack.
- `'ab'.repeat(2**28)` gives OutOfMemory.

## Blocking

- none

## Should fix

1. runner.rs:287 `fn erase<'a>(..) -> Hosts` is a safe fn that turns `'a` borrows into `'static` ones, which is an unsound signature even though it is private. Make it `unsafe fn` and move the SAFETY argument to the single call site (runner.rs:330), so the invariant is checked where the guard lives. The erasure itself is sound as written:
   - `_guard` is declared after `rt`/`ctx`, so it drops first and clears the slot before ctx/rt free the closures, which only hold `Rc`s.
   - Leftover promise jobs are freed without running.
   - FinalizationRegistry/WeakRef are unreachable.
   - A host panic unwinds through the guard.
   - No JS value leaves `run`.
2. The wall cap is not enforced across host calls (runner.rs:53-55, 474; B-323). `ApiFetch::api(&self, url)` gets no deadline, so a hung fetch hangs the run forever, and the interrupt handler only fires once JS resumes. Pass the remaining budget (`deadline: Instant` or `Duration`) to `ApiFetch::api`, or at least state on the trait that implementations must bound their own time. This needs an interface_changes entry.
3. Reader output has no size cap (runner.rs:382-395). The probe returned a 5,000,000-char string as Ok. Readers are meant to return short text (CLAUDE.md rule 5, and the efficiency/token priority), and `api()` JSON from a site can carry HTML strings straight through. Add `RunOpts.max_output` (bytes of serialized JSON) and return `BadReturn` when it is exceeded.
4. The typed error is wrong in two places:
   - (a) Running out of memory inside `json_stringify` comes back as `BadReturn("InternalError: out of memory")` and not `OutOfMemory`, because runner.rs:386 calls `describe` directly instead of `js_err`. The probe showed this.
   - (b) runner.rs:554 matches `msg.contains("out of memory")`, so a tool can `throw new Error("out of memory")` and get `OutOfMemory`. The probe showed this too. Match on `JsError::Allocation` or on an `InternalError` name only.
5. Spec and record drift:
   - D-305 says `Function` and `eval` are "absent". The code loads the Eval intrinsic, which Rust needs to compile modules, and keeps a throwing `Function` global so that `instanceof Function` works. That is reasonable, but write it down as a D-305 amendment or a blocker.
   - The new public API is not recorded under `interface_changes` for I-7/I-12. That covers `run_reader[_with]`, `run_planner[_with]`, `PageHost`, `ApiFetch`, `SheetPage`, `RunOpts`, `RunError`, `table_json`, `ALLOWED_GLOBALS`, `FIND_ALL_K`, and the `table()` shape from B-320. srv-web and sb-mcp will consume it. The handoff leaves this to the sub-orchestrator, but it has to land before those areas build on it.
6. The globals test checks a deny-list, not the allowlist (tests.rs:153-186). The prelude swallows failed deletes (runner.rs:235), so a future non-configurable global would survive without anyone noticing. Assert that `Reflect.ownKeys(globalThis)` (string keys) equals `ALLOWED_GLOBALS ∪ {q}`. The probe shows that holds today.

## Notes

- Nit: `q(undefined)` returns `"\"undefined\""` (probe). A planner that quotes a missing optional arg would type the word "undefined". Consider having `q` throw a TypeError on undefined/null. D-52 does not say either way.
- Nit: tests.rs:234 (dynamic import) accepts any `Threw`. Assert on "could not load module". tests.rs:427 (stack) should assert RangeError.
- Nit: tests.rs:132-151 counts steps with a local splitter because `sb_act::parse` is still `todo!` (T-19). Switch the injection test to `sb_act::parse` when it lands. q() parity with `sb_act::quote` is exact today: same function, and tested at tests.rs:300.
- Nit: Cargo.toml has `rquickjs = "0.14"` inline. Every other dependency comes from `[workspace.dependencies]`.
- `SheetPage::find/find_all/text` call sb-sheet stubs (`todo!`, T-10). A panic there resumes out of `run_reader`, past the guard, and is safe, but callers will see a panic and not a `RunError` until T-10 lands.
- The commit claims T-9, but only invariant 2 (`page` has no document/window/fetch) is in the runner's scope, and it is tested at tests.rs:164-176. The planner class max, the repair checks and the loader are covered elsewhere. q() implements D-52, which the message does not list.
- Planners never get `api`: runner.rs:143 passes None, tested at tests.rs:319. Phase-2 readers never get it: runner.rs:119 has `phase == 1`, tested at tests.rs:331-340. Prototype pollution does not survive across runs because each run gets a fresh Runtime (tests.rs:475). The module loader cannot resolve other files. `import('x.t.js')` resolves to the tool's own module, which is harmless.
- quickjs-ng exposes `Error.prepareStackTrace` and `Error.captureStackTrace`. Tool JS can run inside `describe()` through those or through `name`/`message` getters, but the absolute deadline still bounds it and the slot is still live. Sound.
- Hard rules: no keys or model SDKs, no vault values reachable, no captcha code, no fixed sleeps, and no raw HTML from the page API. The runner itself has no side effect; `api` goes through the caller's gated `ApiFetch`.
- Gate note: merge_gate.py reads `bench/results/review-T-11.md`. This file is runner-scoped as the brief asked, so the sub-orchestrator has to fold it into the task review.

## Verdict

Verdict: approve
