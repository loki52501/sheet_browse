// Pure parts of injected.js under jsdom (D-401): install, identity, roles, name ladder,
// collapse, text blocks, password never read, fingerprint, CHALLENGE as one row.
"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { jsdom, page, SRC, byName, fill } = require("./helpers");

// jsdom objects come from another realm: compare their JSON.
const same = (a, b, m) => assert.deepStrictEqual(JSON.parse(JSON.stringify(a)), b, m);
const tick = () => new Promise((r) => setImmediate(r));

function fillShadow(win) {
  const host = win.document.getElementById("host");
  if (!host) return;
  const sr = host.attachShadow({ mode: "open" });
  sr.innerHTML = '<button type="button" data-expect="Shadow button">Shadow button</button>' +
    '<input aria-label="Shadow field" data-expect="Shadow field">';
}

test("install: frozen, non-configurable, idempotent, no other globals", () => {
  const { win, sb } = jsdom("<p>x</p>");
  const before = Object.keys(win);
  assert.ok(Object.isFrozen(sb));
  const d = Object.getOwnPropertyDescriptor(win, "__sb");
  assert.equal(d.configurable, false);
  assert.equal(d.writable, false);
  win.eval(SRC); // second install is a no-op
  assert.equal(win.__sb, sb);
  assert.throws(() => {
    "use strict";
    win.__sb = {};
  });
  for (const k of ["walk", "id", "resolve", "name", "role", "state", "index", "text", "act", "settle", "fingerprint", "frames"]) {
    assert.equal(typeof sb[k], "function", k);
  }
  for (const k of ["find", "skeleton", "sheet"]) assert.equal(sb[k], undefined, k + " is not in-page (D-51)");
  same(Object.keys(win), before);
});

test("register.html: rows, names, form ids, submit, hidden promo absent", () => {
  const { sb } = jsdom(page("register.html"));
  sb.walk({ base: 1 });
  const rows = fill(sb.index());
  const email = rows.filter((r) => r.name === "Email");
  assert.equal(email.length, 2);
  assert.equal(email[0].form, "f1");
  assert.equal(email[1].form, "f2");
  assert.equal(email[0].role, "textbox");
  assert.equal(email[0].state.input_type, "email");
  const pw = byName(rows, "Password");
  assert.equal(pw.state.input_type, "password");
  same(pw.state.value, { kind: "empty" });
  assert.equal(pw.state.autocomplete, "new-password");
  assert.equal(byName(rows, "Full name").role, "textbox");
  const plan = byName(rows, "Plan");
  assert.equal(plan.role, "combobox");
  same(plan.state.options, ["Basic", "Plus", "Pro"]);
  const agree = byName(rows, "I agree to the Terms of Service");
  assert.equal(agree.role, "checkbox");
  assert.equal(agree.state.checked, false);
  const reg = byName(rows, "Register");
  assert.equal(reg.role, "button");
  assert.ok(reg.flags.submit && reg.state.submitish);
  assert.equal(reg.form, "f1");
  assert.equal(reg.heading, "Create your account");
  assert.equal(reg.landmark, "main");
  // CHALLENGE container and its iframe are one row (D-55)
  const ch = rows.filter((r) => r.flags.challenge);
  assert.equal(ch.length, 1);
  assert.equal(ch[0].role, "iframe");
  assert.equal(ch[0].name, "reCAPTCHA");
  // hidden promo: no rows from the display:none subtree
  assert.ok(!rows.some((r) => /offer/i.test(r.name)));
  // ids in document order, starting at base
  assert.equal(rows[0].id, 1);
  for (let i = 1; i < rows.length; i++) assert.ok(rows[i].id > rows[i - 1].id);
  assert.equal(byName(rows, "Home").landmark, "nav");
  assert.equal(byName(rows, "Subscribe").heading, "Stay in the loop");
});

test("wire shape matches sb_sheet::IndexRow (index_row_wire_shape)", () => {
  const { sb } = jsdom(page("register.html"));
  const rows = sb.index();
  const allowed = new Set(["id", "frame", "role", "name", "nearby", "heading", "state", "flags", "form", "landmark", "child_frame", "box"]);
  const stateKeys = new Set(["visible", "enabled", "checked", "value", "submitish", "opens_new_tab", "options", "covered", "input_type", "autocomplete"]);
  const flagKeys = new Set(["submit", "challenge", "overlay", "opaque"]);
  for (const r of rows) {
    for (const k of Object.keys(r)) assert.ok(allowed.has(k), "row key " + k);
    assert.equal(typeof r.id, "number");
    assert.equal(r.frame, 0);
    assert.equal(typeof r.role, "string");
    assert.equal(r.role, r.role.toLowerCase());
    assert.equal(typeof r.name, "string");
    for (const k of Object.keys(r.state)) assert.ok(stateKeys.has(k), "state key " + k);
    for (const [k, v] of Object.entries(r.flags)) {
      assert.ok(flagKeys.has(k), "flag " + k);
      assert.equal(v, true);
    }
    if (r.state.value) assert.ok(["empty", "filled", "text"].includes(r.state.value.kind));
    if (r.state.value && r.state.value.kind === "text") assert.equal(typeof r.state.value.text, "string");
  }
});

test("name ladder: every rung on controls.html", () => {
  const { doc, sb } = jsdom(page("controls.html"), { before: fillShadow });
  sb.walk({ base: 1 });
  const rows = sb.index();
  const els = Array.from(doc.querySelectorAll("[data-expect]"));
  const shadow = doc.getElementById("host").shadowRoot;
  els.push(...shadow.querySelectorAll("[data-expect]"));
  for (const el of els) {
    const id = sb.id(el);
    const row = rows.find((r) => r.id === id);
    assert.ok(row, "row for " + el.outerHTML.slice(0, 60));
    assert.equal(row.name, el.getAttribute("data-expect"), el.outerHTML.slice(0, 80));
  }
});

test("roles: ARIA, tag, behaviour", () => {
  const { doc, sb } = jsdom(page("controls.html"), { before: fillShadow });
  const rows = sb.index();
  const role = (sel) => {
    const id = sb.id(doc.querySelector(sel));
    return rows.find((r) => r.id === id).role;
  };
  assert.equal(role('input[type="search"]'), "searchbox");
  assert.equal(role("#phone"), "textbox");
  assert.equal(role('input[type="number"]'), "spinbutton");
  assert.equal(role('input[type="checkbox"]'), "checkbox");
  assert.equal(role(".fancy"), "button");
  assert.equal(role('[role="button"]'), "button");
  assert.equal(role("select[multiple]"), "listbox");
  assert.equal(role("[contenteditable]"), "textbox");
  assert.equal(role("#card"), "link");
  assert.equal(role("tr.click"), "row");
  assert.equal(role("#news-h"), "heading");
  assert.equal(sb.role(doc.querySelector("main")), "main");
  assert.equal(sb.role(doc.querySelector("#container")), "");
});

test("collapse: interactive owns its subtree; container emits only its own text; table rows fold", () => {
  const { doc, sb } = jsdom(page("controls.html"), { before: fillShadow });
  const rows = sb.index();
  // the link owns its span and paragraph: no row and no text block for them
  assert.ok(!rows.some((r) => r.name === "Card title"));
  const blocks = sb.text();
  const texts = blocks.map((b) => b.spans.map((s) => s.text).join(""));
  assert.ok(!texts.includes("with a nested paragraph"));
  const card = blocks.find((b) => b.spans.some((s) => s.elem === "#" + sb.id(doc.getElementById("card"))));
  assert.ok(card, "card link is a linked span");
  // container: its own text nodes and its child block, each once, in order
  const i = texts.indexOf("Own text");
  assert.ok(i >= 0, texts.join(" / "));
  assert.equal(texts[i + 1], "Child paragraph");
  assert.equal(texts[i + 2], "tail text");
  // table rows: one TableRow per tr, a span per cell, header row flagged
  const trs = blocks.filter((b) => b.kind === "table_row");
  assert.equal(trs.length, 3);
  same(trs[0].spans.map((s) => s.text), ["Name", "Plan", "Seats"]);
  assert.equal(trs[0].header, true);
  same(trs[1].spans.map((s) => s.text), ["Ada", "Pro", "3"]);
  assert.equal(trs[1].header, undefined);
  assert.equal(trs[0].table, trs[2].table);
  // clickable row folds to a | b | c in the index
  assert.ok(byName(rows, "Bob | Basic | 1", "row"));
  // heading that holds a link: heading row and link row both exist
  assert.ok(byName(rows, "Latest news", "heading"));
  assert.ok(byName(rows, "Latest news", "link"));
  // list nesting depth
  const nested = blocks.find((b) => b.spans.map((s) => s.text).join("") === "Nested point");
  assert.equal(nested.kind, "list_item");
  assert.equal(nested.level, 2);
});

test("consumed label text is not emitted again as text", () => {
  const { sb } = jsdom(page("register.html"));
  const texts = sb.text().map((b) => b.spans.map((s) => s.text).join(""));
  assert.ok(!texts.some((t) => /I agree/.test(t)), texts.join(" / "));
  assert.ok(!texts.includes("Email"));
  assert.ok(!texts.includes("Plan"));
  assert.ok(texts.includes("Start your free trial. No card required."));
  assert.ok(!texts.some((t) => /50% off/.test(t)), "hidden promo text absent");
});

test("text(region): landmark, form id, heading", () => {
  const { sb } = jsdom(page("register.html"));
  const footer = sb.text("footer");
  assert.ok(footer.length > 0 && footer.every((b) => b.landmark === "footer"));
  const f2 = sb.text("f2");
  assert.equal(f2.length, 1);
  assert.equal(f2[0].spans[0].text, "Subscribe");
  const h = sb.text("Stay in the loop");
  assert.ok(h.some((b) => b.kind === "heading"));
  assert.ok(h.every((b) => b.kind === "heading" || b.heading === "Stay in the loop"));
  for (const b of sb.text()) assert.equal(b.frame, 0);
});

test("identity: same ids across 3 walks with unrelated DOM changes; removed -> gone; below base -> stale", async () => {
  const { doc, sb } = jsdom(page("register.html"));
  sb.walk({ base: 100 });
  const first = sb.index();
  assert.equal(first[0].id, 100);
  const key = (r) => r.role + ":" + r.name + ":" + (r.form || "");
  const ids = new Map(first.map((r) => [key(r), r.id]));
  // walk 2: unrelated insertions before and after the form
  const p = doc.createElement("p");
  p.textContent = "A new paragraph";
  doc.querySelector("main").insertBefore(p, doc.querySelector("main").firstChild);
  const b = doc.createElement("button");
  b.textContent = "New button";
  doc.querySelector("footer").appendChild(b);
  await tick();
  sb.walk({ dirty: true });
  const second = sb.index();
  for (const r of first) assert.equal(second.find((x) => key(x) === key(r)).id, ids.get(key(r)), key(r));
  const nb = byName(second, "New button");
  assert.ok(nb.id > Math.max(...first.map((r) => r.id)), "new element gets a new id");
  // walk 3: attribute and text changes elsewhere, then a full walk
  doc.querySelector("h1").setAttribute("data-x", "1");
  doc.querySelector("footer p").append(" more");
  await tick();
  sb.walk({});
  const third = sb.index();
  for (const r of first) assert.equal(third.find((x) => key(x) === key(r)).id, ids.get(key(r)), key(r));
  assert.equal(byName(third, "New button").id, nb.id);
  // removed element -> gone, and its id is not reused
  const email = doc.getElementById("email");
  const emailId = sb.id(email);
  assert.equal(sb.resolve(emailId), email);
  email.remove();
  await tick();
  same(sb.resolve(emailId), { why: "gone" });
  const after = sb.index();
  assert.ok(!after.some((r) => r.id === emailId));
  const again = doc.createElement("input");
  doc.getElementById("signup").prepend(again);
  await tick();
  sb.walk({ dirty: true });
  assert.notEqual(sb.id(again), emailId);
  same(sb.resolve(5), { why: "stale" });
});

test("incremental walk re-walks only dirty subtrees", async () => {
  const { doc, sb } = jsdom(page("register.html"));
  sb.walk({ base: 1 });
  sb.index();
  // a name change inside a button (owned subtree) is picked up by walk(dirty)
  const sub = Array.from(doc.querySelectorAll("button")).find((x) => x.textContent === "Subscribe");
  sub.textContent = "Join the list";
  await tick();
  const rows = sb.index(); // index() refreshes the dirty set itself
  assert.ok(byName(rows, "Join the list"));
  assert.ok(!byName(rows, "Subscribe"));
  // showing the hidden promo adds its rows
  doc.querySelector(".promo").style.display = "block";
  await tick();
  const r2 = sb.index();
  assert.ok(byName(r2, "Register for the offer"));
});

test("password value is never read and never emitted", () => {
  const CANARY = "hunter2-canary-value";
  const { doc, win, sb } = jsdom(page("controls.html"), { before: fillShadow });
  const pw = doc.querySelector('input[type="password"]');
  pw.value = CANARY;
  let reads = 0;
  const proto = win.HTMLInputElement.prototype;
  const desc = Object.getOwnPropertyDescriptor(proto, "value");
  Object.defineProperty(pw, "value", {
    get() {
      reads++;
      return desc.get.call(this);
    },
    set(v) {
      desc.set.call(this, v);
    },
  });
  sb.walk({ base: 1 });
  const rows = sb.index();
  const out = JSON.stringify(rows) + JSON.stringify(sb.text()) + JSON.stringify(sb.frames()) +
    JSON.stringify(sb.fingerprint('input"Account password"')) + JSON.stringify(sb.state(pw));
  assert.equal(reads, 0, "the password value getter was called");
  assert.ok(!out.includes(CANARY));
  const row = byName(rows, "Account password");
  same(row.state.value, { kind: "filled" });
  pw.value = "";
  same(sb.state(pw).value, { kind: "empty" });
  assert.equal(reads, 0);
});

test("fingerprint: match, partial, miss (T-4, D-49 aliases)", () => {
  const { sb } = jsdom(page("register.html"));
  assert.equal(sb.fingerprint('form#f1 button"Register" input"Email"'), "match");
  assert.equal(sb.fingerprint('form#f2 button"Subscribe" SUBMIT'), "match");
  assert.equal(sb.fingerprint('select"Plan" checkbox"I agree"'), "match");
  same(sb.fingerprint('form#f1 button"Register" input"Phone"'), { partial: ['input"Phone"'] });
  assert.equal(sb.fingerprint('form#f9 button"Pay now"'), "miss");
  assert.equal(sb.fingerprint(""), "match");
});

test("CHALLENGE: never read inside a challenge frame; captcha fields flagged", () => {
  const { sb } = jsdom("<form><input name=q aria-label=Query><button>Go</button></form>", {
    url: "https://www.google.com/recaptcha/api2/anchor?k=x",
  });
  same(sb.index(), []);
  same(sb.text(), []);
  assert.equal(sb.frames()[0].challenge, true);
  const t = jsdom('<form><img alt="code"><input name="captcha_answer" aria-label="Type the characters"><button>Go</button></form>');
  const row = byName(t.sb.index(), "Type the characters");
  assert.equal(row.flags.challenge, true);
  // a form wrapper whose class mentions captcha is not swallowed as one row
  const w = jsdom('<form class="captcha-form"><input aria-label="Email"><div class="g-recaptcha"><iframe title="reCAPTCHA"></iframe></div><button>Send</button></form>');
  const rows = w.sb.index();
  assert.ok(byName(rows, "Email") && byName(rows, "Send"));
  assert.equal(rows.filter((r) => r.flags.challenge).length, 1);
});

test("frames(): self plus one entry per iframe row", () => {
  const { sb } = jsdom(page("register.html"));
  const fr = sb.frames();
  assert.equal(fr[0].frame, 0);
  assert.equal(fr[0].parent, null);
  assert.equal(fr.length, 2);
  assert.equal(fr[1].url, "about:srcdoc");
  assert.equal(fr[1].challenge, true);
  assert.equal(typeof fr[1].id, "number");
});

test("icon-only link falls back to its last path segment; empty-text button stays empty", () => {
  const { sb } = jsdom('<nav><a href="/account/shopping-cart"><svg></svg></a><a href="/help.html" title="Help centre"><img src="x.png"></a></nav>');
  const rows = sb.index();
  assert.equal(rows[0].name, "shopping cart");
  assert.equal(rows[1].name, "Help centre");
});
