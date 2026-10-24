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
