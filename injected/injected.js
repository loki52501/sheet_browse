// injected.js: I-11 in-page API on globalThis.__sb, installed in isolated world "sb" in every
// frame (docs/05 B-4 #3, B-5). STUB from wave 0: the function names and return shapes are the
// contract with sb-driver; bodies are area page's (T-19).
//
// Return shapes (JSON):
//   walk({base, dirty}) -> {count}      ids start at `base` (sb-driver passes one per document
//                                       generation), so an id is never reused in a tab frame
//   index()            -> IndexRow[]   exactly the serde shape of sb_sheet::IndexRow (I-5, D-37);
//                                      see crates/sb-sheet/src/index.rs test `index_row_wire_shape`.
//                                      JS emits frame 0 and frame-local ids; sb-driver rewrites
//                                      frame, ElemRefs and child_frame when it merges frames.
//   text(region)       -> TextBlock[]  sb_sheet::TextBlock, frame-local `order`
//   resolve(id)        -> element | {why: "gone" | "stale"}  ("stale": id below this document's base)
//   act(id)            -> {x, y} | {why}
//   settle(opts)       -> Promise<{settled, changed}>
//   fingerprint(spec)  -> "match" | {partial: [missing...]} | "miss"
//   frames()           -> [{frame, parent, url, opaque}]
// skeleton / find / sheet are NOT in-page (D-51): sb-sheet scores the shipped index in Rust.
// Never touch window globals the site can see; never read a password value; never read
// inside a CHALLENGE frame.
(() => {
  "use strict";
  // addScriptToEvaluateOnNewDocument plus an evaluate into already-loaded documents can run
  // this twice in one world.
  if (globalThis.__sb) return;
  const todo = (name) => () => {
    throw new Error("__sb." + name + " not implemented (T-19)");
  };
  const api = {};
  for (const name of [
    "walk", "id", "resolve", "name", "role", "state", "index", "text", "act", "settle",
    "fingerprint", "frames",
  ]) {
    api[name] = todo(name);
  }
  Object.defineProperty(globalThis, "__sb", { value: Object.freeze(api), configurable: false });
})();
