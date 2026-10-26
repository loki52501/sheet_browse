// Pure parts of injected.js under jsdom (D-401): install, identity, roles, name ladder,
// collapse, text blocks, password never read, fingerprint, CHALLENGE as one row.
"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { jsdom, page, SRC, byName, fill } = require("./helpers");

// jsdom objects come from another realm: compare their JSON.
const same = (a, b, m) => assert.deepStrictEqual(JSON.parse(JSON.stringify(a)), b, m);
const tick = () => new Promise((r) => setImmediate(r));
// jsdom() plus the first based walk, as sb-driver does before any read (D-405 (2)).
function walked(html, opts) {
  const j = jsdom(html, opts);
  j.sb.walk({ base: 1 });
  return j;
}

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
  const { sb } = walked(page("register.html"));
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
  const { doc, sb } = walked(page("controls.html"), { before: fillShadow });
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
  const { doc, sb } = walked(page("controls.html"), { before: fillShadow });
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
  const { sb } = walked(page("register.html"));
  const texts = sb.text().map((b) => b.spans.map((s) => s.text).join(""));
  assert.ok(!texts.some((t) => /I agree/.test(t)), texts.join(" / "));
  assert.ok(!texts.includes("Email"));
  assert.ok(!texts.includes("Plan"));
  assert.ok(texts.includes("Start your free trial. No card required."));
  assert.ok(!texts.some((t) => /50% off/.test(t)), "hidden promo text absent");
});

test("text(region): landmark, form id, heading", () => {
  const { sb } = walked(page("register.html"));
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
  // sticky (D-405 (1)): a show-password toggle swaps the type; the value is still never read
  pw.value = CANARY;
  sb.walk({ dirty: true });
  pw.type = "text";
  sb.walk({ dirty: true });
  const shown = JSON.stringify(sb.index()) + JSON.stringify(sb.text()) + JSON.stringify(sb.state(pw));
  sb.walk({});
  const full = JSON.stringify(sb.index()) + JSON.stringify(sb.state(pw));
  assert.equal(reads, 0, "the value getter was called after the type swap");
  assert.ok(!shown.includes(CANARY) && !full.includes(CANARY));
  same(sb.state(pw).value, { kind: "filled" });
  same(byName(sb.index(), "Account password").state.value, { kind: "filled" });
});

test("sensitivity is sticky: type swap before the walk, masked text field, sensitive autocomplete", async () => {
  const CANARY = "s3cret-canary-2";
  const { doc, win, sb } = jsdom('<form><input id="p" type="password" aria-label="Password">' +
    '<input id="m" type="text" aria-label="PIN" style="-webkit-text-security: disc">' +
    '<input id="o" aria-label="Code" autocomplete="one-time-code">' +
    '<input id="n" aria-label="Nickname"></form>');
  let reads = 0;
  const desc = Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value");
  for (const id of ["p", "m", "o"]) {
    const el = doc.getElementById(id);
    desc.set.call(el, CANARY);
    Object.defineProperty(el, "value", {
      get() {
        reads++;
        return desc.get.call(this);
      },
      set(v) {
        desc.set.call(this, v);
      },
    });
  }
  doc.getElementById("n").value = "ada";
  // the type swaps before the first walk: the observer saw the old type (attributeOldValue)
  doc.getElementById("p").type = "text";
  await tick();
  sb.walk({ base: 1 });
  const rows = sb.index();
  const out = JSON.stringify(rows) + JSON.stringify(sb.text());
  assert.equal(reads, 0);
  assert.ok(!out.includes(CANARY), out);
  same(byName(rows, "Password").state.value, { kind: "filled" });
  same(byName(rows, "PIN").state.value, { kind: "filled" });
  same(byName(rows, "Code").state.value, { kind: "filled" });
  same(byName(rows, "Nickname").state.value, { kind: "text", text: "ada" });
  // a masked field that later drops the masking stays sensitive, and so does the OTP field
  doc.getElementById("m").removeAttribute("style");
  doc.getElementById("o").removeAttribute("autocomplete");
  await tick();
  sb.walk({});
  const again = sb.index();
  assert.equal(reads, 0);
  assert.ok(!JSON.stringify(again).includes(CANARY));
  same(byName(again, "PIN").state.value, { kind: "filled" });
  same(byName(again, "Code").state.value, { kind: "filled" });
});

// An input whose value getter counts calls (the value is set through the native setter).
function spied(win, el, value, counter) {
  const proto = el.localName === "textarea" ? win.HTMLTextAreaElement.prototype : win.HTMLInputElement.prototype;
  const desc = Object.getOwnPropertyDescriptor(proto, "value");
  desc.set.call(el, value);
  Object.defineProperty(el, "value", {
    get() {
      counter.reads++;
      return desc.get.call(this);
    },
    set(v) {
      desc.set.call(this, v);
    },
  });
  return el;
}

test("a rebuilt password field inherits sensitivity (D-429 (1)): replaceWith, remove+append, wrapper swap", async () => {
  const CANARY = "rebuilt-canary-429";
  const c = { reads: 0 };
  const { doc, win, sb } = jsdom('<form id="f"><div id="w1"><input id="p1" type="password" aria-label="Password one"></div>' +
    '<div id="w2"><input id="p2" type="password" aria-label="Password two"></div>' +
    '<div id="outer"><div id="w3"><input id="p3" type="password" aria-label="Password three"></div></div>' +
    '<div id="other"></div></form>');
  for (const id of ["p1", "p2", "p3"]) spied(win, doc.getElementById(id), CANARY, c);
  sb.walk({ base: 1 });
  same(byName(sb.index(), "Password one").state.value, { kind: "filled" });
  const mk = (html) => {
    const t = doc.createElement("template");
    t.innerHTML = html;
    return t.content.firstElementChild;
  };
  // 1. the review repro: replaceWith a type=text input carrying the value (a neutral name, so
  // only inheritance can mark it); the walk runs in the same task, before the observer callback
  const t1 = spied(win, mk('<input id="t1" type="text" name="shown" aria-label="Password one">'), CANARY, c);
  doc.getElementById("p1").replaceWith(t1);
  // an unrelated field added by the same batch elsewhere is not sensitive
  const n1 = mk('<input id="n1" aria-label="Nickname">');
  n1.value = "ada";
  doc.getElementById("other").appendChild(n1);
  sb.walk({ dirty: true });
  // 2. remove, then append a new field into the same parent (two records in one batch)
  const t2 = spied(win, mk('<input id="t2" type="text" aria-label="Password two">'), CANARY, c);
  doc.getElementById("p2").remove();
  doc.getElementById("w2").appendChild(t2);
  // 3. the wrapper is rebuilt: the removed subtree held the field
  const w3 = mk('<div id="w3b"><span>Shown</span><input id="t3" type="text" aria-label="Password three"></div>');
  spied(win, w3.querySelector("input"), CANARY, c);
  doc.getElementById("w3").replaceWith(w3);
  await tick(); // this batch reaches the observer callback the normal way
  sb.walk({ dirty: true });
  const rows = sb.index();
  let out = JSON.stringify(rows) + JSON.stringify(sb.text());
  for (const id of ["t1", "t2", "t3"]) out += JSON.stringify(sb.state(doc.getElementById(id)));
  sb.walk({});
  out += JSON.stringify(sb.index());
  assert.equal(c.reads, 0, "the value getter of a rebuilt field was called");
  assert.ok(!out.includes(CANARY), out);
  for (const n of ["Password one", "Password two", "Password three"]) {
    same(byName(sb.index(), n).state.value, { kind: "filled" }, n);
  }
  same(byName(rows, "Nickname").state.value, { kind: "text", text: "ada" });
});

test("pw_ and pw- names are sensitive (D-429 (1))", () => {
  const c = { reads: 0 };
  const { doc, win, sb } = jsdom('<input id="a" name="pw_plain" aria-label="A"><input id="b" name="pw-confirm" aria-label="B">' +
    '<input id="d" name="pw" aria-label="D"><input id="e" name="newpw_2" aria-label="E"><input id="n" name="power" aria-label="N">');
  for (const id of ["a", "b", "d", "e"]) spied(win, doc.getElementById(id), "pwcanary", c);
  doc.getElementById("n").value = "max";
  sb.walk({ base: 1 });
  const rows = sb.index();
  assert.equal(c.reads, 0);
  assert.ok(!JSON.stringify(rows).includes("pwcanary"));
  for (const n of ["A", "B", "D", "E"]) same(byName(rows, n).state.value, { kind: "filled" }, n);
  same(byName(rows, "N").state.value, { kind: "text", text: "max" });
});

test("challenge text (D-429 (2)): ordinary press-and-hold and quiz text are not CHALLENGE, real prompts are", () => {
  const neg = walked('<main><p>Press and hold Shift to select several files.</p>' +
    '<p>Press and hold the mic button to talk</p><p>Press &amp; hold to record a voice message.</p>' +
    '<section><h2>Are you human?</h2><p>Take our quiz about what makes us human.</p>' +
    '<button type="button">Start the quiz</button></section></main>');
  const nr = neg.sb.index();
  assert.ok(!nr.some((r) => r.flags.challenge), JSON.stringify(nr));
  assert.ok(byName(nr, "Are you human?", "heading"));
  const nt = neg.sb.text().map((b) => b.spans.map((x) => x.text).join("")).join(" / ");
  assert.match(nt, /Press and hold Shift/);
  assert.match(nt, /mic button/);
  // the PerimeterX prompt still is one, and so is its widget
  const px = walked('<div><p>Press &amp; Hold to confirm you are a human (and not a bot).</p><div id="px-captcha"></div></div>');
  const pr = px.sb.index().filter((r) => r.flags.challenge);
  assert.equal(pr.length, 2, JSON.stringify(px.sb.index()));
  assert.equal(pr[0].name, "Press & Hold to confirm you are a human (and not a bot).");
  const hold = walked('<p>Press and hold to confirm you are a human (and not a bot).</p><button>Help</button>');
  assert.ok(byName(hold.sb.index(), "Press and hold to confirm you are a human (and not a bot).").flags.challenge);
  // "Are you human?" next to a real widget, a response field or a challenge frame is CHALLENGE
  for (const w of ['<div class="g-recaptcha" data-sitekey="k"></div>',
    '<div><textarea name="h-captcha-response" style="display:none"></textarea></div>',
    '<iframe title="Widget containing a Cloudflare security challenge" src="https://challenges.cloudflare.com/x"></iframe>']) {
    const it = walked('<form><h2>Are you human?</h2>' + w + '<button>Send</button></form>');
    assert.ok(byName(it.sb.index(), "Are you human?", "heading").flags.challenge, w);
  }
  // the widget shows up later: the heading becomes CHALLENGE on the next dirty walk
  const late = walked('<form id="f"><h2>Are you human?</h2><button>Send</button></form>');
  assert.ok(!byName(late.sb.index(), "Are you human?").flags.challenge);
  const box = late.doc.createElement("div");
  box.className = "h-captcha";
  box.setAttribute("data-sitekey", "k");
  late.doc.getElementById("f").appendChild(box);
  late.sb.walk({ dirty: true });
  assert.ok(byName(late.sb.index(), "Are you human?").flags.challenge);
  // the strong prompts need no widget
  for (const p of ["Verify you are human", "I'm not a robot", "Are you a robot?", "Human verification"]) {
    const it = walked("<p>" + p + "</p>");
    assert.ok(it.sb.index().some((r) => r.flags.challenge), p);
  }
});

test("no ids before walk({base}) (D-405 (2)): the review repro", () => {
  const { doc, sb } = jsdom(page("register.html"));
  const btn = Array.from(doc.querySelectorAll("button")).find((b) => b.textContent === "Register");
  const NB = { why: "no base" };
  same(sb.id(btn), NB);
  same(sb.index(), NB);
  same(sb.text(), NB);
  same(sb.frames(), NB);
  same(sb.fingerprint('button"Register"'), NB);
  same(sb.state(btn), NB);
  same(sb.name(btn), NB);
  same(sb.walk({}), NB, "a walk without a base issues nothing");
  same(sb.walk({ dirty: true }), NB);
  for (const b of ["5", 0, -1, 2.5, NaN, Infinity, 2 ** 60, null, true]) same(sb.walk({ base: b }), NB, "base " + String(b));
  assert.equal(sb.role(btn), "button", "role issues no id and works");
  same(sb.resolve(1), { why: "stale" }, "nothing was issued in this document yet");
  assert.equal(sb.walk({ base: 100 }).count > 0, true);
  const id = sb.id(btn);
  assert.ok(id >= 100, String(id));
  assert.equal(sb.resolve(id), btn);
  const rows = sb.index();
  assert.equal(byName(rows, "Register").id, id);
  assert.ok(rows.every((r) => r.id >= 100));
  same(sb.resolve(1), { why: "stale" });
  same(sb.resolve(99), { why: "stale" });
  same(sb.resolve(100000), { why: "gone" }, "never issued and not below base");
  // a later base is ignored: ids keep their numbers for the document's lifetime
  sb.walk({ base: 5000 });
  assert.equal(sb.id(btn), id);
  btn.remove();
  same(sb.resolve(id), { why: "gone" }, "issued here and lost");
});

test("fingerprint: match, partial, miss (T-4, D-49 aliases)", () => {
  const { sb } = walked(page("register.html"));
  assert.equal(sb.fingerprint('form#f1 button"Register" input"Email"'), "match");
  assert.equal(sb.fingerprint('form#f2 button"Subscribe" SUBMIT'), "match");
  assert.equal(sb.fingerprint('select"Plan" checkbox"I agree"'), "match");
  same(sb.fingerprint('form#f1 button"Register" input"Phone"'), { partial: ['input"Phone"'] });
  assert.equal(sb.fingerprint('form#f9 button"Pay now"'), "miss");
  assert.equal(sb.fingerprint(""), "match");
});

test("CHALLENGE: a challenge page is one CHALLENGE row and nothing inside is read; captcha fields flagged", () => {
  for (const url of [
    "https://www.google.com/recaptcha/api2/anchor?k=x",
    "https://newassets.hcaptcha.com/captcha/v1/abc/static/hcaptcha.html",
    "https://challenges.cloudflare.com/cdn-cgi/challenge-platform/h/b/turnstile/if/ov2",
    "https://shop.example.test/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page",
    "https://client-api.arkoselabs.com/fc/gt2/public_key/X",
    "https://static.geetest.com/static/js/fullpage.js",
    "https://www.recaptcha.net/recaptcha/api2/bframe",
  ]) {
    const { sb } = walked("<form><input name=q aria-label=Query><button>Go</button></form>", { url });
    const rows = sb.index();
    assert.equal(rows.length, 1, url);
    assert.equal(rows[0].flags.challenge, true, url);
    assert.equal(rows[0].role, "document");
    assert.ok(!JSON.stringify(rows).includes("Query"), "nothing inside is read: " + url);
    const text = sb.text();
    assert.equal(text.length, 1);
    assert.equal(text[0].spans[0].elem, "#" + rows[0].id);
    assert.ok(!JSON.stringify(text).includes("Go"));
    assert.equal(sb.frames()[0].challenge, true);
    assert.equal(sb.frames().length, 1);
  }
  // the Cloudflare interstitial title, on the site's own host
  const cf = walked("<title>Just a moment...</title><p>Checking</p><button>x</button>", { url: "https://shop.example.test/" });
  assert.equal(cf.sb.index().length, 1);
  assert.equal(cf.sb.index()[0].flags.challenge, true);
  const t = walked('<form><img alt="code"><input name="captcha_answer" aria-label="Type the characters"><button>Go</button></form>');
  const row = byName(t.sb.index(), "Type the characters");
  assert.equal(row.flags.challenge, true);
  // a form wrapper whose class mentions captcha is not swallowed as one row
  const w = walked('<form class="captcha-form"><input aria-label="Email"><div class="g-recaptcha"><iframe title="reCAPTCHA"></iframe></div><button>Send</button></form>');
  const rows = w.sb.index();
  assert.ok(byName(rows, "Email") && byName(rows, "Send"));
  assert.equal(rows.filter((r) => r.flags.challenge).length, 1);
});

test("a URL that only mentions captcha is a normal page (/posts/how-captchas-work)", () => {
  for (const url of ["https://blog.example.org/posts/how-captchas-work", "https://developers.example.org/recaptcha/docs"]) {
    const { sb } = walked(page("how-captchas-work.html"), { url });
    const rows = sb.index();
    assert.ok(!rows.some((r) => r.flags.challenge), url + " " + JSON.stringify(rows.filter((r) => r.flags.challenge)));
    assert.ok(byName(rows, "How captchas work", "heading"));
    assert.ok(byName(rows, "Subscribe", "button"));
    assert.equal(sb.frames()[0].challenge, false);
    const text = sb.text().map((b) => b.spans.map((x) => x.text).join("")).join(" / ");
    assert.match(text, /not a robot/, "the article text is read normally");
  }
});

test("CHALLENGE detection: interstitial prompt, response fields, Arkose, GeeTest; v3 badge and invisible widgets are not", () => {
  // the review repro: a prompt paragraph and a widget behind its response field
  const it = walked(page("challenge-interstitial.html"));
  const rows = it.sb.index();
  const ch = rows.filter((r) => r.flags.challenge);
  assert.equal(ch.length, 2, JSON.stringify(ch));
  assert.equal(ch[0].name, "Verify you are human by completing the action below.");
  assert.equal(it.sb.id(it.doc.getElementById("prompt")), ch[0].id);
  assert.equal(it.sb.id(it.doc.getElementById("widget")), ch[1].id);
  assert.ok(byName(rows, "Why am I seeing this page?", "link"));
  assert.ok(!JSON.stringify(it.sb.text()).includes("Verify you are human"), "the prompt is the row, not text");
  // widgets
  const wd = walked(page("challenge-widgets.html"));
  const wr = wd.sb.index();
  const wch = wr.filter((r) => r.flags.challenge);
  const ids = (sel) => wd.sb.id(wd.doc.querySelector(sel));
  const sorted = (a) => a.slice().sort((x, y) => x - y);
  same(sorted(wch.map((r) => r.id)), sorted([ids("#arkose iframe"), ids(".geetest_holder"), ids("#hc")]));
  assert.equal(byName(wr, "Verification challenge").role, "iframe");
  // v3: the badge frame is an ordinary iframe row; the v3-bound submit is a normal SUBMIT
  const badge = wr.find((r) => r.id === ids(".grecaptcha-badge iframe"));
  assert.ok(badge && !badge.flags.challenge);
  const signin = byName(wr, "Sign in", "button");
  assert.ok(signin.flags.submit && !signin.flags.challenge);
  assert.ok(byName(wr, "Password") && byName(wr, "Username"));
  // invisible-size widget and frame (real Google URLs: jsdom loads no subresources)
  const inv = walked('<form><input aria-label="Email"><div class="g-recaptcha" data-sitekey="k" data-size="invisible">' +
    '<div><iframe title="reCAPTCHA" src="https://www.google.com/recaptcha/api2/anchor?k=k&amp;size=invisible"></iframe></div></div>' +
    '<button>Send</button></form><iframe title="reCAPTCHA" src="https://www.google.com/recaptcha/api2/anchor?ar=1&amp;size=invisible"></iframe>');
  assert.ok(!inv.sb.index().some((r) => r.flags.challenge), JSON.stringify(inv.sb.index()));
  // a v2 checkbox frame is a challenge
  const v2 = walked('<form><iframe title="reCAPTCHA" src="https://www.google.com/recaptcha/api2/anchor?k=k&amp;size=normal"></iframe><button>Send</button></form>');
  assert.equal(v2.sb.index().filter((r) => r.flags.challenge).length, 1);
  // challenge text as a dialog name: a CHALLENGE row
  const dl = walked('<div role="dialog" aria-label="Are you a robot?"><p>Hold on</p><button>Continue</button></div>');
  assert.ok(byName(dl.sb.index(), "Are you a robot?").flags.challenge);
});

test("heading level: aria-level clamped to an integer in 1..6 (D-405 (6))", () => {
  const { sb } = walked('<div role="heading" aria-level="300">A</div><div role="heading" aria-level="2.5">B</div>' +
    '<div role="heading" aria-level="0">C</div><div role="heading" aria-level="x">D</div><div role="heading">E</div>' +
    '<div role="heading" aria-level="4">F</div><h3 aria-level="9">G</h3>');
  const lv = sb.text().filter((b) => b.kind === "heading").map((b) => [b.spans[0].text, b.level]);
  same(lv, [["A", 6], ["B", 2], ["C", 2], ["D", 2], ["E", 2], ["F", 4], ["G", 3]]);
});

test("a named form is landmark form, an unnamed one is not; search is form (D-405 (7))", () => {
  const { sb } = walked('<main><form aria-label="Checkout"><input aria-label="Card"></form>' +
    '<form title="Filters"><input aria-label="Colour"></form>' +
    '<form><input aria-label="Plain"></form><search><input aria-label="Find"></search></main>');
  const rows = sb.index();
  assert.equal(byName(rows, "Card").landmark, "form");
  assert.equal(byName(rows, "Colour").landmark, "form");
  assert.equal(byName(rows, "Plain").landmark, "main");
  assert.equal(byName(rows, "Find").landmark, "form");
  assert.equal(byName(rows, "Card").form, "f1");
});

test("TextBlock wire shape: keys and types sb_sheet::TextBlock accepts", () => {
  const { sb } = walked(page("controls.html"), { before: fillShadow });
  const keys = new Set(["order", "frame", "kind", "level", "landmark", "heading", "table", "header", "spans"]);
  const kinds = new Set(["heading", "paragraph", "list_item", "table_row", "code", "quote", "other"]);
  let last = -1;
  for (const b of sb.text()) {
    for (const k of Object.keys(b)) assert.ok(keys.has(k), "block key " + k);
    assert.ok(Number.isInteger(b.order) && b.order > last);
    last = b.order;
    assert.ok(kinds.has(b.kind), b.kind);
    if (b.level !== undefined) assert.ok(Number.isInteger(b.level) && b.level >= 0 && b.level <= 255);
    if (b.table !== undefined) assert.ok(Number.isInteger(b.table) && b.table >= 0);
    if (b.header !== undefined) assert.equal(typeof b.header, "boolean");
    for (const sp of b.spans) {
      for (const k of Object.keys(sp)) assert.ok(k === "text" || k === "elem", "span key " + k);
      assert.equal(typeof sp.text, "string");
      if (sp.elem !== undefined) assert.match(sp.elem, /^#\d+$/);
    }
  }
});

test("frames(): self plus one entry per iframe row", () => {
  const { sb } = walked(page("register.html"));
  const fr = sb.frames();
  assert.equal(fr[0].frame, 0);
  assert.equal(fr[0].parent, null);
  assert.equal(fr.length, 2);
  assert.equal(fr[1].url, "about:srcdoc");
  assert.equal(fr[1].challenge, true);
  assert.equal(typeof fr[1].id, "number");
});

test("icon-only link falls back to its last path segment; empty-text button stays empty", () => {
  const { sb } = walked('<nav><a href="/account/shopping-cart"><svg></svg></a><a href="/help.html" title="Help centre"><img src="x.png"></a></nav>');
  const rows = sb.index();
  assert.equal(rows[0].name, "shopping cart");
  assert.equal(rows[1].name, "Help centre");
});

// ------------------------------------------------------------------ D-435, D-436
// A frozen, controllable clock for injected.js in this window (now() reads performance.now()
// on use): only advance() moves it, so the 2000 ms boundary is exact whatever the machine load.
function fakeClock(win) {
  const t0 = win.performance.now();
  let off = 0;
  Object.defineProperty(win.performance, "now", { configurable: true, value: () => t0 + off });
  return (ms) => { off += ms; };
}
const tmpl = (doc, html) => {
  const t = doc.createElement("template");
  t.innerHTML = html;
  return t.content.firstElementChild;
};

test("an SPA login swap keeps reading the new page's unrelated fields (D-435 (1))", async () => {
  const CANARY = "spa-canary-435";
  const c = { reads: 0 };
  const { doc, win, sb } = jsdom('<div id="root"><div id="app"><form><input id="u" aria-label="Email">' +
    '<input id="p" type="password" name="password" placeholder="Password" aria-label="Password">' +
    "<button>Sign in</button></form></div></div>");
  spied(win, doc.getElementById("p"), CANARY, c);
  sb.walk({ base: 1 });
  // the app page replaces #app in one record: pre-filled fields that match nothing, and one
  // field that keeps the password's label (a rebuilt password field) and carries the value
  const app = tmpl(doc, '<div id="app"><h1>Profile</h1><input id="dn" aria-label="Display name" value="Ada Lovelace">' +
    '<input name="city" placeholder="City" value="London"><label>Bio <textarea id="bio">Mathematician</textarea></label>' +
    '<input id="again" type="text" aria-label="Password"></div>');
  spied(win, app.querySelector("#again"), CANARY, c);
  doc.getElementById("app").replaceWith(app);
  sb.walk({ dirty: true });
  const rows = sb.index();
  assert.equal(c.reads, 0);
  assert.ok(!JSON.stringify(rows).includes(CANARY));
  same(byName(rows, "Display name").state.value, { kind: "text", text: "Ada Lovelace" });
  same(byName(rows, "City").state.value, { kind: "text", text: "London" });
  same(byName(rows, "Bio").state.value, { kind: "text", text: "Mathematician" });
  same(byName(rows, "Password").state.value, { kind: "filled" });
});

test("rebuild match keys (D-435 (1)): each of id, name, autocomplete, aria-label, placeholder, label text inherits; no match reads", async () => {
  const CANARY = "key-canary-435";
  // [attributes of the old field, html next to the old field, attributes of the new field, html next to it]
  const cases = {
    id: ['id="acct"', "", 'id="acct"', ""],
    name: ['name="login_key"', "", 'name="login_key"', ""],
    autocomplete: ['autocomplete="section-a username"', "", 'autocomplete="username"', ""],
    "aria-label": ['aria-label="Account key"', "", 'aria-label="Account key"', ""],
    placeholder: ['placeholder="Your key"', "", 'placeholder="Your key"', ""],
    "label for": ['id="lbl"', '<label for="lbl">Account key:</label>', 'id="lbl2"', '<label for="lbl2">Account key</label>'],
    labelledby: ['aria-labelledby="cap"', '<span id="cap">Account key</span>', 'aria-labelledby="cap2"', '<span id="cap2">Account key</span>'],
    "wrapping label": ["", "", "", ""],
    // the label[for] leaves with the field: found in the removed subtree
    "label for, removed with the field": ['id="lbl5"', '<label for="lbl5">Account key</label>', 'id="lbl6"', '<label for="lbl6">Account key</label>'],
    // a walked field's row name (here from its title, which is not a key itself)
    "walked row name": ['title="Account key"', "", 'aria-label="Account key"', ""],
  };
  for (const [k, [oldAt, oldX, newAt, newX]] of Object.entries(cases)) {
    const c = { reads: 0 };
    const wrap = k === "wrapping label";
    const oldF = wrap ? '<label id="ol">Account key <input id="o" type="password"></label>'
      : '<input type="password" ' + (oldAt.startsWith("id=") ? oldAt : 'id="o" ' + oldAt) + ">";
    // the old field arrives after the base walk, so no walk has named it (except "walked row
    // name"): each key is tested on its own, not through the cached row name
    const { doc, win, sb } = jsdom('<form><div id="host"></div><div id="b">' + newX + "</div></form>");
    sb.walk({ base: 1 });
    doc.getElementById("host").innerHTML = '<div id="a">' + oldX + oldF + "</div>";
    const old = doc.querySelector("#a input");
    spied(win, old, CANARY, c);
    await tick();
    if (k === "walked row name") sb.walk({ dirty: true });
    // two records (remove, then append elsewhere): not a one-for-one swap, only the match counts
    (wrap ? doc.getElementById("ol") : k.includes("removed with") ? doc.getElementById("a") : old).remove();
    const nf = tmpl(doc, wrap ? "<label>Account key <input></label>" : "<input " + newAt + ">");
    const input = nf.localName === "input" ? nf : nf.querySelector("input");
    spied(win, input, CANARY, c);
    doc.getElementById("b").appendChild(nf);
    sb.walk({ dirty: true });
    const st = sb.state(input);
    assert.equal(c.reads, 0, k);
    assert.ok(!JSON.stringify(st).includes(CANARY), k);
    same(st.value, { kind: "filled" }, k);
  }
  // control: a replacement that shares nothing (autocomplete=off on both) reads its value
  const { doc, win, sb } = jsdom('<form><div id="a"><input id="o" type="password" aria-label="Account key" autocomplete="off"></div><div id="b"></div></form>');
  spied(win, doc.getElementById("o"), CANARY, { reads: 0 });
  sb.walk({ base: 1 });
  doc.getElementById("o").remove();
  const nf = tmpl(doc, '<input id="nick" aria-label="Nickname" autocomplete="off" value="ada">');
  doc.getElementById("b").appendChild(nf);
  sb.walk({ dirty: true });
  same(sb.state(nf).value, { kind: "text", text: "ada" });
});

test("a one-for-one replaceWith inherits with no shared identity (D-435 (1))", async () => {
  const CANARY = "swap-canary-435";
  const c = { reads: 0 };
  const { doc, win, sb } = jsdom('<form><span><input id="p" type="password" aria-label="Password"></span>' +
    '<input id="u" aria-label="User" value="ada"></form>');
  spied(win, doc.getElementById("p"), CANARY, c);
  sb.walk({ base: 1 });
  const t = spied(win, tmpl(doc, '<input id="plain" name="shown" aria-label="Visible text">'), CANARY, c);
  doc.getElementById("p").replaceWith(t);
  await tick();
  sb.walk({ dirty: true });
  const rows = sb.index();
  assert.equal(c.reads, 0);
  assert.ok(!JSON.stringify(rows).includes(CANARY));
  same(byName(rows, "Visible text").state.value, { kind: "filled" });
  same(byName(rows, "User").state.value, { kind: "text", text: "ada" });
});

test("a rebuild split by a microtask or a later task within 2 s inherits; after 2 s it reads (D-435 (2))", async () => {
  const CANARY = "split-canary-435";
  const c = { reads: 0 };
  const { doc, win, sb } = jsdom('<form><div id="w1"><input id="k1" type="password" aria-label="Key one"></div>' +
    '<div id="w2"><input id="k2" type="password" aria-label="Key two"></div>' +
    '<div id="w3"><input id="k3" type="password" aria-label="Key three"></div></form>');
  const advance = fakeClock(win);
  for (const id of ["k1", "k2", "k3"]) spied(win, doc.getElementById(id), CANARY, c);
  sb.walk({ base: 1 });
  const rebuild = (id, label) => spied(win, tmpl(doc, '<input id="' + id + '" type="text" aria-label="' + label + '">'), CANARY, c);
  // 1. remove; await a microtask; append: two observer batches in one task
  let batches = 0;
  const count = new win.MutationObserver(() => { batches++; });
  count.observe(doc, { subtree: true, childList: true });
  await (async () => {
    doc.getElementById("k1").remove();
    await null;
    doc.getElementById("w1").appendChild(rebuild("k1", "Key one"));
  })();
  await tick();
  count.disconnect();
  assert.equal(batches, 2, "the microtask split delivered two batches");
  // 2. remove now, append in a later task 2000 ms later (the window is inclusive)
  doc.getElementById("k2").remove();
  await tick();
  advance(2000);
  doc.getElementById("w2").appendChild(rebuild("k2", "Key two"));
  await tick();
  // 3. remove now, append 2001 ms later: the memory has aged out, the field is a new one
  doc.getElementById("k3").remove();
  await tick();
  advance(2001);
  const late = tmpl(doc, '<input id="k3" type="text" aria-label="Key three">');
  late.value = "plain text";
  doc.getElementById("w3").appendChild(late);
  await tick();
  sb.walk({ dirty: true });
  const rows = sb.index();
  assert.equal(c.reads, 0);
  assert.ok(!JSON.stringify(rows).includes(CANARY));
  same(byName(rows, "Key one").state.value, { kind: "filled" });
  same(byName(rows, "Key two").state.value, { kind: "filled" });
  same(byName(rows, "Key three").state.value, { kind: "text", text: "plain text" });
});

test("press and hold is CHALLENGE text with challenge context, not without (D-435 (3))", () => {
  const p = "<p>Press and hold the button to continue</p><button>Hold</button>";
  const no = walked("<main>" + p + "</main>");
  assert.ok(!no.sb.index().some((r) => r.flags.challenge), JSON.stringify(no.sb.index()));
  const yes = walked("<main>" + p + '<div class="vendor-captcha-box"></div></main>');
  assert.ok(byName(yes.sb.index(), "Press and hold the button to continue").flags.challenge);
});

test("passive reCAPTCHA v3 markers are not challenge context; a visible v2 widget is (D-436)", () => {
  const prompts = "<h2>Are you human?</h2><p>Press and hold the button to continue</p><button>Start the quiz</button>";
  const passive = {
    badge: '<div class="grecaptcha-badge" style="position:fixed;right:0;bottom:14px;width:256px;height:60px">' +
      '<div class="grecaptcha-logo"><iframe title="reCAPTCHA" src="https://www.google.com/recaptcha/api2/anchor?k=k&amp;size=invisible"></iframe></div>' +
      '<textarea id="g-recaptcha-response" name="g-recaptcha-response" class="g-recaptcha-response" style="display:none"></textarea></div>',
    "bound button": '<form><input aria-label="Email"><button class="g-recaptcha" data-sitekey="k" data-callback="cb">Send</button></form>',
    "bound script": '<script data-sitekey="k"></script>',
    "invisible widget": '<div class="g-recaptcha" data-sitekey="k" data-size="invisible"></div>',
    "hidden bframe": '<div style="visibility:hidden"><iframe title="recaptcha challenge expires in two minutes" src="https://www.google.com/recaptcha/api2/bframe?k=k"></iframe></div>',
    "hidden widget": '<div class="h-captcha" data-sitekey="k" style="display:none"></div>',
  };
  for (const [k, m] of Object.entries(passive)) {
    const it = walked("<main>" + prompts + "</main>" + m);
    const rows = it.sb.index();
    assert.ok(byName(rows, "Are you human?", "heading"), k);
    same(rows.filter((r) => r.flags.challenge).map((r) => r.name), [], k);
    assert.match(it.sb.text().map((b) => b.spans.map((x) => x.text).join("")).join(" / "), /Press and hold the button/, k);
  }
  const rows = walked("<main>" + prompts + '<div class="g-recaptcha" data-sitekey="k"><iframe title="reCAPTCHA" ' +
    'src="https://www.google.com/recaptcha/api2/anchor?k=k&amp;size=normal"></iframe></div></main>' + passive.badge).sb.index();
  assert.ok(byName(rows, "Are you human?", "heading").flags.challenge);
  assert.ok(byName(rows, "Press and hold the button to continue").flags.challenge);
});

// ------------------------------------------------------------------ D-438
// add-first.html with injected.js on a frozen clock; every password field counts its value
// getter calls. step(j) changes the page and returns the fields to check.
async function addFirst(canary, step) {
  const c = { reads: 0 };
  const j = jsdom(page("add-first.html"));
  j.advance = fakeClock(j.win);
  j.mk = (html) => tmpl(j.doc, html);
  j.spy = (el) => spied(j.win, el, canary, c);
  j.$ = (id) => j.doc.getElementById(id);
  for (const el of j.doc.querySelectorAll('input[type="password"]')) j.spy(el);
  j.sb.walk({ base: 1 });
  j.sb.index();
  const fields = await step(j);
  await tick();
  j.sb.walk({ dirty: true });
  const states = fields.map((f) => j.sb.state(f).value);
  const blob = JSON.stringify([j.sb.index(), j.sb.text(), states]);
  return { c, states: JSON.parse(JSON.stringify(states)), blob };
}
// Observer batches seen while fn runs (a microtask split must deliver more than one).
async function batches(win, doc, fn) {
  let n = 0;
  const mo = new win.MutationObserver(() => { n++; });
  mo.observe(doc, { subtree: true, childList: true });
  await fn();
  await tick();
  mo.disconnect();
  return n;
}

test("an add-first rebuild split by a microtask or a later task inherits within 2 s (D-438 (1))", async () => {
  const CANARY = "addfirst-canary-438";
  const cases = {
    // the copy keeps the old field's aria-label and lands in another parent: a key match
    "microtask, key": async (j) => {
      const n = j.spy(j.mk('<input id="k1-copy" type="text" aria-label="Key one">'));
      const b = await batches(j.win, j.doc, async () => {
        j.$("b1").appendChild(n);
        await null;
        j.$("k1").remove();
      });
      assert.equal(b, 2, "the microtask split delivered two batches");
      return [n];
    },
    "later task at 2000 ms, key": async (j) => {
      const n = j.spy(j.mk('<input id="k1-copy" type="text" aria-label="Key one">'));
      j.$("b1").appendChild(n);
      await tick();
      j.advance(2000);
      j.$("k1").remove();
      return [n];
    },
    // the copy shares nothing with the old field: the single field added to its parent
    "later task, same parent, no shared key": async (j) => {
      const n = j.spy(j.mk('<input id="k2-plain" type="text" aria-label="Visible key">'));
      j.$("k2").after(n);
      await tick();
      j.advance(1999);
      j.$("k2").remove();
      return [n];
    },
  };
  for (const [k, step] of Object.entries(cases)) {
    const r = await addFirst(CANARY, step);
    assert.equal(r.c.reads, 0, k + ": a value getter was called");
    assert.ok(!r.blob.includes(CANARY), k + ": " + r.blob);
    same(r.states, [{ kind: "filled" }], k);
  }
  // after 2000 ms the memory has aged out: the copy reads its text (two fields added to one
  // parent inherit since D-442: see the D-442 test)
  const late = {
    "key, 2001 ms": async (j) => {
      const n = j.mk('<input id="k1-copy" type="text" aria-label="Key one" value="plain one">');
      j.$("b1").appendChild(n);
      await tick();
      j.advance(2001);
      j.$("k1").remove();
      return [n];
    },
    "same parent, 2001 ms": async (j) => {
      const n = j.mk('<input id="k2-plain" type="text" aria-label="Visible key" value="plain two">');
      j.$("k2").after(n);
      await tick();
      j.advance(2001);
      j.$("k2").remove();
      return [n];
    },
  };
  const want = {
    "key, 2001 ms": [{ kind: "text", text: "plain one" }],
    "same parent, 2001 ms": [{ kind: "text", text: "plain two" }],
  };
  for (const [k, step] of Object.entries(late)) same((await addFirst(CANARY, step)).states, want[k], k);
});

test("while the old field is still there, an added field matching its keys is sensitive at once (D-438 (1))", async () => {
  const CANARY = "overlap-canary-438";
  const r = await addFirst(CANARY, async (j) => {
    // next to the old field in its parent, and in a wrapper inserted next to the old wrapper
    const n1 = j.spy(j.mk('<input id="k1-t" type="text" aria-label="Key one">'));
    const n2 = j.mk('<div id="w2b"><input id="k2-t" type="text" aria-label="Key two"></div>');
    j.spy(n2.firstElementChild);
    const note = j.mk('<input aria-label="Note" value="hello">');
    j.$("k1").after(n1, note);
    j.$("w2").after(n2);
    await tick();
    assert.ok(j.$("k1").isConnected && j.$("k2").isConnected);
    return [n1, n2.firstElementChild, note];
  });
  assert.equal(r.c.reads, 0);
  assert.ok(!r.blob.includes(CANARY), r.blob);
  // the unrelated note was added next to a live password: masked for the window (D-443 (4))
  sameMasked(r.states, [{ kind: "filled" }, { kind: "filled" }, { kind: "masked" }]);
  // past the window, with k1 still there, the note reads its text again
  const late = await addFirst(CANARY, async (j) => {
    const note = j.mk('<input aria-label="Note" value="hello">');
    j.$("k1").after(note);
    await tick();
    j.advance(2001);
    assert.ok(j.$("k1").isConnected);
    return [note];
  });
  assert.equal(late.c.reads, 0);
  same(late.states, [{ kind: "text", text: "hello" }]);
});

test("swap counts take value fields only; a label[for] removed beside its field is its label (D-438 (2), (3))", async () => {
  const CANARY = "count-canary-438";
  const cases = {
    // the wrapper also holds a show checkbox or a hidden input: still a one-for-one swap
    "checkbox in the wrapper": async (j) => {
      const n = j.mk('<div id="w3"><span>Key three</span><input type="text"><input type="checkbox" id="sh3" checked> Show</div>');
      j.spy(n.querySelector('input[type="text"]'));
      j.$("w3").replaceWith(n);
      return [n.querySelector('input[type="text"]')];
    },
    "hidden in the wrapper": async (j) => {
      const n = j.mk('<div id="w4"><span>Key four</span><input type="text"><input type="hidden" name="enc" value="1"></div>');
      j.spy(n.querySelector('input[type="text"]'));
      j.$("w4").replaceWith(n);
      return [n.querySelector('input[type="text"]')];
    },
    // innerHTML rebuild of a never-walked block with new ids and a second value field (so not
    // a one-for-one swap): the label text matches only through the removed label[for]
    "label[for] removed beside the field": async (j) => {
      const w = j.$("w5");
      w.innerHTML = '<label for="acct">Account key</label><input id="acct" type="password"><input id="hint" aria-label="Hint">';
      j.spy(w.querySelector("#acct"));
      await tick();
      w.innerHTML = '<label for="acct2">Account key</label><input id="acct2" type="text"><input id="hint2" aria-label="Hint" value="blue">';
      j.spy(w.querySelector("#acct2"));
      return [w.querySelector("#acct2"), w.querySelector("#hint2")];
    },
  };
  const want = {
    "checkbox in the wrapper": [{ kind: "filled" }],
    "hidden in the wrapper": [{ kind: "filled" }],
    "label[for] removed beside the field": [{ kind: "filled" }, { kind: "text", text: "blue" }],
  };
  for (const [k, step] of Object.entries(cases)) {
    const r = await addFirst(CANARY, step);
    assert.equal(r.c.reads, 0, k);
    assert.ok(!r.blob.includes(CANARY), k + ": " + r.blob);
    same(r.states, want[k], k);
  }
});

test("a two-field login replaced by a one-field pre-filled page reads that field (D-437: lost exactly one)", async () => {
  const { doc, sb } = jsdom('<div id="root"><div id="app"><input aria-label="User">' +
    '<input type="password" aria-label="Password"></div></div>');
  sb.walk({ base: 1 });
  const n = tmpl(doc, '<div id="app"><h1>Home</h1><input aria-label="Search" value="cats"></div>');
  doc.getElementById("app").replaceWith(n);
  await tick();
  sb.walk({ dirty: true });
  same(byName(sb.index(), "Search").state.value, { kind: "text", text: "cats" });
});

test("a removed sensitive field whose keys cannot be read matches every field for 2 s (D-438 (3))", async () => {
  const CANARY = "throw-canary-438";
  // the walk has marked k1 sensitive, so only fieldKeys reads its id (and throws)
  const boom = (el) => Object.defineProperty(el, "id", { configurable: true, get() { throw new Error("id"); } });
  const cases = {
    // added after the removal (remembered removal) and before it (remembered add), elsewhere
    // and with nothing in common
    after: async (j) => {
      boom(j.$("k1")).remove();
      await tick();
      j.advance(2000);
      const n = j.spy(j.mk('<input aria-label="Nickname">'));
      j.$("b5").appendChild(n);
      return [n];
    },
    before: async (j) => {
      const n = j.spy(j.mk('<input aria-label="Nickname">'));
      j.$("b5").appendChild(n);
      await tick();
      j.advance(2000);
      boom(j.$("k1")).remove();
      return [n];
    },
  };
  for (const [k, step] of Object.entries(cases)) {
    const r = await addFirst(CANARY, step);
    assert.equal(r.c.reads, 0, k);
    assert.ok(!r.blob.includes(CANARY), k + ": " + r.blob);
    same(r.states, [{ kind: "filled" }], k);
  }
  const late = await addFirst(CANARY, async (j) => {
    boom(j.$("k1")).remove();
    await tick();
    j.advance(2001);
    const n = j.mk('<input aria-label="Nickname" value="ada">');
    j.$("b5").appendChild(n);
    return [n];
  });
  same(late.states, [{ kind: "text", text: "ada" }]);
});

test("an hCaptcha challenge frame is never passive by URL; size=invisible counts only in a reCAPTCHA query (D-438 (4))", () => {
  const prompts = "<main><h2>Are you human?</h2><p>Press and hold the button to continue</p><button>Start the quiz</button></main>";
  const hc = (frame, size) => '<iframe title="Main content of the hCaptcha challenge" ' +
    'src="https://newassets.hcaptcha.com/captcha/v1/x/static/hcaptcha.html#frame=' + frame + "&amp;id=0&amp;sitekey=k" + size + '"></iframe>';
  const chRows = (html) => walked(prompts + html).sb.index().filter((r) => r.flags.challenge).map((r) => r.role + "|" + r.name);
  const all = ["heading|Are you human?", "group|Press and hold the button to continue", "iframe|Main content of the hCaptcha challenge"];
  // size=invisible in the hash, at body level and inside the invisible widget
  same(chRows("<div>" + hc("challenge", "&amp;size=invisible") + "</div>"), all, "hash");
  same(chRows('<div class="h-captcha" data-sitekey="k" data-size="invisible">' + hc("challenge", "&amp;size=invisible") + "</div>"), all, "in widget");
  // the invisible widget's own checkbox frame and a hidden challenge frame stay passive
  same(chRows('<div class="h-captcha" data-sitekey="k" data-size="invisible">' + hc("checkbox", "&amp;size=invisible") + "</div>"), [], "checkbox");
  same(chRows('<div style="display:none">' + hc("challenge", "") + "</div>"), [], "hidden");
  // a reCAPTCHA anchor is invisible by its query, not by its hash
  const rc = (q, h) => '<iframe title="reCAPTCHA" src="https://www.google.com/recaptcha/api2/anchor?k=k' + q + h + '"></iframe>';
  same(chRows(rc("&amp;size=invisible", "")), [], "query");
  assert.equal(chRows(rc("", "#hl=en&amp;size=invisible")).length, 3, "a hash is not the query");
});

// ------------------------------------------------------------------ D-439
test("a remove-first rebuild into the same parent inherits across batches within 2 s (D-439)", async () => {
  const CANARY = "removefirst-canary-439";
  // the new field shares no key with k2 (id, aria-label, type all differ)
  const plain = (j) => j.spy(j.mk('<input id="k2-plain" type="text" aria-label="Visible key">'));
  const cases = {
    "remove; await null; append": async (j) => {
      const n = plain(j);
      const b = await batches(j.win, j.doc, async () => {
        j.$("k2").remove();
        await null;
        j.$("w2").appendChild(n);
      });
      assert.equal(b, 2, "the microtask split delivered two batches");
      return [n];
    },
    "remove; later task at 2000 ms; append": async (j) => {
      const n = plain(j);
      j.$("k2").remove();
      await tick();
      j.advance(2000);
      j.$("w2").appendChild(n);
      return [n];
    },
    // the wrapper leaves the form and a new one enters it: the form lost one value field
    "wrapper out; later task; wrapper in": async (j) => {
      const w = j.mk('<div id="w2b"><input id="k2-plain" type="text" aria-label="Visible key"></div>');
      j.spy(w.firstElementChild);
      const next = j.$("w2").nextSibling;
      j.$("w2").remove();
      await tick();
      j.$("f").insertBefore(w, next);
      return [w.firstElementChild];
    },
    // a password with no id, name or label has no keys: only the parent rule can match it
    "keyless field; later task; append": async (j) => {
      j.$("w5").innerHTML = '<input type="password">';
      const old = j.spy(j.$("w5").firstElementChild);
      await tick();
      old.remove();
      await tick();
      const n = plain(j);
      j.$("w5").appendChild(n);
      return [n];
    },
  };
  for (const [k, step] of Object.entries(cases)) {
    const r = await addFirst(CANARY, step);
    assert.equal(r.c.reads, 0, k + ": a value getter was called");
    assert.ok(!r.blob.includes(CANARY), k + ": " + r.blob);
    same(r.states, [{ kind: "filled" }], k);
  }
  const controls = {
    "2001 ms": async (j) => {
      j.$("k2").remove();
      await tick();
      j.advance(2001);
      const n = j.mk('<input aria-label="Visible key" value="plain two">');
      j.$("w2").appendChild(n);
      return [n];
    },
    "a later batch adds two fields": async (j) => {
      j.$("k2").remove();
      await tick();
      const a = j.mk('<input aria-label="Street" value="Elm St">');
      const b = j.mk('<input aria-label="Town" value="Leeds">');
      j.$("w2").append(a, b);
      return [a, b];
    },
    // the first later batch that adds to the parent uses the entry up; the second field lands
    // next to the inherited (sensitive) one, so it reads only past the window (D-443 (4))
    "a second later field": async (j) => {
      j.$("k2").remove();
      await tick();
      j.$("w2").appendChild(plain(j));
      await tick();
      const n = j.mk('<input aria-label="Note" value="hello">');
      j.$("w2").appendChild(n);
      await tick();
      j.advance(2001);
      return [n];
    },
    // user + password removed from one parent: lost two, the rule is not armed
    "a login parent that lost two fields": async (j) => {
      j.$("w6").replaceChildren();
      await tick();
      const n = j.mk('<input aria-label="Search" value="cats">');
      j.$("w6").appendChild(n);
      return [n];
    },
  };
  const want = {
    "2001 ms": [{ kind: "text", text: "plain two" }],
    "a later batch adds two fields": [{ kind: "text", text: "Elm St" }, { kind: "text", text: "Leeds" }],
    "a second later field": [{ kind: "text", text: "hello" }],
    "a login parent that lost two fields": [{ kind: "text", text: "cats" }],
  };
  for (const [k, step] of Object.entries(controls)) {
    const r = await addFirst(CANARY, step);
    same(r.states, want[k], k);
    if (k === "a second later field") assert.equal(r.c.reads, 0, k + ": the first field's value getter was called");
  }
});

test("a getter that throws while a field is checked marks it sensitive and the batch goes on (D-439)", async () => {
  const CANARY = "throw-canary-439";
  // an own id getter that returns "" for the first `ok` reads, then throws (same JS world only;
  // a page cannot reach the isolated world's view of an element in Chromium)
  const boom = (el, ok) => {
    let n = 0;
    Object.defineProperty(el, "id", { configurable: true, get() { if (n++ >= ok) throw new Error("id"); return ""; } });
    return el;
  };
  const cases = {
    // sensitive() reads id: the added field's first check throws
    "added, sensitivity unreadable": async (j) => {
      const n = boom(j.spy(j.mk('<input aria-label="Nickname">')), 0);
      j.$("b5").appendChild(n);
      await tick();
      delete n.id;
      return [n];
    },
    // sensitive() reads id once, then fieldKeys reads it next to the sensitive k1 and throws
    "added, keys unreadable next to a sensitive field": async (j) => {
      const n = boom(j.spy(j.mk('<input aria-label="Nickname">')), 1);
      j.$("k1").after(n);
      await tick();
      delete n.id;
      return [n];
    },
    // a removed field whose sensitivity cannot be read counts as a sensitive one with unreadable
    // keys: the field added after it, elsewhere and with nothing in common, is sensitive
    "removed, sensitivity unreadable": async (j) => {
      const u = boom(j.$("u6"), 0);
      u.remove();
      await tick();
      delete u.id;
      const n = j.spy(j.mk('<input aria-label="Nickname">'));
      j.$("b5").appendChild(n);
      return [n];
    },
  };
  for (const [k, step] of Object.entries(cases)) {
    const r = await addFirst(CANARY, step);
    assert.equal(r.c.reads, 0, k);
    assert.ok(!r.blob.includes(CANARY), k + ": " + r.blob);
    same(r.states, [{ kind: "filled" }], k);
  }
  // the rest of the batch still runs: a rebuild of k1 added after the throwing field in the
  // same batch still matches k1's keys
  const r = await addFirst(CANARY, async (j) => {
    const bad = boom(j.mk('<input aria-label="Nickname">'), 0);
    const n = j.spy(j.mk('<input id="k1-copy" type="text" aria-label="Key one">'));
    j.$("b5").append(bad);
    j.$("k1").remove();
    j.$("b1").append(n);
    await tick();
    delete bad.id;
    return [bad, n];
  });
  assert.equal(r.c.reads, 0);
  // the throwing field holds no value: empty (sensitive), not { kind: "text", text: "" }
  same(r.states, [{ kind: "empty" }, { kind: "filled" }]);
});

// ------------------------------------------------------------------ D-442 (supersedes D-440, D-441)
// { kind: "masked" } in want: the field is marked sensitive and reads empty or filled (a value
// set by attribute leaves no caret or validity evidence, so it may read empty), never its text.
function sameMasked(states, want, m) {
  same(states.map((s, i) => (want[i] && want[i].kind === "masked" && (s.kind === "empty" || s.kind === "filled") ? { kind: "masked" } : s)), want, m);
}
test("a one-field swap within 2 s marks every field added to that parent, while loading and after load (D-442, B-415)", async () => {
  const CANARY = "swap-canary-442";
  // a parse-time insert as the observer sees it: the form gains a label holding #nick
  const nick = (j) => {
    const l = j.mk('<label>Nickname <input id="nick" name="nick" value="Ann"></label>');
    j.$("f").insertBefore(l, j.$("w1"));
    return l.firstElementChild;
  };
  const neutral = (j) => j.spy(j.mk('<input id="k2-plain" type="text" aria-label="Visible key">'));
  const wrapSwap = (j) => {
    const w = j.mk('<div id="w2b"></div>');
    const f = neutral(j);
    w.appendChild(f);
    j.$("w2").replaceWith(w);
    return f;
  };
  const cases = {
    // replaceWith of the password's wrapper: the form lost k2 (its only lost value field) and
    // gained the new field; #nick, added to the form 1.5 s earlier, is over-marked (B-415)
    "wrapper swap at 1500 ms, neighbour added while loading": async (j) => {
      assert.equal(j.doc.readyState, "loading");
      const n = nick(j);
      await tick();
      j.advance(1500);
      return [n, wrapSwap(j)];
    },
    "wrapper swap at 1500 ms, neighbour added after load": async (j) => {
      await tick();
      assert.notEqual(j.doc.readyState, "loading");
      const n = nick(j);
      await tick();
      j.advance(1500);
      return [n, wrapSwap(j)];
    },
    // remove + append into the parent the neighbour was added to, one batch (D-437)
    "remove + append next to a neighbour added while loading": async (j) => {
      assert.equal(j.doc.readyState, "loading");
      const n = j.mk('<input id="nick" name="nick" value="Ann">');
      j.$("w2").appendChild(n);
      await tick();
      j.advance(1999);
      const f = neutral(j);
      j.$("k2").remove();
      j.$("w2").appendChild(f);
      return [n, f];
    },
    "remove + append next to a neighbour added after load": async (j) => {
      await tick();
      assert.notEqual(j.doc.readyState, "loading");
      const n = j.spy(j.mk('<input id="nick" name="nick">'));
      j.$("w2").appendChild(n);
      await tick();
      j.advance(1999);
      const f = neutral(j);
      j.$("k2").remove();
      j.$("w2").appendChild(f);
      return [n, f];
    },
    // a remembered field that matches k2 by key inherits too
    "key copy elsewhere": async (j) => {
      const n = nick(j);
      const copy = j.spy(j.mk('<input id="k2-copy" type="text" aria-label="Key two">'));
      j.$("b1").appendChild(copy);
      await tick();
      return [n, copy, wrapSwap(j)];
    },
    // two fields added to the parent in one batch, then the removal (no swap): both inherit
    "two fields added, then removal": async (j) => {
      const a = j.mk('<input aria-label="Street" value="Elm St">');
      const b = j.mk('<input aria-label="Town" value="Leeds">');
      j.$("k2").after(a, b);
      await tick();
      j.$("k2").remove();
      return [a, b];
    },
    // the removing batch adds two fields: the earlier field added to the parent inherits
    // (D-438 (1)), and one sensitive field out with two in and no key match marks both new
    // fields too (D-443 (3), review round 7 N5)
    "add-first, then the removing batch adds two": async (j) => {
      const n = neutral(j);
      j.$("w2").appendChild(n);
      await tick();
      const a = j.mk('<input aria-label="Street" value="Elm St">');
      const b = j.mk('<input aria-label="Town" value="Leeds">');
      j.$("k2").remove();
      j.$("w2").append(a, b);
      return [n, a, b];
    },
  };
  const want = {
    "wrapper swap at 1500 ms, neighbour added while loading": [{ kind: "masked" }, { kind: "filled" }],
    "wrapper swap at 1500 ms, neighbour added after load": [{ kind: "masked" }, { kind: "filled" }],
    "remove + append next to a neighbour added while loading": [{ kind: "masked" }, { kind: "filled" }],
    "remove + append next to a neighbour added after load": [{ kind: "filled" }, { kind: "filled" }],
    "key copy elsewhere": [{ kind: "masked" }, { kind: "filled" }, { kind: "filled" }],
    "two fields added, then removal": [{ kind: "masked" }, { kind: "masked" }],
    "add-first, then the removing batch adds two": [{ kind: "filled" }, { kind: "masked" }, { kind: "masked" }],
  };
  for (const [k, step] of Object.entries(cases)) {
    const r = await addFirst(CANARY, step);
    assert.equal(r.c.reads, 0, k + ": a value getter was called");
    assert.ok(!r.blob.includes(CANARY), k + ": " + r.blob);
    sameMasked(r.states, want[k], k);
  }
  // controls: past the 2 s window the neighbour reads; a parent that lost two value fields
  // (an SPA login swap, D-435 (1)) does not mark the two fields added to it
  const controls = {
    "wrapper swap at 2001 ms": async (j) => {
      const n = nick(j);
      await tick();
      j.advance(2001);
      return [n, wrapSwap(j)];
    },
    "a login parent that lost two fields": async (j) => {
      const a = j.mk('<input aria-label="Street" value="Elm St">');
      const b = j.mk('<input aria-label="Town" value="Leeds">');
      j.$("k6").after(a, b);
      await tick();
      j.$("u6").remove();
      j.$("k6").remove();
      return [a, b];
    },
  };
  const wantC = {
    "wrapper swap at 2001 ms": [{ kind: "text", text: "Ann" }, { kind: "filled" }],
    "a login parent that lost two fields": [{ kind: "text", text: "Elm St" }, { kind: "text", text: "Leeds" }],
  };
  for (const [k, step] of Object.entries(controls)) {
    const r = await addFirst(CANARY, step);
    assert.equal(r.c.reads, 0, k + ": a value getter was called");
    same(r.states, wantC[k], k);
  }
});

// review-T-19 rounds 5 and 6 leak shapes (runs/reviewer-10/probe-r10-jsdom.js V7, V8;
// runs/reviewer-11/probe-r11-jsdom.js N1, N3, N3r). The password and every copy hold the canary;
// reads counts their value getter calls. "loading" steps run synchronously after the base walk.
async function leakShape(step, postload, body) {
  const CANARY = "shape-canary-442";
  const c = { reads: 0 };
  const j = jsdom(body || '<form><div id="w"><input id="acct" type="password" aria-label="Account key">' +
    '<button type="button">Show</button></div><div id="z"></div></form>');
  j.advance = fakeClock(j.win);
  j.mk = (html) => tmpl(j.doc, html);
  j.spy = (el) => spied(j.win, el, CANARY, c);
  j.$ = (id) => j.doc.getElementById(id);
  j.other = () => j.mk('<input id="hint" type="text" placeholder="Hint">');
  for (const el of j.doc.querySelectorAll('input[type="password"]')) j.spy(el);
  j.sb.walk({ base: 1 });
  j.sb.index();
  if (postload) {
    await tick();
    assert.notEqual(j.doc.readyState, "loading");
  } else assert.equal(j.doc.readyState, "loading");
  const fields = await step(j);
  await tick();
  j.sb.walk({ dirty: true });
  const states = fields.map((f) => j.sb.state(f).value);
  const all = Array.from(j.doc.querySelectorAll("input")).map((f) => j.sb.state(f));
  const blob = JSON.stringify([j.sb.index(), j.sb.text(), all]);
  return { c, states: JSON.parse(JSON.stringify(states)), blob, CANARY, j };
}

test("review leak shapes V7, N1, V8, N3, N3r read no value and emit no canary (D-442)", async () => {
  const v7 = async (j) => {
    const n = j.spy(j.mk('<input id="q9" type="text">'));
    j.$("acct").after(n);
    await tick();
    const o = j.other();
    j.$("acct").replaceWith(o);
    return [n, o];
  };
  const v8 = async (j) => {
    const n = j.spy(j.mk('<input id="q9" type="text">'));
    j.$("acct").after(n);
    await tick();
    const m = j.mk('<input id="q10" type="text" value="plain">');
    n.after(m);
    await tick();
    j.$("acct").remove();
    return [n, m];
  };
  const n3 = (swap) => async (j) => {
    const nk = j.mk('<input id="nick" name="nick" value="Ann">');
    j.$("w").appendChild(nk);
    await tick();
    assert.notEqual(j.doc.readyState, "loading");
    const n = j.spy(j.mk('<input id="q9" type="text">'));
    j.$("acct").after(n);
    await tick();
    if (!swap) {
      j.$("acct").remove();
      return [nk, n];
    }
    const o = j.other();
    j.$("acct").replaceWith(o);
    return [nk, n, o];
  };
  const cases = [
    // V7: a neutral copy next to the password, then replaceWith by a different field
    ["V7 while loading", false, v7, [{ kind: "filled" }, { kind: "empty" }]],
    ["V7 after a tick", true, v7, [{ kind: "filled" }, { kind: "empty" }]],
    // N1: the copy is added while loading, the swap comes after load
    ["N1 copy while loading, swap after load", false, async (j) => {
      const n = j.spy(j.mk('<input id="q9" type="text">'));
      j.$("acct").after(n);
      await tick();
      assert.notEqual(j.doc.readyState, "loading");
      const o = j.other();
      j.$("acct").replaceWith(o);
      return [n, o];
    }, [{ kind: "filled" }, { kind: "empty" }]],
    // V8: two fields added to the parent in two batches, then the password is removed
    ["V8 while loading", false, v8, [{ kind: "filled" }, { kind: "masked" }]],
    ["V8 after a tick", true, v8, [{ kind: "filled" }, { kind: "masked" }]],
    // N3 / N3r: a neighbour added while loading, a copy after load, then a swap / a removal
    ["N3 swap", false, n3(true), [{ kind: "masked" }, { kind: "filled" }, { kind: "empty" }]],
    ["N3r plain removal", false, n3(false), [{ kind: "masked" }, { kind: "filled" }]],
  ];
  for (const [k, postload, step, want] of cases) {
    const r = await leakShape(step, postload);
    assert.equal(r.c.reads, 0, k + ": a value getter was called");
    assert.ok(!r.blob.includes(r.CANARY), k + ": " + r.blob);
    sameMasked(r.states, want, k);
  }
});

// ------------------------------------------------------------------ D-441
test("an add-first rebuild with no shared key, after load, inherits when the removing batch swaps in another field (D-441)", async () => {
  const CANARY = "swap-canary-441";
  const cases = {
    // review-T-19 round 5 V7: a neutral copy after k2 holds its value; a later batch
    // replaceWith's k2 by a different field (a D-437 swap in the removing batch)
    "replaceWith": async (j) => {
      await tick();
      assert.notEqual(j.doc.readyState, "loading");
      const n = j.spy(j.mk('<input id="q9" type="text">'));
      j.$("k2").after(n);
      await tick();
      const other = j.mk('<input id="hint" type="text" placeholder="Hint">');
      j.$("k2").replaceWith(other);
      return [n, other];
    },
    // the same at the edge of the window, with the swap as remove + append
    "remove + append at 1999 ms": async (j) => {
      await tick();
      assert.notEqual(j.doc.readyState, "loading");
      const n = j.spy(j.mk('<input id="q9" type="text">'));
      j.$("k2").after(n);
      await tick();
      j.advance(1999);
      const other = j.mk('<input id="hint" type="text" placeholder="Hint">');
      j.$("k2").remove();
      j.$("w2").appendChild(other);
      return [n, other];
    },
  };
  for (const [k, step] of Object.entries(cases)) {
    const r = await addFirst(CANARY, step);
    assert.equal(r.c.reads, 0, k + ": a value getter was called");
    assert.ok(!r.blob.includes(CANARY), k + ": " + r.blob);
    for (const st of r.states) assert.ok(st.kind === "empty" || st.kind === "filled", k + ": " + JSON.stringify(st));
    same(r.states, [{ kind: "filled" }, { kind: "empty" }], k);
  }
});

// ------------------------------------------------------------------ D-443
// review-T-19 round 7: the overlap read (runs/reviewer-10/probe-r10-jsdom.js V3m), a password
// plus confirm rebuilt by one toggle (runs/reviewer-12/probe-r12b-jsdom.js X7, X7f), and one
// password out with two fields in, in one batch (runs/reviewer-11/probe-r11-jsdom.js N5).
// Every password and copy holds the canary; reads counts their value getter calls.
const PW2 = '<form id="f"><div id="o"><div id="w"><input id="p1" type="password"><input id="p2" type="password">' +
  '</div></div><input id="user" name="user" value="ada"></form>';
// every reader, while the page is in its current state (the overlap: both fields present)
function readNow(j) {
  j.sb.walk({ dirty: true });
  const all = Array.from(j.doc.querySelectorAll("input")).map((f) => j.sb.state(f));
  return JSON.stringify([j.sb.index(), j.sb.text(), all]);
}

test("a neutral copy added next to a live password is never read while both exist; rebuilt password + confirm and one-out-many-in inherit (D-443)", async () => {
  const cases = [
    // V3m: a neutral copy next to the password, read while both exist, then the removal
    ["V3m overlap read, while loading", false, undefined, async (j) => {
      const n = j.spy(j.mk('<input id="q9" type="text">'));
      j.$("acct").after(n);
      await tick();
      assert.ok(j.$("acct").isConnected);
      j.mid = [readNow(j), j.sb.state(n).value];
      j.$("acct").remove();
      return [n];
    }, [{ kind: "filled" }]],
    ["V3m overlap read, after load, at 1999 ms", true, undefined, async (j) => {
      const n = j.spy(j.mk('<input id="q9" type="text">'));
      j.$("acct").after(n);
      await tick();
      j.advance(1999);
      j.mid = [readNow(j), j.sb.state(n).value];
      j.$("acct").remove();
      return [n];
    }, [{ kind: "filled" }]],
    // X7: [pw, confirm] wrapper rebuilt add-first with neutral copies in a new wrapper
    ["X7 wrapper [pw, confirm]", true, PW2, async (j) => {
      const w2 = j.mk('<div id="w2"><input id="c1" type="text"><input id="c2" type="text"></div>');
      const a = j.spy(w2.querySelector("#c1"));
      const b = j.spy(w2.querySelector("#c2"));
      j.$("w").after(w2);
      await tick();
      j.mid = [readNow(j), j.sb.state(a).value, j.sb.state(b).value];
      j.$("w").remove();
      return [a, b];
    }, [{ kind: "filled" }, { kind: "filled" }]],
    // X7f: the same parent, a copy after each, both old fields removed in one batch
    ["X7f same parent [pw, confirm]", true, PW2, async (j) => {
      const a = j.spy(j.mk('<input id="c1" type="text">'));
      const b = j.spy(j.mk('<input id="c2" type="text">'));
      j.$("p1").after(a);
      j.$("p2").after(b);
      await tick();
      j.$("p1").remove();
      j.$("p2").remove();
      return [a, b];
    }, [{ kind: "filled" }, { kind: "filled" }]],
    // N5: one batch adds the copy and another field and replaces the password
    ["N5 one batch, two in, one out", true, undefined, async (j) => {
      const n = j.spy(j.mk('<input id="q9" type="text">'));
      const o = j.other();
      j.$("acct").after(n);
      j.$("acct").replaceWith(o);
      return [n, o];
    }, [{ kind: "filled" }, { kind: "masked" }]],
  ];
  for (const [k, postload, body, step, want] of cases) {
    const r = await leakShape(step, postload, body);
    assert.equal(r.c.reads, 0, k + ": a value getter was called");
    assert.ok(!r.blob.includes(r.CANARY), k + ": " + r.blob);
    sameMasked(r.states, want, k);
    if (r.j.mid) {
      assert.ok(!r.j.mid[0].includes(r.CANARY), k + " (overlap): " + r.j.mid[0]);
      for (const v of r.j.mid.slice(1)) assert.ok(v.kind === "empty" || v.kind === "filled", k + " (overlap): " + JSON.stringify(v));
    }
    // the ordinary field outside the rebuilt parent keeps reading
    if (body === PW2) same(r.j.sb.state(r.j.$("user")).value, { kind: "text", text: "ada" }, k + ": user");
  }
});

test("an SPA login swap that removes email and password and adds two fields in one batch still reads them (D-443 (2), (3), D-435 (1))", async () => {
  const CANARY = "spa-canary-443";
  const c = { reads: 0 };
  const { doc, win, sb } = jsdom('<form id="f"><input id="u" aria-label="Email"><input id="p" type="password" aria-label="Key">' +
    '<input type="hidden" name="csrf" value="t"></form><div id="z"><input id="p9" type="password" aria-label="Other key"></div>');
  spied(win, doc.getElementById("p"), CANARY, c);
  sb.walk({ base: 1 });
  sb.index();
  await tick();
  // the same parent loses a value field that is not sensitive too: neither D-443 (2) nor (3) fires
  doc.getElementById("f").replaceChildren(tmpl(doc, '<input aria-label="Display name" value="Ada Lovelace">'),
    tmpl(doc, '<input aria-label="City" value="London">'));
  await tick();
  sb.walk({ dirty: true });
  const rows = sb.index();
  assert.equal(c.reads, 0);
  assert.ok(!JSON.stringify([rows, sb.text()]).includes(CANARY));
  same(byName(rows, "Display name").state.value, { kind: "text", text: "Ada Lovelace" });
  same(byName(rows, "City").state.value, { kind: "text", text: "London" });
});

// ------------------------------------------------------------------ D-444
// review-T-19 round 8 (runs/reviewer-13/probe-r13-jsdom.js Y3, Y3d, Y5): a password moved or
// re-attached in the copy's batch counts as present for the overlap mask (D-444 (2)); the
// one-out-many-in key match ignores a new sensitive field (D-444 (3)). Every password and copy
// holds the canary; reads counts their value getter calls, mid reads included.
test("a password moved or re-attached in the copy's batch, and a new password with the old key, never let the copy read (D-444 (2), (3))", async () => {
  const keyed = (hidden) => async (j) => {
    const n = j.spy(j.mk('<input id="q9" type="text">'));
    const p2 = j.spy(j.mk('<input id="acct2" type="password" aria-label="Account key"' + (hidden ? ' style="display:none"' : "") + ">"));
    j.$("acct").replaceWith(n, p2);
    await tick();
    j.mid = [readNow(j), j.sb.state(n).value];
    return [n];
  };
  const cases = [
    // Y3: one batch appends the copy and moves the password after it (w.append(copy, pw))
    ["Y3 append(copy, pw) in the password's parent", async (j) => {
      const n = j.spy(j.mk('<input id="q9" type="text">'));
      j.$("w").append(n, j.$("acct"));
      await tick();
      j.mid = [readNow(j), j.sb.state(n).value];
      j.$("acct").remove();
      return [n];
    }, [{ kind: "filled" }]],
    // Y3x: the same move into another parent (the removal and the add have different targets)
    ["Y3x append(copy, pw) into another parent", async (j) => {
      const n = j.spy(j.mk('<input id="q9" type="text">'));
      j.$("z").append(n, j.$("acct"));
      await tick();
      j.mid = [readNow(j), j.sb.state(n).value];
      j.$("acct").remove();
      return [n];
    }, [{ kind: "filled" }]],
    // Y3d: the password detached in an earlier batch, re-attached in the copy's batch
    ["Y3d pw detached, then one batch adds the copy and re-attaches pw", async (j) => {
      const pw = j.$("acct");
      pw.remove();
      await tick();
      const n = j.spy(j.mk('<input id="q9" type="text">'));
      j.$("w").append(n, pw);
      await tick();
      j.mid = [readNow(j), j.sb.state(n).value];
      pw.remove();
      return [n];
    }, [{ kind: "filled" }]],
    // Y5: one batch replaces the password with [copy, a display:none password with its label]
    ["Y5 pw -> [copy, hidden new pw, same label]", keyed(true), [{ kind: "masked" }]],
    // Y5v: the same with a visible new password (only D-444 (3) applies)
    ["Y5v pw -> [copy, visible new pw, same label]", keyed(false), [{ kind: "masked" }]],
  ];
  for (const postload of [false, true]) {
    for (const [name, step, want] of cases) {
      const k = name + (postload ? ", after load" : ", while loading");
      const r = await leakShape(step, postload);
      assert.equal(r.c.reads, 0, k + ": a value getter was called");
      assert.ok(!r.blob.includes(r.CANARY), k + ": " + r.blob);
      sameMasked(r.states, want, k);
      assert.ok(!r.j.mid[0].includes(r.CANARY), k + " (mid): " + r.j.mid[0]);
      assert.ok(r.j.mid[1].kind === "empty" || r.j.mid[1].kind === "filled", k + " (mid): " + JSON.stringify(r.j.mid[1]));
    }
  }
});

// D-444 (1) in jsdom (no layout: displayed() walks computed display): a value field next to a
// hidden password in its parent, or alone in a wrapper beside a wrapper holding only a hidden
// password, reads empty|filled (the accepted cost); next to a visible password it reads.
test("a field next to a hidden password reads empty|filled; next to a visible one it reads (D-444 (1))", () => {
  const CANARY = "twin-canary-444";
  const c = { reads: 0 };
  const { doc, win, sb } = jsdom(
    '<form id="a"><div><input id="e1" aria-label="Email one" value="a@x.io"><input id="p1" type="password" style="display:none"></div></form>' +
    '<form id="b"><div><div><input id="e2" aria-label="Email two" value="b@x.io"></div><div hidden><input id="p2" type="password"></div></div></form>' +
    '<form id="c"><div><div><input id="e3" aria-label="Email three" value="c@x.io"></div><div><input id="p3" type="password"></div></div>' +
    '<div><input id="e4" aria-label="Email four" value="d@x.io"><input id="n4" aria-label="Note" style="display:none" value="n"></div></form>' +
    '<form id="d"><div><div><input id="e5" aria-label="Email five" value="e@x.io"><input id="e6" aria-label="Name" value="Ada"></div>' +
    '<div hidden><input id="p5" type="password"></div></div></form>');
  for (const id of ["p1", "p2", "p3", "p5"]) spied(win, doc.getElementById(id), CANARY, c);
  sb.walk({ base: 1 });
  const v = (id) => JSON.parse(JSON.stringify(sb.state(doc.getElementById(id)).value));
  const masked = (s) => s.kind === "empty" || s.kind === "filled";
  assert.ok(masked(v("e1")), "same parent as a hidden password: " + JSON.stringify(v("e1")));
  assert.ok(masked(v("e2")), "alone in a wrapper beside a hidden password's wrapper: " + JSON.stringify(v("e2")));
  same(v("e3"), { kind: "text", text: "c@x.io" }, "beside a visible password's wrapper");
  same(v("e4"), { kind: "text", text: "d@x.io" }, "next to a hidden field that is not sensitive");
  // a wrapper with two value fields is not a twin wrapper (D-444 (1) wants each alone)
  same(v("e5"), { kind: "text", text: "e@x.io" }, "a two-field wrapper beside a hidden password");
  same(v("e6"), { kind: "text", text: "Ada" });
  assert.equal(c.reads, 0);
  assert.ok(!JSON.stringify([sb.index(), sb.text()]).includes(CANARY));
});
