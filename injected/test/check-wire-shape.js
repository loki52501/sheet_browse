// check-wire-shape.js: strict checker for the I-5 IndexRow wire shape that __sb.index() must
// emit (crates/sb-sheet/src/index.rs, test `index_row_wire_shape`). Stricter than serde (which
// ignores unknown keys): every key must be a field of IndexRow / State / Flags / Rect /
// FieldValue, snake_case, with the serde type. Added by the T-19 acceptance tester.
//
//   node injected/test/check-wire-shape.js <rows.json> [...]   exit 0 = ok, 1 = problems listed
//   require("./check-wire-shape").check(rows) -> [problem strings]
"use strict";

const fs = require("node:fs");

const U32 = (v) => Number.isInteger(v) && v >= 0 && v <= 0xffffffff;
const STR = (v) => typeof v === "string";
const BOOL = (v) => typeof v === "boolean";
const F32 = (v) => typeof v === "number" && Number.isFinite(v);
const LANDMARKS = new Set(["main", "nav", "header", "footer", "aside", "form", "region", "dialog"]);
const SNAKE = /^[a-z][a-z0-9_]*$/;

const ROW = {
  id: { req: true, ok: U32, t: "u32" },
  frame: { req: true, ok: U32, t: "u32" },
  role: { req: true, ok: STR, t: "string" },
  name: { req: true, ok: STR, t: "string" },
  nearby: { ok: (v) => v === null || STR(v), t: "Option<String>" },
  heading: { ok: (v) => v === null || STR(v), t: "Option<String>" },
  state: { ok: (v) => v && typeof v === "object" && !Array.isArray(v), t: "State" },
  flags: { ok: (v) => v && typeof v === "object" && !Array.isArray(v), t: "Flags" },
  form: { ok: (v) => v === null || STR(v), t: "Option<String>" },
  landmark: { ok: (v) => v === null || (STR(v) && LANDMARKS.has(v)), t: "Option<landmark vocab>" },
  child_frame: { ok: (v) => v === null || U32(v), t: "Option<u32>" },
  box: { ok: (v) => v === null || (v && typeof v === "object"), t: "Option<Rect>" },
};
const STATE = {
  visible: BOOL, enabled: BOOL, submitish: BOOL, opens_new_tab: BOOL, covered: BOOL,
  checked: (v) => v === null || BOOL(v),
  value: (v) => v === null || (v && typeof v === "object"),
  options: (v) => Array.isArray(v) && v.every(STR),
  input_type: STR,
  autocomplete: STR,
};
const FLAGS = ["submit", "challenge", "overlay", "opaque"];

function check(rows) {
  const p = [];
  if (!Array.isArray(rows)) return ["index() is not an array"];
  const ids = new Set();
  rows.forEach((r, i) => {
    const at = `row[${i}]${r && r.name !== undefined ? " " + JSON.stringify(r.name) : ""}`;
    if (!r || typeof r !== "object" || Array.isArray(r)) {
      p.push(`${at}: not an object`);
      return;
    }
    for (const k of Object.keys(r)) {
      if (!SNAKE.test(k)) p.push(`${at}: key ${k} is not snake_case`);
      if (!ROW[k]) p.push(`${at}: unknown key ${k}`);
      else if (!ROW[k].ok(r[k])) p.push(`${at}: ${k} is not ${ROW[k].t}: ${JSON.stringify(r[k])}`);
    }
    for (const [k, d] of Object.entries(ROW)) if (d.req && !(k in r)) p.push(`${at}: missing ${k}`);
    if (ids.has(r.id)) p.push(`${at}: duplicate id ${r.id}`);
    ids.add(r.id);
    if (r.state && typeof r.state === "object") {
      for (const [k, v] of Object.entries(r.state)) {
        if (!SNAKE.test(k)) p.push(`${at}: state.${k} is not snake_case`);
        if (!STATE[k]) p.push(`${at}: unknown key state.${k}`);
        else if (!STATE[k](v)) p.push(`${at}: state.${k} has the wrong type: ${JSON.stringify(v)}`);
      }
      const fv = r.state.value;
      if (fv && typeof fv === "object") {
        const keys = Object.keys(fv).sort().join(",");
        if (fv.kind === "empty" || fv.kind === "filled") {
          if (keys !== "kind") p.push(`${at}: FieldValue ${fv.kind} must be {kind} only, got ${keys}`);
        } else if (fv.kind === "text") {
          if (keys !== "kind,text" || !STR(fv.text)) p.push(`${at}: FieldValue text must be {kind,text:string}`);
        } else {
          p.push(`${at}: FieldValue kind ${JSON.stringify(fv.kind)} not in empty|filled|text`);
        }
        if (r.state.input_type === "password" && fv.kind === "text") p.push(`${at}: password carries text`);
      }
    }
    if (r.flags && typeof r.flags === "object") {
      for (const [k, v] of Object.entries(r.flags)) {
        if (!FLAGS.includes(k)) p.push(`${at}: unknown flag ${k}`);
        else if (!BOOL(v)) p.push(`${at}: flags.${k} is not a boolean`);
      }
    }
    if (r.box && typeof r.box === "object") {
      const keys = Object.keys(r.box).sort().join(",");
      if (keys !== "h,w,x,y") p.push(`${at}: box keys ${keys}, want x,y,w,h`);
      for (const k of ["x", "y", "w", "h"]) if (!F32(r.box[k])) p.push(`${at}: box.${k} not a finite number`);
    }
  });
  return p;
}

module.exports = { check };

if (require.main === module) {
  let bad = 0;
  for (const f of process.argv.slice(2)) {
    const rows = JSON.parse(fs.readFileSync(f, "utf8"));
    const p = check(rows);
    if (p.length) {
      bad++;
      console.log(`FAIL ${f}: ${p.length} problem(s)`);
      for (const x of p) console.log("  " + x);
    } else {
      console.log(`ok ${f}: ${rows.length} rows match the I-5 shape`);
    }
  }
  process.exit(bad ? 1 : 0);
}
